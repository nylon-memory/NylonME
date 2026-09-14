//! MemoryEngine gRPC 服务实现（proto/nylon/v1/memory.proto）。
//!
//! 逻辑主体在 nylon-service 的 [`EngineCore`]（与移动端共用）；本文件只做：
//! pb 请求/响应转换、API key 鉴权（L2.2）、审计（L2.3）、反思 tokio 队列编排。

use std::sync::Arc;

use nylon_core::Filaments;
use nylon_embed::Embedder;
use nylon_graph::ContextSpectrum;
use nylon_llm::ChatModel;
use nylon_service::{
    reflect_idle_duration, ActivatedCore, CoreError, EngineCore, NodeSummary, SessionEventIn,
    WeaveLlmMode,
};
use nylon_storage::PersistentGraph;
use tonic::{Request, Response, Status};

use crate::audit::Audit;
use crate::auth::{authorize, ApiKeys, KeyGrant, Scope};

pub mod pb {
    tonic::include_proto!("nylon.v1");
}

use pb::memory_engine_server::MemoryEngine;
use pb::{
    ActivatedNode, EventNode, FactNode, GetNodeRequest, GetNodeResponse, ResonateRequest,
    ResonateResponse, SearchRequest, SearchResponse, WeaveRequest, WeaveResponse,
    WeaveSessionRequest, WeaveSessionResponse,
};

pub use nylon_service::EngineStats;
pub use nylon_service::DEFAULT_EMBED_DIMS;

/// core 错误 -> gRPC Status 映射。
fn to_status(e: CoreError) -> Status {
    match e {
        CoreError::Invalid(m) => Status::invalid_argument(m),
        CoreError::Internal(m) => Status::internal(m),
        CoreError::NotFound(m) => Status::not_found(m),
    }
}

fn to_pb_filaments(f: &Filaments) -> pb::Filaments {
    pb::Filaments {
        fact: f.fact.clone(),
        emotion_valence: f.emotion_valence,
        emotion_intensity: f.emotion_intensity,
        created_at: f.created_at,
        decay_rate: f.decay_rate,
        relations: f.relations.clone(),
        confidence: f.confidence,
        mentions_7d: f.mentions_7d,
    }
}

/// 激活节点 -> pb（pb 契约无 tension 字段，current_tension 仅 get_node 返回）。
fn to_activated(n: &ActivatedCore) -> ActivatedNode {
    ActivatedNode {
        node_id: n.node_id,
        resonance: n.resonance,
        filaments: Some(to_pb_filaments(&n.filaments)),
    }
}

fn to_context(ctx: Option<pb::ContextSpectrum>) -> Option<ContextSpectrum> {
    ctx.map(|c| ContextSpectrum {
        task: c.task,
        emotion_valence: c.emotion_valence,
        max_hops: c.max_hops,
    })
}

/// MemoryEngine 服务句柄：鉴权/审计/反思编排 + pb 适配，状态与逻辑全在 EngineCore。
#[derive(Clone)]
pub struct EngineService {
    core: EngineCore,
    /// 空闲反思队列：异步补常识桥接节点。
    reflect_tx: Option<tokio::sync::mpsc::UnboundedSender<nylon_service::ReflectionJob>>,
    /// API key 鉴权（L2.2）：None = 开放模式（单机默认）。
    auth: Option<Arc<ApiKeys>>,
    /// 审计事件流（L2.3）：None = 关闭（NYLON_AUDIT=off 或未挂接）。
    audit: Option<Audit>,
}

impl EngineService {
    pub fn new(
        store: PersistentGraph,
        embed_dims: usize,
        embedder: Option<Arc<dyn Embedder>>,
        llm: Option<Arc<dyn ChatModel>>,
    ) -> Self {
        let core = EngineCore::new(store, embed_dims, embedder, llm);
        // 反思 worker：空闲窗口批量处理（NYLON_REFLECT_IDLE_SECS，默认 600s）
        let reflect_tx = core.llm().clone().map(|_llm| {
            let (tx, mut rx) =
                tokio::sync::mpsc::unbounded_channel::<nylon_service::ReflectionJob>();
            let core = core.clone();
            tokio::spawn(async move {
                let mut pending: Vec<nylon_service::ReflectionJob> = Vec::new();
                loop {
                    match tokio::time::timeout(reflect_idle_duration(), rx.recv()).await {
                        Ok(Some(job)) => pending.push(job),
                        Ok(None) => {
                            if !pending.is_empty() {
                                let jobs = std::mem::take(&mut pending);
                                nylon_service::process_reflection_jobs(&core, jobs).await;
                            }
                            break;
                        }
                        Err(_) => {
                            if !pending.is_empty() {
                                let jobs = std::mem::take(&mut pending);
                                nylon_service::process_reflection_jobs(&core, jobs).await;
                            }
                        }
                    }
                }
            });
            tx
        });
        EngineService {
            core,
            reflect_tx,
            auth: None,
            audit: None,
        }
    }

