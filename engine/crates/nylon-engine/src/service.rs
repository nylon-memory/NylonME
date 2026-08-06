//! MemoryEngine gRPC 服务实现（proto/nylon/v1/memory.proto）。
//!
//! Phase 1 单节点简化语义（后续里程碑逐步替换）：
//! - node_id 即图内局部 ID（单分片），重启后由快照/WAL 保持单调；
//! - Weave 的多丝分解为启发式：fact=原文、关系丝取自 context.task、
//!   置信丝默认 0.8；同 owner 且关系丝重叠的历史节点自动建边（最多 3 条）；
//! - 冲突检测依赖语义推理模型，Phase 1 恒返回空；
//! - Resonate 种子：query 子串命中事实丝 > context.task 命中关系丝 > 最近节点兜底；
//! - Search 走 HNSW，查询向量逐维度截断/补零到索引维度。

use nylon_core::{compute_tension, Filaments, MemoryNode, Tension};
use nylon_graph::{ContextSpectrum, FilamentFilter};
use nylon_storage::PersistentGraph;
use nylon_vector::{HnswIndex, VectorIndex};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tonic::{Request, Response, Status};

pub mod pb {
    tonic::include_proto!("nylon.v1");
}

use pb::memory_engine_server::MemoryEngine;
use pb::{
    ActivatedNode, GetNodeRequest, GetNodeResponse, ResonateRequest, ResonateResponse,
    SearchRequest, SearchResponse, WeaveRequest, WeaveResponse,
};

/// 默认嵌入维度（bge-small 类模型），可用 NYLON_EMBED_DIMS 覆盖。
pub const DEFAULT_EMBED_DIMS: usize = 384;
/// Resonate 种子数量上限。
const MAX_SEEDS: usize = 8;
/// Weave 自动建边上限。
const MAX_AUTO_LINKS: usize = 3;
/// 自动建边权重（Phase 1 固定值，后续由相似度决定）。
const AUTO_LINK_WEIGHT: f32 = 0.5;

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
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

fn to_activated(local: u32, score: f32, node: &MemoryNode) -> ActivatedNode {
    ActivatedNode { node_id: local as u64, resonance: score, filaments: Some(to_pb_filaments(&node.filaments)) }
}

fn to_context(ctx: Option<pb::ContextSpectrum>) -> ContextSpectrum {
    ctx.map(|c| ContextSpectrum { task: c.task, emotion_valence: c.emotion_valence }).unwrap_or_default()
}

struct Inner {
    store: PersistentGraph,
    index: HnswIndex,
}

/// MemoryEngine 服务句柄（内部状态互斥保护，Phase 1 单写者够用）。
pub struct EngineService {
    inner: Mutex<Inner>,
}

impl EngineService {
    pub fn new(store: PersistentGraph, embed_dims: usize) -> Self {
        EngineService { inner: Mutex::new(Inner { store, index: HnswIndex::new(embed_dims) }) }
    }
}

#[tonic::async_trait]
impl MemoryEngine for EngineService {
    async fn weave(&self, req: Request<WeaveRequest>) -> Result<Response<WeaveResponse>, Status> {
        let r = req.into_inner();
        if r.tenant_id.is_empty() || r.owner_id.is_empty() || r.raw_event.is_empty() {
            return Err(Status::invalid_argument("tenant_id / owner_id / raw_event 均不能为空"));
        }
        let mut inner = self.inner.lock().map_err(|_| Status::internal("state lock poisoned"))?;
        let now = now_secs();
        let ctx = to_context(r.context);
        let relations: Vec<String> = ctx.task.clone().into_iter().collect();
        let node = MemoryNode {
            id: 0, // 全局 ID 分配在 Phase 2 接入；node_id 暂用局部 ID
            owner_id: r.owner_id.clone(),
            filaments: Filaments {
                fact: r.raw_event.clone(),
                emotion_valence: ctx.emotion_valence.unwrap_or(0.0),
                emotion_intensity: 0.5,
                created_at: now,
                decay_rate: 0.01,
                relations: relations.clone(),
                confidence: 0.8,
                mentions_7d: 1,
            },
            tension: Tension { baseline: 1.0, last_updated: now },
            embedding: Vec::new(),
        };
        let local = inner.store.add_node(node).map_err(|e| Status::internal(format!("wal append: {e}")))?;

        // 自动建边：同 owner 且关系丝重叠的存活历史节点
        let mut linked = Vec::new();
        if !relations.is_empty() {
            let candidates = inner.store.graph().find_by_filaments(&FilamentFilter {
                relations_any: Some(relations.clone()),
                ..Default::default()
            });
            for cand in candidates {
                if cand == local || linked.len() >= MAX_AUTO_LINKS {
                    continue;
                }
                let is_same_owner = inner
                    .store
                    .graph()
                    .get_node(cand)
                    .map(|n| n.owner_id == r.owner_id)
                    .unwrap_or(false);
                if is_same_owner {
                    inner.store.add_edge(local, cand, AUTO_LINK_WEIGHT).map_err(|e| Status::internal(format!("wal append: {e}")))?;
                    linked.push(cand as u64);
                }
            }
        }
        Ok(Response::new(WeaveResponse { node_id: local as u64, linked_nodes: linked, conflict_nodes: vec![] }))
    }

