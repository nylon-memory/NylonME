//! MemoryEngine gRPC 服务实现（proto/nylon/v1/memory.proto）。
//!
//! Phase 1 单节点简化语义（后续里程碑逐步替换）：
//! - node_id 即图内局部 ID（单分片），重启后由快照/WAL 保持单调；
//! - Weave 的多丝分解为启发式：fact=原文、关系丝取自 context.task、
//!   置信丝默认 0.8；同 owner 且关系丝重叠的历史节点自动建边（最多 3 条）；
//! - 冲突检测依赖语义推理模型，Phase 1 恒返回空；
//! - Resonate 种子：query 词项重叠打分（子串命中加权） > task 命中关系丝 > 最近节点兜底；
//! - Search 走 HNSW，查询向量逐维度截断/补零到索引维度。

use nylon_core::{compute_tension, Filaments, MemoryNode, Tension};
use nylon_embed::Embedder;
use nylon_llm::ChatModel;
use std::sync::Arc;
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
/// 向量种子保底名额：语义通道的召回兜底，防止被词面种子挤占。
const VEC_SEED_QUOTA: usize = 4;
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
    /// 嵌入通道：None 时退回 Phase 1 行为（无向量写入、无向量种子）。
    embedder: Option<Arc<dyn Embedder>>,
    /// LLM ch: None shi guan bi bian zhi fen jie yu chong tu jian ce.
    llm: Option<Arc<dyn ChatModel>>,
}

impl EngineService {
    pub fn new(store: PersistentGraph, embed_dims: usize, embedder: Option<Arc<dyn Embedder>>, llm: Option<Arc<dyn ChatModel>>) -> Self {
        EngineService { inner: Mutex::new(Inner { store, index: HnswIndex::new(embed_dims) }), embedder, llm }
    }
}


