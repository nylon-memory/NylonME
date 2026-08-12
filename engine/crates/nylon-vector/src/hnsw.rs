//! HNSW（Hierarchical Navigable Small World）近似最近邻索引。
//!
//! 算法参考 Malkov & Yashunin (2016)
//! "Efficient and robust approximate nearest neighbor search using
//!  Hierarchical Navigable Small World graphs"。
//!
//! 实现要点：
//! - 层级按指数衰减分布采样：level = floor(-ln(u) * level_mult)，level_mult = 1/ln(M)
//! - 距离度量为余弦距离（1 - cosine），与 `BruteForceIndex` 完全一致；
//!   向量入库时归一化，距离退化为 1 - 点积，查询更快
//! - 高层贪心下降到目标层，目标层做 ef_construction 束搜索选邻居，双向连边并剪枝
//! - 零外部依赖：层级采样使用内置 xorshift64*，保证离线可构建、结果可复现

use std::cell::{Cell, RefCell};
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

use crate::VectorIndex;

/// HNSW 构建与查询参数。
///
/// 默认值参考论文：M=16, ef_construction=200, ef_search=50。
#[derive(Debug, Clone)]
pub struct HnswParams {
    /// 每个节点的最大连接数（0 层按论文惯例使用 2M）。
    pub m: usize,
    /// 构建时的束搜索宽度，越大图质量越高、构建越慢。
    pub ef_construction: usize,
    /// 查询时的束搜索宽度，越大召回越高、查询越慢（实际取 max(ef_search, k)）。
    pub ef_search: usize,
    /// 层级采样随机种子，相同种子 + 相同插入顺序 => 完全相同的索引。
    pub seed: u64,
}

impl Default for HnswParams {
    fn default() -> Self {
        HnswParams {
            m: 16,
            ef_construction: 200,
            ef_search: 50,
            seed: 0x9E37_79B9_7F4A_7C15,
        }
    }
}

/// xorshift64* 确定性随机数发生器（不引入 rand crate）。
struct XorShift64(u64);

impl XorShift64 {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 均匀采样 (0, 1]，避开 ln(0)。
    fn next_f64_open(&mut self) -> f64 {
        let v = self.next() >> 11; // 53 位有效精度
        (v as f64 + 0.5) / 9_007_199_254_740_992.0
    }
}

/// 堆元素：(距离, 节点号)。按距离全序比较，距离相同比节点号，保证行为确定。
#[derive(Clone, Copy, PartialEq)]
struct Scored(f32, u32);

impl Eq for Scored {}

impl Ord for Scored {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .total_cmp(&other.0)
            .then_with(|| self.1.cmp(&other.1))
    }
}

impl PartialOrd for Scored {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// 归一化向量；零向量原样返回（点积恒为 0，与 `cosine()` 对零向量返回 0 一致）。
fn normalize(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

/// 点积（要求两个向量均已归一化，调用方保证）。
/// 4 路累加器手动展开，利用指令级并行（f32 无 fast-math，编译器不会自动重排）。
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len() / 4 * 4;
    let (mut s0, mut s1, mut s2, mut s3) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut i = 0;
    while i < n {
        s0 += a[i] * b[i];
        s1 += a[i + 1] * b[i + 1];
        s2 += a[i + 2] * b[i + 2];
        s3 += a[i + 3] * b[i + 3];
        i += 4;
    }
    let mut s = s0 + s1 + s2 + s3;
    while i < a.len() {
        s += a[i] * b[i];
        i += 1;
    }
    s
}

/// HNSW 近似最近邻索引（余弦距离），实现 [`VectorIndex`]。
///
/// 单线程使用（内部用 `RefCell` 做访问标记缓存，未做线程安全）。
///
/// # 示例
/// ```
/// use nylon_vector::{HnswIndex, VectorIndex};
/// let mut idx = HnswIndex::new(2);
/// idx.add(1, &[1.0, 0.0]);
/// idx.add(2, &[0.0, 1.0]);
/// let out = idx.search(&[1.0, 0.0], 1);
/// assert_eq!(out[0].0, 1);
/// ```
pub struct HnswIndex {
    dims: usize,
    m: usize,
    /// 0 层最大连接数（论文取 2M）。
    m_max0: usize,
    ef_construction: usize,
    ef_search: usize,
    /// 层级衰减系数 1/ln(M)。
    level_mult: f64,
    rng: XorShift64,
    /// 入口点（当前最高层节点）。
    entry: Option<u32>,
    max_level: usize,
    ids: Vec<u32>,
    /// 归一化后的向量，余弦距离 = 1 - 点积。
    data: Vec<f32>,
    /// 每个节点的最高层。
    levels: Vec<u8>,
    /// 邻接表：neighbors[节点][层] = 邻居节点编号。
    neighbors: Vec<Vec<Vec<u32>>>,
    /// 访问标记（epoch 计数，避免每次束搜索都 O(n) 清零）。
    visited: RefCell<Vec<u32>>,
    epoch: Cell<u32>,
}

/// 层级上限：防止病态随机值导致内存异常，32 层对亿级数据也绰绰有余。
const MAX_LEVEL_CAP: usize = 32;

impl HnswIndex {
    /// 以默认参数（M=16, ef_construction=200, ef_search=50）创建索引。
    pub fn new(dims: usize) -> Self {
        Self::with_params(dims, HnswParams::default())
    }