    /// 启用 API key 鉴权（gRPC 拦截器与 HTTP 网关共用同一张 key 表）。
    pub fn with_auth(mut self, auth: Option<Arc<ApiKeys>>) -> Self {
        self.auth = auth;
        self
    }

    /// 挂接审计事件流（serve / MCP 内嵌模式由 main.rs 按数据目录启动）。
    pub fn with_audit(mut self, audit: Option<Audit>) -> Self {
        self.audit = audit;
        self
    }

    /// 审计查询入口（REST /v1/audit）。
    pub(crate) fn audit(&self) -> Option<&Audit> {
        self.audit.as_ref()
    }

    /// 记录一次操作（审计关闭时为零开销空调用）。
    fn audit_op(&self, action: &str, tenant: &str, owner: &str, detail: String) {
        if let Some(a) = &self.audit {
            a.emit(action, tenant, owner, detail);
        }
    }

    /// 鉴权 + 审计一体：拒绝时先落一条 denied 事件再返回错误（L2.3）。
    fn check(
        &self,
        grant: Option<&KeyGrant>,
        scope: Scope,
        tenant: &str,
        action: &str,
        owner: &str,
    ) -> Result<(), Status> {
        if let Err(s) = authorize(grant, scope, tenant) {
            self.audit_op(
                "denied",
                tenant,
                owner,
                format!("{action}: {}", s.message()),
            );
            return Err(s);
        }
        Ok(())
    }

    /// 鉴权配置（HTTP 网关读取；gRPC 侧由 main.rs 拦截器使用同一配置）。
    pub(crate) fn auth(&self) -> &Option<Arc<ApiKeys>> {
        &self.auth
    }

    /// 分页列出节点（按创建时间倒序），按 tenant 强制过滤，可选叠加 owner 过滤。
    pub(crate) fn list_nodes(
        &self,
        tenant: &str,
        owner: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<(usize, Vec<NodeSummary>), Status> {
        self.core
            .list_nodes(tenant, owner, offset, limit)
            .map_err(to_status)
    }

    /// 引擎运行统计。
    pub(crate) fn stats(&self) -> Result<EngineStats, Status> {
        Ok(self.core.stats())
    }

    /// 手动 checkpoint：快照落盘 + 截断 WAL（L2.4 备份前置步骤）。
    /// serve 模式另有周期任务自动 checkpoint（NYLON_CHECKPOINT_SECS，默认 600s）。
    pub(crate) fn checkpoint(&self) -> Result<(), Status> {
        self.core.checkpoint().map_err(to_status)?;
        self.audit_op("checkpoint", "", "", "snapshot + wal truncate".into());
        Ok(())
    }
}

#[tonic::async_trait]
impl MemoryEngine for EngineService {
    async fn weave(&self, req: Request<WeaveRequest>) -> Result<Response<WeaveResponse>, Status> {
        let grant = req.extensions().get::<KeyGrant>().cloned();
        let r = req.into_inner();
        self.check(
            grant.as_ref(),
            Scope::Write,
            &r.tenant_id,
            "weave",
            &r.owner_id,
        )?;
        let out = self
            .core
            .weave_one(
                &r.tenant_id,
                &r.owner_id,
                &r.raw_event,
                to_context(r.context),
                WeaveLlmMode::Full,
            )
            .await
            .map_err(to_status)?;
        self.audit_op(
            "weave",
            &r.tenant_id,
            &r.owner_id,
            format!(
                "node={} linked={} conflicts={}",
                out.node_id,
                out.linked_nodes.len(),
                out.conflict_nodes.len()
            ),
        );
        Ok(Response::new(WeaveResponse {
            node_id: out.node_id,
            linked_nodes: out.linked_nodes,
            conflict_nodes: out.conflict_nodes,
        }))
    }

    /// 双层写入：叶子层=逐事件原文入库；抽象层=引擎侧 LLM session 分解出原子事实。
    async fn weave_session(
        &self,
        req: Request<WeaveSessionRequest>,
    ) -> Result<Response<WeaveSessionResponse>, Status> {
        let grant = req.extensions().get::<KeyGrant>().cloned();
        let r = req.into_inner();
        self.check(
            grant.as_ref(),
            Scope::Write,
            &r.tenant_id,
            "weave_session",
            &r.owner_id,
        )?;
        let events: Vec<SessionEventIn> = r
            .events
            .iter()
            .filter(|e| !e.text.is_empty())
            .map(|e| SessionEventIn {
                event_id: e.event_id.clone(),
                speaker: e.speaker.clone(),
                text: e.text.clone(),
            })
            .collect();
        let out = self
            .core
            .weave_session(&r.tenant_id, &r.owner_id, events, r.skip_abstract)
            .await
            .map_err(to_status)?;
        if let Some(job) = out.reflection {
            if let Some(tx) = &self.reflect_tx {
                if tx.send(job).is_err() {
                    eprintln!("[weave_session] reflection worker 已关闭，跳过异步常识桥接");
                }
            }
        }
        self.audit_op(
            "weave_session",
            &r.tenant_id,
            &r.owner_id,
            format!("leaves={} facts={}", out.leaf_nodes.len(), out.fact_nodes.len()),
        );
        Ok(Response::new(WeaveSessionResponse {
            leaf_nodes: out
                .leaf_nodes
                .into_iter()
                .map(|l| EventNode {
                    event_id: l.event_id,
                    node_id: l.node_id,
                })
                .collect(),
            fact_nodes: out
                .fact_nodes
                .into_iter()
                .map(|f| FactNode {
                    node_id: f.node_id,
                    fact: f.fact,
                    source_event_ids: f.source_event_ids,
                })
                .collect(),
        }))
    }