/// LLM weave decomposition: extract six-filament structure from raw event.
/// Returns None when LLM unavailable/fails, caller falls back to heuristic decomposition.
async fn decompose(llm: &dyn ChatModel, raw_event: &str) -> Option<(String, Vec<String>, f32, f32, f32)> {
    let system = "You are a memory extraction engine. Given a raw event or statement, extract structured memory filaments. Output ONLY valid JSON with: fact (concise factual statement preserving key details), relations (array of topic tags e.g. [\"travel\", \"food\"]), emotion_valence (float -1.0 to 1.0, negative=unpleasant), emotion_intensity (float 0.0 to 1.0), confidence (float 0.0 to 1.0).";
    let user = format!("Raw event: {raw_event}");
    match llm.chat_json(system, &user).await {
        Ok(v) => {
            let fact = v.get("fact").and_then(|f| f.as_str()).unwrap_or(raw_event).to_string();
            let relations: Vec<String> = v.get("relations")
                .and_then(|r| r.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            let valence = v.get("emotion_valence").and_then(|f| f.as_f64()).map(|f| f as f32).unwrap_or(0.0)
                .clamp(-1.0, 1.0);
            let intensity = v.get("emotion_intensity").and_then(|f| f.as_f64()).map(|f| f as f32).unwrap_or(0.5)
                .clamp(0.0, 1.0);
            let confidence = v.get("confidence").and_then(|f| f.as_f64()).map(|f| f as f32).unwrap_or(0.8)
                .clamp(0.0, 1.0);
            Some((fact, relations, valence, intensity, confidence))
        }
        Err(_) => None,
    }
}

/// LLM conflict detection: compare new memory against candidate existing memories.
/// Returns node_ids of candidates that have factual contradictions.
async fn judge_conflicts(llm: &dyn ChatModel, new_fact: &str, candidates: &[(u32, String)]) -> Vec<u64> {
    use std::fmt::Write;
    let mut user = String::from("New memory:\n");
    user.push_str(new_fact);
    user.push_str("\n\nCandidate existing memories:\n");
    for (i, (_, fact)) in candidates.iter().enumerate() {
        let _ = write!(&mut user, "{}. {}\n", i + 1, fact);
    }
    let system = "You are a memory conflict detector. Compare the new memory against the candidate existing memories. Only return candidates that have factual contradictions (not supplements, not related-but-different). Output JSON: {\x22conflicts\x22: [candidate_numbers]}";
    match llm.chat_json(system, &user).await {
        Ok(v) => {
            let mut out = Vec::new();
            if let Some(arr) = v.get("conflicts").and_then(|v| v.as_array()) {
                for idx in arr {
                    if let Some(i) = idx.as_u64() {
                        if i >= 1 && i <= candidates.len() as u64 {
                            out.push(candidates[i as usize - 1].0 as u64);
                        }
                    }
                }
            }
            out
        }
        Err(_) => Vec::new(),
    }
}

#[tonic::async_trait]
impl MemoryEngine for EngineService {
    async fn weave(&self, req: Request<WeaveRequest>) -> Result<Response<WeaveResponse>, Status> {
        let r = req.into_inner();
        if r.tenant_id.is_empty() || r.owner_id.is_empty() || r.raw_event.is_empty() {
            return Err(Status::invalid_argument("tenant_id / owner_id / raw_event 均不能为空"));
        }
        let now = now_secs();
        let ctx = to_context(r.context);
        let (fact, relations, emotion_valence, emotion_intensity, confidence) = if let Some(llm) = &self.llm {
            decompose(llm.as_ref(), &r.raw_event).await.unwrap_or_else(|| {
                let rels: Vec<String> = ctx.task.clone().into_iter().collect();
                (r.raw_event.clone(), rels, ctx.emotion_valence.unwrap_or(0.0), 0.5, 0.8)
            })
        } else {
            let rels: Vec<String> = ctx.task.clone().into_iter().collect();
            (r.raw_event.clone(), rels, ctx.emotion_valence.unwrap_or(0.0), 0.5, 0.8)
        };
        // Embed the decomposed fact before moving it into the node
        let embedding = if let Some(emb) = &self.embedder {
            match emb.embed(std::slice::from_ref(&fact)).await {
                Ok(mut v) => v.pop(),
                Err(e) => return Err(Status::internal(format!("嵌入失败: {e}"))),
            }
        } else {
            None
        };
        let mut node = MemoryNode {
            id: 0, // 全局 ID 分配在 Phase 2 接入；node_id 暂用局部 ID
            owner_id: r.owner_id.clone(),
            filaments: Filaments {
                fact,
                emotion_valence,
                emotion_intensity,
                created_at: now,
                decay_rate: 0.01,
                relations: relations.clone(),
                confidence,
                mentions_7d: 1,
            },
            tension: Tension { baseline: 1.0, last_updated: now },
            embedding: Vec::new(),
        };
        // REMOVED_DUPLICATE（锁外计算，避免持锁等网络）
        let llm = self.llm.clone();
        let (local, linked, final_ticket, candidate_facts) = {
        let mut inner = self.inner.lock().map_err(|_| Status::internal("state lock poisoned"))?;
        if let Some(vec) = embedding {
            node.embedding = vec;
        }
        let (local, node_ticket) = inner.store.add_node(node).map_err(|e| Status::internal(format!("wal append: {e}")))?;
        if !inner.store.graph().get_node(local).unwrap().embedding.is_empty() {
            let emb = inner.store.graph().get_node(local).unwrap().embedding.clone();
            inner.index.add(local, &emb);
        }

        // 自动建边：同 owner 且关系丝重叠的存活历史节点
        let mut linked = Vec::new();
        let mut edge_ticket = None;
        if !relations.is_empty() {
            let mut picks: Vec<u32> = Vec::new();
            // 关系丝倒排索引取候选，凑齐 MAX_AUTO_LINKS 条同 owner 边即停（免全图扫描）
            'tags: for tag in &relations {
                for cand in inner.store.graph().relation_candidates(tag) {
                    if picks.len() >= MAX_AUTO_LINKS {
                        break 'tags;
                    }
                    if cand == local || picks.contains(&cand) {
                        continue;
                    }
                    let is_same_owner = inner
                        .store
                        .graph()
                        .get_node(cand)
                        .map(|n| n.owner_id == r.owner_id)
                        .unwrap_or(false);
                    if is_same_owner {
                        picks.push(cand);
                    }
                }
            }
            for cand in picks {
                edge_ticket = Some(inner.store.add_edge(local, cand, AUTO_LINK_WEIGHT).map_err(|e| Status::internal(format!("wal append: {e}")))?);
                linked.push(cand as u64);
            }
        }
            let mut candidates: Vec<(u32, String)> = Vec::new();
            if !inner.store.graph().get_node(local).unwrap().embedding.is_empty() {
                let emb = inner.store.graph().get_node(local).unwrap().embedding.clone();
                for (cid, _) in inner.index.search(&emb, 8) {
                    if cid == local { continue; }
                    if candidates.len() >= 4 { break; }
                    if let Some(cn) = inner.store.graph().get_node(cid) {
                        if cn.owner_id == r.owner_id {
                            candidates.push((cid, cn.filaments.fact.clone()));
                        }
                    }
                }
            }
            (local, linked, edge_ticket.unwrap_or(node_ticket), candidates)
        };
        let conflict_nodes: Vec<u64> = if let Some(llm) = &llm {
            if !candidate_facts.is_empty() {
                judge_conflicts(llm.as_ref(), &r.raw_event, &candidate_facts).await
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        let resp = WeaveResponse { node_id: local as u64, linked_nodes: linked, conflict_nodes };
        let final_ticket = final_ticket;
        // group commit：刷盘等待移出全局锁，并发写共享同一批次 fsync
        tokio::task::spawn_blocking(move || final_ticket.wait())
            .await
            .map_err(|e| Status::internal(format!("durability join: {e}")))?
            .map_err(|e| Status::internal(format!("wal durability: {e}")))?;
        Ok(Response::new(resp))
    }

    async fn resonate(&self, req: Request<ResonateRequest>) -> Result<Response<ResonateResponse>, Status> {
        let r = req.into_inner();
        if r.tenant_id.is_empty() || r.owner_id.is_empty() {
            return Err(Status::invalid_argument("tenant_id / owner_id 不能为空"));
        }
        let ctx = to_context(r.context);
        let query = r.query.to_lowercase();
        // 向量种子（锁外计算）
        let vec_seeds: Vec<u32> = if let (Some(emb), false) = (&self.embedder, query.is_empty()) {
            match emb.embed(std::slice::from_ref(&r.query)).await {
                Ok(v) => {
                    let inner = self.inner.lock().map_err(|_| Status::internal("state lock poisoned"))?;
                    inner.index.search(&v[0], MAX_SEEDS).into_iter().map(|(id, _)| id).collect()
                }
                Err(_) => Vec::new(), // 嵌入服务故障时降级为纯词面
            }
        } else {
            Vec::new()
        };
        let inner = self.inner.lock().map_err(|_| Status::internal("state lock poisoned"))?;
        let g = inner.store.graph();

        // 种子选择：query 词项重叠打分（完整子串命中加权） > task 命中关系丝 > 最近节点兜底。
        // 词项化按非字母数字切分；CJK 查询无空格时退化为整串 contains，行为与原先一致。
        let mut lex_seeds: Vec<u32> = Vec::new();
        if !query.is_empty() {
            let terms: Vec<&str> = query
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| !t.is_empty())
                .collect();
            let mut scored: Vec<(u32, usize)> = g
                .live_nodes()
                .filter(|(_, n)| n.owner_id == r.owner_id)
                .filter_map(|(id, n)| {
                    let fact = n.filaments.fact.to_lowercase();
                    let mut score = terms.iter().filter(|t| fact.contains(**t)).count();
                    if !terms.is_empty() && fact.contains(&query) {
                        score += terms.len(); // 完整子串命中额外加权
                    }
                    if score > 0 { Some((id, score)) } else { None }
                })
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            lex_seeds = scored.into_iter().map(|(id, _)| id).collect();
        }
        // 融合向量种子：去重后并入（词面优先）
        // 融合：向量种子保底 VEC_SEED_QUOTA 个名额，词面种子去重补满
        let mut seeds: Vec<u32> = Vec::new();
        for id in vec_seeds.into_iter().take(VEC_SEED_QUOTA) {
            seeds.push(id);
        }
        for id in lex_seeds {
            if seeds.len() >= MAX_SEEDS {
                break;
            }
            if !seeds.contains(&id) {
                seeds.push(id);
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use nylon_embed::StubEmbedder;
    use nylon_llm::StubChatModel;
    use std::sync::Arc;

    #[tokio::test]
    async fn weave_with_stub_llm_no_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let store = PersistentGraph::open(dir.path()).unwrap();
        let embedder = Arc::new(StubEmbedder::new(64));
        let llm = Arc::new(StubChatModel::new(serde_json::json!({"conflicts": [1]})));

        let svc = EngineService::new(store, 64, Some(embedder.clone()), Some(llm.clone()));

        // First weave: no candidates, no conflicts
        let req = WeaveRequest {
            tenant_id: "test".into(),
            owner_id: "alice".into(),
            raw_event: "I like coffee".into(),
            context: None,
        };
        let resp = svc.weave(tonic::Request::new(req)).await.unwrap();
        let body = resp.into_inner();
        assert!(body.conflict_nodes.is_empty());
        assert!(body.linked_nodes.is_empty());

        // Second weave: similar topic, HNSW may find candidate
        let req2 = WeaveRequest {
            tenant_id: "test".into(),
            owner_id: "alice".into(),
            raw_event: "I prefer tea over coffee".into(),
            context: None,
        };
        let resp2 = svc.weave(tonic::Request::new(req2)).await.unwrap();
        let body2 = resp2.into_inner();
        assert!(body2.node_id > 0);
    }
}

