//! CSR 主图 + Delta 缓冲 + 情境共振遍历。
//!
//! 对应《尼龙技术架构 v0.2》3.2 节增量更新方案：
//! 新增边写入 Delta（HashMap 邻接表），读取时合并视图，达到阈值后 compaction 重建 CSR。
//! 共振遍历使用优先级队列（按强度出队）+ 全局激活预算，防止高扇出节点扩散爆炸。

use nylon_core::{compute_tension, MemoryNode};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

pub const MAX_DEPTH: u8 = 3;
pub const DECAY_FACTOR: f32 = 0.7;
pub const MIN_STRENGTH: f32 = 0.01;
pub const DEFAULT_BUDGET: usize = 64;
pub const DELTA_COMPACT_THRESHOLD: usize = 100_000;

/// CSR 静态主图（内存连续、缓存友好）。
#[derive(Debug, Default, Clone)]
pub struct CsrGraph {
    offsets: Vec<u32>,
    targets: Vec<u32>,
    weights: Vec<f32>,
}

impl CsrGraph {
    /// 从边列表构建。节点 ID 需为分片内局部 ID（0..num_nodes）。
    pub fn from_edges(num_nodes: usize, edges: &[(u32, u32, f32)]) -> Self {
        let mut degree = vec![0u32; num_nodes + 1];
        for &(src, _, _) in edges {
            degree[src as usize + 1] += 1;
        }
        for i in 1..=num_nodes {
            degree[i] += degree[i - 1];
        }
        let mut targets = vec![0u32; edges.len()];
        let mut weights = vec![0.0f32; edges.len()];
        let mut cursor = degree.clone();
        for &(src, dst, w) in edges {
            let pos = cursor[src as usize] as usize;
            targets[pos] = dst;
            weights[pos] = w;
            cursor[src as usize] += 1;
        }
        CsrGraph { offsets: degree, targets, weights }
    }

    pub fn neighbors(&self, node: u32) -> impl Iterator<Item = (u32, f32)> + '_ {
        let (s, e) = (self.offsets[node as usize] as usize, self.offsets[node as usize + 1] as usize);
        self.targets[s..e].iter().zip(self.weights[s..e].iter()).map(|(&t, &w)| (t, w))
    }
}

/// Delta 缓冲：新增边的临时邻接表。
#[derive(Debug, Default)]
struct DeltaGraph {
    adj: HashMap<u32, Vec<(u32, f32)>>,
    edge_count: usize,
}

/// 情境信号（对应《尼龙记忆模型》2.5 节，v1 先实现任务 + 情绪两维）。
#[derive(Debug, Default, Clone)]
pub struct ContextSpectrum {
    pub task: Option<String>,
    pub emotion_valence: Option<f32>,
}

