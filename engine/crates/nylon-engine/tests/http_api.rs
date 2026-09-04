//! REST 网关集成测试：与 gRPC 共享同一 EngineService 写路径。
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
#[path = "../src/audit.rs"]
mod audit;

#[path = "../src/auth.rs"]
mod auth;
#[path = "../src/http.rs"]
mod http;
#[path = "../src/service.rs"]
mod service;
use nylon_storage::PersistentGraph;
use serde_json::{json, Value};
use service::EngineService;
use tower::ServiceExt;

fn test_svc(dir: &std::path::Path) -> EngineService {
    let store = PersistentGraph::open(dir).unwrap();
    EngineService::new(store, 8, None, None)
}

async fn call(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn rest_weave_list_get_resonate_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let app = http::router(test_svc(dir.path()));

    // weave
    let (s, b) = call(
        &app,
        Request::post("/v1/weave")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"owner_id": "alice", "raw_event": "Alice prefers window seats", "task": "travel"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let node_id = b["node_id"].as_u64().unwrap();

    // list
    let (s, b) = call(
        &app,
        Request::get("/v1/nodes?owner=alice")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["total"].as_u64().unwrap(), 1);
    assert_eq!(b["nodes"][0]["fact"], "Alice prefers window seats");

    // owner filter excludes other tenants' owners
    let (s, b) = call(
        &app,
        Request::get("/v1/nodes?owner=bob")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["total"].as_u64().unwrap(), 0);

    // get_node
    let (s, b) = call(
        &app,
        Request::get(format!("/v1/nodes/{node_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(b["current_tension"].as_f64().unwrap() > 0.0);

    // get_node 404
    let (s, _) = call(
        &app,
        Request::get("/v1/nodes/9999").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // resonate
    let (s, b) = call(
        &app,
        Request::post("/v1/resonate")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"owner_id": "alice", "query": "window seat", "budget": 5}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["activated"].as_array().unwrap().len(), 1);

    // stats
    let (s, b) = call(&app, Request::get("/v1/stats").body(Body::empty()).unwrap()).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["nodes"].as_u64().unwrap(), 1);
    assert_eq!(b["embedder"], false);

    // openapi served
    let (s, b) = call(
        &app,
        Request::get("/openapi.json").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["info"]["title"], "NylonME Memory Engine REST API");

    // ui served
    let resp = app
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("NylonME Console"));
}

#[tokio::test]
async fn rest_weave_validation_error() {
    let dir = tempfile::tempdir().unwrap();
    let app = http::router(test_svc(dir.path()));
    let (s, b) = call(
        &app,
        Request::post("/v1/weave")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"owner_id": "", "raw_event": "x"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(b["error"].as_str().unwrap().contains("不能为空"));
}
