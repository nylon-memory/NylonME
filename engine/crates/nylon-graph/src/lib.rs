//! CSR 主图 + Delta 缓冲 + 情境共振遍历。
//!
//! 对应《尼龙技术架构 v0.2》3.2 节增量更新方案：
//! 新增边写入 Delta（HashMap 邻接表），读取时合并视图，达到阈值后 compaction 重建 CSR。
//! 删除走墓碑（tombstone）标记：逻辑删除即时生效，物理清理由 compaction 完成。
//! 共振遍历使用优先级队列（按强度出队）+ 全局激活预算，防止高扇出节点扩散爆炸。

use nylon_core::{compute_tension, MemoryNode};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

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

    /// 定位 from→to 在 targets/weights 中的下标（O(出度) 线性扫描）。
    fn edge_pos(&self, from: u32, to: u32) -> Option<usize> {
        if (from as usize) + 1 >= self.offsets.len() {
            return None;
        }
        let (s, e) = (self.offsets[from as usize] as usize, self.offsets[from as usize + 1] as usize);
        (s..e).find(|&i| self.targets[i] == to)
    }
}

/// Delta 缓冲：新增边的临时邻接表。
#[derive(Debug, Default)]
struct DeltaGraph {
    adj: HashMap<u32, Vec<(u32, f32)>>,
    edge_count: usize,
}

/// 六丝组合检索过滤器：各字段 AND 组合，None 表示不限制。
#[derive(Debug, Default, Clone)]
pub struct FilamentFilter {
    /// 关系丝：命中任一标签即匹配
    pub relations_any: Option<Vec<String>>,
    /// 情感丝：效价闭区间
    pub emotion_range: Option<(f32, f32)>,
    /// 时序丝：创建时间闭区间
    pub time_range: Option<(i64, i64)>,
    /// 置信丝：下限
    pub min_confidence: Option<f32>,
    /// 频次丝：下限
    pub min_mentions: Option<u32>,
}