impl ContextSpectrum {
    /// 情境匹配度 ∈ (0, 1]：任务命中关系丝加成，情绪效价接近加成。
    fn match_score(&self, node: &MemoryNode) -> f32 {
        let mut score = 1.0f32;
        if let Some(task) = &self.task {
            if node.filaments.relations.iter().any(|r| r == task) {
                score *= 1.2;
            }
        }
        if let Some(v) = self.emotion_valence {
            score *= 1.0 - 0.3 * (v - node.filaments.emotion_valence).abs().min(1.0);
        }
        score.clamp(0.05, 1.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct State {
    score: f32,
    node: u32,
    depth: u8,
}
impl Eq for State {}
impl Ord for State {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score.total_cmp(&other.score)
    }
}
impl PartialOrd for State {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// 记忆图：CSR 主图 + Delta 缓冲 + 节点存储。
#[derive(Default)]
pub struct MemoryGraph {
    csr: CsrGraph,
    delta: DeltaGraph,
    nodes: HashMap<u32, MemoryNode>,
    next_local_id: u32,
}

impl MemoryGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// 写入节点，返回分片内局部 ID。
    pub fn add_node(&mut self, node: MemoryNode) -> u32 {
        let local = self.next_local_id;
        self.next_local_id += 1;
        self.nodes.insert(local, node);
        local
    }

    /// 新增边写入 Delta 缓冲（不触发 CSR 重建）。
    pub fn add_edge(&mut self, from: u32, to: u32, weight: f32) {
        self.delta.adj.entry(from).or_default().push((to, weight));
        self.delta.edge_count += 1;
    }

    /// 按分片内局部 ID 读取节点。
    pub fn get_node(&self, local_id: u32) -> Option<&MemoryNode> {
        self.nodes.get(&local_id)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Delta 边数超阈值时应触发 compaction 重建主 CSR。
    pub fn needs_compaction(&self) -> bool {
        self.delta.edge_count >= DELTA_COMPACT_THRESHOLD
    }

    fn neighbors(&self, node: u32) -> Vec<(u32, f32)> {
        let mut out: Vec<(u32, f32)> = Vec::new();
        if (node as usize) + 1 < self.csr.offsets.len() {
            out.extend(self.csr.neighbors(node));
        }
        if let Some(edges) = self.delta.adj.get(&node) {
            out.extend(edges.iter().copied());
        }
        out
    }

    /// 情境共振：优先级队列（按强度出队）+ 全局激活预算（Top-K 截断）。
    pub fn resonate(
        &self,
        seeds: &[u32],
        ctx: &ContextSpectrum,
        now: i64,
        budget: usize,
    ) -> Vec<(u32, f32)> {
        let mut heap = BinaryHeap::new();
        let mut best: HashMap<u32, f32> = HashMap::new();
        for &s in seeds {
            heap.push(State { score: 1.0, node: s, depth: 0 });
        }
        while let Some(State { score, node, depth }) = heap.pop() {
            if depth > MAX_DEPTH || score < MIN_STRENGTH {
                continue;
            }
            if best.get(&node).is_some_and(|&b| b >= score) {
                continue; // 已以更优强度处理过
            }
            let Some(n) = self.nodes.get(&node) else { continue };
            let resonance = score * ctx.match_score(n) * compute_tension(n, now, 1.0);
            best.insert(node, resonance);
            if best.len() >= budget {
                break; // 全局激活预算：截断扩散
            }
            for (nb, w) in self.neighbors(node) {
                let next = score * w * DECAY_FACTOR;
                if next >= MIN_STRENGTH && best.get(&nb).is_none_or(|&b| b < next) {
                    heap.push(State { score: next, node: nb, depth: depth + 1 });
                }
            }
        }
        let mut out: Vec<(u32, f32)> = best.into_iter().collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nylon_core::{Filaments, Tension};

    fn node(fact: &str, relations: &[&str]) -> MemoryNode {
        MemoryNode {
            id: 0,
            owner_id: "alice".into(),
            filaments: Filaments {
                fact: fact.into(),
                emotion_valence: 0.5,
                emotion_intensity: 0.5,
                created_at: 0,
                decay_rate: 0.01,
                relations: relations.iter().map(|s| s.to_string()).collect(),
                confidence: 0.9,
                mentions_7d: 0,
            },
            tension: Tension { baseline: 0.8, last_updated: 0 },
            embedding: vec![],
        }
    }

    #[test]
    fn csr_neighbors_roundtrip() {
        let g = CsrGraph::from_edges(3, &[(0, 1, 0.9), (0, 2, 0.5), (2, 0, 0.3)]);
        let mut ns: Vec<_> = g.neighbors(0).collect();
        ns.sort_by_key(|&(t, _)| t);
        assert_eq!(ns, vec![(1, 0.9), (2, 0.5)]);
    }

    #[test]
    fn resonance_spreads_and_decays_with_depth() {
        let mut g = MemoryGraph::new();
        let a = g.add_node(node("机票", &["出差"]));
        let b = g.add_node(node("酒店偏好", &["出差"]));
        let c = g.add_node(node("会员卡号", &[]));
        g.add_edge(a, b, 0.9);
        g.add_edge(b, c, 0.9);
        let ctx = ContextSpectrum::default();
        let out = g.resonate(&[a], &ctx, 0, DEFAULT_BUDGET);
        let score_of = |id: u32| out.iter().find(|&&(n, _)| n == id).map(|&(_, s)| s);
        assert!(score_of(a) > score_of(b), "1 跳应弱于种子");
        assert!(score_of(b).unwrap() > score_of(c).unwrap_or(0.0), "2 跳应进一步衰减");
    }

    #[test]
    fn budget_truncates_star_explosion() {
        let mut g = MemoryGraph::new();
        let hub = g.add_node(node("热门实体", &[]));
        for i in 0..500 {
            let leaf = g.add_node(node(&format!("叶子 {i}"), &[]));
            g.add_edge(hub, leaf, 0.9);
        }
        let out = g.resonate(&[hub], &ContextSpectrum::default(), 0, 32);
        assert!(out.len() <= 32, "全局预算应截断星型扩散，实际 {}", out.len());
    }
}