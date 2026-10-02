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
use nylon_graph::{ContextSpectrum, FilamentFilter};
use nylon_llm::ChatModel;
use nylon_storage::PersistentGraph;
use nylon_vector::{HnswIndex, VectorIndex};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tonic::{Request, Response, Status};

use crate::audit::Audit;
use crate::auth::{authorize, ApiKeys, KeyGrant, Scope};

pub mod pb {
    tonic::include_proto!("nylon.v1");
}

use pb::memory_engine_server::MemoryEngine;
use pb::{
    ActivatedNode, EventNode, FactNode, FeedbackRequest, FeedbackResponse, GetNodeRequest,
    GetNodeResponse, ResonateRequest, ResonateResponse, SearchRequest, SearchResponse,
    SessionEvent, WeaveRequest, WeaveResponse, WeaveSessionRequest, WeaveSessionResponse,
};

/// 默认嵌入维度（bge-small 类模型），可用 NYLON_EMBED_DIMS 覆盖。
pub const DEFAULT_EMBED_DIMS: usize = 384;
/// Resonate 种子数量上限。
/// 英文停用词表：词面选种与启发式关系丝共用
const STOPWORDS: &[&str] = &[
    "what", "when", "where", "which", "who", "whom", "whose", "why", "how", "did", "does", "do",
    "is", "are", "was", "were", "be", "been", "being", "the", "a", "an", "and", "or", "but", "if",
    "then", "than", "so", "they", "them", "their", "he", "she", "his", "her", "it", "its", "you",
    "your", "we", "our", "i", "me", "my", "have", "has", "had", "say", "said", "tell", "told",
    "talk", "talked", "about", "would", "could", "should", "will", "shall", "can", "may", "might",
    "many", "much", "often", "ever", "never", "any", "some", "all", "both", "first", "last", "go",
    "went", "going", "come", "came", "get", "got", "make", "made", "take", "took", "to", "of",
    "in", "on", "at", "for", "with", "from", "by", "as", "into", "out", "up", "down",
];

/// 启发式关系丝抽取：无 LLM / 无 context 时从原文提取内容标签。
/// 句中大写开头的专有名（人名/地名/机构）优先，其次长度 >=4 的非停用实词，最多 3 个。
/// 这是自动建边（auto-link）的标签来源——没有它图是零边，扩散空转。
fn heuristic_relations(text: &str) -> Vec<String> {
    let mut cands: Vec<(i32, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (idx, w) in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .enumerate()
    {
        let lower = w.to_lowercase();
        if lower.len() < 4 || STOPWORDS.contains(&lower.as_str()) {
            continue;
        }
        if !seen.insert(lower.clone()) {
            continue;
        }
        let mut score = lower.len().min(8) as i32;
        if idx > 0 && w.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            score += 10; // 句中大写开头 = 专有名
        }
        cands.push((score, lower));
    }
    cands.sort_by_key(|c| std::cmp::Reverse(c.0));
    cands.into_iter().take(3).map(|(_, w)| w).collect()
}

/// 种子池上限默认值，可用 NYLON_MAX_SEEDS 覆盖（实验旋钮）
fn max_seeds() -> usize {
    std::env::var("NYLON_MAX_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
}

/// 空闲反思窗口：距离最后一次 session 写入多久后开始补常识桥接。
fn reflect_idle_duration() -> std::time::Duration {
    let secs = std::env::var("NYLON_REFLECT_IDLE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(600);
    std::time::Duration::from_secs(secs.max(1))
}
/// 向量种子保底名额：语义通道的召回兜底，防止被词面种子挤占。
const VEC_SEED_QUOTA: usize = 8;
/// Weave 自动建边上限。
const MAX_AUTO_LINKS: usize = 3;
/// 自动建边权重（Phase 1 固定值，后续由相似度决定）。
const AUTO_LINK_WEIGHT: f32 = 0.5;
/// 层间显式边权重（抽象层事实 <-> 来源叶子），高于自动建边。
const DERIVED_EDGE_WEIGHT: f32 = 1.0;
/// 常识桥接节点标签：作为扩散中间层，不进入最终激活结果。
const WORLD_KNOWLEDGE_TAG: &str = "__world_knowledge__";
/// 个人化推断节点标记（NYLON_REFLECT_PERSONAL）：可进 resonate 输出与作答上下文，
/// 与 WORLD_KNOWLEDGE_TAG（只许扩散、输出层过滤）结构性区分。
const INFERRED_TAG: &str = "inferred";
/// 画像节点标签（ relations 前缀）：persona + person:<规范名>
const PERSONA_TAG: &str = "persona";
const PERSONA_NAME_PREFIX: &str = "person:";
const PERSONA_EDGE_WEIGHT: f32 = 0.9;
/// 画像→叶子边权重（低于画像→事实，最后一跳）
const PERSONA_LEAF_EDGE_WEIGHT: f32 = 0.7;
/// 常识桥接节点与抽象事实节点之间的边权重。
const WORLD_BRIDGE_EDGE_WEIGHT: f32 = 0.8;
/// 个人化推断节点 → 来源事实的边权（与常识桥同档）。
const INFER_EDGE_WEIGHT: f32 = 0.8;

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
    ActivatedNode {
        node_id: local as u64,
        resonance: score,
        filaments: Some(to_pb_filaments(&node.filaments)),
    }
}

fn to_context(ctx: Option<pb::ContextSpectrum>) -> ContextSpectrum {
    ctx.map(|c| ContextSpectrum {
        task: c.task,
        emotion_valence: c.emotion_valence,
        max_hops: c.max_hops,
    })
    .unwrap_or_default()
}

struct Inner {
    store: PersistentGraph,
    index: HnswIndex,
}

struct ReflectionJob {
    tenant_id: String,
    owner_id: String,
    session_text: String,
    fact_ids: Vec<u32>,
    /// 本 session 的叶子节点 ID（画像节点连边用，打通 画像→叶子 最后一跳）
    leaf_ids: Vec<u32>,
}

/// 回答质量回执（反馈驱动反思的输入信号）。
/// 持久化到 <store>/feedback.jsonl，反思 worker 空闲时定向补推断。
#[derive(Clone)]
pub struct FeedbackRecord {
    pub tenant_id: String,
    pub owner_id: String,
    pub query: String,
    pub rating: String,
    pub comment: String,
    pub shown_node_ids: Vec<u32>,
    pub ts: i64,
}

impl FeedbackRecord {
    /// 去重键：同一 owner 对同一查询的重复回执只反思一次。
    fn dedup_key(&self) -> String {
        format!("{}|{}|{}", self.tenant_id, self.owner_id, self.query)
    }
}

/// 反思 worker 的工作项：会话反思（定期）或失败回执（反馈驱动）。
enum ReflectWork {
    Session(ReflectionJob),
    Feedback(FeedbackRecord),
}

// ---------- 反馈回执的持久化（feedback.jsonl 追加写 + processed 标记防重） ----------

#[derive(serde::Serialize, serde::Deserialize)]
struct FeedbackLogLine {
    ts: i64,
    tenant: String,
    owner: String,
    query: String,
    rating: String,
    #[serde(default)]
    comment: String,
    #[serde(default)]
    shown: Vec<u32>,
}

impl From<&FeedbackRecord> for FeedbackLogLine {
    fn from(r: &FeedbackRecord) -> Self {
        FeedbackLogLine {
            ts: r.ts,
            tenant: r.tenant_id.clone(),
            owner: r.owner_id.clone(),
            query: r.query.clone(),
            rating: r.rating.clone(),
            comment: r.comment.clone(),
            shown: r.shown_node_ids.clone(),
        }
    }
}

impl From<FeedbackLogLine> for FeedbackRecord {
    fn from(l: FeedbackLogLine) -> Self {
        FeedbackRecord {
            tenant_id: l.tenant,
            owner_id: l.owner,
            query: l.query,
            rating: l.rating,
            comment: l.comment,
            shown_node_ids: l.shown,
            ts: l.ts,
        }
    }
}

fn feedback_log_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("feedback.jsonl")
}

fn feedback_processed_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("feedback-processed.jsonl")
}

/// 追加一条回执到 feedback.jsonl（先落盘后入队，崩溃不丢）。
fn append_feedback_log(dir: &std::path::Path, rec: &FeedbackRecord) -> Result<(), Status> {
    let line = serde_json::to_string(&FeedbackLogLine::from(rec))
        .map_err(|e| Status::internal(format!("反馈序列化失败: {e}")))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(feedback_log_path(dir))
        .map_err(|e| Status::internal(format!("反馈日志打开失败: {e}")))?;
    use std::io::Write;
    f.write_all(line.as_bytes())
        .and_then(|_| f.write_all(b"\n"))
        .map_err(|e| Status::internal(format!("反馈日志写入失败: {e}")))
}

fn load_processed_keys(dir: &std::path::Path) -> std::collections::HashSet<String> {
    std::fs::read_to_string(feedback_processed_path(dir))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<String>(l).ok())
        .collect()
}

fn mark_feedback_processed(dir: &std::path::Path, key: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(feedback_processed_path(dir))
    {
        use std::io::Write;
        if let Ok(line) = serde_json::to_string(&key.to_string()) {
            let _ = f.write_all(line.as_bytes());
            let _ = f.write_all(b"\n");
        }
    }
}

/// 启动回放：feedback.jsonl 中尚未处理的回执（按去重键过滤）。
fn load_unprocessed_feedback(dir: &std::path::Path) -> Vec<FeedbackRecord> {
    let processed = load_processed_keys(dir);
    std::fs::read_to_string(feedback_log_path(dir))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<FeedbackLogLine>(l).ok())
        .map(FeedbackRecord::from)
        .filter(|r| !processed.contains(&r.dedup_key()))
        .collect()
}

/// MemoryEngine 服务句柄（内部状态互斥保护，Phase 1 单写者够用）。
#[derive(Clone)]
pub struct EngineService {
    inner: Arc<Mutex<Inner>>,
    /// 空闲反思队列：异步补常识桥接节点。
    reflect_tx: Option<tokio::sync::mpsc::UnboundedSender<ReflectWork>>,
    /// 嵌入通道：None 时退回 Phase 1 行为（无向量写入、无向量种子）。
    embedder: Option<Arc<dyn Embedder>>,
    /// 嵌入通道运行健康（issue #4）：连续失败计数 + 最近错误，
    /// 让「配置了但端点挂了」在 stats 里可见，而不是静默退化成无区分度排序。
    embed_health: Arc<EmbedHealth>,
    /// LLM 通道：None 时关闭编织分解与冲突检测。
    llm: Option<Arc<dyn ChatModel>>,
    /// API key 鉴权（L2.2）：None = 开放模式（单机默认）。
    auth: Option<Arc<ApiKeys>>,
    /// 审计事件流（L2.3）：None = 关闭（NYLON_AUDIT=off 或未挂接）。
    audit: Option<Audit>,
}

/// 嵌入通道运行健康（issue #4）。热路径（weave/resonate）逐次上报，
/// 反思 worker 的嵌入失败暂不纳入（信号已被热路径覆盖）。
#[derive(Default)]
pub(crate) struct EmbedHealth {
    consecutive_failures: std::sync::atomic::AtomicU32,
    last_error: std::sync::Mutex<Option<String>>,
}

impl EngineService {
    pub fn new(
        store: PersistentGraph,
        embed_dims: usize,
        embedder: Option<Arc<dyn Embedder>>,
        llm: Option<Arc<dyn ChatModel>>,
    ) -> Self {
        // 重启/重开库时从持久化节点回填 HNSW 索引——此前索引只在 weave 时增量构建，
        // 重启后向量种子通道静默失效（2026-09-18 评测缓存复用时暴露：
        // 种子召回 90.7% -> 84.7%，生产服务器每次重启同样中招）。
        let mut index = HnswIndex::new(embed_dims);
        {
            let g = store.graph();
            let mut restored = 0usize;
            for (id, n) in g.live_nodes() {
                if n.embedding.len() == embed_dims {
                    index.add(id, &n.embedding);
                    restored += 1;
                }
            }
            if restored > 0 {
                eprintln!("[engine] HNSW 索引已从持久化节点回填 {restored} 条");
            }
        }
        let inner = Arc::new(Mutex::new(Inner { store, index }));
        let reflect_tx = llm.clone().map(|llm| {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ReflectWork>();
            let inner = Arc::clone(&inner);
            let embedder = embedder.clone();
            tokio::spawn(async move {
                let mut pending: Vec<ReflectWork> = Vec::new();
                let mut feedback_done: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                loop {
                    match tokio::time::timeout(reflect_idle_duration(), rx.recv()).await {
                        Ok(Some(job)) => pending.push(job),
                        Ok(None) => {
                            if !pending.is_empty() {
                                let jobs = std::mem::take(&mut pending);
                                process_reflect_work(
                                    &inner,
                                    embedder.as_ref(),
                                    llm.as_ref(),
                                    jobs,
                                    &mut feedback_done,
                                )
                                .await;
                            }
                            break;
                        }
                        Err(_) => {
                            if !pending.is_empty() {
                                let jobs = std::mem::take(&mut pending);
                                process_reflect_work(
                                    &inner,
                                    embedder.as_ref(),
                                    llm.as_ref(),
                                    jobs,
                                    &mut feedback_done,
                                )
                                .await;
                            }
                        }
                    }
                }
            });
            tx
        });
        // 启动时回放未处理的反馈回执：崩溃/重启不丢失败信号（反馈驱动反思）
        if let Some(tx) = &reflect_tx {
            let dir = inner.lock().ok().map(|i| i.store.dir().to_path_buf());
            if let Some(dir) = dir {
                let backlog = load_unprocessed_feedback(&dir);
                if !backlog.is_empty() {
                    eprintln!("[reflect] 回放未处理反馈回执 {} 条", backlog.len());
                    for rec in backlog {
                        let _ = tx.send(ReflectWork::Feedback(rec));
                    }
                }
            }
        }
        EngineService {
            inner,
            embedder,
            embed_health: Arc::new(EmbedHealth::default()),
            llm,
            reflect_tx,
            auth: None,
            audit: None,
        }
    }