    async fn resonate(&self, req: Request<ResonateRequest>) -> Result<Response<ResonateResponse>, Status> {
        let r = req.into_inner();
        if r.tenant_id.is_empty() || r.owner_id.is_empty() {
            return Err(Status::invalid_argument("tenant_id / owner_id 不能为空"));
        }
        let inner = self.inner.lock().map_err(|_| Status::internal("state lock poisoned"))?;
        let g = inner.store.graph();
        let ctx = to_context(r.context);
        let query = r.query.to_lowercase();

        // 种子选择：query 子串命中事实丝 > task 命中关系丝 > 最近节点兜底
        let mut seeds: Vec<u32> = Vec::new();
        if !query.is_empty() {
            seeds = g
                .live_nodes()
                .filter(|(_, n)| n.owner_id == r.owner_id && n.filaments.fact.to_lowercase().contains(&query))
                .map(|(id, _)| id)
                .collect();
        }
        if seeds.is_empty() {
            if let Some(task) = &ctx.task {
                seeds = g
                    .find_by_filaments(&FilamentFilter { relations_any: Some(vec![task.clone()]), ..Default::default() })
                    .into_iter()
                    .filter(|&id| g.get_node(id).map(|n| n.owner_id == r.owner_id).unwrap_or(false))
                    .collect();
            }
        }
        if seeds.is_empty() {
            let mut recent: Vec<(u32, i64)> = g
                .live_nodes()
                .filter(|(_, n)| n.owner_id == r.owner_id)
                .map(|(id, n)| (id, n.filaments.created_at))
                .collect();
            recent.sort_by_key(|&(_, ts)| std::cmp::Reverse(ts));
            seeds = recent.into_iter().map(|(id, _)| id).collect();
        }
        seeds.truncate(MAX_SEEDS);

        let budget = if r.budget == 0 { nylon_graph::DEFAULT_BUDGET } else { r.budget as usize };
        let activated = g.resonate(&seeds, &ctx, now_secs(), budget);
        let out = activated
            .into_iter()
            .filter_map(|(id, score)| g.get_node(id).map(|n| to_activated(id, score, n)))
            .collect();
        Ok(Response::new(ResonateResponse { activated: out }))
    }

    async fn search(&self, req: Request<SearchRequest>) -> Result<Response<SearchResponse>, Status> {
        let r = req.into_inner();
        if r.tenant_id.is_empty() || r.owner_id.is_empty() {
            return Err(Status::invalid_argument("tenant_id / owner_id 不能为空"));
        }
        if r.query_embedding.len() % 4 != 0 {
            return Err(Status::invalid_argument("query_embedding 必须是 f32 小端字节序列（长度被 4 整除）"));
        }
        let inner = self.inner.lock().map_err(|_| Status::internal("state lock poisoned"))?;
        let dims = inner.index.dims();
        let raw: Vec<f32> = r.query_embedding.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        // 维度对齐：截断或补零（Phase 1 宽容策略，避免维度演进期硬失败）
        let mut query = vec![0.0f32; dims];
        for (i, v) in raw.iter().take(dims).enumerate() {
            query[i] = *v;
        }
        let k = if r.top_k == 0 { 10 } else { r.top_k as usize };
        let g = inner.store.graph();
        let out = inner
            .index
            .search(&query, k)
            .into_iter()
            .filter_map(|(id, sim)| g.get_node(id).map(|n| to_activated(id, sim, n)))
            .collect();
        Ok(Response::new(SearchResponse { neighbors: out }))
    }

    async fn get_node(&self, req: Request<GetNodeRequest>) -> Result<Response<GetNodeResponse>, Status> {
        let r = req.into_inner();
        if r.tenant_id.is_empty() {
            return Err(Status::invalid_argument("tenant_id 不能为空"));
        }
        let inner = self.inner.lock().map_err(|_| Status::internal("state lock poisoned"))?;
        let local = u32::try_from(r.node_id).map_err(|_| Status::invalid_argument("node_id 超出局部 ID 范围"))?;
        let node = inner.store.graph().get_node(local).ok_or_else(|| Status::not_found(format!("node {} 不存在或已删除", r.node_id)))?;
        let tension = compute_tension(node, now_secs(), 1.0);
        Ok(Response::new(GetNodeResponse {
            node_id: r.node_id,
            filaments: Some(to_pb_filaments(&node.filaments)),
            current_tension: tension,
        }))
    }
}
