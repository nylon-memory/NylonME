//! HTTP REST 网关 + 内嵌社区版 Web UI（Phase 2 L1.2/L1.3）。
//!
//! REST 是 gRPC 契约的薄 JSON 翻译层：处理器直接调用 MemoryEngine 实现，
//! 与 gRPC/CLI/MCP 共享同一条写路径（编织、建边、反思行为完全一致）。
//! Web UI 为零构建步骤的静态文件，编译期内嵌进二进制。

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tonic::Request;

use crate::service::{pb, EngineService};
use pb::memory_engine_server::MemoryEngine;

const INDEX_HTML: &str = include_str!("../ui/index.html");
const APP_JS: &str = include_str!("../ui/app.js");
const STYLE_CSS: &str = include_str!("../ui/style.css");
const OPENAPI_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../docs/api/openapi.json"
));

const DEFAULT_TENANT: &str = "default";

// ---------- 错误映射 ----------

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

fn map_status(s: tonic::Status) -> ApiError {
    let code = match s.code() {
        tonic::Code::InvalidArgument => StatusCode::BAD_REQUEST,
        tonic::Code::NotFound => StatusCode::NOT_FOUND,
        tonic::Code::Unauthenticated => StatusCode::UNAUTHORIZED,
        tonic::Code::PermissionDenied => StatusCode::FORBIDDEN,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    ApiError(code, s.message().to_string())
}

// ---------- JSON DTO ----------

#[derive(Deserialize)]
struct WeaveBody {
    owner_id: String,
    raw_event: String,
    tenant_id: Option<String>,
    task: Option<String>,
    emotion_valence: Option<f32>,
}

#[derive(Deserialize)]
struct ResonateBody {
    owner_id: String,
    query: String,
    tenant_id: Option<String>,
    budget: Option<u32>,
    max_hops: Option<u32>,
    task: Option<String>,
    emotion_valence: Option<f32>,
}

#[derive(Deserialize)]
struct SessionEventBody {
    text: String,
    speaker: Option<String>,
    event_id: Option<String>,
}

#[derive(Deserialize)]
struct WeaveSessionBody {
    owner_id: String,
    events: Vec<SessionEventBody>,
    tenant_id: Option<String>,
    skip_abstract: Option<bool>,
}

#[derive(Deserialize)]
struct SearchBody {
    owner_id: String,
    embedding: Vec<f32>,
    tenant_id: Option<String>,
    top_k: Option<u32>,
}

#[derive(Deserialize)]
struct ListQuery {
    owner: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct FilamentsJson {
    fact: String,
    emotion_valence: f32,
    emotion_intensity: f32,
    created_at: i64,
    decay_rate: f32,
    relations: Vec<String>,
    confidence: f32,
    mentions_7d: u32,
}

fn filaments_json(f: pb::Filaments) -> FilamentsJson {
    FilamentsJson {
        fact: f.fact,
        emotion_valence: f.emotion_valence,
        emotion_intensity: f.emotion_intensity,
        created_at: f.created_at,
        decay_rate: f.decay_rate,
        relations: f.relations,
        confidence: f.confidence,
        mentions_7d: f.mentions_7d,
    }
}

#[derive(Serialize)]
struct ActivatedJson {
    node_id: u64,
    resonance: f32,
    filaments: Option<FilamentsJson>,
}

fn activated_json(a: pb::ActivatedNode) -> ActivatedJson {
    ActivatedJson {
        node_id: a.node_id,
        resonance: a.resonance,
        filaments: a.filaments.map(filaments_json),
    }
}

fn ctx(
    task: Option<String>,
    emotion_valence: Option<f32>,
    max_hops: Option<u32>,
) -> Option<pb::ContextSpectrum> {
    if task.is_none() && emotion_valence.is_none() && max_hops.is_none() {
        return None;
    }
    Some(pb::ContextSpectrum {
        task,
        emotion_valence,
        max_hops,
        device: None,
    })
}

// ---------- 静态资源 / 文档 ----------

// UI 资源随二进制升级而变，禁缓存避免升级后浏览器拿到旧页面。
async fn ui_index() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        INDEX_HTML,
    )
}

async fn ui_js() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        APP_JS,
    )
}

async fn ui_css() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        STYLE_CSS,
    )
}

async fn openapi() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        OPENAPI_JSON,
    )
}

// ---------- REST 端点 ----------

async fn stats(State(svc): State<EngineService>) -> Result<impl IntoResponse, ApiError> {
    svc.stats().map(Json).map_err(map_status)
}

async fn list_nodes(
    State(svc): State<EngineService>,
    Query(q): Query<ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let limit = q.limit.unwrap_or(50).min(500);
    let (total, nodes) = svc
        .list_nodes(q.owner.as_deref(), q.offset.unwrap_or(0), limit)
        .map_err(map_status)?;
    Ok(Json(serde_json::json!({ "total": total, "nodes": nodes })))
}