    /// 记录一次嵌入调用成功（issue #4）：清零连续失败计数。
    pub(crate) fn note_embed_ok(&self) {
        self.embed_health
            .consecutive_failures
            .store(0, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut slot) = self.embed_health.last_error.lock() {
            *slot = None;
        }
    }

    /// 记录一次嵌入调用失败（issue #4）：计数 + 最近错误，首次与每 10 次告警。
    pub(crate) fn note_embed_failure(&self, err: &str) {
        let n = self
            .embed_health
            .consecutive_failures
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if let Ok(mut slot) = self.embed_health.last_error.lock() {
            *slot = Some(err.chars().take(300).collect());
        }
        if n == 1 || n.is_multiple_of(10) {
            eprintln!(
                "[embed] 嵌入调用连续失败 {n} 次：{err} —— 语义召回已退化（stats.embedder_status=degraded）"
            );
        }
    }

    /// 启动探测（serve 模式，issue #4）：确认已配置的嵌入端点真的可用；
    /// 未配置/失败都打出醒目提示而不是静默退化，结果计入健康状态。
    pub async fn probe_embedder(&self) {
        let Some(emb) = &self.embedder else {
            eprintln!(
                "[warn] 未配置 NYLON_EMBED_URL：语义召回关闭，resonate 分数无区分度（stats.embedder_status=disabled）。配置嵌入端点可开启语义召回（见 docs/GETTING_STARTED.md）"
            );
            return;
        };
        let probe = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            emb.embed(&["nylon startup probe".to_string()]),
        )
        .await;
        match probe {
            Ok(Ok(_)) => {
                self.note_embed_ok();
                println!("嵌入通道已启用 (NYLON_EMBED_URL，启动探测成功)");
            }
            Ok(Err(e)) => {
                self.note_embed_failure(&e.to_string());
                eprintln!(
                    "[warn] 嵌入端点已配置但启动探测失败：{e} —— 写入会报「嵌入失败」、共振退化为纯词面（stats.embedder_status=degraded）。请检查嵌入服务是否已拉起"
                );
            }
            Err(_) => {
                self.note_embed_failure("启动探测超时（5s）");
                eprintln!(
                    "[warn] 嵌入端点启动探测超时（5s）——写入会报「嵌入失败」、共振退化为纯词面（stats.embedder_status=degraded）。请检查嵌入服务是否已拉起"
                );
            }
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
}

/// REST/社区版 UI 的节点摘要（只读视图，写路径仍走 gRPC 契约）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct NodeSummary {
    pub id: u32,
    pub tenant_id: String,
    pub owner_id: String,
    pub fact: String,
    pub tension: f32,
    pub created_at: i64,
    pub relations: Vec<String>,
    pub confidence: f32,
    pub mentions_7d: u32,
}

/// 引擎运行统计（UI 头部状态条）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct EngineStats {
    pub nodes: usize,
    pub edges: usize,
    pub embed_dims: usize,
    pub embedder: bool,
    /// 嵌入通道健康（issue #4）：disabled=未配置 | ok=工作正常 | degraded=连续失败中。
    /// embedder=true 只代表配置了 NYLON_EMBED_URL，不代表端点可用。
    pub embedder_status: String,
    /// 嵌入调用连续失败次数（0 = 健康）。
    pub embed_failures: u32,
    /// 最近一次嵌入错误摘要（健康时省略）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedder_last_error: Option<String>,
    pub llm: bool,
}

/// 图可视化视图（UI Graph 页）：tenant 内最新一批节点 + 集合内部边。
#[derive(Clone, Debug, serde::Serialize)]
pub struct GraphView {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// tenant 内节点总数（可能被 limit 截断，供 UI 提示）。
    pub total: usize,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct GraphNode {
    pub id: u32,
    pub owner_id: String,
    pub fact: String,
    pub tension: f32,
    pub created_at: i64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct GraphEdge {
    pub from: u32,
    pub to: u32,
    pub weight: f32,
}

impl EngineService {
    /// 分页列出节点（按创建时间倒序），按 tenant 强制过滤，可选叠加 owner 过滤。
    pub(crate) fn list_nodes(
        &self,
        tenant: &str,
        owner: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<(usize, Vec<NodeSummary>), Status> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| Status::internal("state lock poisoned"))?;
        let now = now_secs();
        let mut all: Vec<NodeSummary> = inner
            .store
            .graph()
            .live_nodes()
            .filter(|(_, n)| n.tenant_id == tenant && owner.is_none_or(|o| n.owner_id == o))
            .map(|(id, n)| NodeSummary {
                id,
                tenant_id: n.tenant_id.clone(),
                owner_id: n.owner_id.clone(),
                fact: n.filaments.fact.clone(),
                tension: compute_tension(n, now, 1.0),
                created_at: n.filaments.created_at,
                relations: n.filaments.relations.clone(),
                confidence: n.filaments.confidence,
                mentions_7d: n.filaments.mentions_7d,
            })
            .collect();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        let total = all.len();
        let page = all.into_iter().skip(offset).take(limit).collect();
        Ok((total, page))
    }

    /// 引擎运行统计。
    pub(crate) fn stats(&self) -> Result<EngineStats, Status> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| Status::internal("state lock poisoned"))?;
        let (embedder_status, embed_failures, embedder_last_error) = if self.embedder.is_none() {
            ("disabled".to_string(), 0, None)
        } else {
            let fails = self
                .embed_health
                .consecutive_failures
                .load(std::sync::atomic::Ordering::Relaxed);
            if fails == 0 {
                ("ok".to_string(), 0, None)
            } else {
                let last = self
                    .embed_health
                    .last_error
                    .lock()
                    .ok()
                    .and_then(|e| e.clone());
                ("degraded".to_string(), fails, last)
            }
        };
        Ok(EngineStats {
            nodes: inner.store.graph().node_count(),
            edges: inner.store.graph().edges().len(),
            embed_dims: inner.index.dims(),
            embedder: self.embedder.is_some(),
            embedder_status,
            embed_failures,
            embedder_last_error,
            llm: self.llm.is_some(),
        })
    }

    /// 手动 checkpoint：快照落盘 + 截断 WAL（L2.4 备份前置步骤）。
    /// serve 模式另有周期任务自动 checkpoint（NYLON_CHECKPOINT_SECS，默认 600s）。
    pub(crate) fn checkpoint(&self) -> Result<(), Status> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| Status::internal("state lock poisoned"))?;
        inner
            .store
            .checkpoint()
            .map_err(|e| Status::internal(format!("checkpoint: {e}")))?;
        self.audit_op("checkpoint", "", "", "snapshot + wal truncate".into());
        Ok(())
    }

    /// 图可视化视图：按创建时间取 tenant 内最新 limit 个节点，边只保留两端都在集合内的。
    pub(crate) fn graph_view(&self, tenant: &str, limit: usize) -> Result<GraphView, Status> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| Status::internal("state lock poisoned"))?;
        let now = now_secs();
        let mut nodes: Vec<GraphNode> = inner
            .store
            .graph()
            .live_nodes()
            .filter(|(_, n)| n.tenant_id == tenant)
            .map(|(id, n)| GraphNode {
                id,
                owner_id: n.owner_id.clone(),
                fact: n.filaments.fact.chars().take(120).collect(),
                tension: compute_tension(n, now, 1.0),
                created_at: n.filaments.created_at,
            })
            .collect();
        nodes.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        let total = nodes.len();
        nodes.truncate(limit);
        let keep: std::collections::HashSet<u32> = nodes.iter().map(|n| n.id).collect();
        let edges = inner
            .store
            .graph()
            .edges()
            .into_iter()
            .filter(|(f, t, _)| keep.contains(f) && keep.contains(t))
            .map(|(from, to, weight)| GraphEdge { from, to, weight })
            .collect();
        Ok(GraphView {
            nodes,
            edges,
            total,
        })
    }

    /// 删除节点（产品语义 = "遗忘"）：校验租户归属后打墓碑，WAL 落盘后返回。
    /// 跨租户删除按不存在处理，不暴露节点存在性（与 get_node 同一策略）。
    pub(crate) async fn remove_node(&self, tenant: &str, id: u32) -> Result<bool, Status> {
        let (existed, ticket) = {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| Status::internal("state lock poisoned"))?;
            let node = inner
                .store
                .graph()
                .get_node(id)
                .ok_or_else(|| Status::not_found(format!("node {id} 不存在或已删除")))?;
            if node.tenant_id != tenant {
                return Err(Status::not_found(format!("node {id} 不存在或已删除")));
            }
            let owner = node.owner_id.clone();
            let (existed, ticket) = inner
                .store
                .remove_node(id)
                .map_err(|e| Status::internal(format!("remove_node: {e}")))?;
            self.audit_op("delete_node", tenant, &owner, format!("node={id}"));
            (existed, ticket)
        };
        if existed {
            tokio::task::spawn_blocking(move || ticket.wait())
                .await
                .map_err(|e| Status::internal(format!("durability wait: {e}")))?
                .map_err(|e| Status::internal(format!("wal: {e}")))?;
        }
        Ok(existed)
    }
}

