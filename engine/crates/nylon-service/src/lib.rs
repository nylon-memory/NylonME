//! nylon-service：NylonME 引擎核心逻辑（传输无关）。
//!
//! 从 nylon-engine/src/service.rs 抽取的编织 / 共振 / 检索 / 反思逻辑，
//! gRPC 服务（nylon-engine）与移动端绑定（nylon-mobile）共用同一份实现。
//! 本 crate 不依赖 tonic/axum，错误用 [`CoreError`] 表达，由调用方映射到
//! 各自的传输层错误（gRPC Status / UniFFI 异常）。
//!
//! 对齐说明：逻辑迁移自 service.rs（Phase 1 简化语义），行为保持一致；
//! 相对迁移前的两处刻意变化：
//! 1. heuristic_relations 增加 CJK 二字组兜底（原纯字母切分对中文对话
//!    抽不出关系丝，端上日常对话以中文为主——服务端同步受益）；
//! 2. 反思队列的 tokio worker 由调用方（nylon-engine）持有，core 只暴露
//!    process_reflection_jobs 批量处理与 reflect_idle_duration 配置读取。

use std::sync::{Arc, Mutex};

use nylon_core::{compute_tension, Filaments, MemoryNode, Tension};
use nylon_embed::Embedder;
use nylon_graph::{ContextSpectrum, FilamentFilter};
use nylon_llm::ChatModel;
use nylon_storage::PersistentGraph;
use nylon_vector::{HnswIndex, VectorIndex};