    /// 以自定义参数创建索引。
    pub fn with_params(dims: usize, params: HnswParams) -> Self {
        assert!(dims > 0, "维度必须为正");
        assert!(params.m >= 2, "M 至少为 2");
        assert!(params.seed != 0, "xorshift 种子不能为 0");
        HnswIndex {
            dims,
            m: params.m,
            m_max0: params.m * 2,
            ef_construction: params.ef_construction.max(1),
            ef_search: params.ef_search.max(1),
            level_mult: 1.0 / (params.m as f64).ln(),
            rng: XorShift64(params.seed),
            entry: None,
            max_level: 0,
            ids: Vec::new(),
            data: Vec::new(),
            levels: Vec::new(),
            neighbors: Vec::new(),
            visited: RefCell::new(Vec::new()),
            epoch: Cell::new(0),
        }
    }

    /// 向量维度。
    pub fn dims(&self) -> usize {
        self.dims
    }

    /// 当前查询束宽。
    pub fn ef_search(&self) -> usize {
        self.ef_search
    }

    /// 调整查询束宽（召回与延迟的权衡，运行时可调）。
    pub fn set_ef_search(&mut self, ef: usize) {
        self.ef_search = ef.max(1);
    }

    fn vector(&self, node: u32) -> &[f32] {
        &self.data[node as usize * self.dims..(node as usize + 1) * self.dims]
    }

    /// 查询向量（已归一化）到节点的余弦距离。
    fn distance(&self, q: &[f32], node: u32) -> f32 {
        1.0 - dot(q, self.vector(node))
    }

    /// 两个已入库节点之间的余弦距离（用于连边剪枝）。
    fn dist_nodes(&self, a: u32, b: u32) -> f32 {
        1.0 - dot(self.vector(a), self.vector(b))
    }

    /// 按指数衰减分布采样新节点层级：P(level >= l) = exp(-l / level_mult)。
    fn random_level(&mut self) -> usize {
        let u = self.rng.next_f64_open();
        ((-u.ln() * self.level_mult) as usize).min(MAX_LEVEL_CAP)
    }

    /// 单层贪心下降：反复移动到更近的邻居，直到局部最优。
    fn greedy(&self, q: &[f32], mut cur: u32, level: usize) -> u32 {
        let mut d_cur = self.distance(q, cur);
        loop {
            let mut best = None;
            for &nb in &self.neighbors[cur as usize][level] {
                let d = self.distance(q, nb);
                if d < d_cur {
                    d_cur = d;
                    best = Some(nb);
                }
            }
            match best {
                Some(nb) => cur = nb,
                None => return cur,
            }
        }
    }

