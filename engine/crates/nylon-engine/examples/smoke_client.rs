//! 远程冒烟客户端：对 NYLON_SERVER（默认 http://127.0.0.1:50051）跑一遍四个 RPC。
#[path = "../src/service.rs"]
mod service;

use service::pb::memory_engine_client::MemoryEngineClient;
use service::pb::*;

#[tokio::main]
async fn main() {
    let addr = std::env::var("NYLON_SERVER").unwrap_or_else(|_| "http://127.0.0.1:50051".into());
    let mut client = MemoryEngineClient::connect(addr.clone())
        .await
        .expect("connect");
    println!("connected: {addr}");

    let ctx = || {
        Some(ContextSpectrum {
            task: Some("codex-dogfood".into()),
            emotion_valence: None,
            device: None,
            max_hops: None,
        })
    };
    let weave = |fact: &str| WeaveRequest {
        tenant_id: "codex".into(),
        owner_id: "nylonme-dev".into(),
        raw_event: fact.into(),
        context: ctx(),
    };

    let a = client
        .weave(weave("NylonME 引擎已部署到 192.168.1.5，gRPC 端口 50051"))
        .await
        .unwrap()
        .into_inner();
    let b = client
        .weave(weave("嵌入模型是本机 ollama 的 bge-m3，1024 维"))
        .await
        .unwrap()
        .into_inner();
    client
        .weave(weave("理解层 LLM 用 deepseek-v4-flash，需关闭 thinking"))
        .await
        .unwrap();
    println!(
        "weave ok: a={} b={} linked(b->a)={}",
        a.node_id,
        b.node_id,
        b.linked_nodes.contains(&a.node_id)
    );

    let got = client
        .get_node(GetNodeRequest {
            tenant_id: "codex".into(),
            node_id: a.node_id,
        })
        .await
        .unwrap()
        .into_inner();
    println!(
        "get_node ok: fact={:?} tension={:.3}",
        got.filaments.unwrap().fact,
        got.current_tension
    );

    let res = client
        .resonate(ResonateRequest {
            tenant_id: "codex".into(),
            owner_id: "nylonme-dev".into(),
            query: "部署在哪个机器上".into(),
            context: ctx(),
            budget: 8,
        })
        .await
        .unwrap()
        .into_inner();
    println!("resonate ok: {} activated", res.activated.len());
    for n in res.activated.iter().take(3) {
        println!("  node {} score={:.3}", n.node_id, n.resonance);
    }

    println!("SMOKE_OK");
}