async fn get_node(
    State(svc): State<EngineService>,
    Path(id): Path<u64>,
    Query(q): Query<ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let _tenant = q.owner; // tenant 强隔离属 L2.1，当前仅 owner 过滤
    let resp = svc
        .get_node(Request::new(pb::GetNodeRequest {
            tenant_id: DEFAULT_TENANT.into(),
            node_id: id,
        }))
        .await
        .map_err(map_status)?
        .into_inner();
    Ok(Json(serde_json::json!({
        "node_id": resp.node_id,
        "filaments": resp.filaments.map(filaments_json),
        "current_tension": resp.current_tension,
    })))
}

async fn weave(
    State(svc): State<EngineService>,
    Json(b): Json<WeaveBody>,
) -> Result<impl IntoResponse, ApiError> {
    let resp = svc
        .weave(Request::new(pb::WeaveRequest {
            tenant_id: b.tenant_id.unwrap_or_else(|| DEFAULT_TENANT.into()),
            owner_id: b.owner_id,
            raw_event: b.raw_event,
            context: ctx(b.task, b.emotion_valence, None),
        }))
        .await
        .map_err(map_status)?
        .into_inner();
    Ok(Json(serde_json::json!({
        "node_id": resp.node_id,
        "linked_nodes": resp.linked_nodes,
        "conflict_nodes": resp.conflict_nodes,
    })))
}

async fn weave_session(
    State(svc): State<EngineService>,
    Json(b): Json<WeaveSessionBody>,
) -> Result<impl IntoResponse, ApiError> {
    let events: Vec<pb::SessionEvent> = b
        .events
        .into_iter()
        .map(|e| pb::SessionEvent {
            event_id: e.event_id.unwrap_or_default(),
            speaker: e.speaker.unwrap_or_default(),
            text: e.text,
        })
        .collect();
    let resp = svc
        .weave_session(Request::new(pb::WeaveSessionRequest {
            tenant_id: b.tenant_id.unwrap_or_else(|| DEFAULT_TENANT.into()),
            owner_id: b.owner_id,
            events,
            skip_abstract: b.skip_abstract.unwrap_or(false),
        }))
        .await
        .map_err(map_status)?
        .into_inner();
    Ok(Json(serde_json::json!({
        "leaf_nodes": resp.leaf_nodes.iter().map(|e| serde_json::json!({
            "event_id": e.event_id, "node_id": e.node_id,
        })).collect::<Vec<_>>(),
        "fact_nodes": resp.fact_nodes.iter().map(|f| serde_json::json!({
            "node_id": f.node_id, "fact": f.fact, "source_event_ids": f.source_event_ids,
        })).collect::<Vec<_>>(),
    })))
}

async fn resonate(
    State(svc): State<EngineService>,
    Json(b): Json<ResonateBody>,
) -> Result<impl IntoResponse, ApiError> {
    let resp = svc
        .resonate(Request::new(pb::ResonateRequest {
            tenant_id: b.tenant_id.unwrap_or_else(|| DEFAULT_TENANT.into()),
            owner_id: b.owner_id,
            query: b.query,
            context: ctx(b.task, b.emotion_valence, b.max_hops),
            budget: b.budget.unwrap_or(0),
        }))
        .await
        .map_err(map_status)?
        .into_inner();
    Ok(Json(serde_json::json!({
        "activated": resp.activated.into_iter().map(activated_json).collect::<Vec<_>>(),
        "seed_ids": resp.seed_ids,
    })))
}

async fn search(
    State(svc): State<EngineService>,
    Json(b): Json<SearchBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut bytes = Vec::with_capacity(b.embedding.len() * 4);
    for v in &b.embedding {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    let resp = svc
        .search(Request::new(pb::SearchRequest {
            tenant_id: b.tenant_id.unwrap_or_else(|| DEFAULT_TENANT.into()),
            owner_id: b.owner_id,
            query_embedding: bytes,
            top_k: b.top_k.unwrap_or(10),
        }))
        .await
        .map_err(map_status)?
        .into_inner();
    Ok(Json(serde_json::json!({
        "neighbors": resp.neighbors.into_iter().map(activated_json).collect::<Vec<_>>(),
    })))
}

// ---------- 路由与启动 ----------

pub fn router(svc: EngineService) -> Router {
    Router::new()
        .route("/", get(ui_index))
        .route("/app.js", get(ui_js))
        .route("/style.css", get(ui_css))
        .route("/openapi.json", get(openapi))
        .route("/v1/stats", get(stats))
        .route("/v1/nodes", get(list_nodes))
        .route("/v1/nodes/{id}", get(get_node))
        .route("/v1/weave", post(weave))
        .route("/v1/weave_session", post(weave_session))
        .route("/v1/resonate", post(resonate))
        .route("/v1/search", post(search))
        .with_state(svc)
}

/// 启动 HTTP 网关（REST + Web UI）。与 gRPC 服务并行运行。
pub async fn serve(svc: EngineService, addr: &str) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("nylon-engine HTTP/UI listening on http://{addr}");
    axum::serve(listener, router(svc)).await
}