    /// 束搜索（论文 SEARCH-LAYER）：从入口点出发，在指定层保留 ef 个最近候选。
    /// 返回按距离升序的 (节点, 距离)。
    fn search_layer(&self, q: &[f32], eps: &[u32], ef: usize, level: usize) -> Vec<(u32, f32)> {
        // epoch 计数充当 visited 标记，避免每次 O(n) 清零
        let epoch = self.epoch.get().wrapping_add(1);
        if epoch == 0 {
            self.visited.borrow_mut().fill(0);
        }
        self.epoch.set(epoch);

        let mut candidates: BinaryHeap<Reverse<Scored>> = BinaryHeap::new(); // 距离最小堆
        let mut results: BinaryHeap<Scored> = BinaryHeap::new(); // 距离最大堆（容量 ef）
        {
            let mut visited = self.visited.borrow_mut();
            for &ep in eps {
                let d = self.distance(q, ep);
                visited[ep as usize] = epoch;
                candidates.push(Reverse(Scored(d, ep)));
                results.push(Scored(d, ep));
            }
            while let Some(Reverse(Scored(d_cand, cand))) = candidates.pop() {
                let worst = results.peek().map(|s| s.0).unwrap_or(f32::INFINITY);
                if d_cand > worst && results.len() >= ef {
                    break; // 最近候选都比当前结果集最差者远，搜索收敛
                }
                for &nb in &self.neighbors[cand as usize][level] {
                    if visited[nb as usize] == epoch {
                        continue;
                    }
                    visited[nb as usize] = epoch;
                    let d = self.distance(q, nb);
                    let worst = results.peek().map(|s| s.0).unwrap_or(f32::INFINITY);
                    if d < worst || results.len() < ef {
                        candidates.push(Reverse(Scored(d, nb)));
                        results.push(Scored(d, nb));
                        if results.len() > ef {
                            results.pop();
                        }
                    }
                }
            }
        }
        let mut out: Vec<(u32, f32)> = results.into_iter().map(|s| (s.1, s.0)).collect();
        out.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        out
    }

    /// 邻居选择（论文 SELECT-NEIGHBORS-HEURISTIC, Algorithm 4）。
    ///
    /// 相比简单 nearest-M，要求入选者比任何已选邻居都更靠近查询点，
    /// 保证邻居方向互异，图在近似等距/聚簇数据上连通性显著更好。
    /// - `extend`：先把候选的邻居并入候选池（构建连边时 true，论文 extendCandidates）
    /// - `keep_pruned`：用被多样性淘汰的候选回填剩余名额（构建时 true，剪枝时 false）
    fn select_neighbors_heuristic(
        &self,
        q: &[f32],
        mut candidates: Vec<(u32, f32)>,
        cap: usize,
        extend: bool,
        keep_pruned: bool,
        level: usize,
    ) -> Vec<u32> {
        // 候选已不足 cap 个时无需筛选
        if candidates.len() <= cap {
            return candidates.into_iter().map(|c| c.0).collect();
        }
        if extend {
            // epoch 标记去重后，把所有候选在该层的邻居并入候选池
            let epoch = self.epoch.get().wrapping_add(1);
            if epoch == 0 {
                self.visited.borrow_mut().fill(0);
            }
            self.epoch.set(epoch);
            {
                let mut visited = self.visited.borrow_mut();
                for &(c, _) in &candidates {
                    visited[c as usize] = epoch;
                }
                let base_len = candidates.len();
                for i in 0..base_len {
                    for &e in &self.neighbors[candidates[i].0 as usize][level] {
                        if visited[e as usize] == epoch {
                            continue;
                        }
                        visited[e as usize] = epoch;
                        candidates.push((e, self.distance(q, e)));
                    }
                }
            }
            candidates.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        }
        // 按到 q 距离升序扫描：c 必须比所有已选邻居更靠近 q，否则冗余淘汰
        let mut selected: Vec<u32> = Vec::with_capacity(cap);
        let mut pruned: Vec<u32> = Vec::new();
        'outer: for (c, d) in candidates {
            if selected.len() >= cap {
                break;
            }
            for &r in &selected {
                if self.dist_nodes(c, r) < d {
                    pruned.push(c);
                    continue 'outer;
                }
            }
            selected.push(c);
        }
        if keep_pruned {
            for c in pruned {
                if selected.len() >= cap {
                    break;
                }
                selected.push(c);
            }
        }
        selected
    }

