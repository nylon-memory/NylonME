//! nylon-mobile：NylonME 记忆引擎的移动端进程内绑定（UniFFI）。
//!
//! 定位：Android（Kotlin/JNI）与 iOS（Swift）共用本 crate 编译出的
//! cdylib/staticlib，进程内直接调用 weave / resonate，**不开端口、不走 gRPC**。
//!
//! 骨架现状（与 nylon-engine/src/service.rs 的 Phase 1 简化语义对齐）：
//! - weave：单丝编织（fact=原文）+ 关系丝启发式抽取 + 同 owner 自动建边（≤3）；
//! - resonate：词面选种（子串命中，CJK 二元组兜底）+ 图扩散；
//! - 嵌入通道：暂未接入（端上 OnnxEmbedder 见 README 路线图），embedding 留空。
//!
//! TODO（骨架后续迭代）：
//! 1. service.rs 的完整编织/共振逻辑抽为共享库后，本 crate 切换为薄绑定；
//! 2. 接入端上嵌入模型（tract + ONNX，dims 须与 NYLON_EMBED_DIMS 一致）；
//! 3. uniffi-bindgen 生成 Kotlin/Swift 绑定 + Android NDK 交叉编译接入 CI。

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use nylon_core::{compute_tension, Filaments, MemoryNode, Tension, DEFAULT_TENANT};
use nylon_graph::ContextSpectrum;
use nylon_storage::PersistentGraph;

uniffi::setup_scaffolding!();

/// 共振种子数量上限（对齐 service.rs NYLON_MAX_SEEDS 默认值）。
const MAX_SEEDS: usize = 20;
/// Weave 自动建边上限（对齐 service.rs MAX_AUTO_LINKS）。
const MAX_AUTO_LINKS: usize = 3;
/// 自动建边权重（对齐 service.rs AUTO_LINK_WEIGHT）。
const AUTO_LINK_WEIGHT: f32 = 0.5;
/// 词面种子基础强度。
const SEED_BASE_STRENGTH: f32 = 0.5;

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 引擎配置（App 侧构造一次，进程内单例）。
#[derive(uniffi::Record)]
pub struct MobileConfig {
    /// 数据目录（App 私有沙箱路径，如 filesDir/nylon）。
    pub data_dir: String,
    /// 记忆归属者 ID（单用户 App 固定一个即可；多账号切换时换 owner）。
    pub owner: String,
}

/// resonate 返回的激活记忆。
#[derive(uniffi::Record)]
pub struct ActivatedMemory {
    pub local_id: u32,
    pub fact: String,
    pub relations: Vec<String>,
    /// 共振强度（图扩散得分）。
    pub resonance: f32,
    /// 当前张力（含时间衰减与频次强化）。
    pub tension: f32,
}

/// get 返回的记忆详情。
#[derive(uniffi::Record)]
pub struct MobileMemory {
    pub local_id: u32,
    pub fact: String,
    pub relations: Vec<String>,
    pub created_at: i64,
    pub tension: f32,
}

/// 移动端错误（映射为 Kotlin/Swift 异常）。
#[derive(uniffi::Error, Debug)]
pub enum MobileError {
    Io { message: String },
    Locked { message: String },
    NotFound { message: String },
}

impl std::fmt::Display for MobileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MobileError::Io { message } => write!(f, "io: {message}"),
            MobileError::Locked { message } => write!(f, "locked: {message}"),
            MobileError::NotFound { message } => write!(f, "not found: {message}"),
        }
    }
}

impl std::error::Error for MobileError {}

fn lock<T>(m: &Mutex<T>) -> Result<MutexGuard<'_, T>, MobileError> {
    m.lock()
        .map_err(|_| MobileError::Locked { message: "engine mutex poisoned".into() })
}

/// 英文停用词表（对齐 service.rs STOPWORDS，用于关系丝抽取与选种）。
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

