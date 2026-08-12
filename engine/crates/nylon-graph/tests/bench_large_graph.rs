//! 大规模合成图基准：100 万节点 / 1000 万边（Phase 1 Week 3-4）。
//!
//! 日常 `cargo test` 自动跳过；手动触发：
//!   cargo test -p nylon-graph --test bench_large_graph -- --ignored --nocapture
//! 建议使用 release 模式（debug 下 10M 边构建会慢一个数量级）：
//!   cargo test -p nylon-graph --release --test bench_large_graph -- --ignored --nocapture

use nylon_core::{Filaments, MemoryNode, Tension};
use nylon_graph::{ContextSpectrum, MemoryGraph};
use std::time::Instant;

const NUM_NODES: usize = 1_000_000;
const NUM_EDGES: usize = 10_000_000;
const NUM_SEEDS: usize = 100;
/// 大于生产默认预算 64，避免预算截断掩盖真实遍历成本。
const BENCH_BUDGET: usize = 4096;

/// 确定性伪随机数（LCG，Numerical Recipes 常数），零外部依赖。
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11 // 取高位，低位的线性同余周期性差
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn synth_node(i: usize, rng: &mut Lcg) -> MemoryNode {
    MemoryNode {
        id: i as u64,
        owner_id: "bench".into(),
        filaments: Filaments {
            fact: format!("合成事实 {i}"),
            emotion_valence: 0.5,
            emotion_intensity: 0.5,
            created_at: 1_700_000_000 + (rng.below(86_400 * 365)) as i64,
            decay_rate: 0.01,
            relations: vec![],
            confidence: 0.9,
            mentions_7d: 0,
        },
        tension: Tension {
            baseline: 0.8,
            last_updated: 0,
        },
        embedding: vec![],
    }
}

fn percentile(sorted: &[u128], p: f64) -> u128 {
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

#[test]
#[ignore = "百万级基准：仅在显式 --ignored 时运行"]
fn bench_1m_nodes_10m_edges() {
    let mut rng = Lcg(0x5EED_5EED_5EED_5EED);
    let mut g = MemoryGraph::new();

    let t = Instant::now();
    for i in 0..NUM_NODES {
        g.add_node(synth_node(i, &mut rng));
    }
    println!("节点写入: {:?} ({} 节点)", t.elapsed(), NUM_NODES);

    let t = Instant::now();
    for _ in 0..NUM_EDGES {
        let from = rng.below(NUM_NODES as u64) as u32;
        let to = rng.below(NUM_NODES as u64) as u32;
        let w = 0.1 + 0.9 * (rng.below(1000)) as f32 / 1000.0;
        g.add_edge(from, to, w);
    }
    println!("Delta 边写入: {:?} ({} 边)", t.elapsed(), NUM_EDGES);
    assert!(g.needs_compaction(), "10M 边应远超 compaction 阈值");

    // CSR 构建：compaction 把 Delta 合并进主图
    let t = Instant::now();
    g.compact();
    println!("CSR 构建 (compact): {:?}", t.elapsed());
    assert!(!g.needs_compaction(), "compact 后阈值应复位");

    // 3-hop resonate 延迟：100 个随机种子，取 P50/P99
    let ctx = ContextSpectrum::default();
    let mut lat_us: Vec<u128> = Vec::with_capacity(NUM_SEEDS);
    let mut total_activated = 0usize;
    for _ in 0..NUM_SEEDS {
        let seed = rng.below(NUM_NODES as u64) as u32;
        let t = Instant::now();
        let out = g.resonate(&[(seed, 1.0)], &ctx, 0, BENCH_BUDGET);
        lat_us.push(t.elapsed().as_micros());
        assert!(!out.is_empty(), "种子自身应总在结果中");
        total_activated += out.len();
    }
    lat_us.sort_unstable();
    let p50 = percentile(&lat_us, 0.50);
    let p99 = percentile(&lat_us, 0.99);
    println!(
        "3-hop resonate (budget {BENCH_BUDGET}): P50 = {p50} us, P99 = {p99} us, 平均激活 {} 节点",
        total_activated / NUM_SEEDS
    );
    assert!(p99 >= p50);

    // 内存估算：std 无进程内存查询接口，按结构尺寸估算（未计 HashMap/String 堆分配与 Vec 冗余容量）
    let node_bytes = NUM_NODES * std::mem::size_of::<MemoryNode>();
    let edge_bytes = NUM_EDGES * 12; // target u32 + weight f32 + offsets 摊销 ≈ 12 B/边
    let offset_bytes = (NUM_NODES + 1) * 4;
    println!(
        "内存估算: 节点 {} MB + 边 {} MB + offsets {} MB = 约 {} MB（未计 HashMap/String 堆分配）",
        node_bytes / 1_048_576,
        edge_bytes / 1_048_576,
        offset_bytes / 1_048_576,
        (node_bytes + edge_bytes + offset_bytes) / 1_048_576
    );
}