impl FilamentFilter {
    fn matches(&self, node: &MemoryNode) -> bool {
        let f = &node.filaments;
        if let Some(tags) = &self.relations_any {
            if !tags.iter().any(|t| f.relations.contains(t)) {
                return false;
            }
        }
        if let Some((lo, hi)) = self.emotion_range {
            if f.emotion_valence < lo || f.emotion_valence > hi {
                return false;
            }
        }
        if let Some((lo, hi)) = self.time_range {
            if f.created_at < lo || f.created_at > hi {
                return false;
            }
        }
        if let Some(min) = self.min_confidence {
            if f.confidence < min {
                return false;
            }
        }
        if let Some(min) = self.min_mentions {
            if f.mentions_7d < min {
                return false;
            }
        }
        true
    }
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
    /// 节点墓碑：逻辑删除立即生效，物理清理由 compact() 完成。
    tombstones: HashSet<u32>,
    /// CSR 中已删除边的墓碑（Delta 边直接物理移除，无需墓碑）。
    edge_tombstones: HashSet<(u32, u32)>,
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
        let list = self.delta.adj.entry(from).or_default();
        // Delta 内去重：同目标边已存在则视为权重更新；
        // CSR 中的旧副本由合并视图与 compact 的"Delta 优先"规则覆盖
        if let Some(e) = list.iter_mut().find(|e| e.0 == to) {
            e.1 = weight;
            return;
        }
        list.push((to, weight));
        self.delta.edge_count += 1;
    }

    /// 更新节点各丝字段。节点不存在或已删除时返回 false。
    pub fn update_node(&mut self, local_id: u32, node: MemoryNode) -> bool {
        if self.tombstones.contains(&local_id) {
            return false;
        }
        match self.nodes.get_mut(&local_id) {
            Some(slot) => {
                *slot = node;
                true
            }
            None => false,
        }
    }

    /// 逻辑删除节点：打墓碑标记，同时清理其出边（Delta 直接移除，CSR 加边墓碑）。
    /// 物理删除留给 compact()。节点不存在或已删除时返回 false。
    pub fn remove_node(&mut self, local_id: u32) -> bool {
        if !self.nodes.contains_key(&local_id) || self.tombstones.contains(&local_id) {
            return false;
        }
        self.tombstones.insert(local_id);
        if let Some(edges) = self.delta.adj.remove(&local_id) {
            self.delta.edge_count -= edges.len();
        }
        if (local_id as usize) + 1 < self.csr.offsets.len() {
            for (t, _) in self.csr.neighbors(local_id) {
                self.edge_tombstones.insert((local_id, t));
            }
        }
        true
    }

    /// 删除边：Delta 中直接移除，CSR 中加墓碑。返回是否确有边被删除。
    pub fn remove_edge(&mut self, from: u32, to: u32) -> bool {
        let mut removed = false;
        let mut emptied = false;
        if let Some(list) = self.delta.adj.get_mut(&from) {
            let before = list.len();
            list.retain(|&(t, _)| t != to);
            let n = before - list.len();
            if n > 0 {
                self.delta.edge_count -= n;
                removed = true;
            }
            emptied = list.is_empty();
        }
        if emptied {
            self.delta.adj.remove(&from);
        }
        if self.csr.edge_pos(from, to).is_some() {
            // insert 返回 false 表示墓碑已存在（重复删除）
            removed |= self.edge_tombstones.insert((from, to));
        }
        removed
    }

    /// 更新边权重：Delta 与 CSR 中的副本都原地更新，保证合并视图一致。
    /// 边不存在或已删除（墓碑）时返回 false。
    pub fn update_edge(&mut self, from: u32, to: u32, weight: f32) -> bool {
        let mut found = false;
        if let Some(list) = self.delta.adj.get_mut(&from) {
            for e in list.iter_mut() {
                if e.0 == to {
                    e.1 = weight;
                    found = true;
                }
            }
        }
        if self.edge_tombstones.contains(&(from, to)) {
            return found; // CSR 副本已删除，不复活
        }
        if let Some(pos) = self.csr.edge_pos(from, to) {
            self.csr.weights[pos] = weight;
            found = true;
        }
        found
    }

    /// 六丝组合检索：多条件 AND 组合，排除墓碑节点，结果按局部 ID 升序。
    /// 空过滤器返回全部存活节点。
    pub fn find_by_filaments(&self, filter: &FilamentFilter) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .nodes
            .iter()
            .filter(|(id, n)| !self.tombstones.contains(id) && filter.matches(n))
            .map(|(id, _)| *id)
            .collect();
        out.sort_unstable();
        out
    }

    /// 按分片内局部 ID 读取节点（已删除节点对外不可见）。
    pub fn get_node(&self, local_id: u32) -> Option<&MemoryNode> {
        if self.tombstones.contains(&local_id) {
            return None;
        }
        self.nodes.get(&local_id)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len() - self.tombstones.len()
    }

    /// 按关系丝标签过滤，返回命中的局部 ID（升序，已删除节点除外）。
    pub fn find_by_relation(&self, tag: &str) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .nodes
            .iter()
            .filter(|(id, n)| {
                !self.tombstones.contains(id) && n.filaments.relations.iter().any(|r| r == tag)
            })
            .map(|(id, _)| *id)
            .collect();
        out.sort_unstable();
        out
    }

    /// 按创建时间范围过滤（闭区间，基于时序丝 created_at），返回局部 ID（升序）。
    pub fn find_by_time_range(&self, start: i64, end: i64) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .nodes
            .iter()
            .filter(|(id, n)| {
                !self.tombstones.contains(id) && (start..=end).contains(&n.filaments.created_at)
            })
            .map(|(id, _)| *id)
            .collect();
        out.sort_unstable();
        out
    }

    /// Delta 边数或墓碑数超阈值时应触发 compaction 重建主 CSR。
    pub fn needs_compaction(&self) -> bool {
        self.delta.edge_count >= DELTA_COMPACT_THRESHOLD
            || self.tombstones.len() + self.edge_tombstones.len() >= DELTA_COMPACT_THRESHOLD
    }

    /// Compaction：把 Delta 缓冲与墓碑合并进 CSR 主图。
    /// 丢弃指向/起自墓碑节点的边与边墓碑，物理删除墓碑节点，随后清空 Delta 与墓碑集。
    pub fn compact(&mut self) {
        // 新 CSR 需覆盖现有局部 ID 与所有存活边的端点
        let mut num_nodes = self.next_local_id as usize;
        // 合并去重：同一 (src, dst) 只保留一条，Delta（较新）覆盖 CSR
        let mut merged: HashMap<(u32, u32), f32> = HashMap::new();
        for src in 0..self.next_local_id {
            if self.tombstones.contains(&src) {
                continue;
            }
            if (src as usize) + 1 < self.csr.offsets.len() {
                for (t, w) in self.csr.neighbors(src) {
                    if self.tombstones.contains(&t) || self.edge_tombstones.contains(&(src, t)) {
                        continue;
                    }
                    merged.insert((src, t), w);
                }
            }
        }
        for (&src, list) in &self.delta.adj {
            if self.tombstones.contains(&src) {
                continue;
            }
            for &(t, w) in list {
                if self.tombstones.contains(&t) {
                    continue;
                }
                num_nodes = num_nodes.max(src as usize + 1).max(t as usize + 1);
                merged.insert((src, t), w);
            }
        }
        let edges: Vec<(u32, u32, f32)> =
            merged.into_iter().map(|((s, t), w)| (s, t, w)).collect();
        self.csr = CsrGraph::from_edges(num_nodes, &edges);
        self.delta = DeltaGraph::default();
        self.edge_tombstones.clear();
        for id in std::mem::take(&mut self.tombstones) {
            self.nodes.remove(&id); // 物理删除
        }
    }

    fn neighbors(&self, node: u32) -> Vec<(u32, f32)> {
        // 合并视图去重：同一目标只保留一条，Delta（较新）优先于 CSR
        let mut merged: HashMap<u32, f32> = HashMap::new();
        if (node as usize) + 1 < self.csr.offsets.len() {
            for (t, w) in self.csr.neighbors(node) {
                if !self.edge_tombstones.contains(&(node, t)) {
                    merged.insert(t, w);
                }
            }
        }
        if let Some(edges) = self.delta.adj.get(&node) {
            for &(t, w) in edges {
                merged.insert(t, w);
            }
        }
        merged.into_iter().collect()
    }

    /// 情境共振：优先级队列（按强度出队）+ 全局激活预算（Top-K 截断）。
    /// 已删除（墓碑）节点不参与遍历，也不会出现在结果中。
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
            let Some(n) = self.get_node(node) else { continue };
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

    fn node_at(fact: &str, relations: &[&str], created_at: i64) -> MemoryNode {
        let mut n = node(fact, relations);
        n.filaments.created_at = created_at;
        n
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

    #[test]
    fn update_node_rewrites_filaments() {
        let mut g = MemoryGraph::new();
        let a = g.add_node(node("旧事实", &["工作"]));
        let mut updated = node("新事实", &["生活"]);
        updated.filaments.emotion_valence = -0.5;
        assert!(g.update_node(a, updated));
        let n = g.get_node(a).unwrap();
        assert_eq!(n.filaments.fact, "新事实");
        assert_eq!(n.filaments.relations, vec!["生活".to_string()]);
        assert_eq!(n.filaments.emotion_valence, -0.5);
        assert!(!g.update_node(999, node("幽灵", &[])), "不存在的节点应返回 false");
    }

    #[test]
    fn remove_node_excludes_from_resonance_and_lookup() {
        let mut g = MemoryGraph::new();
        let a = g.add_node(node("a", &[]));
        let b = g.add_node(node("b", &[]));
        let c = g.add_node(node("c", &[]));
        g.add_edge(a, b, 0.9);
        g.add_edge(b, c, 0.9);
        assert!(g.remove_node(b));
        assert!(g.get_node(b).is_none(), "墓碑节点对外不可见");
        assert_eq!(g.node_count(), 2);
        assert!(!g.remove_node(b), "重复删除应返回 false");
        assert!(g.delta.adj.get(&b).is_none(), "b 的 Delta 出边应被清理");
        let out = g.resonate(&[a], &ContextSpectrum::default(), 0, DEFAULT_BUDGET);
        let ids: Vec<u32> = out.iter().map(|&(n, _)| n).collect();
        assert!(ids.contains(&a));
        assert!(!ids.contains(&b), "删除的节点不参与共振结果");
        assert!(!ids.contains(&c), "b 删除后 c 应不可达");
    }

    #[test]
    fn remove_node_tombstones_csr_outgoing_edges() {
        let mut g = MemoryGraph::new();
        let a = g.add_node(node("a", &[]));
        let b = g.add_node(node("b", &[]));
        g.add_edge(a, b, 0.9);
        g.compact(); // 边进 CSR
        assert!(g.remove_node(a));
        assert!(g.neighbors(a).is_empty(), "CSR 出边应被边墓碑屏蔽");
        assert!(g.edge_tombstones.contains(&(a, b)));
    }

    #[test]
    fn remove_edge_works_in_delta_and_csr() {
        let mut g = MemoryGraph::new();
        let a = g.add_node(node("a", &[]));
        let b = g.add_node(node("b", &[]));
        let c = g.add_node(node("c", &[]));
        // Delta 路径：直接移除
        g.add_edge(a, b, 0.9);
        assert!(g.remove_edge(a, b), "Delta 中的边应直接移除");
        assert!(!g.remove_edge(a, b), "重复删除应返回 false");
        assert_eq!(g.delta.edge_count, 0);
        // CSR 路径：compaction 后加墓碑
        g.add_edge(a, b, 0.9);
        g.add_edge(b, c, 0.9);
        g.compact();
        assert!(g.remove_edge(a, b), "CSR 中的边应加墓碑");
        assert!(g.edge_tombstones.contains(&(a, b)));
        let out = g.resonate(&[a], &ContextSpectrum::default(), 0, DEFAULT_BUDGET);
        let ids: Vec<u32> = out.iter().map(|&(n, _)| n).collect();
        assert!(!ids.contains(&b) && !ids.contains(&c), "删边后 b/c 不可达");
    }

    #[test]
    fn update_edge_changes_weight_in_delta_and_csr() {
        let mut g = MemoryGraph::new();
        let a = g.add_node(node("a", &[]));
        let b = g.add_node(node("b", &[]));
        g.add_edge(a, b, 0.5);
        assert!(g.update_edge(a, b, 0.9), "Delta 中的边应可更新");
        assert!(g.neighbors(a).contains(&(b, 0.9)));
        g.compact();
        assert!(g.update_edge(a, b, 0.3), "CSR 中的边应可更新");
        assert!(g.neighbors(a).contains(&(b, 0.3)));
        assert!(!g.neighbors(a).contains(&(b, 0.9)), "旧权重不应残留");
        assert!(!g.update_edge(a, 999, 0.1), "不存在的边应返回 false");
        // 已删除的边不复活
        assert!(g.remove_edge(a, b));
        assert!(!g.update_edge(a, b, 1.0), "墓碑边不应被更新复活");
    }

    #[test]
    fn find_by_relation_filters_live_nodes() {
        let mut g = MemoryGraph::new();
        let a = g.add_node(node("靠窗座位", &["出差", "住宿"]));
        let b = g.add_node(node("科幻小说", &["阅读"]));
        let _c = g.add_node(node("无标签", &[]));
        assert_eq!(g.find_by_relation("出差"), vec![a]);
        assert_eq!(g.find_by_relation("阅读"), vec![b]);
        assert!(g.find_by_relation("不存在").is_empty());
        g.remove_node(a);
        assert!(g.find_by_relation("出差").is_empty(), "已删除节点不应命中");
    }

    #[test]
    fn find_by_time_range_filters_inclusive_bounds() {
        let mut g = MemoryGraph::new();
        let a = g.add_node(node_at("早", &[], 100));
        let b = g.add_node(node_at("中", &[], 200));
        let c = g.add_node(node_at("晚", &[], 300));
        assert_eq!(g.find_by_time_range(150, 250), vec![b]);
        assert_eq!(g.find_by_time_range(100, 300), vec![a, b, c], "闭区间边界应包含端点");
        assert_eq!(g.find_by_time_range(100, 100), vec![a]);
        assert!(g.find_by_time_range(400, 500).is_empty());
        g.remove_node(b);
        assert_eq!(g.find_by_time_range(100, 300), vec![a, c], "已删除节点不应命中");
    }

    #[test]
    fn compact_merges_delta_and_applies_tombstones() {
        let mut g = MemoryGraph::new();
        let a = g.add_node(node("a", &[]));
        let b = g.add_node(node("b", &[]));
        let c = g.add_node(node("c", &[]));
        let d = g.add_node(node("d", &[]));
        g.add_edge(a, b, 0.9);
        g.add_edge(a, c, 0.5);
        g.compact(); // 首批边进 CSR
        assert!(!g.needs_compaction());
        assert_eq!(g.delta.edge_count, 0);
        g.add_edge(b, d, 0.9); // 进 Delta
        g.remove_edge(a, c); // CSR 边墓碑
        g.remove_node(b); // 节点墓碑（含 b→d 出边清理）
        g.compact(); // 合并 Delta + 应用墓碑
        assert_eq!(g.node_count(), 3, "b 应被物理删除");
        assert!(g.get_node(b).is_none());
        assert_eq!(g.delta.edge_count, 0);
        assert!(g.tombstones.is_empty() && g.edge_tombstones.is_empty(), "墓碑应被清空");
        assert!(g.neighbors(a).is_empty(), "a→b/a→c 都应被物理清除");
        let out = g.resonate(&[a], &ContextSpectrum::default(), 0, DEFAULT_BUDGET);
        let ids: Vec<u32> = out.iter().map(|&(n, _)| n).collect();
        assert_eq!(ids, vec![a], "compact 后只剩 a 可达");
        // compact 后仍能正常写入
        g.add_edge(a, d, 0.7);
        let out = g.resonate(&[a], &ContextSpectrum::default(), 0, DEFAULT_BUDGET);
        assert!(out.iter().any(|&(n, _)| n == d), "compact 后新增边应立即可达");
    }

    #[test]
    fn duplicate_edges_dedup_with_delta_winning() {
        let mut g = MemoryGraph::new();
        let a = g.add_node(node("A", &[]));
        let b = g.add_node(node("B", &[]));
        g.add_edge(a, b, 0.5);
        g.compact();
        // CSR 与 Delta 同时存在同一条边：合并视图应去重且 Delta 优先
        g.add_edge(a, b, 0.9);
        let ns = g.neighbors(a);
        assert_eq!(ns.len(), 1, "合并视图不应有重复边: {ns:?}");
        assert_eq!(ns[0].1, 0.9, "Delta 应覆盖 CSR");
        // compact 后同样无重复
        g.compact();
        let ns = g.neighbors(a);
        assert_eq!(ns.len(), 1, "compact 后不应有重复边: {ns:?}");
        assert_eq!(ns[0].1, 0.9);
        // Delta 内重复 add_edge 视为权重更新
        g.add_edge(a, b, 0.3);
        g.add_edge(a, b, 0.7);
        let ns = g.neighbors(a);
        assert_eq!(ns.len(), 1, "Delta 内重复边应合并: {ns:?}");
        assert_eq!(ns[0].1, 0.7);
    }

    #[test]
    fn find_by_filaments_combines_conditions() {
        let mut g = MemoryGraph::new();
        let a = g.add_node(node("出差偏好", &["出差"]));
        let b = g.add_node(node("科幻小说", &["阅读"]));
        let c = g.add_node(node("出差报销", &["出差", "财务"]));
        // 单条件
        let f = FilamentFilter { relations_any: Some(vec!["出差".into()]), ..Default::default() };
        assert_eq!(g.find_by_filaments(&f), vec![a, c]);
        // 多条件 AND
        let f = FilamentFilter {
            relations_any: Some(vec!["出差".into()]),
            min_confidence: Some(0.95),
            ..Default::default()
        };
        assert_eq!(g.find_by_filaments(&f), vec![], "置信度 0.9 应被 0.95 过滤");
        let f = FilamentFilter {
            relations_any: Some(vec!["出差".into(), "阅读".into()]),
            min_mentions: Some(0),
            ..Default::default()
        };
        assert_eq!(g.find_by_filaments(&f), vec![a, b, c], "任一标签命中");
        // 空过滤器返回全部活节点
        assert_eq!(g.find_by_filaments(&FilamentFilter::default()), vec![a, b, c]);
        // 墓碑排除
        g.remove_node(b);
        assert_eq!(g.find_by_filaments(&FilamentFilter::default()), vec![a, c]);
    }
}