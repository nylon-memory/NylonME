//! 写入吞吐基准：并发 worker 持续 Weave，测端到端 TPS。
//! 手动运行：cargo test --release -p nylon-engine --test bench_weave_tps -- --ignored --nocapture
//!
//! 成功指标（路线图 2.4）：记忆写入 > 10K TPS。
//! 注意当前每次 Weave 都有 WAL fsync（单条刷盘），group commit 是 Phase 2 优化项。

#[path = "../src/service.rs"]
mod service;

use nylon_storage::PersistentGraph;
use service::pb::memory_engine_client::MemoryEngineClient;
use service::pb::memory_engine_server::MemoryEngineServer;
use service::pb::*;
use service::EngineService;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

const TOTAL: usize = 10_000;
const WORKERS: usize = 32;

#[tokio::test]
#[ignore = "基准测试，手动运行"]
async fn weave_tps() {
    let dir = tempfile::tempdir().unwrap();
    let store = PersistentGraph::open(dir.path()).unwrap();
    let svc = EngineService::new(store, service::DEFAULT_EMBED_DIMS, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(MemoryEngineServer::new(svc))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    let counter = Arc::new(AtomicUsize::new(0));
    let start = Instant::now();
    let mut handles = Vec::new();
    for _ in 0..WORKERS {
        let addr = addr.clone();
        let counter = counter.clone();
        handles.push(tokio::spawn(async move {
            let mut client = MemoryEngineClient::connect(addr).await.unwrap();
            loop {
                let i = counter.fetch_add(1, Ordering::Relaxed);
                if i >= TOTAL {
                    break;
                }
                client
                    .weave(WeaveRequest {
                        tenant_id: "bench".into(),
                        owner_id: format!("user-{}", i % 64),
                        raw_event: format!("bench event number {i} with some realistic text"),
                        context: Some(ContextSpectrum {
                            task: Some("bench".into()),
                            emotion_valence: None,
                            device: None,
                        }),
                    })
                    .await
                    .unwrap();
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let elapsed = start.elapsed();
    let tps = TOTAL as f64 / elapsed.as_secs_f64();
    println!();
    println!("=== Weave 写入吞吐基准 ===");
    println!("总量 {TOTAL} ops, 并发 {WORKERS}, 耗时 {:.2}s", elapsed.as_secs_f64());
    println!("端到端 TPS: {tps:.0} (目标 > 10000)");
    println!("注: 每次写入含 WAL fsync 单条刷盘; group commit 属 Phase 2 优化");
}
