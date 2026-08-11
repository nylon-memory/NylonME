//! 尼龙记忆引擎 — Phase 1 脚手架自检入口。
//! gRPC 服务化（tonic）为路线图 Week 3-4 任务，需先定义 proto codegen（见 proto/）。

use nylon_core::{Filaments, MemoryNode, Tension};
use nylon_graph::{ContextSpectrum, FilamentFilter, MemoryGraph, DEFAULT_BUDGET};
mod service;

use nylon_storage::PersistentGraph;
use nylon_vector::{BruteForceIndex, VectorIndex};

fn demo_node(id: u64, fact: &str, relations: &[&str], mentions: u32) -> MemoryNode {
    MemoryNode {
        id,
        owner_id: "alice".into(),
        filaments: Filaments {
            fact: fact.into(),
            emotion_valence: 0.6,
            emotion_intensity: 0.5,
            created_at: 0,
            decay_rate: 0.01,
            relations: relations.iter().map(|s| s.to_string()).collect(),
            confidence: 0.9,
            mentions_7d: mentions,
        },
        tension: Tension { baseline: 0.8, last_updated: 0 },
        embedding: vec![],
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("serve") {
        let addr = args.get(2).cloned().unwrap_or_else(|| "127.0.0.1:50051".into());
        let data = std::env::var("NYLON_DATA_DIR").unwrap_or_else(|_| "./nylon-data".into());
        let dims: usize = std::env::var("NYLON_EMBED_DIMS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(service::DEFAULT_EMBED_DIMS);
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async move {
            let store = PersistentGraph::open(&data).expect("open persistent store");
                let llm = nylon_llm::llm_from_env();
    let embedder = nylon_embed::embedder_from_env(dims);
            if embedder.is_some() {
                println!("嵌入通道已启用 (NYLON_EMBED_URL)");
            }
            let svc = service::EngineService::new(store, dims, embedder, llm);
            let sock = addr.parse().expect("invalid listen addr");
            println!("nylon-engine gRPC listening on {addr} (data={data}, dims={dims})");
            tonic::transport::Server::builder()
                .add_service(service::pb::memory_engine_server::MemoryEngineServer::new(svc))
                .serve(sock)
                .await
                .expect("gRPC server");
        });
        return;
    }

    println!("nylon-engine v0.1.0 (Phase 1 scaffold)");

    // 构建一个迷你记忆网：机票 -> 出差偏好 -> 酒店偏好 / 上次出差时间
    let mut g = MemoryGraph::new();
    let flight = g.add_node(demo_node(1, "用户问机票", &["出差"], 3));
    let trip = g.add_node(demo_node(2, "出差偏好：靠窗座位", &["出差"], 5));
    let hotel = g.add_node(demo_node(3, "酒店偏好：近地铁", &["出差", "住宿"], 2));
    let last = g.add_node(demo_node(4, "上次出差：2026-06 上海", &["出差"], 0));
    let hobby = g.add_node(demo_node(5, "喜欢科幻小说", &["阅读"], 1));
    g.add_edge(flight, trip, 0.9);
    g.add_edge(trip, hotel, 0.8);
    g.add_edge(trip, last, 0.7);
    g.add_edge(hobby, last, 0.1);

    let ctx = ContextSpectrum { task: Some("出差".into()), emotion_valence: None, max_hops: None };
    let activated = g.resonate(&[(flight, 1.0)], &ctx, 0, DEFAULT_BUDGET);

    println!("\n情境共振（种子=机票, 任务=出差）:");
    for (id, score) in &activated {
        let fact = g.get_node(*id).map(|n| n.filaments.fact.as_str()).unwrap_or("?");
        println!("  node {id}: resonance={score:.3}  {fact}");
    }
    assert!(activated.len() >= 3, "应激活出差记忆簇");

    // 向量检索冒烟
    let mut idx = BruteForceIndex::new(3);
    idx.add(1, &[1.0, 0.0, 0.0]);
    idx.add(2, &[0.8, 0.2, 0.0]);
    idx.add(3, &[0.0, 0.0, 1.0]);
    let top = idx.search(&[1.0, 0.1, 0.0], 1);
    println!("\n向量 Top-1: node {} sim={:.3}", top[0].0, top[0].1);

    // .nylon 二进制编解码 roundtrip
    let snapshot: Vec<MemoryNode> =
        (0..5u32).filter_map(|i| g.get_node(i).cloned()).collect();
    let bytes = nylon_core::encode_nodes(&snapshot);
    let restored = nylon_core::decode_nodes(&bytes).expect("decode .nylon");
    assert_eq!(restored, snapshot, ".nylon roundtrip 应完全保真");
    println!("\n.nylon 编解码: {} 节点 -> {} 字节, roundtrip 保真", snapshot.len(), bytes.len());

    // 六丝组合检索演示
    let hits = g.find_by_filaments(&FilamentFilter {
        relations_any: Some(vec!["出差".into()]),
        min_confidence: Some(0.5),
        ..Default::default()
    });
    println!("六丝检索（出差 + 置信>=0.5）: 命中 {} 个节点 {:?}", hits.len(), hits);
    assert_eq!(hits.len(), 4);

    // 持久化演示：WAL + 快照 + 崩溃恢复
    let dir = std::env::temp_dir().join("nylon-engine-demo");
    let _ = std::fs::remove_dir_all(&dir);
    {
        let mut pg = PersistentGraph::open(&dir).expect("open store");
        let (a, _ticket_a) = pg.add_node(demo_node(101, "持久化节点 A", &["演示"], 1)).unwrap();
        let (b, _ticket_b) = pg.add_node(demo_node(102, "持久化节点 B", &["演示"], 1)).unwrap();
        pg.add_edge(a, b, 0.9).unwrap();
        pg.checkpoint().unwrap();
        // checkpoint 后再写一条（只在 WAL 里），模拟未落快照的增量
        pg.add_node(demo_node(103, "持久化节点 C（仅 WAL）", &["演示"], 0)).unwrap();
    } // drop = 模拟进程退出
    let pg = PersistentGraph::open(&dir).expect("reopen store");
    assert_eq!(pg.graph().node_count(), 3, "快照 + WAL 重放应恢复全部 3 个节点");
    let c = pg.graph().get_node(2).expect("节点 C 应由 WAL 重放恢复");
    println!(
        "\n持久化: checkpoint + WAL 重放恢复 {} 节点, 仅 WAL 节点: {}",
        pg.graph().node_count(),
        c.filaments.fact
    );
    let _ = std::fs::remove_dir_all(&dir);

    println!("\n自检通过：图遍历 + 张力 + 向量检索 + .nylon 编解码 + 六丝检索 + 持久化基线可用。");
}