/// 引擎错误（传输无关，由适配层映射）。
#[derive(Debug)]
pub enum CoreError {
    /// 参数非法（映射 gRPC invalid_argument）。
    Invalid(String),
    /// 内部错误：IO / 锁中毒 / 嵌入失败等（映射 gRPC internal）。
    Internal(String),
    /// 资源不存在（映射 gRPC not_found）。
    NotFound(String),
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreError::Invalid(m) => write!(f, "{m}"),
            CoreError::Internal(m) => write!(f, "{m}"),
            CoreError::NotFound(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for CoreError {}

/// 默认嵌入维度（bge-small 类模型），可用 NYLON_EMBED_DIMS 覆盖。
pub const DEFAULT_EMBED_DIMS: usize = 384;

/// Resonate 种子数量上限默认值，可用 NYLON_MAX_SEEDS 覆盖（实验旋钮）。
pub fn max_seeds() -> usize {
    std::env::var("NYLON_MAX_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
}

/// 空闲反思窗口：距离最后一次 session 写入多久后开始补常识桥接。
pub fn reflect_idle_duration() -> std::time::Duration {
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
/// 画像节点标签（ relations 前缀）：persona + person:<规范名>
const PERSONA_TAG: &str = "persona";
const PERSONA_NAME_PREFIX: &str = "person:";
const PERSONA_EDGE_WEIGHT: f32 = 0.9;
/// 画像→叶子边权重（低于画像→事实，最后一跳）
const PERSONA_LEAF_EDGE_WEIGHT: f32 = 0.7;
/// 常识桥接节点与抽象事实节点之间的边权重。
const WORLD_BRIDGE_EDGE_WEIGHT: f32 = 0.8;

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

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

/// CJK 停用字：单字切词时剔除，避免高频虚字淹没稀有实体字。
const CJK_STOPS: &[char] = &[
    '的', '了', '在', '是', '我', '你', '他', '她', '它', '们', '这', '那', '有', '吗', '呢', '吧',
    '啊', '哦', '么', '着', '过', '都', '不', '没', '就', '要', '会', '能', '很', '也', '和', '跟',
    '与', '对', '把', '被', '让', '到', '从', '向', '于', '但', '而', '或', '及', '等',
];

/// 统一 CJK 判定（基本汉字区）。
fn is_cjk(c: char) -> bool {
    (0x4E00..=0x9FFF).contains(&(c as u32))
}

/// 查询词项展开：纯英文 token 原样返回；含汉字的 token 展开为二字组 + 非停用单字。
/// 单字靠 IDF 分层：「用户」这类高频词权重被压低，「猫」这类稀有实体字成为召回主力；
/// 二字组保留短语粒度（如「跑步」「杭州」）。中英文混合查询两路并行。
fn cjk_terms(tok: &str) -> Vec<String> {
    let chars: Vec<char> = tok.chars().collect();
    if !chars.iter().any(|&c| is_cjk(c)) {
        return vec![tok.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for &c in &chars {
        if !is_cjk(c) || CJK_STOPS.contains(&c) {
            continue;
        }
        let s = c.to_string();
        if seen.insert(s.clone()) {
            out.push(s);
        }
    }
    for w in chars.windows(2) {
        // 跨停用字的二字组（如「户的」）会撞上大量「用户的X」句式造成误命中，跳过。
        if !(is_cjk(w[0]) && is_cjk(w[1])) || CJK_STOPS.contains(&w[0]) || CJK_STOPS.contains(&w[1])
        {
            continue;
        }
        let s = format!("{}{}", w[0], w[1]);
        if seen.insert(s.clone()) {
            out.push(s);
        }
    }
    out
}

/// 启发式关系丝抽取：无 LLM / 无 context 时从原文提取内容标签。
/// 英文：句中大写开头的专有名（人名/地名/机构）优先，其次长度 >=4 的非停用实词；
/// 中文：无字母数字 token 时退化为 CJK 二字组（日常对话以中文为主，纯字母切分
/// 在中文语料上抽不出关系丝——服务端与端上共用此行为）。
/// 这是自动建边（auto-link）的标签来源——没有它图是零边，扩散空转。
fn heuristic_relations(text: &str) -> Vec<String> {
    let mut cands: Vec<(i32, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let has_alpha = text.chars().any(|c| c.is_ascii_alphanumeric());
    if !has_alpha {
        let mut chars = text
            .chars()
            .filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation());
        let mut prev = chars.next();
        while let (Some(a), Some(b)) = (prev, chars.next()) {
            prev = Some(b);
            if !(a as u32 >= 0x4E00 && (b as u32) >= 0x4E00) {
                continue; // 只保留汉字二元组
            }
            let gram = format!("{a}{b}");
            if seen.insert(gram.clone()) {
                cands.push((4, gram));
            }
        }
        return cands.into_iter().take(3).map(|(_, w)| w).collect();
    }
    for (idx, w) in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .enumerate()
    {
        let lower = w.to_lowercase();
        if lower.len() < 4 || STOPWORDS.contains(&lower.as_str()) || !seen.insert(lower.clone()) {
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

struct Inner {
    store: PersistentGraph,
    index: HnswIndex,
}

/// 引擎核心句柄（Clone = 共享同一底层存储，供服务层多任务持有）。
#[derive(Clone)]
pub struct EngineCore {
    inner: Arc<Mutex<Inner>>,
    /// 嵌入通道：None 时退回 Phase 1 行为（无向量写入、无向量种子）。
    embedder: Option<Arc<dyn Embedder>>,
    /// LLM 通道：None 时关闭编织分解与冲突检测。
    llm: Option<Arc<dyn ChatModel>>,
}

impl EngineCore {
    pub fn new(
        store: PersistentGraph,
        embed_dims: usize,
        embedder: Option<Arc<dyn Embedder>>,
        llm: Option<Arc<dyn ChatModel>>,
    ) -> Self {
        EngineCore {
            inner: Arc::new(Mutex::new(Inner {
                store,
                index: HnswIndex::new(embed_dims),
            })),
            embedder,
            llm,
        }
    }

    /// 嵌入通道（反思 worker 等服务侧编排使用）。
    pub fn embedder(&self) -> &Option<Arc<dyn Embedder>> {
        &self.embedder
    }

    /// LLM 通道（反思 worker 等服务侧编排使用）。
    pub fn llm(&self) -> &Option<Arc<dyn ChatModel>> {
        &self.llm
    }
}

/// weave 的 LLM 使用模式：Full=分解+冲突检测（单事件路径）；Off=纯启发式（session 双层写入路径）
#[derive(Clone, Copy, PartialEq)]
pub enum WeaveLlmMode {
    Full,
    Off,
}

/// 单事件编织结果。
#[derive(Debug, Clone)]
pub struct WeaveOutcome {
    pub node_id: u64,
    pub linked_nodes: Vec<u64>,
    pub conflict_nodes: Vec<u64>,
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
    let system = "You are a memory conflict detector. Compare the new memory against the candidate existing memories. Only return candidates that have factual contradictions (not supplements, not related-but-different). Output JSON: {\"conflicts\": [candidate_numbers]}";
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

fn lock<T>(m: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, CoreError> {
    m.lock()
        .map_err(|_| CoreError::Internal("state lock poisoned".into()))
}

/// 等待 WAL 持久化票证（group commit：刷盘等待移出全局锁，并发写共享同一批次 fsync）。
async fn await_ticket(t: nylon_storage::DurabilityTicket) -> Result<(), CoreError> {
    tokio::task::spawn_blocking(move || t.wait())
        .await
        .map_err(|e| CoreError::Internal(format!("durability join: {e}")))?
        .map_err(|e| CoreError::Internal(format!("wal durability: {e}")))
}

impl EngineCore {
    /// 单事件编织核心：单事件 RPC 与 session 双层写入共用。
    pub async fn weave_one(
        &self,
        tenant_id: &str,
        owner_id: &str,
        raw_event: &str,
        context: Option<ContextSpectrum>,
        llm_mode: WeaveLlmMode,
    ) -> Result<WeaveOutcome, CoreError> {
        if tenant_id.is_empty() || owner_id.is_empty() || raw_event.is_empty() {
            return Err(CoreError::Invalid(
                "tenant_id / owner_id / raw_event 均不能为空".into(),
            ));
        }
        let llm_gate = if llm_mode == WeaveLlmMode::Full {
            self.llm.clone()
        } else {
            None
        };
        let now = now_secs();
        let ctx = context.unwrap_or_default();
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
            match emb.embed(std::slice::from_ref(&fact)).await {
                Ok(mut v) => v.pop(),
                Err(e) => return Err(CoreError::Internal(format!("嵌入失败: {e}"))),
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
        let (local, linked, final_ticket, candidate_facts) = {
            let mut inner = lock(&self.inner)?;
            if let Some(vec) = embedding {
                node.embedding = vec;
            }
            let (local, node_ticket) = inner
                .store
                .add_node(node)
                .map_err(|e| CoreError::Internal(format!("wal append: {e}")))?;
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
                            .map_err(|e| CoreError::Internal(format!("wal append: {e}")))?,
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
        await_ticket(final_ticket).await?;
        Ok(WeaveOutcome {
            node_id: local as u64,
            linked_nodes: linked,
            conflict_nodes,
        })
    }
}

/// Session 事件输入（传输无关）。
#[derive(Debug, Clone)]
pub struct SessionEventIn {
    pub event_id: String,
    pub speaker: String,
    pub text: String,
}

/// 叶子层节点（逐事件原文）。
#[derive(Debug, Clone)]
pub struct LeafNodeCore {
    pub event_id: String,
    pub node_id: u64,
}

/// 抽象层事实节点（LLM session 分解）。
#[derive(Debug, Clone)]
pub struct FactNodeCore {
    pub node_id: u64,
    pub fact: String,
    pub source_event_ids: Vec<String>,
}

/// 空闲反思任务：异步补常识桥接 + 画像节点（服务层编排入队，core 批量处理）。
#[derive(Debug, Clone)]
pub struct ReflectionJob {
    pub tenant_id: String,
    pub owner_id: String,
    pub session_text: String,
    pub fact_ids: Vec<u32>,
    /// 本 session 的叶子节点 ID（画像节点连边用，打通 画像→叶子 最后一跳）
    pub leaf_ids: Vec<u32>,
}

/// 双层写入结果。`reflection` 由调用方决定何时/如何调度（gRPC 服务入 tokio 队列）。
#[derive(Debug, Clone)]
pub struct WeaveSessionOutcome {
    pub leaf_nodes: Vec<LeafNodeCore>,
    pub fact_nodes: Vec<FactNodeCore>,
    pub reflection: Option<ReflectionJob>,
}

/// Session 级抽象事实抽取（双层写入的理解层）：整段 session 一次 LLM 调用，
/// 指代消解+原子事实+来源 event_id；失败返回空（调用方跳过抽象层）。
async fn extract_session_facts(
    llm: &dyn ChatModel,
    session_text: &str,
) -> Vec<(String, Vec<String>)> {
    let system = "You are a memory extraction engine. Given a dialogue session with turn IDs, extract atomic factual memories worth remembering long-term. Resolve pronouns and partial names to canonical full names (e.g. 'she' -> the person's name). Merge duplicate information. Preserve exact details: dates, numbers, places, names. If turns carry a [date] prefix, treat it as the absolute time of those turns; when event timing matters, include the absolute date in the fact rather than relative words like 'yesterday' or 'last week'. Each fact must be self-contained. Output ONLY valid JSON: {\"facts\": [{\"fact\": \"...\", \"source\": [\"event_id\", ...]}]}. Skip greetings and small talk without facts.";
    match llm.chat_json(system, session_text).await {
        Ok(v) => v
            .get("facts")
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
            .unwrap_or_default(),
        Err(e) => {
            eprintln!("[weave_session] LLM session 分解失败，跳过抽象层: {e}");
            Vec::new()
        }
    }
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

impl EngineCore {
    /// 双层写入：叶子层=逐事件原文入库；抽象层=LLM session 分解出原子事实，
    /// 事实节点与来源叶子节点之间建显式 derived 边（层间互联，供共振跨层扩散）。
    pub async fn weave_session(
        &self,
        tenant_id: &str,
        owner_id: &str,
        events: Vec<SessionEventIn>,
        skip_abstract: bool,
    ) -> Result<WeaveSessionOutcome, CoreError> {
        if tenant_id.is_empty() || owner_id.is_empty() {
            return Err(CoreError::Invalid("tenant_id / owner_id 均不能为空".into()));
        }
        let events: Vec<&SessionEventIn> = events.iter().filter(|e| !e.text.is_empty()).collect();
        // 叶子层：逐事件原文（启发式路径，与验证过的双层实验口径一致）
        let mut leaf_nodes: Vec<LeafNodeCore> = Vec::new();
        let mut id2local: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for ev in &events {
            let raw = format!("{}: {}", ev.speaker, ev.text);
            let outcome = self
                .weave_one(tenant_id, owner_id, &raw, None, WeaveLlmMode::Off)
                .await?;
            if !ev.event_id.is_empty() {
                id2local.insert(ev.event_id.clone(), outcome.node_id as u32);
            }
            leaf_nodes.push(LeafNodeCore {
                event_id: ev.event_id.clone(),
                node_id: outcome.node_id,
            });
        }
        // 抽象层：整段 session 一次 LLM 调用，分解为原子事实+来源标注
        let mut fact_nodes: Vec<FactNodeCore> = Vec::new();
        let mut derived: Vec<(u32, u32)> = Vec::new();
        let mut reflection: Option<ReflectionJob> = None;
        if !skip_abstract && !events.is_empty() {
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
                for (fact, sources) in extract_session_facts(llm.as_ref(), &lines.join("\n")).await
                {
                    let outcome = self
                        .weave_one(tenant_id, owner_id, &fact, None, WeaveLlmMode::Off)
                        .await?;
                    for sid in &sources {
                        if let Some(l) = id2local.get(sid) {
                            derived.push((outcome.node_id as u32, *l));
                        }
                    }
                    fact_nodes.push(FactNodeCore {
                        node_id: outcome.node_id,
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
                        let ctx = Some(ContextSpectrum {
                            task: Some(WORLD_KNOWLEDGE_TAG.to_string()),
                            ..Default::default()
                        });
                        let outcome = self
                            .weave_one(tenant_id, owner_id, &bridge, ctx, WeaveLlmMode::Off)
                            .await?;
                        let world_local = outcome.node_id as u32;
                        let mut t = None;
                        {
                            let mut inner = lock(&self.inner)?;
                            for fid in &fact_ids {
                                t = Some(
                                    inner
                                        .store
                                        .add_edge(world_local, *fid, WORLD_BRIDGE_EDGE_WEIGHT)
                                        .map_err(|e| {
                                            CoreError::Internal(format!("wal append: {e}"))
                                        })?,
                                );
                            }
                        }
                        if let Some(t) = t {
                            await_ticket(t).await?;
                        }
                    }
                }
                if std::env::var("NYLON_WORLD_BRIDGES_ASYNC").is_ok() && !fact_nodes.is_empty() {
                    reflection = Some(ReflectionJob {
                        tenant_id: tenant_id.to_string(),
                        owner_id: owner_id.to_string(),
                        session_text: lines.join("\n"),
                        fact_ids: fact_nodes.iter().map(|f| f.node_id as u32).collect(),
                        leaf_ids: leaf_nodes.iter().map(|l| l.node_id as u32).collect(),
                    });
                }
            }
        }
        // 层间显式边：统一一批写入，一次 durability 等待
        // NYLON_DERIVED_EDGES=1 才开（实测全开对 Cat2/3 有负收益，自动建边的隐式互联更稳，默认关）
        let derived_on = std::env::var("NYLON_DERIVED_EDGES")
            .map(|v| v != "0")
            .unwrap_or(false);
        if derived_on && !derived.is_empty() {
            let t = {
                let mut inner = lock(&self.inner)?;
                let mut t = None;
                for (a, b) in derived {
                    t = Some(
                        inner
                            .store
                            .add_edge(a, b, DERIVED_EDGE_WEIGHT)
                            .map_err(|e| CoreError::Internal(format!("wal append: {e}")))?,
                    );
                }
                t
            };
            if let Some(t) = t {
                await_ticket(t).await?;
            }
        }
        Ok(WeaveSessionOutcome {
            leaf_nodes,
            fact_nodes,
            reflection,
        })
    }
}

/// 空闲反思批处理：常识桥接 + 画像节点（服务层反思 worker 空闲窗口调用）。
pub async fn process_reflection_jobs(core: &EngineCore, jobs: Vec<ReflectionJob>) {
    let Some(llm) = core.llm.clone() else { return };
    for job in jobs {
        if job.fact_ids.is_empty() {
            continue;
        }
        let bridges = extract_commonsense_bridges(llm.as_ref(), &job.session_text).await;
        if !bridges.is_empty() {
            let mut wrote = 0usize;
            for bridge in bridges {
                match core
                    .write_world_bridge(&job.tenant_id, &job.owner_id, &job.fact_ids, bridge)
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
        // 画像层：聚合人物特质为锚点节点（NYLON_PERSONA_REFLECT=1 开启）
        if std::env::var("NYLON_PERSONA_REFLECT").is_ok() {
            core.reflect_personas(llm.as_ref(), &job).await;
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

impl EngineCore {
    async fn write_persona(
        &self,
        tenant_id: &str,
        owner_id: &str,
        fact_ids: &[u32],
        leaf_ids: &[u32],
        name: &str,
        fact_text: String,
    ) -> Result<u32, CoreError> {
        let embedding = if let Some(emb) = &self.embedder {
            emb.embed(std::slice::from_ref(&fact_text))
                .await
                .map_err(|e| CoreError::Internal(format!("嵌入失败: {e}")))?
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
            let mut inner = lock(&self.inner)?;
            let (local, node_ticket) = inner
                .store
                .add_node(node)
                .map_err(|e| CoreError::Internal(format!("wal append: {e}")))?;
            if !index_embedding.is_empty() {
                inner.index.add(local, &index_embedding);
            }
            let mut t = Some(node_ticket);
            for fid in fact_ids {
                t = Some(
                    inner
                        .store
                        .add_edge(local, *fid, PERSONA_EDGE_WEIGHT)
                        .map_err(|e| CoreError::Internal(format!("wal append: {e}")))?,
                );
            }
            for lid in leaf_ids {
                t = Some(
                    inner
                        .store
                        .add_edge(local, *lid, PERSONA_LEAF_EDGE_WEIGHT)
                        .map_err(|e| CoreError::Internal(format!("wal append: {e}")))?,
                );
            }
            (local, t)
        };
        if let Some(t) = ticket {
            await_ticket(t).await?;
        }
        Ok(local)
    }

    async fn write_world_bridge(
        &self,
        tenant_id: &str,
        owner_id: &str,
        fact_ids: &[u32],
        bridge: String,
    ) -> Result<(), CoreError> {
        let embedding = if let Some(emb) = &self.embedder {
            emb.embed(std::slice::from_ref(&bridge))
                .await
                .map_err(|e| CoreError::Internal(format!("嵌入失败: {e}")))?
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
            let mut inner = lock(&self.inner)?;
            let (local, node_ticket) = inner
                .store
                .add_node(node)
                .map_err(|e| CoreError::Internal(format!("wal append: {e}")))?;
            if !index_embedding.is_empty() {
                inner.index.add(local, &index_embedding);
            }
            let mut t = Some(node_ticket);
            for fid in fact_ids {
                t = Some(
                    inner
                        .store
                        .add_edge(local, *fid, WORLD_BRIDGE_EDGE_WEIGHT)
                        .map_err(|e| CoreError::Internal(format!("wal append: {e}")))?,
                );
            }
            t
        };
        if let Some(t) = ticket {
            await_ticket(t).await?;
        }
        Ok(())
    }

    /// 画像层反思：为 session 中的人物生成/更新画像节点，连边到支撑事实，旧版画像墓碑化。
    async fn reflect_personas(&self, llm: &dyn ChatModel, job: &ReflectionJob) {
        // 该 owner 已有画像（姓名小写做键，同人合并）：(节点 id, 姓名小写, 画像文本)
        let existing: Vec<(u32, String, String)> = {
            let Ok(inner) = lock(&self.inner) else {
                return;
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
            match self
                .write_persona(
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
                            let Ok(inner) = lock(&self.inner) else {
                                break;
                            };
                            old_ids
                                .iter()
                                .flat_map(|id| inner.store.graph().neighbors(*id))
                                .collect()
                        };
                        let ticket = {
                            let Ok(mut inner) = lock(&self.inner) else {
                                break;
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
                            let _ = await_ticket(t).await;
                        }
                    }
                    for (old_id, old_name, _) in &existing {
                        if *old_name == lower && *old_id != local {
                            let res = {
                                let Ok(mut inner) = lock(&self.inner) else {
                                    break;
                                };
                                inner.store.remove_node(*old_id)
                            };
                            match res {
                                Ok((true, t)) => {
                                    let _ = await_ticket(t).await;
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
}

/// 共振激活节点（传输无关，含当前张力供 UI/端上直接展示）。
#[derive(Debug, Clone)]
pub struct ActivatedCore {
    pub node_id: u64,
    pub resonance: f32,
    pub filaments: Filaments,
    pub current_tension: f32,
}

/// resonate 结果。
#[derive(Debug, Clone)]
pub struct ResonateOutcome {
    pub activated: Vec<ActivatedCore>,
    pub seed_ids: Vec<u64>,
}

/// get_node 结果。
#[derive(Debug, Clone)]
pub struct GetNodeOutcome {
    pub node_id: u64,
    pub owner_id: String,
    pub filaments: Filaments,
    pub current_tension: f32,
}

/// REST/社区版 UI 的节点摘要（只读视图）。
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
    pub llm: bool,
}

impl EngineCore {
    /// 情境共振：向量/词面双通道选种（IDF 加权 + 完整子串命中加权 + task 关系丝 +
    /// 最近节点兜底），图多跳扩散，可选向量重排与种子保底置顶。
    pub async fn resonate(
        &self,
        tenant_id: &str,
        owner_id: &str,
        query: &str,
        context: Option<ContextSpectrum>,
        budget: u32,
    ) -> Result<ResonateOutcome, CoreError> {
        if tenant_id.is_empty() || owner_id.is_empty() {
            return Err(CoreError::Invalid("tenant_id / owner_id 不能为空".into()));
        }
        let ctx = context.unwrap_or_default();
        let query = query.to_lowercase();
        // 向量种子（锁外计算）；HNSW 是全局索引，候选必须按 tenant+owner 过滤（L2.1）
        let mut qvec: Option<Vec<f32>> = None;
        let vec_seeds: Vec<(u32, f32)> =
            if let (Some(emb), false) = (&self.embedder, query.is_empty()) {
                match emb.embed(std::slice::from_ref(&query)).await {
                    Ok(v) => {
                        qvec = v.first().cloned();
                        let inner = lock(&self.inner)?;
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
                                    .map(|n| n.tenant_id == tenant_id && n.owner_id == owner_id)
                                    .unwrap_or(false)
                            })
                            .take(max_seeds())
                            .collect()
                    }
                    Err(_) => Vec::new(), // 嵌入服务故障时降级为纯词面
                }
            } else {
                Vec::new()
            };
        let inner = lock(&self.inner)?;
        let g = inner.store.graph();

        // 种子选择：query 词项重叠打分（完整子串命中加权） > task 命中关系丝 > 最近节点兜底。
        // 词项化按非字母数字切分；CJK 查询无空格时退化为整串 contains，行为与原先一致。
        let mut lex_seeds: Vec<(u32, f32)> = Vec::new();
        if !query.is_empty() {
            let mut terms: Vec<String> = query
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| !t.is_empty())
                .filter(|t| t.len() >= 3 && !STOPWORDS.contains(t))
                .flat_map(cjk_terms)
                .collect();
            if terms.is_empty() {
                // 全是停用词的查询（如 "When did they meet?"）：退回未过滤词项
                terms = query
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|t| !t.is_empty())
                    .flat_map(cjk_terms)
                    .collect();
            }
            let terms: Vec<&str> = terms.iter().map(String::as_str).collect();
            // 第一遍：统计词项文档频率（df），IDF 加权——稀有词（实体）权重远高于常见词
            let owner_facts: Vec<(u32, String)> = g
                .live_nodes()
                .filter(|(_, n)| n.tenant_id == tenant_id && n.owner_id == owner_id)
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
                            .map(|n| n.tenant_id == tenant_id && n.owner_id == owner_id)
                            .unwrap_or(false)
                    })
                    .map(|id| (id, 0.5f32))
                    .collect();
            }
        }
        if seeds.is_empty() {
            let mut recent: Vec<(u32, i64)> = g
                .live_nodes()
                .filter(|(_, n)| n.tenant_id == tenant_id && n.owner_id == owner_id)
                .map(|(id, n)| (id, n.filaments.created_at))
                .collect();
            recent.sort_by_key(|&(_, ts)| std::cmp::Reverse(ts));
            seeds = recent.into_iter().map(|(id, _)| (id, 0.5f32)).collect();
        }
        seeds.truncate(max_seeds());

        let budget = if budget == 0 {
            nylon_graph::DEFAULT_BUDGET
        } else {
            budget as usize
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
        // 种子保底提升统一在向量重排之后做（服务侧），否则重排会打乱图内的提升结果
        let mut activated = g.resonate_opts(&seeds, &ctx, now_secs(), budget, tension_floor, 0);
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
        let now = now_secs();
        let out: Vec<_> = activated
            .into_iter()
            .filter_map(|(id, score)| {
                let n = g.get_node(id)?;
                // 隔离兜底：扩散结果不允许跨租户（L2.1）
                if n.tenant_id != tenant_id {
                    return None;
                }
                if n.filaments
                    .relations
                    .iter()
                    .any(|r| r == WORLD_KNOWLEDGE_TAG)
                {
                    return None;
                }
                Some(ActivatedCore {
                    node_id: id as u64,
                    resonance: score,
                    filaments: n.filaments.clone(),
                    current_tension: compute_tension(n, now, 1.0),
                })
            })
            .collect();
        Ok(ResonateOutcome {
            activated: out,
            seed_ids: seeds.iter().map(|&(sid, _)| sid as u64).collect(),
        })
    }

    /// 向量检索：HNSW 全局索引，过取 4 倍再按 tenant+owner 过滤（L2.1 强制隔离）。
    /// 维度对齐（截断/补零）在 core 内完成，调用方负责校验字节序列合法性。
    pub fn search(
        &self,
        tenant_id: &str,
        owner_id: &str,
        query_embedding: &[f32],
        top_k: u32,
    ) -> Result<Vec<ActivatedCore>, CoreError> {
        if tenant_id.is_empty() || owner_id.is_empty() {
            return Err(CoreError::Invalid("tenant_id / owner_id 不能为空".into()));
        }
        let inner = lock(&self.inner)?;
        let dims = inner.index.dims();
        // 维度对齐：截断或补零（Phase 1 宽容策略，避免维度演进期硬失败）
        let mut query = vec![0.0f32; dims];
        for (i, v) in query_embedding.iter().take(dims).enumerate() {
            query[i] = *v;
        }
        let k = if top_k == 0 { 10 } else { top_k as usize };
        let g = inner.store.graph();
        let now = now_secs();
        Ok(inner
            .index
            .search(&query, k.saturating_mul(4))
            .into_iter()
            .filter_map(|(id, sim)| {
                g.get_node(id).and_then(|n| {
                    (n.tenant_id == tenant_id && n.owner_id == owner_id).then(|| ActivatedCore {
                        node_id: id as u64,
                        resonance: sim,
                        filaments: n.filaments.clone(),
                        current_tension: compute_tension(n, now, 1.0),
                    })
                })
            })
            .take(k)
            .collect())
    }

    /// 读单条节点；跨租户一律按不存在处理，不暴露节点存在性（L2.1）。
    pub fn get_node(&self, tenant_id: &str, node_id: u64) -> Result<GetNodeOutcome, CoreError> {
        if tenant_id.is_empty() {
            return Err(CoreError::Invalid("tenant_id 不能为空".into()));
        }
        let inner = lock(&self.inner)?;
        let local = u32::try_from(node_id)
            .map_err(|_| CoreError::Invalid("node_id 超出局部 ID 范围".into()))?;
        let node = inner
            .store
            .graph()
            .get_node(local)
            .ok_or_else(|| CoreError::NotFound(format!("node {node_id} 不存在或已删除")))?;
        if node.tenant_id != tenant_id {
            return Err(CoreError::NotFound(format!(
                "node {node_id} 不存在或已删除"
            )));
        }
        Ok(GetNodeOutcome {
            node_id,
            owner_id: node.owner_id.clone(),
            filaments: node.filaments.clone(),
            current_tension: compute_tension(node, now_secs(), 1.0),
        })
    }

    /// 分页列出节点（按创建时间倒序），按 tenant 强制过滤，可选叠加 owner 过滤。
    pub fn list_nodes(
        &self,
        tenant: &str,
        owner: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<(usize, Vec<NodeSummary>), CoreError> {
        let inner = lock(&self.inner)?;
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
    pub fn stats(&self) -> EngineStats {
        let Ok(inner) = self.inner.lock() else {
            return EngineStats {
                nodes: 0,
                edges: 0,
                embed_dims: 0,
                embedder: self.embedder.is_some(),
                llm: self.llm.is_some(),
            };
        };
        EngineStats {
            nodes: inner.store.graph().node_count(),
            edges: inner.store.graph().edges().len(),
            embed_dims: inner.index.dims(),
            embedder: self.embedder.is_some(),
            llm: self.llm.is_some(),
        }
    }

    /// 指定 tenant+owner 的存活记忆数（端上 SDK 用）。
    pub fn node_count_for(&self, tenant: &str, owner: &str) -> u64 {
        let Ok(inner) = self.inner.lock() else {
            return 0;
        };
        inner
            .store
            .graph()
            .live_nodes()
            .filter(|(_, n)| n.tenant_id == tenant && n.owner_id == owner)
            .count() as u64
    }

    /// 手动 checkpoint：快照落盘 + 截断 WAL（L2.4 备份前置步骤）。
    /// serve 模式另有周期任务自动 checkpoint（NYLON_CHECKPOINT_SECS，默认 600s）。
    pub fn checkpoint(&self) -> Result<(), CoreError> {
        let mut inner = lock(&self.inner)?;
        inner
            .store
            .checkpoint()
            .map_err(|e| CoreError::Internal(format!("checkpoint: {e}")))
    }

    /// 内存 delta 合并进 CSR（纯内存操作，供端上 shutdown/低内存前整理）。
    pub fn compact(&self) -> Result<(), CoreError> {
        let mut inner = lock(&self.inner)?;
        inner.store.compact();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nylon_embed::StubEmbedder;
    use nylon_llm::StubChatModel;

    fn core_with_embed(dims: usize) -> (EngineCore, Arc<StubEmbedder>) {
        let dir = tempfile::tempdir().unwrap();
        let store = PersistentGraph::open(dir.path()).unwrap();
        let embedder = Arc::new(StubEmbedder::new(dims));
        let core = EngineCore::new(store, dims, Some(embedder.clone()), None);
        // tempdir 借 store 的 PersistentGraph 存活不够——泄漏句柄换取测试期有效
        std::mem::forget(dir);
        (core, embedder)
    }

    async fn weave_as(core: &EngineCore, tenant: &str, owner: &str, fact: &str) -> u64 {
        core.weave_one(tenant, owner, fact, None, WeaveLlmMode::Full)
            .await
            .unwrap()
            .node_id
    }

    #[tokio::test]
    async fn weave_with_stub_llm_no_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let store = PersistentGraph::open(dir.path()).unwrap();
        let embedder = Arc::new(StubEmbedder::new(64));
        let llm = Arc::new(StubChatModel::new(serde_json::json!({"conflicts": [1]})));

        let core = EngineCore::new(store, 64, Some(embedder.clone()), Some(llm.clone()));

        // First weave: no candidates, no conflicts
        let out = core
            .weave_one("test", "alice", "I like coffee", None, WeaveLlmMode::Full)
            .await
            .unwrap();
        assert!(out.conflict_nodes.is_empty());
        assert!(out.linked_nodes.is_empty());

        // Second weave: similar topic, HNSW may find candidate
        let out2 = core
            .weave_one(
                "test",
                "alice",
                "I prefer tea over coffee",
                None,
                WeaveLlmMode::Full,
            )
            .await
            .unwrap();
        assert!(out2.node_id > 0);
    }

    /// L2.1：跨租户共振不可见（词面 + 最近兜底两条种子路径都覆盖）。
    #[tokio::test]
    async fn resonate_isolated_across_tenants() {
        let (core, _emb) = core_with_embed(64);
        let a_id = weave_as(&core, "tenant-a", "alice", "喜欢手冲咖啡和浅烘豆").await;
        let b_id = weave_as(&core, "tenant-b", "alice", "喜欢手冲咖啡和深烘豆").await;

        let resp = core
            .resonate("tenant-a", "alice", "咖啡", None, 10)
            .await
            .unwrap();
        let ids: Vec<u64> = resp.activated.iter().map(|n| n.node_id).collect();
        assert!(ids.contains(&a_id), "本租户节点应命中: {ids:?}");
        assert!(
            !ids.contains(&b_id),
            "跨租户节点不得出现在共振结果: {ids:?}"
        );

        // 空查询走最近节点兜底，同样不得跨租户
        let resp = core
            .resonate("tenant-b", "alice", "", None, 10)
            .await
            .unwrap();
        let ids: Vec<u64> = resp.activated.iter().map(|n| n.node_id).collect();
        assert!(ids.contains(&b_id));
        assert!(!ids.contains(&a_id), "兜底种子也不得跨租户: {ids:?}");
    }

    /// L2.1：向量检索 Search 不得跨租户（历史漏洞：HNSW 全局索引未过滤）。
    #[tokio::test]
    async fn search_isolated_across_tenants() {
        let (core, emb) = core_with_embed(64);
        let a_id = weave_as(&core, "tenant-a", "alice", "节点 A 的内容").await;
        let _b_id = weave_as(&core, "tenant-b", "alice", "节点 B 的内容").await;

        let q = emb.embed(&["节点".to_string()]).await.unwrap().remove(0);
        let resp = core.search("tenant-a", "alice", &q, 10).unwrap();
        assert!(
            resp.iter().all(|n| n.node_id == a_id),
            "Search 结果必须全部属于 tenant-a: {:?}",
            resp.iter().map(|n| n.node_id).collect::<Vec<_>>()
        );
    }

    /// L2.1：GetNode 跨租户按不存在处理（不暴露存在性）。
    #[tokio::test]
    async fn get_node_cross_tenant_not_found() {
        let (core, _emb) = core_with_embed(64);
        let a_id = weave_as(&core, "tenant-a", "alice", "只有 tenant-a 可见").await;

        let err = core.get_node("tenant-b", a_id).unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)));

        assert!(core.get_node("tenant-a", a_id).is_ok());
    }

    /// L2.1：自动建边不跨租户（同关系丝、同 owner、不同 tenant 不得建边）。
    #[tokio::test]
    async fn auto_link_never_crosses_tenant() {
        let (core, _emb) = core_with_embed(64);
        let ctx = || {
            Some(ContextSpectrum {
                task: Some("咖啡".into()),
                ..Default::default()
            })
        };
        core.weave_one(
            "tenant-a",
            "alice",
            "手冲咖啡笔记",
            ctx(),
            WeaveLlmMode::Full,
        )
        .await
        .unwrap();
        core.weave_one(
            "tenant-b",
            "alice",
            "手冲咖啡笔记",
            ctx(),
            WeaveLlmMode::Full,
        )
        .await
        .unwrap();
        // 第三条与第一条同租户：只能链上第一条（第二条跨租户不可见）
        let out = core
            .weave_one(
                "tenant-a",
                "alice",
                "再来一条咖啡记录",
                ctx(),
                WeaveLlmMode::Full,
            )
            .await
            .unwrap();
        assert_eq!(
            out.linked_nodes.len(),
            1,
            "跨租户节点不得被自动建边: {:?}",
            out.linked_nodes
        );
    }

    /// 中文查询分词召回（真机测试发现：CJK 整串 contains 命不中同义表述）。
    #[tokio::test]
    async fn resonate_cjk_query_tokenized() {
        let (core, _emb) = core_with_embed(64);
        let cat_id = weave_as(&core, "tenant-a", "alice", "用户养了一只三岁的橘猫，叫咪咪").await;
        let _other = weave_as(&core, "tenant-a", "alice", "用户每天早上七点起床跑步三公里").await;
        let _other2 = weave_as(&core, "tenant-a", "alice", "用户的女朋友是小学老师，教语文").await;

        let out = core
            .resonate("tenant-a", "alice", "用户的猫", None, 5)
            .await
            .unwrap();
        let ids: Vec<u64> = out.activated.iter().map(|n| n.node_id).collect();
        assert!(ids.contains(&cat_id), "「用户的猫」应命中橘猫记忆: {ids:?}");

        let out2 = core
            .resonate("tenant-a", "alice", "早上跑步", None, 5)
            .await
            .unwrap();
        let ids2: Vec<u64> = out2.activated.iter().map(|n| n.node_id).collect();
        assert!(
            out2.activated
                .iter()
                .any(|n| n.filaments.fact.contains("跑步")),
            "「早上跑步」应命中跑步记忆: {ids2:?}"
        );
    }
    /// 中文对话的关系丝抽取（CJK 二字组兜底，端上与服务器共用行为）。
    #[test]
    fn heuristic_relations_cjk() {
        let rels = heuristic_relations("我明天要去杭州出差见客户");
        assert!(!rels.is_empty(), "CJK bigram relations should not be empty");
    }
}