    /// 在指定层建立 node <-> nb 双向连边；邻居表超容时按距离剪枝保留最近 cap 个。
    fn connect(&mut self, node: u32, nb: u32, level: usize, cap: usize) {
        self.neighbors[node as usize][level].push(nb);
        {
            let list = &mut self.neighbors[nb as usize][level];
            if !list.contains(&node) {
                list.push(node);
            }
        }
        if self.neighbors[nb as usize][level].len() > cap {
            // 超容时用同一启发式剪枝：以 nb 为“查询点”，保留方向互异的 cap 个
            let kept = {
                let q = self.vector(nb);
                let mut w: Vec<(u32, f32)> = self.neighbors[nb as usize][level]
                    .iter()
                    .map(|&x| (x, self.distance(q, x)))
                    .collect();
                w.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
                self.select_neighbors_heuristic(q, w, cap, false, false, level)
            };
            self.neighbors[nb as usize][level] = kept;
        }
    }
}

impl VectorIndex for HnswIndex {
    fn add(&mut self, id: u32, vector: &[f32]) {
        assert_eq!(vector.len(), self.dims, "向量维度不匹配");
        let node = self.len() as u32;
        let level = self.random_level();

        self.ids.push(id);
        self.data.extend_from_slice(&normalize(vector));
        self.levels.push(level as u8);
        self.neighbors.push(vec![Vec::new(); level + 1]);
        self.visited.borrow_mut().push(0);

        let Some(ep) = self.entry else {
            // 首个节点直接成为入口点
            self.entry = Some(node);
            self.max_level = level;
            return;
        };

        // 1+2. 先在不可变借用作用域内完成贪心下降与各层束搜索，
        // 生成连接计划，再统一连边（避开 data 借用与 connect 可变借用的冲突）
        let top = level.min(self.max_level);
        let plan: Vec<(usize, Vec<u32>)> = {
            let q = self.vector(node);
            // 从入口点自顶向下贪心下降到新节点层级之上
            let mut cur = ep;
            for lc in ((level + 1)..=self.max_level).rev() {
                cur = self.greedy(q, cur, lc);
            }
            // 在 min(level, max_level) 到 0 每层做束搜索选邻居
            let mut plan = Vec::with_capacity(top + 1);
            for lc in (0..=top).rev() {
                let w = self.search_layer(q, &[cur], self.ef_construction, lc);
                let cap = if lc == 0 { self.m_max0 } else { self.m };
                cur = w[0].0; // 最近候选作为下一层入口
                plan.push((
                    lc,
                    self.select_neighbors_heuristic(q, w, cap, true, true, lc),
                ));
            }
            plan
        };
        for (lc, selected) in plan {
            let cap = if lc == 0 { self.m_max0 } else { self.m };
            for nb in selected {
                self.connect(node, nb, lc, cap);
            }
        }

        // 3. 新节点更高，更新入口点
        if level > self.max_level {
            self.entry = Some(node);
            self.max_level = level;
        }
    }

    fn search(&self, query: &[f32], k: usize) -> Vec<(u32, f32)> {
        assert_eq!(query.len(), self.dims, "查询向量维度不匹配");
        let Some(ep) = self.entry else {
            return Vec::new();
        };
        if k == 0 {
            return Vec::new();
        }

        let q = normalize(query);
        // 高层贪心下降到 1 层
        let mut cur = ep;
        for lc in (1..=self.max_level).rev() {
            cur = self.greedy(&q, cur, lc);
        }
        // 0 层束搜索，ef 取 max(ef_search, k)
        let ef = self.ef_search.max(k);
        let w = self.search_layer(&q, &[cur], ef, 0);
        // w 已按距离升序 => 相似度降序
        w.into_iter()
            .take(k)
            .map(|(node, dist)| (self.ids[node as usize], 1.0 - dist))
            .collect()
    }