/// 启发式关系丝抽取。
/// 英文：大写开头的专有名优先，其次长度 >=4 的非停用实词；
/// 中文：无字母数字 token 时退化为 CJK 二字组（日常对话以中文为主，服务端版
/// 的纯字母切分在端侧会抽不出关系丝，这是端上版相对 service.rs 的刻意修正）。
fn heuristic_relations(text: &str) -> Vec<String> {
    let mut cands: Vec<(i32, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let has_alpha = text.chars().any(|c| c.is_ascii_alphanumeric());
    if !has_alpha {
        let mut chars = text.chars().filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation());
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

/// 查询切词：英文按非字母数字切 + 中文二字组。
fn query_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut cjk_run = String::new();
    let push_cjk = |run: &mut String, terms: &mut Vec<String>| {
        let cs: Vec<char> = run.chars().collect();
        for w in cs.windows(2) {
            terms.push(format!("{}{}", w[0], w[1]));
        }
        run.clear();
    };
    for tok in query.split(|c: char| !c.is_alphanumeric()) {
        if tok.is_empty() {
            continue;
        }
        if tok.chars().all(|c| (c as u32) >= 0x4E00 && (c as u32) <= 0x9FFF) {
            cjk_run.push_str(tok);
        } else {
            push_cjk(&mut cjk_run, &mut terms);
            let lower = tok.to_lowercase();
            if lower.len() >= 2 && !STOPWORDS.contains(&lower.as_str()) {
                terms.push(lower);
            }
        }
    }
    push_cjk(&mut cjk_run, &mut terms);
    terms
}

/// 移动端记忆引擎句柄。App 侧单例持有，线程安全。#[derive(uniffi::Object)] 注册为 UniFFI 对象（生成 Arc FFI 转换）。
#[derive(uniffi::Object)]
pub struct MobileEngine {
    store: Mutex<PersistentGraph>,
    owner: String,
}

#[uniffi::export]
impl MobileEngine {
    /// 打开（或创建）本地记忆库。
    #[uniffi::constructor]
    pub fn new(config: MobileConfig) -> Result<Arc<Self>, MobileError> {
        let store = PersistentGraph::open(&config.data_dir).map_err(|e| MobileError::Io {
            message: format!("open {}: {e}", config.data_dir),
        })?;
        Ok(Arc::new(Self { store: Mutex::new(store), owner: config.owner }))
    }

    /// 编织一条记忆：fact=原文，关系丝取自 task 或启发式抽取；
    /// 与同 owner 且关系丝重叠的历史节点自动建边（最多 3 条）。
    /// 返回局部 node_id。
    pub fn weave(
        &self,
        fact: String,
        task: Option<String>,
        emotion_valence: Option<f32>,
    ) -> Result<u64, MobileError> {
        if fact.trim().is_empty() {
            return Err(MobileError::Io { message: "fact is empty".into() });
        }
        let mut relations: Vec<String> = task.into_iter().collect();
        if relations.is_empty() {
            relations = heuristic_relations(&fact);
        }
        let now = now_secs();
        let node = MemoryNode {
            id: 0, // 全局 ID 分配在分布式阶段接入；当前 node_id 即局部 ID
            tenant_id: DEFAULT_TENANT.to_string(),
            owner_id: self.owner.clone(),
            filaments: Filaments {
                fact: fact.trim().to_string(),
                emotion_valence: emotion_valence.unwrap_or(0.0).clamp(-1.0, 1.0),
                emotion_intensity: 0.5,
                created_at: now,
                decay_rate: 0.01,
                relations: relations.clone(),
                confidence: 0.8,
                mentions_7d: 1,
            },
            tension: Tension { baseline: 1.0, last_updated: now },
            embedding: Vec::new(), // TODO: 端上嵌入模型接入后填充
        };
        let mut store = lock(&self.store)?;
        let (local, _ticket) = store.add_node(node).map_err(|e| MobileError::Io {
            message: format!("wal append: {e}"),
        })?;
        // 自动建边：关系丝倒排索引取候选，凑齐 MAX_AUTO_LINKS 即停。
        if !relations.is_empty() {
            let mut picks: Vec<u32> = Vec::new();
            'tags: for tag in &relations {
                for cand in store.graph().relation_candidates(tag) {
                    if picks.len() >= MAX_AUTO_LINKS {
                        break 'tags;
                    }
                    if cand == local || picks.contains(&cand) {
                        continue;
                    }
                    let same_owner = store
                        .graph()
                        .get_node(cand)
                        .map(|n| n.owner_id == self.owner)
                        .unwrap_or(false);
                    if same_owner {
                        picks.push(cand);
                    }
                }
            }
            for cand in picks {
                let _ = store.add_edge(local, cand, AUTO_LINK_WEIGHT);
            }
        }
        Ok(local as u64)
    }

    /// 情境共振：词面选种（查询词项在 fact 中子串命中）+ 图多跳扩散。
    /// 无命中种子时以最近节点兜底（对齐服务端“最近节点兜底”语义）。
    pub fn resonate(&self, query: String, budget: u32) -> Result<Vec<ActivatedMemory>, MobileError> {
        let budget = (budget as usize).clamp(1, 64);
        let now = now_secs();
        let terms = query_terms(&query);
        let store = lock(&self.store)?;
        let graph = store.graph();

        let mut seeds: Vec<(u32, f32)> = Vec::new();
        if !terms.is_empty() {
            for (local, node) in graph.live_nodes() {
                if node.owner_id != self.owner {
                    continue;
                }
                let hits = terms
                    .iter()
                    .filter(|t| node.filaments.fact.to_lowercase().contains(t.as_str()))
                    .count();
                if hits > 0 {
                    let w = (SEED_BASE_STRENGTH + 0.1 * hits as f32).min(1.0);
                    seeds.push((local, w));
                }
            }
            seeds.sort_by(|a, b| b.1.total_cmp(&a.1));
            seeds.truncate(MAX_SEEDS);
        }
        if seeds.is_empty() {
            // 兜底：按创建时间取最近 3 条同 owner 记忆。
            let mut recent: Vec<(u32, i64)> = graph
                .live_nodes()
                .filter(|(_, n)| n.owner_id == self.owner)
                .map(|(id, n)| (id, n.filaments.created_at))
                .collect();
            recent.sort_by_key(|r| std::cmp::Reverse(r.1));
            seeds = recent
                .into_iter()
                .take(3)
                .map(|(id, _)| (id, 0.3))
                .collect();
        }

        let ctx = ContextSpectrum { task: None, emotion_valence: None, max_hops: None };
        let activated = graph.resonate(&seeds, &ctx, now, budget);
        Ok(activated
            .into_iter()
            .filter_map(|(local, score)| {
                graph.get_node(local).map(|n| ActivatedMemory {
                    local_id: local,
                    fact: n.filaments.fact.clone(),
                    relations: n.filaments.relations.clone(),
                    resonance: score,
                    tension: compute_tension(n, now, 1.0),
                })
            })
            .collect())
    }

    /// 按局部 ID 读取记忆详情。
    pub fn get(&self, local_id: u32) -> Result<MobileMemory, MobileError> {
        let store = lock(&self.store)?;
        let now = now_secs();
        store
            .graph()
            .get_node(local_id)
            .filter(|n| n.owner_id == self.owner)
            .map(|n| MobileMemory {
                local_id,
                fact: n.filaments.fact.clone(),
                relations: n.filaments.relations.clone(),
                created_at: n.filaments.created_at,
                tension: compute_tension(n, now, 1.0),
            })
            .ok_or_else(|| MobileError::NotFound {
                message: format!("node {local_id} (owner {})", self.owner),
            })
    }

    /// 当前 owner 的存活记忆数。
    pub fn node_count(&self) -> Result<u64, MobileError> {
        let store = lock(&self.store)?;
        Ok(store
            .graph()
            .live_nodes()
            .filter(|(_, n)| n.owner_id == self.owner)
            .count() as u64)
    }

    /// 主动 checkpoint：App 退后台/低内存时调用，防进程被杀丢 WAL。
    pub fn checkpoint(&self) -> Result<(), MobileError> {
        let mut store = lock(&self.store)?;
        store.checkpoint().map_err(|e| MobileError::Io { message: format!("checkpoint: {e}") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(dir: &str) -> Arc<MobileEngine> {
        MobileEngine::new(MobileConfig { data_dir: dir.into(), owner: "tester".into() }).unwrap()
    }

    #[test]
    fn weave_and_resonate_roundtrip() {
        let dir = format!("{}/nylon-mobile-test-{}", std::env::temp_dir().display(), now_secs());
        let e = engine(&dir);
        let id = e.weave("User prefers oat milk latte".into(), None, None).unwrap();
        assert!(e.get(id as u32).is_ok(), "weave 返回的 id 应可读回");
        let hits = e.resonate("oat milk".into(), 5).unwrap();
        assert!(!hits.is_empty());
        assert!(hits.iter().any(|h| h.fact.contains("oat milk")));
        e.checkpoint().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cjk_relations_extracted() {
        let rels = heuristic_relations("我明天要去杭州出差见客户");
        assert!(!rels.is_empty(), "CJK bigram relations should not be empty");
    }
}


