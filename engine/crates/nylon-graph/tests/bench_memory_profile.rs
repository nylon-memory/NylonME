//! 常驻内存 profiling：100 万节点 + 1000 万边（Phase 2，回填设计包“内存占用 < 300MB”行）。
//!
//! 用计数全局分配器测堆内存增量：空图 → 填充节点 → 加边（Delta）→ compact 后稳态。
//! 节点用接近真实的丝绿文本（事实丝 ~80 字节中文 + 2 条关系丝），不带嵌入向量。
//!
//! 手动运行（release）：
//!   cargo test -p nylon-graph --release --test bench_memory_profile -- --ignored --nocapture

use nylon_core::{Filaments, MemoryNode, Tension};
use nylon_graph::MemoryGraph;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

// ---------- 计数分配器 ----------

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            ALLOCATED.fetch_add(l.size(), Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        ALLOCATED.fetch_sub(l.size(), Ordering::Relaxed);
    }
}

#[global_allocator]
static A: CountingAlloc = CountingAlloc;

fn allocated_mb() -> f64 {
    ALLOCATED.load(Ordering::Relaxed) as f64 / 1024.0 / 1024.0
}

// ---------- 合成数据 ----------

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// 接近真实的记忆节点：事实丝为中文句子，带两条关系丝。
fn realistic_node(i: usize) -> MemoryNode {
    MemoryNode {
        id: i as u64,
        owner_id: format!("user-{:05}", i % 50_000),
        filaments: Filaments {
            fact: format!(
                "用户 {} 提到：出差偏好靠窗座位，上次坐的是 CA{:04}，报销走财务共享中心，下次提前两周订票",
                i % 50_000,
                1000 + (i % 9000)
            ),
            emotion_valence: 0.5,
            emotion_intensity: 0.5,
            created_at: 1_700_000_000 + (i % 31_536_000) as i64,
            decay_rate: 0.01,
            relations: vec![format!("task-{:03}", i % 500), "出差".into()],
            confidence: 0.9,
            mentions_7d: (i % 20) as u32,
        },
        tension: Tension { baseline: 0.8, last_updated: 0 },
        embedding: Vec::new(), // 嵌入向量属于向量索引侧，本行不计
    }
}

/// 极简节点（与 bench_large_graph 同形状）：短事实丝、无关系丝，衡量纯结构开销。
fn minimal_node(i: usize) -> MemoryNode {
    MemoryNode {
        id: i as u64,
        owner_id: "bench".into(),
        filaments: Filaments {
            fact: format!("合成事实 {i}"),
            emotion_valence: 0.5,
            emotion_intensity: 0.5,
            created_at: 1_700_000_000,
            decay_rate: 0.01,
            relations: vec![],
            confidence: 0.9,
            mentions_7d: 0,
        },
        tension: Tension {
            baseline: 0.8,
            last_updated: 0,
        },
        embedding: Vec::new(),
    }
}

const NUM_NODES: usize = 1_000_000;
const NUM_EDGES: usize = 10_000_000;

#[test]
#[ignore = "百万级内存 profiling，手动运行"]
fn profile_1m_nodes_10m_edges() {
    let t0 = Instant::now();
    let base = allocated_mb();
    let mut g = MemoryGraph::new();
    let after_new = allocated_mb();

    let minimal = std::env::var_os("NYLON_PROFILE_MINIMAL").is_some();
    for i in 0..NUM_NODES {
        g.add_node(if minimal {
            minimal_node(i)
        } else {
            realistic_node(i)
        });
    }
    println!(
        "节点形状: {}",
        if minimal {
            "极简（NYLON_PROFILE_MINIMAL）"
        } else {
            "真实文本"
        }
    );
    let after_nodes = allocated_mb();

    let mut rng = Lcg(0x5EED_5EED_5EED_5EED);
    let n = NUM_NODES as u64;
    for _ in 0..NUM_EDGES {
        let from = rng.below(n) as u32;
        let to = rng.below(n) as u32;
        if from != to {
            g.add_edge(from, to, 0.5);
        }
    }
    let after_delta = allocated_mb();

    g.compact();
    let after_compact = allocated_mb();
    let elapsed = t0.elapsed();

    println!();
    println!("=== 常驻内存 profiling：100 万节点 + 1000 万边 ===");
    println!("空图基线:        {base:.1} MB");
    println!("空图结构:        {after_new:.1} MB");
    println!(
        "节点填充后:      {after_nodes:.1} MB  (节点净增 {:.1} MB, {:.0} B/节点)",
        after_nodes - after_new,
        (after_nodes - after_new) * 1024.0 * 1024.0 / NUM_NODES as f64
    );
    println!(
        "+Delta 边后:       {after_delta:.1} MB  (边净增 {:.1} MB)",
        after_delta - after_nodes
    );
    println!("compact 后稳态:   {after_compact:.1} MB  (目标 < 300 MB)");
    println!("构建耗时:        {:.1}s", elapsed.as_secs_f64());
    let verdict = if after_compact < 300.0 {
        "达标"
    } else {
        "超标"
    };
    println!("结论: 100 万节点 + 1000 万边稳态常驻 {after_compact:.0} MB，{verdict}。");
}