    fn len(&self) -> usize {
        self.ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BruteForceIndex;
    use std::collections::HashSet;

    /// 确定性随机向量生成器（xorshift -> [-1, 1)），保证测试可复现。
    struct VecGen {
        rng: XorShift64,
        dims: usize,
    }

    impl VecGen {
        fn new(seed: u64, dims: usize) -> Self {
            VecGen {
                rng: XorShift64(seed),
                dims,
            }
        }

        fn next_vec(&mut self) -> Vec<f32> {
            (0..self.dims)
                .map(|_| {
                    let v = (self.rng.next() >> 40) as u32; // 24 位
                    (v as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
                })
                .collect()
        }
    }

    fn recall(truth: &[(u32, f32)], approx: &[(u32, f32)]) -> f64 {
        let t: HashSet<u32> = truth.iter().map(|r| r.0).collect();
        let hit = approx.iter().filter(|r| t.contains(&r.0)).count();
        hit as f64 / truth.len().max(1) as f64
    }

    #[test]
    fn empty_index_returns_empty() {
        let idx = HnswIndex::new(4);
        assert!(idx.is_empty());
        assert!(idx.search(&[0.0; 4], 5).is_empty());
    }

    #[test]
    fn single_node_is_found() {
        let mut idx = HnswIndex::new(2);
        idx.add(7, &[1.0, 0.0]);
        assert_eq!(idx.len(), 1);
        let out = idx.search(&[1.0, 0.0], 1);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 7);
        assert!((out[0].1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn k_greater_than_n_returns_all() {
        let mut idx = HnswIndex::new(2);
        idx.add(1, &[1.0, 0.0]);
        idx.add(2, &[0.9, 0.1]);
        idx.add(3, &[0.0, 1.0]);
        let out = idx.search(&[1.0, 0.0], 10);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].0, 1);
        assert_eq!(out[1].0, 2);
        assert_eq!(out[2].0, 3);
    }

    #[test]
    fn k_zero_returns_empty() {
        let mut idx = HnswIndex::new(2);
        idx.add(1, &[1.0, 0.0]);
        assert!(idx.search(&[1.0, 0.0], 0).is_empty());
    }

    #[test]
    fn zero_vector_is_safe() {
        // 零向量与 cosine() 行为一致：相似度视为 0，不产生 NaN
        let mut idx = HnswIndex::new(2);
        idx.add(1, &[0.0, 0.0]);
        idx.add(2, &[1.0, 0.0]);
        let out = idx.search(&[1.0, 0.0], 2);
        assert_eq!(out[0].0, 2);
        assert!(out.iter().all(|r| r.1.is_finite()));
    }

    #[test]
    #[should_panic(expected = "向量维度不匹配")]
    fn add_rejects_wrong_dims() {
        let mut idx = HnswIndex::new(3);
        idx.add(1, &[1.0, 0.0]);
    }

    #[test]
    fn same_seed_is_deterministic() {
        let build = || {
            let mut gen = VecGen::new(11, 16);
            let mut idx = HnswIndex::new(16);
            for i in 0..200u32 {
                idx.add(i, &gen.next_vec());
            }
            idx
        };
        let a = build();
        let b = build();
        let mut gen = VecGen::new(99, 16);
        for _ in 0..10 {
            let q = gen.next_vec();
            assert_eq!(a.search(&q, 10), b.search(&q, 10));
        }
    }

    #[test]
    fn recall_at_10_vs_brute_force() {
        let (n, dims, k, nq) = (1000usize, 128usize, 10usize, 50usize);
        let mut gen = VecGen::new(42, dims);
        let vectors: Vec<Vec<f32>> = (0..n).map(|_| gen.next_vec()).collect();

        let mut hnsw = HnswIndex::new(dims);
        let mut brute = BruteForceIndex::new(dims);
        for (i, v) in vectors.iter().enumerate() {
            hnsw.add(i as u32, v);
            brute.add(i as u32, v);
        }
        assert_eq!(hnsw.len(), n);

        let mut total = 0.0;
        for _ in 0..nq {
            let q = gen.next_vec();
            total += recall(&brute.search(&q, k), &hnsw.search(&q, k));
        }
        let avg = total / nq as f64;
        assert!(avg >= 0.9, "recall@10 = {avg:.4} < 0.9");
    }

    /// 诊断：固定图，扫描 ef_search 观察 recall@100 变化，用于区分“图质量问题”与“ef 不足”。
    #[test]
    #[ignore = "诊断测试，手动运行"]
    fn diag_ef_sweep() {
        let (n, dims, k, nq) = (10_000usize, 128usize, 100usize, 20usize);
        let mut gen = VecGen::new(21, dims);
        let vectors: Vec<Vec<f32>> = (0..n).map(|_| gen.next_vec()).collect();
        let queries: Vec<Vec<f32>> = (0..nq).map(|_| gen.next_vec()).collect();
        let mut hnsw = HnswIndex::new(dims);
        let mut brute = BruteForceIndex::new(dims);
        for (i, v) in vectors.iter().enumerate() {
            hnsw.add(i as u32, v);
            brute.add(i as u32, v);
        }
        for ef in [100usize, 500, 2000] {
            hnsw.set_ef_search(ef);
            let (mut total, mut top1) = (0.0, 0usize);
            for q in &queries {
                let truth = brute.search(q, k);
                let approx = hnsw.search(q, k);
                total += recall(&truth, &approx);
                if approx.first().map(|r| r.0) == truth.first().map(|r| r.0) {
                    top1 += 1;
                }
            }
            eprintln!(
                "ef={ef}: recall@100={:.4}, top1_hit={top1}/{nq}",
                total / nq as f64
            );
        }
    }

    /// 基准：10 万个 128 维向量，报告构建时间、Top-100 查询 P50/P99、recall@100。
    /// 手动运行：cargo test --release -p nylon-vector bench_100k -- --ignored --nocapture
    #[test]
    #[ignore = "基准测试，手动运行"]
    fn bench_100k() {
        use std::time::Instant;

        let (n, dims, k) = (100_000usize, 128usize, 100usize);
        let nq = 200; // 延迟统计的查询数
        let truth_q = 50; // 召回统计的查询子集（暴力基准较慢）

        let mut gen = VecGen::new(7, dims);
        let vectors: Vec<Vec<f32>> = (0..n).map(|_| gen.next_vec()).collect();
        let queries: Vec<Vec<f32>> = (0..nq).map(|_| gen.next_vec()).collect();

        // 构建
        let t0 = Instant::now();
        let mut hnsw = HnswIndex::new(dims);
        for (i, v) in vectors.iter().enumerate() {
            hnsw.add(i as u32, v);
        }
        let build = t0.elapsed();

        // 真值（子集，暴力基准较慢，只算一次）
        let mut brute = BruteForceIndex::new(dims);
        for (i, v) in vectors.iter().enumerate() {
            brute.add(i as u32, v);
        }
        let truths: Vec<Vec<(u32, f32)>> = (0..truth_q)
            .map(|qi| brute.search(&queries[qi], k))
            .collect();

        // 两个工作点：默认 ef（k=100 时实际 ef=100）与高召回点 ef=500
        eprintln!("=== HNSW bench: n={n}, dims={dims}, M=16, ef_construction=200 ===");
        eprintln!(
            "build: {build:.2?} ({:.0} vec/s)",
            n as f64 / build.as_secs_f64()
        );
        for ef in [0usize, 500] {
            if ef > 0 {
                hnsw.set_ef_search(ef);
            }
            let label = if ef == 0 {
                format!("ef=100 (默认 max(ef_search={} , k))", hnsw.ef_search())
            } else {
                format!("ef={ef}")
            };
            let mut lat: Vec<f64> = Vec::with_capacity(nq);
            let mut approx: Vec<Vec<(u32, f32)>> = Vec::with_capacity(nq);
            for q in &queries {
                let t = Instant::now();
                approx.push(hnsw.search(q, k));
                lat.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            lat.sort_by(|a, b| a.total_cmp(b));
            let p50 = lat[nq / 2];
            let p99 = lat[(nq as f64 * 0.99) as usize - 1];
            let mut total = 0.0;
            for qi in 0..truth_q {
                total += recall(&truths[qi], &approx[qi]);
            }
            eprintln!(
                "{label}: top-{k} p50={p50:.3} ms, p99={p99:.3} ms, recall@{k}={:.4} (subset {truth_q})",
                total / truth_q as f64
            );
        }
    }
}
