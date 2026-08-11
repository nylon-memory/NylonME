//! gRPC 端到端冒烟：内存随机端口起服务，真实 client 走一遍四个 RPC。

#[path = "../src/service.rs"]
mod service;

use nylon_storage::PersistentGraph;
use service::pb::memory_engine_client::MemoryEngineClient;
use service::pb::memory_engine_server::MemoryEngineServer;
use service::pb::*;
use service::EngineService;

async fn start() -> (String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = PersistentGraph::open(dir.path()).unwrap();
    let svc = EngineService::new(store, service::DEFAULT_EMBED_DIMS, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(MemoryEngineServer::new(svc))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    (addr, dir)
}

fn weave_req(fact: &str) -> WeaveRequest {
    WeaveRequest {
        tenant_id: "t1".into(),
        owner_id: "alice".into(),
        raw_event: fact.into(),
        context: Some(ContextSpectrum { task: Some("出差".into()), emotion_valence: None, device: None, max_hops: None }),
    }
}

#[tokio::test]
async fn grpc_roundtrip() {
    let (addr, _dir) = start().await;
    let mut client = MemoryEngineClient::connect(addr).await.unwrap();

    // Weave x3：后两条与第一条同任务，应自动建边
    let a = client.weave(weave_req("用户订了去上海的机票")).await.unwrap().into_inner();
    let b = client.weave(weave_req("出差偏好是靠窗座位")).await.unwrap().into_inner();
    client.weave(weave_req("酒店要近地铁")).await.unwrap();
    assert!(b.linked_nodes.contains(&a.node_id), "同任务节点应自动建边");

    // GetNode
    let got = client
        .get_node(GetNodeRequest { tenant_id: "t1".into(), node_id: a.node_id })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(got.filaments.unwrap().fact, "用户订了去上海的机票");
    assert!(got.current_tension > 0.0);

    // Resonate：query 命中事实丝
    let res = client
        .resonate(ResonateRequest {
            tenant_id: "t1".into(),
            owner_id: "alice".into(),
            query: "机票".into(),
            context: Some(ContextSpectrum { task: Some("出差".into()), emotion_valence: None, device: None, max_hops: None }),
            budget: 16,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(!res.activated.is_empty(), "共振应至少返回种子节点");
    assert_eq!(res.activated[0].node_id, a.node_id, "种子应排第一（强度最高）");

    // Search：空索引返回空（Weave 暂不产生 embedding）
    let resp = client
        .search(SearchRequest {
            tenant_id: "t1".into(),
            owner_id: "alice".into(),
            query_embedding: vec![0u8; 16],
            top_k: 5,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(resp.neighbors.is_empty());

    // 参数校验
    let err = client
        .get_node(GetNodeRequest { tenant_id: "t1".into(), node_id: 999 })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

/// Phase 2 语义通道：Stub 嵌入器下，Search 应有结果，无词面重叠的 query 也能靠向量种子共振。
#[tokio::test]
async fn grpc_with_stub_embedder() {
    use nylon_embed::{Embedder, StubEmbedder};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let store = PersistentGraph::open(dir.path()).unwrap();
    let svc = EngineService::new(store, 32, Some(Arc::new(StubEmbedder::new(32))), None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(MemoryEngineServer::new(svc))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    let mut client = MemoryEngineClient::connect(addr).await.unwrap();

    client.weave(WeaveRequest {
        tenant_id: "t1".into(), owner_id: "bob".into(),
        raw_event: "我去上海出差订了机票".into(), context: None,
    }).await.unwrap();
    client.weave(WeaveRequest {
        tenant_id: "t1".into(), owner_id: "bob".into(),
        raw_event: "周末在家读完了三体".into(), context: None,
    }).await.unwrap();

    // Search：用 Stub 嵌入"出差 航班 机票"查询，应命中机票节点
    let q = StubEmbedder::new(32).embed(&["出差 航班 机票".to_string()]).await.unwrap().pop().unwrap();
    let bytes: Vec<u8> = q.iter().flat_map(|f| f.to_le_bytes()).collect();
    let resp = client.search(SearchRequest {
        tenant_id: "t1".into(), owner_id: "bob".into(),
        query_embedding: bytes, top_k: 5,
    }).await.unwrap().into_inner();
    assert!(!resp.neighbors.is_empty(), "语义通道下 Search 应有结果");
    assert_eq!(resp.neighbors[0].node_id, 0, "机票节点应排第一: {:?}", resp.neighbors);

    // Resonate：query 用词面重叠为零的表达，向量种子应兜住
    let res = client.resonate(ResonateRequest {
        tenant_id: "t1".into(), owner_id: "bob".into(),
        query: "航班".into(), context: None, budget: 8,
    }).await.unwrap().into_inner();
    assert!(!res.activated.is_empty(), "向量种子应激活节点: {:?}", res.activated);
}
