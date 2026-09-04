//! gRPC 端到端冒烟：内存随机端口起服务，真实 client 走一遍四个 RPC。

#[path = "../src/audit.rs"]
mod audit;

#[path = "../src/auth.rs"]
mod auth;

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

/// 带 API key 鉴权的服务（L2.2）：keys JSON 注入拦截器。
async fn start_with_auth(keys_json: &str) -> (String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = PersistentGraph::open(dir.path()).unwrap();
    let keys = Some(std::sync::Arc::new(
        auth::ApiKeys::parse(keys_json).unwrap(),
    ));
    let svc =
        EngineService::new(store, service::DEFAULT_EMBED_DIMS, None, None).with_auth(keys.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(MemoryEngineServer::with_interceptor(svc, move |req| {
                auth::grpc_intercept(&keys, req)
            }))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    (addr, dir)
}

/// 给请求附加 x-api-key metadata。
fn with_key<T>(mut req: tonic::Request<T>, key: &str) -> tonic::Request<T> {
    req.metadata_mut().insert("x-api-key", key.parse().unwrap());
    req
}

/// L2.2 端到端：真实 tonic server 下，拦截器 → handler 的 grant 传递与档位/租户判定。
#[tokio::test]
async fn grpc_auth_end_to_end() {
    let (addr, _dir) = start_with_auth(
        r#"[
            {"key": "k-read", "tenant": "t1", "scope": "read"},
            {"key": "k-write", "tenant": "t1", "scope": "write"}
        ]"#,
    )
    .await;
    let mut client = MemoryEngineClient::connect(addr).await.unwrap();

    // 无 key：Unauthenticated
    let err = client.weave(weave_req("无 key 写入")).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unauthenticated);

    // 只读 key 写入：PermissionDenied（handler 档位检查）
    let err = client
        .weave(with_key(
            tonic::Request::new(weave_req("只读 key 写入")),
            "k-read",
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);

    // 写 key + 正确租户：成功
    let resp = client
        .weave(with_key(
            tonic::Request::new(weave_req("授权写入成功")),
            "k-write",
        ))
        .await
        .unwrap()
        .into_inner();
    let node_id = resp.node_id;

    // 写 key 但请求体是别的租户：PermissionDenied（handler 租户比对）
    let mut cross = weave_req("跨租户写入");
    cross.tenant_id = "t2".into();
    let err = client
        .weave(with_key(tonic::Request::new(cross), "k-write"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);

    // 只读 key 读同租户：成功
    let got = client
        .get_node(with_key(
            tonic::Request::new(GetNodeRequest {
                tenant_id: "t1".into(),
                node_id,
            }),
            "k-read",
        ))
        .await;
    assert!(got.is_ok(), "只读 key 应能读同租户节点: {got:?}");

    // 只读 key 读其他租户：PermissionDenied
    let err = client
        .get_node(with_key(
            tonic::Request::new(GetNodeRequest {
                tenant_id: "t2".into(),
                node_id,
            }),
            "k-read",
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

fn weave_req(fact: &str) -> WeaveRequest {
    WeaveRequest {
        tenant_id: "t1".into(),
        owner_id: "alice".into(),
        raw_event: fact.into(),
        context: Some(ContextSpectrum {
            task: Some("出差".into()),
            emotion_valence: None,
            device: None,
            max_hops: None,
        }),
    }
}

#[tokio::test]
async fn grpc_roundtrip() {
    let (addr, _dir) = start().await;
    let mut client = MemoryEngineClient::connect(addr).await.unwrap();

    // Weave x3：后两条与第一条同任务，应自动建边
    let a = client
        .weave(weave_req("用户订了去上海的机票"))
        .await
        .unwrap()
        .into_inner();
    let b = client
        .weave(weave_req("出差偏好是靠窗座位"))
        .await
        .unwrap()
        .into_inner();
    client.weave(weave_req("酒店要近地铁")).await.unwrap();
    assert!(b.linked_nodes.contains(&a.node_id), "同任务节点应自动建边");

    // GetNode
    let got = client
        .get_node(GetNodeRequest {
            tenant_id: "t1".into(),
            node_id: a.node_id,
        })
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
            context: Some(ContextSpectrum {
                task: Some("出差".into()),
                emotion_valence: None,
                device: None,
                max_hops: None,
            }),
            budget: 16,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(!res.activated.is_empty(), "共振应至少返回种子节点");
    assert_eq!(
        res.activated[0].node_id, a.node_id,
        "种子应排第一（强度最高）"
    );

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
        .get_node(GetNodeRequest {
            tenant_id: "t1".into(),
            node_id: 999,
        })
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

    client
        .weave(WeaveRequest {
            tenant_id: "t1".into(),
            owner_id: "bob".into(),
            raw_event: "我去上海出差订了机票".into(),
            context: None,
        })
        .await
        .unwrap();
    client
        .weave(WeaveRequest {
            tenant_id: "t1".into(),
            owner_id: "bob".into(),
            raw_event: "周末在家读完了三体".into(),
            context: None,
        })
        .await
        .unwrap();

    // Search：用 Stub 嵌入"出差 航班 机票"查询，应命中机票节点
    let q = StubEmbedder::new(32)
        .embed(&["出差 航班 机票".to_string()])
        .await
        .unwrap()
        .pop()
        .unwrap();
    let bytes: Vec<u8> = q.iter().flat_map(|f| f.to_le_bytes()).collect();
    let resp = client
        .search(SearchRequest {
            tenant_id: "t1".into(),
            owner_id: "bob".into(),
            query_embedding: bytes,
            top_k: 5,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(!resp.neighbors.is_empty(), "语义通道下 Search 应有结果");
    assert_eq!(
        resp.neighbors[0].node_id, 0,
        "机票节点应排第一: {:?}",
        resp.neighbors
    );

    // Resonate：query 用词面重叠为零的表达，向量种子应兜住
    let res = client
        .resonate(ResonateRequest {
            tenant_id: "t1".into(),
            owner_id: "bob".into(),
            query: "航班".into(),
            context: None,
            budget: 8,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(
        !res.activated.is_empty(),
        "向量种子应激活节点: {:?}",
        res.activated
    );
}