/// LLM weave decomposition: extract six-filament structure from raw event.
/// Returns None when LLM unavailable/fails, caller falls back to heuristic decomposition.
async fn decompose(
    llm: &dyn ChatModel,
    raw_event: &str,
) -> Option<(String, Vec<String>, f32, f32, f32)> {
    let system = "You are a memory extraction engine. Given a raw event or statement, extract structured memory filaments. Output ONLY valid JSON with: fact (concise factual statement preserving key details), relations (array of topic tags e.g. [\"travel\", \"food\"]), emotion_valence (float -1.0 to 1.0, negative=unpleasant), emotion_intensity (float 0.0 to 1.0), confidence (float 0.0 to 1.0).";
    let user = format!("Raw event: {raw_event}");
    match llm.chat_json(system, &user).await {
        Ok(v) => {
            let fact = v
                .get("fact")
                .and_then(|f| f.as_str())
                .unwrap_or(raw_event)
                .to_string();
            let relations: Vec<String> = v
                .get("relations")
                .and_then(|r| r.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let valence = v
                .get("emotion_valence")
                .and_then(|f| f.as_f64())
                .map(|f| f as f32)
                .unwrap_or(0.0)
                .clamp(-1.0, 1.0);
            let intensity = v
                .get("emotion_intensity")
                .and_then(|f| f.as_f64())
                .map(|f| f as f32)
                .unwrap_or(0.5)
                .clamp(0.0, 1.0);
            let confidence = v
                .get("confidence")
                .and_then(|f| f.as_f64())
                .map(|f| f as f32)
                .unwrap_or(0.8)
                .clamp(0.0, 1.0);
            Some((fact, relations, valence, intensity, confidence))
        }
        Err(e) => {
            eprintln!("[weave] LLM decompose 调用失败，回退启发式: {e}");
            None
        }
    }
}

/// LLM conflict detection: compare new memory against candidate existing memories.
/// Returns node_ids of candidates that have factual contradictions.
async fn judge_conflicts(
    llm: &dyn ChatModel,
    new_fact: &str,
    candidates: &[(u32, String)],
) -> Vec<u64> {
    use std::fmt::Write;
    let mut user = String::from("New memory:\n");
    user.push_str(new_fact);
    user.push_str("\n\nCandidate existing memories:\n");
    for (i, (_, fact)) in candidates.iter().enumerate() {
        let _ = writeln!(&mut user, "{}. {}", i + 1, fact);
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
        Err(e) => {
            eprintln!("[weave] LLM 冲突检测调用失败，按无冲突处理: {e}");
            Vec::new()
        }
    }
}

/// weave 的 LLM 使用模式：Full=分解+冲突检测（单事件 RPC）；Off=纯启发式（session 双层写入路径）
#[derive(Clone, Copy, PartialEq)]
pub enum WeaveLlmMode {
    Full,
    Off,
}

impl EngineService {
    /// 单事件编织核心：单事件 RPC 与 session 双层写入共用。返回 (局部 ID, 自动建边目标, 冲突节点)。
    async fn weave_one(
        &self,
        tenant_id: &str,
        owner_id: &str,
        raw_event: &str,
        context: Option<pb::ContextSpectrum>,
        llm_mode: WeaveLlmMode,
    ) -> Result<(u32, Vec<u64>, Vec<u64>), Status> {
        if tenant_id.is_empty() || owner_id.is_empty() || raw_event.is_empty() {
            return Err(Status::invalid_argument(
                "tenant_id / owner_id / raw_event 均不能为空",
            ));
        }
        let llm_gate = if llm_mode == WeaveLlmMode::Full {
            self.llm.clone()
        } else {
            None
        };
        let now = now_secs();
        let ctx = to_context(context);
        let (fact, relations, emotion_valence, emotion_intensity, confidence) =
            if let Some(llm) = &llm_gate {
                decompose(llm.as_ref(), raw_event).await.unwrap_or_else(|| {
                    let mut rels: Vec<String> = ctx.task.clone().into_iter().collect();
                    if rels.is_empty() {
                        rels = heuristic_relations(raw_event);
                    }
                    (
                        raw_event.to_string(),
                        rels,
                        ctx.emotion_valence.unwrap_or(0.0),
                        0.5,
                        0.8,
                    )
                })
            } else {
                let mut rels: Vec<String> = ctx.task.clone().into_iter().collect();
                if rels.is_empty() {
                    rels = heuristic_relations(raw_event);
                }
                (
                    raw_event.to_string(),
                    rels,
                    ctx.emotion_valence.unwrap_or(0.0),
                    0.5,
                    0.8,
                )
            };
        // Embed the decomposed fact before moving it into the node
        let embedding = if let Some(emb) = &self.embedder {
            let res = emb.embed(std::slice::from_ref(&fact)).await;
            match res {
                Ok(mut v) => {
                    self.note_embed_ok();
                    v.pop()
                }
                Err(e) => {
                    self.note_embed_failure(&e.to_string());
                    return Err(Status::internal(format!("嵌入失败: {e}")));
                }
            }
        } else {
            None
        };
        let mut node = MemoryNode {
            id: 0, // 全局 ID 分配在 Phase 2 接入；node_id 暂用局部 ID
            tenant_id: tenant_id.to_string(),
            owner_id: owner_id.to_string(),
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
            tension: Tension {
                baseline: 1.0,
                last_updated: now,
            },
            embedding: Vec::new(),
        };
        // REMOVED_DUPLICATE（锁外计算，避免持锁等网络）
        let _llm = self.llm.clone();
        let (local, linked, final_ticket, candidate_facts) = {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| Status::internal("state lock poisoned"))?;
            if let Some(vec) = embedding {
                node.embedding = vec;
            }
            let (local, node_ticket) = inner
                .store
                .add_node(node)
                .map_err(|e| Status::internal(format!("wal append: {e}")))?;
            if !inner
                .store
                .graph()
                .get_node(local)
                .unwrap()
                .embedding
                .is_empty()
            {
                let emb = inner
                    .store
                    .graph()
                    .get_node(local)
                    .unwrap()
                    .embedding
                    .clone();
                inner.index.add(local, &emb);
            }

            // 自动建边：同 tenant + 同 owner 且关系丝重叠的存活历史节点（L2.1 强制隔离）
            let mut linked = Vec::new();
            let mut edge_ticket = None;
            if !relations.is_empty() {
                let mut picks: Vec<u32> = Vec::new();
                // 关系丝倒排索引取候选，凑齐 MAX_AUTO_LINKS 条同租户边即停（免全图扫描）
                'tags: for tag in &relations {
                    for cand in inner.store.graph().relation_candidates(tag) {
                        if picks.len() >= MAX_AUTO_LINKS {
                            break 'tags;
                        }
                        if cand == local || picks.contains(&cand) {
                            continue;
                        }
                        let in_scope = inner
                            .store
                            .graph()
                            .get_node(cand)
                            .map(|n| n.tenant_id == tenant_id && n.owner_id == owner_id)
                            .unwrap_or(false);
                        if in_scope {
                            picks.push(cand);
                        }
                    }
                }
                for cand in picks {
                    edge_ticket = Some(
                        inner
                            .store
                            .add_edge(local, cand, AUTO_LINK_WEIGHT)
                            .map_err(|e| Status::internal(format!("wal append: {e}")))?,
                    );
                    linked.push(cand as u64);
                }
            }
            let mut candidates: Vec<(u32, String)> = Vec::new();
            if !inner
                .store
                .graph()
                .get_node(local)
                .unwrap()
                .embedding
                .is_empty()
            {
                let emb = inner
                    .store
                    .graph()
                    .get_node(local)
                    .unwrap()
                    .embedding
                    .clone();
                for (cid, _) in inner.index.search(&emb, 8) {
                    if cid == local {
                        continue;
                    }
                    if candidates.len() >= 4 {
                        break;
                    }
                    if let Some(cn) = inner.store.graph().get_node(cid) {
                        if cn.tenant_id == tenant_id && cn.owner_id == owner_id {
                            candidates.push((cid, cn.filaments.fact.clone()));
                        }
                    }
                }
            }
            (
                local,
                linked,
                edge_ticket.unwrap_or(node_ticket),
                candidates,
            )
        };
        let conflict_nodes: Vec<u64> = if let Some(llm) = &llm_gate {
            if !candidate_facts.is_empty() {
                judge_conflicts(llm.as_ref(), raw_event, &candidate_facts).await
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        // group commit：刷盘等待移出全局锁，并发写共享同一批次 fsync
        tokio::task::spawn_blocking(move || final_ticket.wait())
            .await
            .map_err(|e| Status::internal(format!("durability join: {e}")))?
            .map_err(|e| Status::internal(format!("wal durability: {e}")))?;
        Ok((local, linked, conflict_nodes))
    }
}

/// 会话拆分重试下限：批次 ≤ 该值不再对半拆分（issue #1 建议下限约 5 条事件）。
const SESSION_SPLIT_FLOOR: usize = 5;

/// Session 抽象事实抽取结果：facts + 全部（子）批次是否均 LLM 成功。
struct SessionExtraction {
    facts: Vec<(String, Vec<String>)>,
    all_ok: bool,
}

/// Session 级抽象事实抽取（双层写入的理解层）：整段 session 一次 LLM 调用，
/// 指代消解+原子事实+来源 event_id。
/// 失败处理（issue #1）：输出预算按输入规模自适应（1536 硬顶会截断 JSON）；
/// 解析/调用失败时对半拆分递归重试；截断 JSON 由 llm 层抢救完整元素。
async fn extract_session_facts(llm: &dyn ChatModel, lines: &[String]) -> SessionExtraction {
    let system = "You are a memory extraction engine. Given a dialogue session with turn IDs, extract atomic factual memories worth remembering long-term. Resolve pronouns and partial names to canonical full names (e.g. 'she' -> the person's name). Merge duplicate information. Preserve exact details: dates, numbers, places, names. If turns carry a [date] prefix, treat it as the absolute time of those turns; when event timing matters, include the absolute date in the fact rather than relative words like 'yesterday' or 'last week'. Each fact must be self-contained. Extract at most 20 facts, each under 40 words. Output ONLY valid JSON: {\"facts\": [{\"fact\": \"...\", \"source\": [\"event_id\", ...]}]}. Skip greetings and small talk without facts.";
    let text = lines.join("\n");
    // 自适应输出预算：事实量随输入规模增长，预算钳制在 [2048, 8192]
    let budget = (256 + text.len() / 20).clamp(2048, 8192) as u32;
    match llm.chat_json_budget(system, &text, budget).await {
        Ok(v) => SessionExtraction {
            facts: parse_facts_value(&v),
            all_ok: true,
        },
        Err(e) => {
            if lines.len() > SESSION_SPLIT_FLOOR {
                // 长批次失败大概率是输出截断：对半拆分递归重试
                let mid = lines.len() / 2;
                let a = Box::pin(extract_session_facts(llm, &lines[..mid])).await;
                let b = Box::pin(extract_session_facts(llm, &lines[mid..])).await;
                let mut facts = a.facts;
                facts.extend(b.facts);
                SessionExtraction {
                    facts,
                    all_ok: a.all_ok && b.all_ok,
                }
            } else {
                eprintln!(
                    "[weave_session] LLM session 分解失败（{} 条事件），跳过该批次: {e}",
                    lines.len()
                );
                SessionExtraction {
                    facts: Vec::new(),
                    all_ok: false,
                }
            }
        }
    }
}

/// 从 LLM JSON 中取出 (事实, 来源 event_id 列表)。
fn parse_facts_value(v: &serde_json::Value) -> Vec<(String, Vec<String>)> {
    v.get("facts")
        .and_then(|f| f.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    let fact = f
                        .get("fact")
                        .and_then(|x| x.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())?;
                    let srcs: Vec<String> = f
                        .get("source")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    Some((fact, srcs))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 短回显判定（issue #3）：节点文本短（≤64 字符）且其一半以上内容
/// 与查询共享同一段连续子串 —— 典型形态是用户短指令（"装一份 ZeroClaw"）
/// 在向量通道压过真正的解释性答案。仅用于 NYLON_ECHO_DEMOTE 降权。
fn is_query_echo(query_lower: &str, text_lower: &str) -> bool {
    let t_len = text_lower.chars().count();
    if t_len == 0 || t_len > 64 {
        return false;
    }
    longest_common_substring_len(query_lower, text_lower) * 2 >= t_len
}

/// 最长公共子串长度（字符级 DP；两边都是短文本，开销可忽略）。
fn longest_common_substring_len(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev = vec![0usize; b.len() + 1];
    let mut cur = vec![0usize; b.len() + 1];
    let mut best = 0;
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            cur[j] = if a[i - 1] == b[j - 1] {
                prev[j - 1] + 1
            } else {
                0
            };
            best = best.max(cur[j]);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    best
}

/// 从 session 抽取一般世界/常识事实，作为桥接节点使用。失败返回空。
async fn extract_commonsense_bridges(llm: &dyn ChatModel, session_text: &str) -> Vec<String> {
    let system = "You are a commonsense memory linker. Given a dialogue session with turn IDs, extract 1-3 general world or common-sense facts implied by the events that would help connect or infer them. Do not include personal identities or private details. Each fact must be self-contained and neutral. Output ONLY valid JSON: {\"bridges\": [\"...\", ...]}.";
    match llm.chat_json(system, session_text).await {
        Ok(v) => v
            .get("bridges")
            .and_then(|b| b.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        Err(e) => {
            eprintln!("[weave_session] LLM commonsense bridge 分解失败，跳过: {e}");
            Vec::new()
        }
    }
}

/// 从 session 提炼个人化推断（"她对咖啡因的回避始于怀孕"），作为**可输出**的推断节点。
/// 结构性修复：常识桥带 WORLD_KNOWLEDGE_TAG 只许扩散、输出层被过滤，内容永远到不了
/// 作答 LLM；个人化推断锚定对话中的具体人物、不带过滤标记，可进 Top-K 与作答上下文。
/// 失败返回空。
async fn extract_personal_inferences(llm: &dyn ChatModel, session_text: &str) -> Vec<String> {
    let system = "You are a personal memory reasoner. Given a dialogue session, derive 1-3 personalized inference statements that connect or explain facts about the people in the dialogue (motivations, causes, preference changes, life events linking multiple facts). Each statement must: name the specific person(s), be self-contained, be grounded in the dialogue (no generic world knowledge), and be phrased as a careful inference. Output ONLY valid JSON: {\"inferences\": [\"...\", ...]}. If nothing meaningful can be inferred, output {\"inferences\": []}.";
    match llm.chat_json(system, session_text).await {
        Ok(v) => v
            .get("inferences")
            .and_then(|b| b.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        Err(e) => {
            eprintln!("[reflect] LLM 个人化推断分解失败，跳过: {e}");
            Vec::new()
        }
    }
}

/// 反思 worker 的统一入口：会话反思 + 反馈驱动反思（失败回执）。
/// 反馈消费受 NYLON_FEEDBACK_REFLECT=1 控制（记录不受开关影响，已在 API 层落盘）。
async fn process_reflect_work(
    inner: &Arc<Mutex<Inner>>,
    embedder: Option<&Arc<dyn Embedder>>,
    llm: &dyn ChatModel,
    items: Vec<ReflectWork>,
    feedback_done: &mut std::collections::HashSet<String>,
) {
    let mut sessions = Vec::new();
    let mut feedbacks = Vec::new();
    for item in items {
        match item {
            ReflectWork::Session(j) => sessions.push(j),
            ReflectWork::Feedback(f) => {
                // 运行期去重：同一 owner 对同一查询的重复回执只反思一次
                if feedback_done.insert(f.dedup_key()) {
                    feedbacks.push(f);
                }
            }
        }
    }
    if !sessions.is_empty() {
        process_reflection_jobs(inner, embedder, llm, sessions).await;
    }
    if std::env::var("NYLON_FEEDBACK_REFLECT").is_ok() {
        for rec in feedbacks {
            process_feedback(inner, embedder, llm, &rec).await;
        }
    }
}

/// 反馈驱动反思：针对一次失败回答，诊断"缺连接还是缺信息"，
/// 缺连接则补 1-2 条个人化推断节点（可进作答上下文）；缺信息则什么都不写——
/// 反思必须学会"无话可说"，否则变成幻觉注水。
async fn process_feedback(
    inner: &Arc<Mutex<Inner>>,
    embedder: Option<&Arc<dyn Embedder>>,
    llm: &dyn ChatModel,
    rec: &FeedbackRecord,
) {
    let dir = inner.lock().ok().map(|i| i.store.dir().to_path_buf());
    let Some(dir) = dir else { return };

    // 1. 失败上下文：优先客户端回传的展示节点；否则用查询向量取 top-32 近邻
    let mut ctx_ids: Vec<u32> = rec.shown_node_ids.clone();
    if ctx_ids.is_empty() {
        let Some(emb) = embedder else {
            mark_feedback_processed(&dir, &rec.dedup_key());
            return;
        };
        match emb.embed(std::slice::from_ref(&rec.query)).await {
            Ok(v) => {
                if let Some(qv) = v.into_iter().next() {
                    if let Ok(g) = inner.lock() {
                        ctx_ids = g
                            .index
                            .search(&qv, 32)
                            .into_iter()
                            .map(|(id, _)| id)
                            .collect();
                    }
                }
            }
            Err(e) => {
                eprintln!("[reflect] 反馈上下文嵌入失败（下轮重试）: {e}");
                return; // 瞬时错误不打 processed 标记，重启后重试
            }
        }
    }
    // 2. 收集上下文事实（限本租户；排除世界知识桥——只给用户可见的记忆）
    let (ctx_ids, ctx_facts): (Vec<u32>, Vec<String>) = {
        let Ok(g) = inner.lock() else { return };
        let mut ids = Vec::new();
        let mut facts = Vec::new();
        for id in ctx_ids {
            if let Some(n) = g.store.graph().get_node(id) {
                if n.tenant_id == rec.tenant_id
                    && !n
                        .filaments
                        .relations
                        .iter()
                        .any(|r| r == WORLD_KNOWLEDGE_TAG)
                {
                    ids.push(id);
                    facts.push(n.filaments.fact.clone());
                }
            }
        }
        (ids, facts)
    };
    if ctx_facts.is_empty() {
        // 记忆里确实什么都没有——缺信息而非缺连接，无话可说，标记完成
        mark_feedback_processed(&dir, &rec.dedup_key());
        return;
    }
    // 3. LLM 诊断 + 定向推断
    let numbered = ctx_facts
        .iter()
        .enumerate()
        .map(|(i, f)| format!("{}. {}", i + 1, f))
        .collect::<Vec<_>>()
        .join("\n");
    let system = "You are a memory self-repair reasoner. A user asked the memory system a question and reported the answer as unsatisfactory. You are given the memories the system had retrieved. Decide: (a) the retrieved memories DO contain relevant facts, but an unstated connection or inference is missing — then write 1-2 personalized inference statements (naming the people involved, self-contained, phrased as careful inference) that would let a future retrieval answer the question; or (b) the information is simply absent from the memories — then output an empty list (do NOT invent facts not supported by the memories). Output ONLY valid JSON: {\"inferences\": [\"...\", ...]}.";
    let prompt = format!(
        "Question: {}\nReported problem: {} {}\n\nRetrieved memories:\n{}",
        rec.query,
        rec.rating,
        if rec.comment.is_empty() {
            String::new()
        } else {
            format!("({})", rec.comment)
        },
        numbered
    );
    let inferences = match llm.chat_json(system, &prompt).await {
        Ok(v) => v
            .get("inferences")
            .and_then(|b| b.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .take(2)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        Err(e) => {
            eprintln!("[reflect] 反馈反思 LLM 失败（下轮重试）: {e}");
            return; // 瞬时错误不打标记
        }
    };
    // 4. 写回推断节点（边连失败上下文，下次同类查询可被种子/扩散命中）
    let mut wrote = 0usize;
    for inf in inferences {
        match write_personal_inference(
            inner,
            embedder,
            &rec.tenant_id,
            &rec.owner_id,
            &ctx_ids,
            inf,
        )
        .await
        {
            Ok(()) => wrote += 1,
            Err(e) => eprintln!("[reflect] 反馈推断写入失败: {e}"),
        }
    }
    eprintln!(
        "[reflect] 反馈驱动反思 tenant={} owner={} rating={} 补推断 {} 个 :: {:.60}",
        rec.tenant_id, rec.owner_id, rec.rating, wrote, rec.query
    );
    mark_feedback_processed(&dir, &rec.dedup_key());
}

async fn process_reflection_jobs(
    inner: &Arc<Mutex<Inner>>,
    embedder: Option<&Arc<dyn Embedder>>,
    llm: &dyn ChatModel,
    jobs: Vec<ReflectionJob>,
) {
    for job in jobs {
        if job.fact_ids.is_empty() {
            continue;
        }
        // NYLON_WORLD_BRIDGES_OFF=1 可单独关掉泛泛常识桥（A/B 第三臂用）
        let bridges = if std::env::var("NYLON_WORLD_BRIDGES_OFF").is_ok() {
            Vec::new()
        } else {
            extract_commonsense_bridges(llm, &job.session_text).await
        };
        if !bridges.is_empty() {
            let mut wrote = 0usize;
            for bridge in bridges {
                match write_world_bridge(
                    inner,
                    embedder,
                    &job.tenant_id,
                    &job.owner_id,
                    &job.fact_ids,
                    bridge,
                )
                .await
                {
                    Ok(()) => wrote += 1,
                    Err(e) => eprintln!("[reflect] 常识桥接写入失败: {e}"),
                }
            }
            if wrote > 0 {
                eprintln!(
                    "[reflect] tenant={} owner={} 补常识桥接 {} 个",
                    job.tenant_id, job.owner_id, wrote
                );
            }
        }
        // 个人化推断层（NYLON_REFLECT_PERSONAL=1）：锚定具体人物的可输出推断节点，
        // 修复"桥只许扩散不许输出"的结构性问题——推断内容可进 Top-K 与作答上下文。
        if std::env::var("NYLON_REFLECT_PERSONAL").is_ok() {
            let inferences = extract_personal_inferences(llm, &job.session_text).await;
            let mut wrote = 0usize;
            for inf in inferences {
                match write_personal_inference(
                    inner,
                    embedder,
                    &job.tenant_id,
                    &job.owner_id,
                    &job.fact_ids,
                    inf,
                )
                .await
                {
                    Ok(()) => wrote += 1,
                    Err(e) => eprintln!("[reflect] 个人化推断写入失败: {e}"),
                }
            }
            if wrote > 0 {
                eprintln!(
                    "[reflect] tenant={} owner={} 补个人化推断 {} 个",
                    job.tenant_id, job.owner_id, wrote
                );
            }
        }
        // 画像层：聚合人物特质为锚点节点（NYLON_PERSONA_REFLECT=1 开启）
        if std::env::var("NYLON_PERSONA_REFLECT").is_ok() {
            reflect_personas(inner, embedder, llm, &job).await;
        }
    }
}

/// 从 session 抽取/更新人物画像。existing 为已有画像（姓名, 画像文本），供 LLM 融合改写。
async fn extract_personas(
    llm: &dyn ChatModel,
    session_text: &str,
    existing: &[(String, String)],
) -> Vec<(String, String)> {
    let prev = if existing.is_empty() {
        String::new()
    } else {
        let list = existing
            .iter()
            .map(|(n, p)| format!("- {n}: {p}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "\n\nExisting profiles to merge with new evidence (rewrite them, do not copy verbatim; preserve specific identity markers, affiliations and distinctive facts from the existing profiles, do not generalize them away):\n{list}"
        )
    };
    let system = "You are a person-profile memory builder. Given a dialogue session, identify each person with substantive information and write a concise profile aggregating stable traits, preferences, relationships, life situation, and implied values or political/social leanings when inferable (mark inferences with 'likely'). Each profile must be self-contained and grounded in the dialogue. Output ONLY valid JSON: {\"personas\": [{\"name\": \"...\", \"profile\": \"...\"}]}. Skip people with no substantive information.";
    let prompt = format!("{session_text}{prev}");
    // 画像要重写全部已有画像 + 新画像，输出随 session 数增长；
    // 默认 1536 预算在后段 session 必截断（实测失败率 18/58），给足 4096
    match llm.chat_json_budget(system, &prompt, 4096).await {
        Ok(v) => v
            .get("personas")
            .and_then(|p| p.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| {
                        let name = x.get("name")?.as_str()?.trim().to_string();
                        let profile = x.get("profile")?.as_str()?.trim().to_string();
                        if name.is_empty() || profile.is_empty() {
                            None
                        } else {
                            Some((name, profile))
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Err(e) => {
            eprintln!("[reflect] LLM 画像抽取失败，跳过: {e}");
            Vec::new()
        }
    }
}

/// 画像层反思：为 session 中的人物生成/更新画像节点，连边到支撑事实，旧版画像墓碑化。
async fn reflect_personas(
    inner: &Arc<Mutex<Inner>>,
    embedder: Option<&Arc<dyn Embedder>>,
    llm: &dyn ChatModel,
    job: &ReflectionJob,
) {
    // 该 owner 已有画像（姓名小写做键，同人合并）：(节点 id, 姓名小写, 画像文本)
    let existing: Vec<(u32, String, String)> = {
        let inner = match inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        inner
            .store
            .graph()
            .live_nodes()
            .filter(|(_, n)| n.tenant_id == job.tenant_id && n.owner_id == job.owner_id)
            .filter_map(|(id, n)| {
                n.filaments
                    .relations
                    .iter()
                    .find(|r| r.starts_with(PERSONA_NAME_PREFIX))
                    .map(|r| {
                        (
                            id,
                            r[PERSONA_NAME_PREFIX.len()..].to_lowercase(),
                            n.filaments.fact.clone(),
                        )
                    })
            })
            .collect()
    };
    let name_fact: Vec<(String, String)> = existing
        .iter()
        .map(|(_, name, fact)| (name.clone(), fact.clone()))
        .collect();
    let personas = extract_personas(llm, &job.session_text, &name_fact).await;
    for (name, profile) in personas {
        let fact_text = format!("{name}: {profile}");
        match write_persona(
            inner,
            embedder,
            &job.tenant_id,
            &job.owner_id,
            &job.fact_ids,
            &job.leaf_ids,
            &name,
            fact_text,
        )
        .await
        {
            Ok(local) => {
                eprintln!(
                    "[reflect] tenant={} owner={} 画像节点 {} (person:{}): {:.160}",
                    job.tenant_id, job.owner_id, local, name, profile
                );
                // 旧版画像：先把它的全部边继承给新节点（枢纽汇聚，画像生命周期内触达的
                // 事实+叶子不断累积），再墓碑化旧版
                let lower = name.to_lowercase();
                let old_ids: Vec<u32> = existing
                    .iter()
                    .filter(|(id, n, _)| *n == lower && *id != local)
                    .map(|(id, _, _)| *id)
                    .collect();
                if !old_ids.is_empty() {
                    let inherited: Vec<(u32, f32)> = {
                        let inner = match inner.lock() {
                            Ok(g) => g,
                            Err(_) => break,
                        };
                        old_ids
                            .iter()
                            .flat_map(|id| inner.store.graph().neighbors(*id))
                            .collect()
                    };
                    let ticket = {
                        let mut inner = match inner.lock() {
                            Ok(g) => g,
                            Err(_) => break,
                        };
                        let mut t = None;
                        for (target, w) in inherited {
                            match inner.store.add_edge(local, target, w) {
                                Ok(nt) => t = Some(nt),
                                Err(e) => eprintln!("[reflect] 画像边继承失败: {e}"),
                            }
                        }
                        t
                    };
                    if let Some(t) = ticket {
                        let _ = tokio::task::spawn_blocking(move || t.wait()).await;
                    }
                }
                for (old_id, old_name, _) in &existing {
                    if *old_name == lower && *old_id != local {
                        let res = {
                            let mut inner = match inner.lock() {
                                Ok(g) => g,
                                Err(_) => break,
                            };
                            inner.store.remove_node(*old_id)
                        };
                        match res {
                            Ok((true, t)) => {
                                let _ = tokio::task::spawn_blocking(move || t.wait()).await;
                            }
                            Ok((false, _)) => {}
                            Err(e) => eprintln!("[reflect] 旧画像墓碑化失败: {e}"),
                        }
                    }
                }
            }
            Err(e) => eprintln!("[reflect] 画像写入失败: {e}"),
        }
    }
}

#[allow(clippy::too_many_arguments)] // 8 个参数，多为传入的只读上下文；拆结构体收益低于可读性
async fn write_persona(
    inner: &Arc<Mutex<Inner>>,
    embedder: Option<&Arc<dyn Embedder>>,
    tenant_id: &str,
    owner_id: &str,
    fact_ids: &[u32],
    leaf_ids: &[u32],
    name: &str,
    fact_text: String,
) -> Result<u32, Status> {
    let embedding = if let Some(emb) = embedder {
        emb.embed(std::slice::from_ref(&fact_text))
            .await
            .map_err(|e| Status::internal(format!("嵌入失败: {e}")))?
            .pop()
    } else {
        None
    };
    let now = now_secs();
    let node = MemoryNode {
        id: 0,
        tenant_id: tenant_id.to_string(),
        owner_id: owner_id.to_string(),
        filaments: Filaments {
            fact: fact_text,
            emotion_valence: 0.0,
            emotion_intensity: 0.0,
            created_at: now,
            decay_rate: 0.01,
            relations: vec![
                PERSONA_TAG.to_string(),
                format!("{PERSONA_NAME_PREFIX}{name}"),
            ],
            confidence: 0.6, // 推断内容，低于事实层
            mentions_7d: 0,
        },
        tension: Tension {
            baseline: 0.7, // 实体锚点，高于普通事实
            last_updated: now,
        },
        embedding: embedding.unwrap_or_default(),
    };
    let index_embedding = node.embedding.clone();
    let (local, ticket) = {
        let mut inner = inner
            .lock()
            .map_err(|_| Status::internal("state lock poisoned"))?;
        let (local, node_ticket) = inner
            .store
            .add_node(node)
            .map_err(|e| Status::internal(format!("wal append: {e}")))?;
        if !index_embedding.is_empty() {
            inner.index.add(local, &index_embedding);
        }
        let mut t = Some(node_ticket);
        for fid in fact_ids {
            t = Some(
                inner
                    .store
                    .add_edge(local, *fid, PERSONA_EDGE_WEIGHT)
                    .map_err(|e| Status::internal(format!("wal append: {e}")))?,
            );
        }
        for lid in leaf_ids {
            t = Some(
                inner
                    .store
                    .add_edge(local, *lid, PERSONA_LEAF_EDGE_WEIGHT)
                    .map_err(|e| Status::internal(format!("wal append: {e}")))?,
            );
        }
        (local, t)
    };
    if let Some(t) = ticket {
        tokio::task::spawn_blocking(move || t.wait())
            .await
            .map_err(|e| Status::internal(format!("durability join: {e}")))?
            .map_err(|e| Status::internal(format!("wal durability: {e}")))?;
    }
    Ok(local)
}

async fn write_world_bridge(
    inner: &Arc<Mutex<Inner>>,
    embedder: Option<&Arc<dyn Embedder>>,
    tenant_id: &str,
    owner_id: &str,
    fact_ids: &[u32],
    bridge: String,
) -> Result<(), Status> {
    let embedding = if let Some(emb) = embedder {
        emb.embed(std::slice::from_ref(&bridge))
            .await
            .map_err(|e| Status::internal(format!("嵌入失败: {e}")))?
            .pop()
    } else {
        None
    };
    let now = now_secs();
    let node = MemoryNode {
        id: 0,
        tenant_id: tenant_id.to_string(),
        owner_id: owner_id.to_string(),
        filaments: Filaments {
            fact: bridge,
            emotion_valence: 0.0,
            emotion_intensity: 0.0,
            created_at: now,
            decay_rate: 0.01,
            relations: vec![WORLD_KNOWLEDGE_TAG.to_string()],
            confidence: 0.6,
            mentions_7d: 0,
        },
        tension: Tension {
            baseline: 0.6,
            last_updated: now,
        },
        embedding: embedding.unwrap_or_default(),
    };
    let index_embedding = node.embedding.clone();
    let ticket = {
        let mut inner = inner
            .lock()
            .map_err(|_| Status::internal("state lock poisoned"))?;
        let (local, node_ticket) = inner
            .store
            .add_node(node)
            .map_err(|e| Status::internal(format!("wal append: {e}")))?;
        if !index_embedding.is_empty() {
            inner.index.add(local, &index_embedding);
        }
        let mut t = Some(node_ticket);
        for fid in fact_ids {
            t = Some(
                inner
                    .store
                    .add_edge(local, *fid, WORLD_BRIDGE_EDGE_WEIGHT)
                    .map_err(|e| Status::internal(format!("wal append: {e}")))?,
            );
        }
        t
    };
    if let Some(t) = ticket {
        tokio::task::spawn_blocking(move || t.wait())
            .await
            .map_err(|e| Status::internal(format!("durability join: {e}")))?
            .map_err(|e| Status::internal(format!("wal durability: {e}")))?;
    }
    Ok(())
}

/// 写个人化推断节点：不带 WORLD_KNOWLEDGE_TAG（可进 resonate 输出），
/// relations 打 "inferred" 标记供下游/评测区分，边连来源事实。
async fn write_personal_inference(
    inner: &Arc<Mutex<Inner>>,
    embedder: Option<&Arc<dyn Embedder>>,
    tenant_id: &str,
    owner_id: &str,
    fact_ids: &[u32],
    inference: String,
) -> Result<(), Status> {
    let embedding = if let Some(emb) = embedder {
        emb.embed(std::slice::from_ref(&inference))
            .await
            .map_err(|e| Status::internal(format!("嵌入失败: {e}")))?
            .pop()
    } else {
        None
    };
    let now = now_secs();
    let node = MemoryNode {
        id: 0,
        tenant_id: tenant_id.to_string(),
        owner_id: owner_id.to_string(),
        filaments: Filaments {
            fact: inference,
            emotion_valence: 0.0,
            emotion_intensity: 0.0,
            created_at: now,
            decay_rate: 0.01,
            relations: vec![INFERRED_TAG.to_string()],
            confidence: 0.55,
            mentions_7d: 0,
        },
        tension: Tension {
            baseline: 0.65,
            last_updated: now,
        },
        embedding: embedding.unwrap_or_default(),
    };
    let index_embedding = node.embedding.clone();
    let ticket = {
        let mut inner = inner
            .lock()
            .map_err(|_| Status::internal("state lock poisoned"))?;
        let (local, node_ticket) = inner
            .store
            .add_node(node)
            .map_err(|e| Status::internal(format!("wal append: {e}")))?;
        if !index_embedding.is_empty() {
            inner.index.add(local, &index_embedding);
        }
        let mut t = Some(node_ticket);
        for fid in fact_ids {
            t = Some(
                inner
                    .store
                    .add_edge(local, *fid, INFER_EDGE_WEIGHT)
                    .map_err(|e| Status::internal(format!("wal append: {e}")))?,
            );
        }
        t
    };
    if let Some(t) = ticket {
        tokio::task::spawn_blocking(move || t.wait())
            .await
            .map_err(|e| Status::internal(format!("durability join: {e}")))?
            .map_err(|e| Status::internal(format!("wal durability: {e}")))?;
    }
    Ok(())
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
        let (local, linked, conflict_nodes) = self
            .weave_one(
                &r.tenant_id,
                &r.owner_id,
                &r.raw_event,
                r.context,
                WeaveLlmMode::Full,
            )
            .await?;
        self.audit_op(
            "weave",
            &r.tenant_id,
            &r.owner_id,
            format!(
                "node={} linked={} conflicts={}",
                local,
                linked.len(),
                conflict_nodes.len()
            ),
        );
        Ok(Response::new(WeaveResponse {
            node_id: local as u64,
            linked_nodes: linked,
            conflict_nodes,
        }))
    }

    /// 双层写入：叶子层=逐事件原文入库；抽象层=引擎侧 LLM session 分解出原子事实，
    /// 事实节点与来源叶子节点之间建显式 derived 边（层间互联，供共振跨层扩散）。
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
        if r.tenant_id.is_empty() || r.owner_id.is_empty() {
            return Err(Status::invalid_argument("tenant_id / owner_id 均不能为空"));
        }
        let events: Vec<&SessionEvent> = r.events.iter().filter(|e| !e.text.is_empty()).collect();
        // 叶子层：逐事件原文（启发式路径，与验证过的双层实验口径一致）
        // NYLON_SESSION_DEDUP=1：同 (tenant, owner) 下原文相同的事件复用既有节点，
        // 客户端重试/回灌相同 event 不再产生重复叶子（issue #3 可选幂等）。
        let dedup_on = std::env::var("NYLON_SESSION_DEDUP").is_ok();
        let mut text2node: std::collections::HashMap<String, u32> = if dedup_on {
            let inner = self
                .inner
                .lock()
                .map_err(|_| Status::internal("state lock poisoned"))?;
            inner
                .store
                .graph()
                .live_nodes()
                .filter(|(_, n)| n.tenant_id == r.tenant_id && n.owner_id == r.owner_id)
                .map(|(id, n)| (n.filaments.fact.clone(), id))
                .collect()
        } else {
            std::collections::HashMap::new()
        };
        let mut leaf_nodes: Vec<EventNode> = Vec::new();
        let mut id2local: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for ev in &events {
            let raw = format!("{}: {}", ev.speaker, ev.text);
            let local = match text2node.get(&raw) {
                Some(&id) => id,
                None => {
                    let (local, _, _) = self
                        .weave_one(&r.tenant_id, &r.owner_id, &raw, None, WeaveLlmMode::Off)
                        .await?;
                    if dedup_on {
                        text2node.insert(raw.clone(), local);
                    }
                    local
                }
            };
            if !ev.event_id.is_empty() {
                id2local.insert(ev.event_id.clone(), local);
            }
            leaf_nodes.push(EventNode {
                event_id: ev.event_id.clone(),
                node_id: local as u64,
            });
        }
        // 抽象层：整段 session 一次 LLM 调用，分解为原子事实+来源标注
        let mut fact_nodes: Vec<FactNode> = Vec::new();
        let mut derived: Vec<(u32, u32)> = Vec::new();
        let mut abstract_status = "skipped";
        if !r.skip_abstract && !events.is_empty() {
            if let Some(llm) = &self.llm {
                let lines: Vec<String> = events
                    .iter()
                    .map(|ev| {
                        if ev.event_id.is_empty() {
                            format!("{}: {}", ev.speaker, ev.text)
                        } else {
                            format!("{} {}: {}", ev.event_id, ev.speaker, ev.text)
                        }
                    })
                    .collect();
                let extraction = extract_session_facts(llm.as_ref(), &lines).await;
                abstract_status = if !extraction.facts.is_empty() {
                    "ok"
                } else if extraction.all_ok {
                    "empty"
                } else {
                    "failed"
                };
                for (fact, sources) in extraction.facts {
                    let (local, _, _) = self
                        .weave_one(&r.tenant_id, &r.owner_id, &fact, None, WeaveLlmMode::Off)
                        .await?;
                    for sid in &sources {
                        if let Some(l) = id2local.get(sid) {
                            derived.push((local, *l));
                        }
                    }
                    fact_nodes.push(FactNode {
                        node_id: local as u64,
                        fact,
                        source_event_ids: sources,
                    });
                }
                if std::env::var("NYLON_WORLD_BRIDGES_ASYNC").is_err()
                    && std::env::var("NYLON_WORLD_BRIDGES").is_ok()
                    && !fact_nodes.is_empty()
                {
                    let bridges =
                        extract_commonsense_bridges(llm.as_ref(), &lines.join("\n")).await;
                    let fact_ids: Vec<u32> = fact_nodes.iter().map(|f| f.node_id as u32).collect();
                    for bridge in bridges {
                        let ctx = Some(pb::ContextSpectrum {
                            task: Some(WORLD_KNOWLEDGE_TAG.to_string()),
                            emotion_valence: None,
                            device: None,
                            max_hops: None,
                        });
                        let (world_local, _, _) = self
                            .weave_one(&r.tenant_id, &r.owner_id, &bridge, ctx, WeaveLlmMode::Off)
                            .await?;
                        let ticket = {
                            let mut inner = self
                                .inner
                                .lock()
                                .map_err(|_| Status::internal("state lock poisoned"))?;
                            let mut t = None;
                            for fid in &fact_ids {
                                t = Some(
                                    inner
                                        .store
                                        .add_edge(world_local, *fid, WORLD_BRIDGE_EDGE_WEIGHT)
                                        .map_err(|e| {
                                            Status::internal(format!("wal append: {e}"))
                                        })?,
                                );
                            }
                            t
                        };
                        if let Some(t) = ticket {
                            tokio::task::spawn_blocking(move || t.wait())
                                .await
                                .map_err(|e| Status::internal(format!("durability join: {e}")))?
                                .map_err(|e| Status::internal(format!("wal durability: {e}")))?;
                        }
                    }
                }
                if std::env::var("NYLON_WORLD_BRIDGES_ASYNC").is_ok() && !fact_nodes.is_empty() {
                    if let Some(tx) = &self.reflect_tx {
                        let job = ReflectionJob {
                            tenant_id: r.tenant_id.clone(),
                            owner_id: r.owner_id.clone(),
                            session_text: lines.join("\n"),
                            fact_ids: fact_nodes.iter().map(|f| f.node_id as u32).collect(),
                            leaf_ids: leaf_nodes.iter().map(|l| l.node_id as u32).collect(),
                        };
                        if tx.send(ReflectWork::Session(job)).is_err() {
                            eprintln!("[weave_session] reflection worker 已关闭，跳过异步常识桥接");
                        }
                    }
                }
            } else {
                abstract_status = "disabled";
            }
        }
        // 层间显式边：统一一批写入，一次 durability 等待
        // NYLON_DERIVED_EDGES=1 才开（实测全开对 Cat2/3 有负收益，自动建边的隐式互联更稳，默认关）
        let derived_on = std::env::var("NYLON_DERIVED_EDGES")
            .map(|v| v != "0")
            .unwrap_or(false);
        if derived_on && !derived.is_empty() {
            let ticket = {
                let mut inner = self
                    .inner
                    .lock()
                    .map_err(|_| Status::internal("state lock poisoned"))?;
                let mut t = None;
                for (a, b) in derived {
                    t = Some(
                        inner
                            .store
                            .add_edge(a, b, DERIVED_EDGE_WEIGHT)
                            .map_err(|e| Status::internal(format!("wal append: {e}")))?,
                    );
                }
                t
            };
            if let Some(t) = ticket {
                tokio::task::spawn_blocking(move || t.wait())
                    .await
                    .map_err(|e| Status::internal(format!("durability join: {e}")))?
                    .map_err(|e| Status::internal(format!("wal durability: {e}")))?;
            }
        }
        self.audit_op(
            "weave_session",
            &r.tenant_id,
            &r.owner_id,
            format!("leaves={} facts={}", leaf_nodes.len(), fact_nodes.len()),
        );
        Ok(Response::new(WeaveSessionResponse {
            leaf_nodes,
            fact_nodes,
            abstract_status: abstract_status.to_string(),
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
        if r.tenant_id.is_empty() || r.owner_id.is_empty() {
            return Err(Status::invalid_argument("tenant_id / owner_id 不能为空"));
        }
        let ctx = to_context(r.context);
        let query = r.query.to_lowercase();
        // 向量种子（锁外计算）；HNSW 是全局索引，候选必须按 tenant+owner 过滤（L2.1）
        let mut qvec: Option<Vec<f32>> = None;
        let vec_seeds: Vec<(u32, f32)> =
            if let (Some(emb), false) = (&self.embedder, query.is_empty()) {
                match emb.embed(std::slice::from_ref(&r.query)).await {
                    Ok(v) => {
                        self.note_embed_ok();
                        qvec = v.first().cloned();
                        let inner = self
                            .inner
                            .lock()
                            .map_err(|_| Status::internal("state lock poisoned"))?;
                        // 过取 4 倍再按租户过滤，避免过滤后种子不足
                        inner
                            .index
                            .search(&v[0], max_seeds().saturating_mul(4))
                            .into_iter()
                            .filter(|(id, _)| {
                                inner
                                    .store
                                    .graph()
                                    .get_node(*id)
                                    .map(|n| n.tenant_id == r.tenant_id && n.owner_id == r.owner_id)
                                    .unwrap_or(false)
                            })
                            .take(max_seeds())
                            .collect()
                    }
                    Err(e) => {
                        // 嵌入服务故障时降级为纯词面——故障计入健康状态（issue #4），不再无声
                        self.note_embed_failure(&e.to_string());
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
        let inner = self
            .inner
            .lock()
            .map_err(|_| Status::internal("state lock poisoned"))?;
        let g = inner.store.graph();

        // 种子选择：query 词项重叠打分（完整子串命中加权） > task 命中关系丝 > 最近节点兜底。
        // 词项化按非字母数字切分；CJK 查询无空格时退化为整串 contains，行为与原先一致。
        let mut lex_seeds: Vec<(u32, f32)> = Vec::new();
        if !query.is_empty() {
            let mut terms: Vec<&str> = query
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| !t.is_empty())
                .filter(|t| t.len() >= 3 && !STOPWORDS.contains(t))
                .collect();
            if terms.is_empty() {
                // 全是停用词的查询（如 "When did they meet?"）：退回未过滤词项
                terms = query
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|t| !t.is_empty())
                    .collect();
            }
            // 第一遍：统计词项文档频率（df），IDF 加权——稀有词（实体）权重远高于常见词
            let owner_facts: Vec<(u32, String)> = g
                .live_nodes()
                .filter(|(_, n)| n.tenant_id == r.tenant_id && n.owner_id == r.owner_id)
                .map(|(id, n)| (id, n.filaments.fact.to_lowercase()))
                .collect();
            let n_docs = owner_facts.len().max(1) as f32;
            let mut df: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
            for (_, fact) in &owner_facts {
                for t in &terms {
                    if fact.contains(*t) {
                        *df.entry(*t).or_insert(0) += 1;
                    }
                }
            }
            let idf_len = |t: &str| {
                let d = df.get(t).copied().unwrap_or(0) as f32;
                (((n_docs + 1.0) / (d + 1.0)).ln() + 1.0) * (t.len().min(8) as f32)
            };
            let full: f32 = terms.iter().map(|t| idf_len(t)).sum();
            let norm = (full * 2.0).max(1.0);
            let mut scored: Vec<(u32, f32)> = owner_facts
                .iter()
                .filter_map(|(id, fact)| {
                    let mut score: f32 = terms
                        .iter()
                        .filter(|t| fact.contains(**t))
                        .map(|t| idf_len(t))
                        .sum();
                    if !terms.is_empty() && fact.contains(&query) {
                        score += full; // 完整子串命中额外加权
                    }
                    if score > 0.0 {
                        Some((*id, score))
                    } else {
                        None
                    }
                })
                .collect();
            scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            lex_seeds = scored
                .into_iter()
                .map(|(id, sc)| (id, (sc / norm).clamp(0.1, 1.0)))
                .collect();
        }
        // 融合向量种子：去重后并入（词面优先）
        // 融合：向量种子保底 VEC_SEED_QUOTA 个名额，词面种子去重补满
        let mut seeds: Vec<(u32, f32)> = Vec::new();
        for (id, sim) in vec_seeds.into_iter().take(VEC_SEED_QUOTA) {
            seeds.push((id, sim.clamp(0.05, 1.0)));
        }
        for (id, w) in lex_seeds {
            if seeds.len() >= max_seeds() {
                break;
            }
            if let Some(slot) = seeds.iter_mut().find(|(sid, _)| *sid == id) {
                slot.1 = (slot.1.max(w) + 0.15).min(1.0); // 词面+向量双通道命中：取高者并加成
            } else {
                seeds.push((id, w));
            }
        }
        if seeds.is_empty() {
            if let Some(task) = &ctx.task {
                seeds = g
                    .find_by_filaments(&FilamentFilter {
                        relations_any: Some(vec![task.clone()]),
                        ..Default::default()
                    })
                    .into_iter()
                    .filter(|&id| {
                        g.get_node(id)
                            .map(|n| n.tenant_id == r.tenant_id && n.owner_id == r.owner_id)
                            .unwrap_or(false)
                    })
                    .map(|id| (id, 0.5f32))
                    .collect();
            }
        }
        if seeds.is_empty() {
            let mut recent: Vec<(u32, i64)> = g
                .live_nodes()
                .filter(|(_, n)| n.tenant_id == r.tenant_id && n.owner_id == r.owner_id)
                .map(|(id, n)| (id, n.filaments.created_at))
                .collect();
            recent.sort_by_key(|&(_, ts)| std::cmp::Reverse(ts));
            seeds = recent.into_iter().map(|(id, _)| (id, 0.5f32)).collect();
        }
        seeds.truncate(max_seeds());

        let budget = if r.budget == 0 {
            nylon_graph::DEFAULT_BUDGET
        } else {
            r.budget as usize
        };
        let tension_floor = std::env::var("NYLON_TENSION_FLOOR")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.0);
        let seed_quota = std::env::var("NYLON_SEED_QUOTA")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let rerank_alpha = std::env::var("NYLON_RERANK_VEC")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.0);
        // NYLON_ECHO_DEMOTE（0,1）：与查询高度重叠的短文本（如"装一份 ZeroClaw"
        // 这类用户指令回显）向量分往往压过真正的解释性内容，按比例降权（issue #3）。
        let echo_demote = std::env::var("NYLON_ECHO_DEMOTE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|f| *f > 0.0 && *f < 1.0)
            .unwrap_or(0.0);
        // NYLON_MULTIPATH_BONUS（默认 0=关闭）：多路径佐证系数。被 k 个种子经独立
        // 路径到达的节点，在向量重排后的混合分上乘 1+bonus*min(k-1,3)。
        // 必须加在重排之后：图内共振分与余弦分尺度差异大，加在图内会被 blend 稀释
        // （2026-09-23 A/B 实测：图内加成 recall 变化 ±0.1pp，洗脱实锤）。
        let multipath_bonus = std::env::var("NYLON_MULTIPATH_BONUS")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.0);
        // 种子保底提升统一在向量重排之后做（服务侧），否则重排会打乱图内的提升结果
        let (mut activated, corroboration) = g.resonate_opts(
            &seeds,
            &ctx,
            now_secs(),
            budget,
            tension_floor,
            0,
            multipath_bonus,
        );
        // 种子补齐：扩散阶段可能因 budget 截断/张力门槛把部分种子挡在激活集外
        // （2026-09-14 十会话评测发现 13 例 seed_hit=true 但 evidence_pos=None）。
        // 直接命中的种子必须留在候选集内，交由后续重排/保底决定最终位次。
        {
            let present: std::collections::HashSet<u32> =
                activated.iter().map(|(id, _)| *id).collect();
            for (sid, sscore) in &seeds {
                if !present.contains(sid) {
                    activated.push((*sid, *sscore));
                }
            }
        }
        // 向量重排：用查询向量对激活集做直接余弦相似度混合打分，校正共振排序
        if rerank_alpha > 0.0 {
            if let Some(q) = &qvec {
                let qn = q.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
                for (id, s) in activated.iter_mut() {
                    let sim = g
                        .get_node(*id)
                        .filter(|n| n.embedding.len() == q.len())
                        .map(|n| {
                            let dot: f32 =
                                q.iter().zip(n.embedding.iter()).map(|(a, b)| a * b).sum();
                            let nn = n
                                .embedding
                                .iter()
                                .map(|x| x * x)
                                .sum::<f32>()
                                .sqrt()
                                .max(1e-9);
                            dot / (qn * nn)
                        })
                        .unwrap_or(0.0);
                    *s = (1.0 - rerank_alpha) * *s + rerank_alpha * sim;
                }
                activated.sort_by(|a, b| b.1.total_cmp(&a.1));
            }
        }
        if echo_demote > 0.0 && !query.is_empty() {
            for (id, s) in activated.iter_mut() {
                let is_echo = g
                    .get_node(*id)
                    .map(|n| is_query_echo(&query, &n.filaments.fact.to_lowercase()))
                    .unwrap_or(false);
                if is_echo {
                    *s *= echo_demote;
                }
            }
            activated.sort_by(|a, b| b.1.total_cmp(&a.1));
        }
        // 多路径佐证加成：混合打分（共振+向量）之后、种子保底置顶之前应用。
        // 被多个种子独立到达的证据节点上浮，单路径高相似节点相对下沉。
        if multipath_bonus > 0.0 && !corroboration.is_empty() {
            for (id, s) in activated.iter_mut() {
                if let Some(&k) = corroboration.get(id) {
                    if k > 1 {
                        *s *= 1.0 + multipath_bonus * (k - 1).min(3) as f32;
                    }
                }
            }
            activated.sort_by(|a, b| b.1.total_cmp(&a.1));
        }
        // 种子保底：直接命中的种子提升置顶（取重排后种子的相对顺序），
        // 防止词面/向量双通道的精确命中被高张力扩散邻居挤出 Top-K
        if seed_quota > 0 {
            let seed_set: std::collections::HashSet<u32> = seeds.iter().map(|&(s, _)| s).collect();
            let mut hoisted: Vec<(u32, f32)> = Vec::new();
            let mut rest: Vec<(u32, f32)> = Vec::with_capacity(activated.len());
            for item in activated {
                if hoisted.len() < seed_quota && seed_set.contains(&item.0) {
                    hoisted.push(item);
                } else {
                    rest.push(item);
                }
            }
            hoisted.extend(rest);
            activated = hoisted;
        }
        let mut out: Vec<_> = activated
            .into_iter()
            .filter_map(|(id, score)| {
                let n = g.get_node(id)?;
                // 隔离兜底：扩散结果不允许跨租户（L2.1）
                if n.tenant_id != r.tenant_id {
                    return None;
                }
                if n.filaments
                    .relations
                    .iter()
                    .any(|r| r == WORLD_KNOWLEDGE_TAG)
                {
                    return None;
                }
                Some(to_activated(id, score, n))
            })
            .collect();
        // top_k（issue #3）：显式返回条数上限；0 = 不限制（历史行为）。
        // budget 只控制图扩散的激活规模，不负责截断返回。
        if r.top_k > 0 {
            out.truncate(r.top_k as usize);
        }
        // 失败驱动反思的数据采集（v2）：零命中/弱命中查询追加到 JSONL，
        // 只用引擎内部信号（命中数/最高张力），不依赖任何金标签。
        // 离线反思 worker 读这个日志对失败簇定向补推断节点。
        if let Ok(path) = std::env::var("NYLON_FAILURE_LOG") {
            let top = out.first().map(|a| a.resonance).unwrap_or(0.0);
            let min_score = std::env::var("NYLON_FAILURE_MIN_SCORE")
                .ok()
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(0.5);
            // NYLON_FAILURE_LOG_ALL=1：全量记录（离线调阈值/分析用）；
            // 否则只记失败嫌疑：无种子、零命中、或 top 张力低于阈值。
            let log_all = std::env::var("NYLON_FAILURE_LOG_ALL").is_ok();
            if log_all || seeds.is_empty() || out.is_empty() || top < min_score {
                let esc = |s: &str| {
                    s.replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace(['\n', '\r'], " ")
                };
                let line = format!(
                    "{{\"ts\":{},\"tenant\":\"{}\",\"owner\":\"{}\",\"query\":\"{}\",\"hits\":{},\"seeds\":{},\"top\":{:.4}}}\n",
                    now_secs(),
                    esc(&r.tenant_id),
                    esc(&r.owner_id),
                    esc(&r.query),
                    out.len(),
                    seeds.len(),
                    top
                );
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    use std::io::Write;
                    let _ = f.write_all(line.as_bytes());
                }
            }
        }
        self.audit_op(
            "resonate",
            &r.tenant_id,
            &r.owner_id,
            format!(
                "query={:.80} hits={} seeds={}",
                r.query,
                out.len(),
                seeds.len()
            ),
        );
        Ok(Response::new(ResonateResponse {
            activated: out,
            seed_ids: seeds.iter().map(|&(sid, _)| sid as u64).collect(),
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
        let inner = self
            .inner
            .lock()
            .map_err(|_| Status::internal("state lock poisoned"))?;
        let dims = inner.index.dims();
        let raw: Vec<f32> = r
            .query_embedding
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        // 维度对齐：截断或补零（Phase 1 宽容策略，避免维度演进期硬失败）
        let mut query = vec![0.0f32; dims];
        for (i, v) in raw.iter().take(dims).enumerate() {
            query[i] = *v;
        }
        let k = if r.top_k == 0 { 10 } else { r.top_k as usize };
        let g = inner.store.graph();
        // HNSW 全局索引：过取 4 倍再按 tenant+owner 过滤（L2.1 强制隔离）
        let out: Vec<_> = inner
            .index
            .search(&query, k.saturating_mul(4))
            .into_iter()
            .filter_map(|(id, sim)| {
                g.get_node(id).and_then(|n| {
                    (n.tenant_id == r.tenant_id && n.owner_id == r.owner_id)
                        .then(|| to_activated(id, sim, n))
                })
            })
            .take(k)
            .collect();
        self.audit_op(
            "search",
            &r.tenant_id,
            &r.owner_id,
            format!("top_k={} hits={}", k, out.len()),
        );
        Ok(Response::new(SearchResponse { neighbors: out }))
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
        let inner = self
            .inner
            .lock()
            .map_err(|_| Status::internal("state lock poisoned"))?;
        let local = u32::try_from(r.node_id)
            .map_err(|_| Status::invalid_argument("node_id 超出局部 ID 范围"))?;
        let node = inner
            .store
            .graph()
            .get_node(local)
            .ok_or_else(|| Status::not_found(format!("node {} 不存在或已删除", r.node_id)))?;
        // 跨租户读取一律按不存在处理，不暴露节点存在性（L2.1）
        if node.tenant_id != r.tenant_id {
            self.audit_op(
                "get_node",
                &r.tenant_id,
                "",
                format!("node={} miss(cross-tenant)", r.node_id),
            );
            return Err(Status::not_found(format!(
                "node {} 不存在或已删除",
                r.node_id
            )));
        }
        let tension = compute_tension(node, now_secs(), 1.0);
        self.audit_op(
            "get_node",
            &r.tenant_id,
            &node.owner_id,
            format!("node={}", r.node_id),
        );
        Ok(Response::new(GetNodeResponse {
            node_id: r.node_id,
            filaments: Some(to_pb_filaments(&node.filaments)),
            current_tension: tension,
        }))
    }

    /// 回答质量回执（反馈驱动反思入口）：持久化到 feedback.jsonl（先落盘），
    /// 再入队反思 worker；worker 空闲时对失败簇定向补个人化推断。
    /// 记录行为不受 NYLON_FEEDBACK_REFLECT 影响（开关只管 LLM 消费）。
    async fn report_feedback(
        &self,
        req: Request<FeedbackRequest>,
    ) -> Result<Response<FeedbackResponse>, Status> {
        let grant = req.extensions().get::<KeyGrant>().cloned();
        let r = req.into_inner();
        self.check(
            grant.as_ref(),
            Scope::Write,
            &r.tenant_id,
            "report_feedback",
            &r.owner_id,
        )?;
        if r.tenant_id.is_empty() || r.owner_id.is_empty() || r.query.is_empty() {
            return Err(Status::invalid_argument(
                "tenant_id / owner_id / query 不能为空",
            ));
        }
        let rec = FeedbackRecord {
            tenant_id: r.tenant_id.clone(),
            owner_id: r.owner_id.clone(),
            query: r.query.clone(),
            rating: if r.rating.is_empty() {
                "down".to_string()
            } else {
                r.rating.clone()
            },
            comment: r.comment.clone(),
            shown_node_ids: r.shown_node_ids.iter().map(|&v| v as u32).collect(),
            ts: now_secs(),
        };
        let dir = {
            let inner = self
                .inner
                .lock()
                .map_err(|_| Status::internal("state lock poisoned"))?;
            inner.store.dir().to_path_buf()
        };
        append_feedback_log(&dir, &rec)?;
        if let Some(tx) = &self.reflect_tx {
            let _ = tx.send(ReflectWork::Feedback(rec.clone()));
        }
        self.audit_op(
            "report_feedback",
            &rec.tenant_id,
            &rec.owner_id,
            format!("rating={} query={:.60}", rec.rating, rec.query),
        );
        Ok(Response::new(FeedbackResponse { recorded: true }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nylon_embed::StubEmbedder;
    use nylon_llm::StubChatModel;
    use std::sync::Arc;

    fn svc_with_embed(dims: usize) -> (EngineService, Arc<StubEmbedder>) {
        let dir = tempfile::tempdir().unwrap();
        let store = PersistentGraph::open(dir.path()).unwrap();
        let embedder = Arc::new(StubEmbedder::new(dims));
        let svc = EngineService::new(store, dims, Some(embedder.clone()), None);
        // tempdir 借 service 的 PersistentGraph 存活不够——泄漏句柄换取测试期有效
        std::mem::forget(dir);
        (svc, embedder)
    }

    async fn weave_as(svc: &EngineService, tenant: &str, owner: &str, fact: &str) -> u64 {
        let resp = svc
            .weave(Request::new(WeaveRequest {
                tenant_id: tenant.into(),
                owner_id: owner.into(),
                raw_event: fact.into(),
                context: None,
            }))
            .await
            .unwrap();
        resp.into_inner().node_id
    }

    /// 反馈驱动反思全链路：回执落盘 → 空闲 worker 消费 → inferred 推断节点生成。
    #[tokio::test]
    async fn feedback_recorded_and_reflected_to_inference_node() {
        std::env::set_var("NYLON_FEEDBACK_REFLECT", "1");
        std::env::set_var("NYLON_REFLECT_IDLE_SECS", "1");
        let canned = serde_json::json!({
            "inferences": ["Alice likely prefers window seats because she works on long flights."]
        });
        let dir = tempfile::tempdir().unwrap();
        let store = PersistentGraph::open(dir.path()).unwrap();
        let embedder = Arc::new(StubEmbedder::new(64));
        let llm = Arc::new(StubChatModel::new(canned));
        let svc = EngineService::new(store, 64, Some(embedder), Some(llm));

        svc.weave(Request::new(WeaveRequest {
            tenant_id: "fb-test".into(),
            owner_id: "alice".into(),
            raw_event: "Alice prefers window seats".into(),
            context: None,
        }))
        .await
        .unwrap();

        let resp = svc
            .report_feedback(Request::new(FeedbackRequest {
                tenant_id: "fb-test".into(),
                owner_id: "alice".into(),
                query: "Which seat does Alice like?".into(),
                rating: "down".into(),
                comment: String::new(),
                shown_node_ids: Vec::new(),
            }))
            .await
            .unwrap();
        assert!(resp.into_inner().recorded);
        // 先落盘：feedback.jsonl 立即存在（崩溃不丢）
        assert!(dir.path().join("feedback.jsonl").exists());

        // 等空闲反思（1s 空闲触发）写出推断节点 + processed 标记
        // （标记在节点之后落盘，两者必须一起等，否则竞态抖动）
        let mut found = false;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            let hit = {
                let inner = svc.inner.lock().unwrap();
                let mut any_inferred = false;
                for (_, n) in inner.store.graph().live_nodes() {
                    if n.filaments.relations.iter().any(|r| r == INFERRED_TAG) {
                        any_inferred = true;
                        break;
                    }
                }
                any_inferred
            };
            if hit && dir.path().join("feedback-processed.jsonl").exists() {
                found = true;
                break;
            }
        }
        std::env::remove_var("NYLON_FEEDBACK_REFLECT");
        std::env::remove_var("NYLON_REFLECT_IDLE_SECS");
        assert!(found, "反馈反思应生成 inferred 推断节点");
    }

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

    /// L2.1：跨租户共振不可见（词面 + 最近兜底两条种子路径都覆盖）。
    #[tokio::test]
    async fn resonate_isolated_across_tenants() {
        let (svc, _emb) = svc_with_embed(64);
        let a_id = weave_as(&svc, "tenant-a", "alice", "喜欢手冲咖啡和浅烘豆").await;
        let b_id = weave_as(&svc, "tenant-b", "alice", "喜欢手冲咖啡和深烘豆").await;

        let resp = svc
            .resonate(Request::new(ResonateRequest {
                tenant_id: "tenant-a".into(),
                owner_id: "alice".into(),
                query: "咖啡".into(),
                context: None,
                budget: 10,
                top_k: 0,
            }))
            .await
            .unwrap()
            .into_inner();
        let ids: Vec<u64> = resp.activated.iter().map(|n| n.node_id).collect();
        assert!(ids.contains(&a_id), "本租户节点应命中: {ids:?}");
        assert!(
            !ids.contains(&b_id),
            "跨租户节点不得出现在共振结果: {ids:?}"
        );

        // 空查询走最近节点兜底，同样不得跨租户
        let resp = svc
            .resonate(Request::new(ResonateRequest {
                tenant_id: "tenant-b".into(),
                owner_id: "alice".into(),
                query: String::new(),
                context: None,
                budget: 10,
                top_k: 0,
            }))
            .await
            .unwrap()
            .into_inner();
        let ids: Vec<u64> = resp.activated.iter().map(|n| n.node_id).collect();
        assert!(ids.contains(&b_id));
        assert!(!ids.contains(&a_id), "兜底种子也不得跨租户: {ids:?}");
    }

    /// issue #3：top_k 是返回条数硬上限；budget 只控制扩散规模。
    #[tokio::test]
    async fn resonate_top_k_caps_returned_count() {
        let (svc, _emb) = svc_with_embed(64);
        for i in 0..6 {
            weave_as(&svc, "t1", "alice", &format!("咖啡 偏好记录 {i} 号")).await;
        }
        let resp = svc
            .resonate(Request::new(ResonateRequest {
                tenant_id: "t1".into(),
                owner_id: "alice".into(),
                query: "咖啡".into(),
                context: None,
                budget: 64,
                top_k: 3,
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(
            resp.activated.len() <= 3,
            "top_k=3 时返回不得超过 3 条，实际 {}",
            resp.activated.len()
        );
        // top_k=0 保持历史行为（不截断）
        let resp = svc
            .resonate(Request::new(ResonateRequest {
                tenant_id: "t1".into(),
                owner_id: "alice".into(),
                query: "咖啡".into(),
                context: None,
                budget: 64,
                top_k: 0,
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.activated.len() >= 6, "不截断时应返回全部命中");
    }

    /// issue #3：短回显判定——用户短指令应命中，长解释性文本不命中。
    #[test]
    fn echo_detection_matches_short_echoes_only() {
        assert!(is_query_echo("zeroclaw 是什么", "详细介绍一下 zeroclaw"));
        assert!(is_query_echo("zeroclaw 是什么", "装一份 zeroclaw"));
        assert!(!is_query_echo(
            "zeroclaw 是什么",
            "zeroclaw 是一个开源的分布式任务调度框架，支持毫秒级触发与租户隔离，最初由 infra 团队在 2024 年开源。"
        ));
        // 真正的短事实不应被误判：公共子串不足文本一半
        assert!(!is_query_echo("window seat", "alice prefers window seats"));
        assert!(!is_query_echo("", "任意文本"));
    }

    /// issue #1：长批次 LLM 失败时对半拆分重试，子批成功即可回收事实。
    #[tokio::test]
    async fn extract_session_facts_splits_oversized_batches() {
        use nylon_llm::{ChatModel, LlmError};
        // 模拟 max_tokens 截断：输入越长越容易失败，短输入正常返回
        struct FailOnLong;
        #[async_trait::async_trait]
        impl ChatModel for FailOnLong {
            async fn chat_json(
                &self,
                _system: &str,
                user: &str,
            ) -> Result<serde_json::Value, LlmError> {
                if user.len() > 200 {
                    Err(LlmError("响应不是 JSON: 截断".into()))
                } else {
                    Ok(serde_json::json!({"facts": [{"fact": "子批事实", "source": []}]}))
                }
            }
        }
        let lines: Vec<String> = (0..10)
            .map(|i| format!("t:{i} user: 内容内容内容内容{i}"))
            .collect();
        // 整批 ~350B 触发失败；对半后 ~175B 成功（每行含 8 个汉字=24B）
        assert!(lines.join("\n").len() > 200);
        assert!(lines[..5].join("\n").len() <= 200);
        let r = extract_session_facts(&FailOnLong, &lines).await;
        assert!(!r.facts.is_empty(), "拆分后应回收到事实");
        assert!(r.all_ok);
    }

    /// issue #1：全部子批都失败时 all_ok=false（abstract_status=failed）。
    #[tokio::test]
    async fn extract_session_facts_reports_failure_when_all_chunks_fail() {
        use nylon_llm::{ChatModel, LlmError};
        struct AlwaysFail;
        #[async_trait::async_trait]
        impl ChatModel for AlwaysFail {
            async fn chat_json(
                &self,
                _system: &str,
                _user: &str,
            ) -> Result<serde_json::Value, LlmError> {
                Err(LlmError("网络错误".into()))
            }
        }
        let lines: Vec<String> = (0..8).map(|i| format!("user: 内容{i}")).collect();
        let r = extract_session_facts(&AlwaysFail, &lines).await;
        assert!(r.facts.is_empty());
        assert!(!r.all_ok);
    }

    /// issue #4：stats 必须区分嵌入通道的 disabled / ok / degraded 三态。
    #[tokio::test]
    async fn stats_reports_embedder_health_states() {
        // disabled：未配置 embedder
        let dir = tempfile::tempdir().unwrap();
        let store = PersistentGraph::open(dir.path()).unwrap();
        std::mem::forget(dir);
        let svc = EngineService::new(store, 64, None, None);
        let s = svc.stats().unwrap();
        assert!(!s.embedder);
        assert_eq!(s.embedder_status, "disabled");
        assert_eq!(s.embed_failures, 0);

        // ok：embedder 在位且无失败
        let (svc, _emb) = svc_with_embed(64);
        let s = svc.stats().unwrap();
        assert!(s.embedder);
        assert_eq!(s.embedder_status, "ok");

        // degraded：连续失败被记录，含最近错误摘要
        svc.note_embed_failure("endpoint down");
        let s = svc.stats().unwrap();
        assert_eq!(s.embedder_status, "degraded");
        assert_eq!(s.embed_failures, 1);
        assert_eq!(s.embedder_last_error.as_deref(), Some("endpoint down"));

        // 成功一次即恢复 ok
        svc.note_embed_ok();
        let s = svc.stats().unwrap();
        assert_eq!(s.embedder_status, "ok");
        assert_eq!(s.embedder_last_error, None);
    }

    /// issue #4：resonate 的嵌入失败不再静默——降级为纯词面的同时计入健康状态。
    #[tokio::test]
    async fn resonate_embed_failure_marks_degraded() {
        struct FailingEmbedder;
        #[async_trait::async_trait]
        impl Embedder for FailingEmbedder {
            async fn embed(
                &self,
                _texts: &[String],
            ) -> Result<Vec<Vec<f32>>, nylon_embed::EmbedError> {
                Err(nylon_embed::EmbedError("endpoint down".into()))
            }
            fn dims(&self) -> usize {
                64
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let store = PersistentGraph::open(dir.path()).unwrap();
        std::mem::forget(dir);
        let svc = EngineService::new(store, 64, Some(Arc::new(FailingEmbedder)), None);
        assert_eq!(svc.stats().unwrap().embedder_status, "ok"); // 尚未有调用，配置≠故障
        let _ = svc
            .resonate(Request::new(ResonateRequest {
                tenant_id: "t1".into(),
                owner_id: "alice".into(),
                query: "咖啡".into(),
                context: None,
                budget: 8,
                top_k: 0,
            }))
            .await
            .unwrap();
        let s = svc.stats().unwrap();
        assert_eq!(s.embedder_status, "degraded");
        assert!(s.embed_failures >= 1);
        assert!(s.embedder_last_error.unwrap().contains("endpoint down"));
    }

    /// L2.1：向量检索 Search 不得跨租户（历史漏洞：HNSW 全局索引未过滤）。
    #[tokio::test]
    async fn search_isolated_across_tenants() {
        let (svc, emb) = svc_with_embed(64);
        let a_id = weave_as(&svc, "tenant-a", "alice", "节点 A 的内容").await;
        let _b_id = weave_as(&svc, "tenant-b", "alice", "节点 B 的内容").await;

        let q = emb.embed(&["节点".to_string()]).await.unwrap().remove(0);
        let mut bytes = Vec::with_capacity(q.len() * 4);
        for v in &q {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let resp = svc
            .search(Request::new(SearchRequest {
                tenant_id: "tenant-a".into(),
                owner_id: "alice".into(),
                query_embedding: bytes,
                top_k: 10,
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(
            resp.neighbors.iter().all(|n| n.node_id == a_id),
            "Search 结果必须全部属于 tenant-a: {:?}",
            resp.neighbors.iter().map(|n| n.node_id).collect::<Vec<_>>()
        );
    }

    /// L2.1：GetNode 跨租户按不存在处理（不暴露存在性）。
    #[tokio::test]
    async fn get_node_cross_tenant_not_found() {
        let (svc, _emb) = svc_with_embed(64);
        let a_id = weave_as(&svc, "tenant-a", "alice", "只有 tenant-a 可见").await;

        let err = svc
            .get_node(Request::new(GetNodeRequest {
                tenant_id: "tenant-b".into(),
                node_id: a_id,
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);

        let ok = svc
            .get_node(Request::new(GetNodeRequest {
                tenant_id: "tenant-a".into(),
                node_id: a_id,
            }))
            .await;
        assert!(ok.is_ok());
    }

    /// 节点删除（"遗忘"）：本租户可删、跨租户按不存在、删后列表与读取均不可见。
    #[tokio::test]
    async fn remove_node_tenant_scoped() {
        let (svc, _emb) = svc_with_embed(64);
        let a_id = weave_as(&svc, "tenant-a", "alice", "将被遗忘的事实").await;

        // 跨租户删除：按不存在处理，不暴露存在性
        let err = svc.remove_node("tenant-b", a_id as u32).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);

        // 本租户删除成功；重复删除按不存在处理
        assert!(svc.remove_node("tenant-a", a_id as u32).await.unwrap());
        assert!(svc.remove_node("tenant-a", a_id as u32).await.is_err());

        // 删除后列表与读取均不可见
        let (total, _) = svc.list_nodes("tenant-a", None, 0, 50).unwrap();
        assert_eq!(total, 0);
        let err = svc
            .get_node(Request::new(GetNodeRequest {
                tenant_id: "tenant-a".into(),
                node_id: a_id,
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    /// 图可视化视图：只含本租户节点，边两端都在返回集合内，limit 截断生效。
    #[tokio::test]
    async fn graph_view_tenant_scoped_and_edge_filtered() {
        let (svc, _emb) = svc_with_embed(64);
        let a1 = weave_as(&svc, "tenant-a", "alice", "手冲咖啡笔记").await;
        let a2 = weave_as(&svc, "tenant-a", "alice", "手冲咖啡进阶").await;
        let _b1 = weave_as(&svc, "tenant-b", "bob", "tenant-b 的私密记忆").await;

        let view = svc.graph_view("tenant-a", 300).unwrap();
        assert_eq!(view.total, 2);
        let ids: std::collections::HashSet<u32> = view.nodes.iter().map(|n| n.id).collect();
        assert!(ids.contains(&(a1 as u32)) && ids.contains(&(a2 as u32)));
        assert!(view
            .edges
            .iter()
            .all(|e| ids.contains(&e.from) && ids.contains(&e.to)));

        // limit 截断：total 仍报全集大小，边因端点被截断而过滤
        let view1 = svc.graph_view("tenant-a", 1).unwrap();
        assert_eq!(view1.nodes.len(), 1);
        assert_eq!(view1.total, 2);
        assert!(view1.edges.is_empty());
    }

    /// L2.1：自动建边不跨租户（同关系丝、同 owner、不同 tenant 不得建边）。
    #[tokio::test]
    async fn auto_link_never_crosses_tenant() {
        let (svc, _emb) = svc_with_embed(64);
        let req = |tenant: &str, fact: &str| {
            Request::new(WeaveRequest {
                tenant_id: tenant.into(),
                owner_id: "alice".into(),
                raw_event: fact.into(),
                context: Some(pb::ContextSpectrum {
                    task: Some("咖啡".into()),
                    emotion_valence: None,
                    device: None,
                    max_hops: None,
                }),
            })
        };
        svc.weave(req("tenant-a", "手冲咖啡笔记")).await.unwrap();
        svc.weave(req("tenant-b", "手冲咖啡笔记")).await.unwrap();
        // 第三条与第一条同租户：只能链上第一条（第二条跨租户不可见）
        let resp = svc
            .weave(req("tenant-a", "再来一条咖啡记录"))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            resp.linked_nodes.len(),
            1,
            "跨租户节点不得被自动建边: {:?}",
            resp.linked_nodes
        );
    }

    /// L2.2：grant 与请求体租户不匹配时拒绝（gRPC 拦截器之后的 handler 比对）。
    #[tokio::test]
    async fn grant_tenant_mismatch_rejected() {
        let (svc, _emb) = svc_with_embed(64);
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

        // admin 通配放行
        let mut req = Request::new(WeaveRequest {
            tenant_id: "tenant-b".into(),
            owner_id: "alice".into(),
            raw_event: "管理端写入".into(),
            context: None,
        });
        req.extensions_mut().insert(KeyGrant {
            tenant: "*".into(),
            scope: crate::auth::Scope::Admin,
        });
        assert!(svc.weave(req).await.is_ok());
    }
}