    async fn resonate(
        &self,
        req: Request<ResonateRequest>,
    ) -> Result<Response<ResonateResponse>, Status> {
        let grant = req.extensions().get::<KeyGrant>().cloned();
        let r = req.into_inner();
        self.check(
            grant.as_ref(),
            Scope::Read,
            &r.tenant_id,
            "resonate",
            &r.owner_id,
        )?;
        let out = self
            .core
            .resonate(
                &r.tenant_id,
                &r.owner_id,
                &r.query,
                to_context(r.context),
                r.budget,
            )
            .await
            .map_err(to_status)?;
        self.audit_op(
            "resonate",
            &r.tenant_id,
            &r.owner_id,
            format!(
                "query={:.80} hits={} seeds={}",
                r.query,
                out.activated.len(),
                out.seed_ids.len()
            ),
        );
        Ok(Response::new(ResonateResponse {
            activated: out.activated.iter().map(to_activated).collect(),
            seed_ids: out.seed_ids,
        }))
    }

    async fn search(
        &self,
        req: Request<SearchRequest>,
    ) -> Result<Response<SearchResponse>, Status> {
        let grant = req.extensions().get::<KeyGrant>().cloned();
        let r = req.into_inner();
        self.check(
            grant.as_ref(),
            Scope::Read,
            &r.tenant_id,
            "search",
            &r.owner_id,
        )?;
        if r.tenant_id.is_empty() || r.owner_id.is_empty() {
            return Err(Status::invalid_argument("tenant_id / owner_id 不能为空"));
        }
        if !r.query_embedding.len().is_multiple_of(4) {
            return Err(Status::invalid_argument(
                "query_embedding 必须是 f32 小端字节序列（长度被 4 整除）",
            ));
        }
        let raw: Vec<f32> = r
            .query_embedding
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        let neighbors = self
            .core
            .search(&r.tenant_id, &r.owner_id, &raw, r.top_k)
            .map_err(to_status)?;
        self.audit_op(
            "search",
            &r.tenant_id,
            &r.owner_id,
            format!("top_k={} hits={}", r.top_k, neighbors.len()),
        );
        Ok(Response::new(SearchResponse {
            neighbors: neighbors.iter().map(to_activated).collect(),
        }))
    }

    async fn get_node(
        &self,
        req: Request<GetNodeRequest>,
    ) -> Result<Response<GetNodeResponse>, Status> {
        let grant = req.extensions().get::<KeyGrant>().cloned();
        let r = req.into_inner();
        self.check(grant.as_ref(), Scope::Read, &r.tenant_id, "get_node", "")?;
        if r.tenant_id.is_empty() {
            return Err(Status::invalid_argument("tenant_id 不能为空"));
        }
        let out = match self.core.get_node(&r.tenant_id, r.node_id) {
            Ok(o) => o,
            Err(CoreError::NotFound(m)) => {
                self.audit_op(
                    "get_node",
                    &r.tenant_id,
                    "",
                    format!("node={} miss", r.node_id),
                );
                return Err(Status::not_found(m));
            }
            Err(e) => return Err(to_status(e)),
        };
        self.audit_op(
            "get_node",
            &r.tenant_id,
            &out.owner_id,
            format!("node={}", r.node_id),
        );
        Ok(Response::new(GetNodeResponse {
            node_id: r.node_id,
            filaments: Some(to_pb_filaments(&out.filaments)),
            current_tension: out.current_tension,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nylon_embed::StubEmbedder;

    fn svc_with_embed(dims: usize) -> EngineService {
        let dir = tempfile::tempdir().unwrap();
        let store = PersistentGraph::open(dir.path()).unwrap();
        let embedder = Arc::new(StubEmbedder::new(dims));
        // tempdir 借 service 的 PersistentGraph 存活不够——泄漏句柄换取测试期有效
        std::mem::forget(dir);
        EngineService::new(store, dims, Some(embedder), None)
    }

    /// L2.2：grant 与请求体租户不匹配时拒绝（gRPC 拦截器之后的 handler 比对）。
    #[tokio::test]
    async fn grant_tenant_mismatch_rejected() {
        let svc = svc_with_embed(64);
        let mut req = Request::new(WeaveRequest {
            tenant_id: "tenant-b".into(),
            owner_id: "alice".into(),
            raw_event: "越权写入".into(),
            context: None,
        });
        req.extensions_mut().insert(KeyGrant {
            tenant: "tenant-a".into(),
            scope: crate::auth::Scope::Write,
        });
        let err = svc.weave(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }
}
