//! HTTP REST 网关 + 内嵌社区版 Web UI（Phase 2 L1.2/L1.3）。
//!
//! REST 是 gRPC 契约的薄 JSON 翻译层：处理器直接调用 MemoryEngine 实现，
//! 与 gRPC/CLI/MCP 共享同一条写路径（编织、建边、反思行为完全一致）。
//! Web UI 为零构建步骤的静态文件，编译期内嵌进二进制。

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tonic::Request;

use crate::auth::{http_authorize, Scope};
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
    tenant: Option<String>,
    owner: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct AuditQuery {
    tenant: Option<String>,
    owner: Option<String>,
    action: Option<String>,
    limit: Option<usize>,
}

/// 审计事件查询（L2.3）。鉴权模式下非通配 key 只能看自己租户的事件。
async fn audit_events(
    State(svc): State<EngineService>,
    headers: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let grant = http_authorize(svc.auth(), &headers, Scope::Read, None).map_err(map_status)?;
    // 通配 admin 可跨租户查询；租户 key 强制限定本租户
    let tenant = match &grant {
        Some(g) if g.tenant != "*" => Some(g.tenant.clone()),
        _ => q.tenant.clone(),
    };
    let Some(audit) = svc.audit() else {
        return Ok(Json(serde_json::json!({ "events": [], "disabled": true })));
    };
    let limit = q.limit.unwrap_or(200).min(1000);
    let events = audit.query(
        tenant.as_deref(),
        q.owner.as_deref(),
        q.action.as_deref(),
        limit,
    );
    Ok(Json(serde_json::json!({ "events": events })))
}

/// 把 HTTP 侧已验证的 grant 透传给 service handler（与 gRPC 拦截器同一条比对路径）。
fn signed_request<T>(grant: Option<crate::auth::KeyGrant>, body: T) -> Request<T> {
    let mut req = Request::new(body);
    if let Some(g) = grant {
        req.extensions_mut().insert(g);
    }
    req
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
    // 资源引用带版本号，升级后自动击穿浏览器磁盘缓存中的旧 app.js/style.css
    let html = INDEX_HTML.replace("__NYLON_VER__", env!("CARGO_PKG_VERSION"));
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        html,
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

async fn stats(
    State(svc): State<EngineService>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    // 全局统计不属于任何租户：只校验 key 与档位，不做租户比对
    http_authorize(svc.auth(), &headers, Scope::Read, None).map_err(map_status)?;
    svc.stats().map(Json).map_err(map_status)
}

async fn list_nodes(
    State(svc): State<EngineService>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let tenant = q.tenant.clone().unwrap_or_else(|| DEFAULT_TENANT.into());
    http_authorize(svc.auth(), &headers, Scope::Read, Some(&tenant)).map_err(map_status)?;
    let limit = q.limit.unwrap_or(50).min(500);
    let (total, nodes) = svc
        .list_nodes(&tenant, q.owner.as_deref(), q.offset.unwrap_or(0), limit)
        .map_err(map_status)?;
    Ok(Json(serde_json::json!({ "total": total, "nodes": nodes })))
}

async fn get_node(
    State(svc): State<EngineService>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    Query(q): Query<ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let tenant = q.tenant.clone().unwrap_or_else(|| DEFAULT_TENANT.into());
    let grant =
        http_authorize(svc.auth(), &headers, Scope::Read, Some(&tenant)).map_err(map_status)?;
    let resp = svc
        .get_node(signed_request(
            grant,
            pb::GetNodeRequest {
                tenant_id: tenant,
                node_id: id,
            },
        ))
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
    headers: HeaderMap,
    Json(b): Json<WeaveBody>,
) -> Result<impl IntoResponse, ApiError> {
    let tenant = b.tenant_id.clone().unwrap_or_else(|| DEFAULT_TENANT.into());
    let grant =
        http_authorize(svc.auth(), &headers, Scope::Write, Some(&tenant)).map_err(map_status)?;
    let resp = svc
        .weave(signed_request(
            grant,
            pb::WeaveRequest {
                tenant_id: tenant,
                owner_id: b.owner_id,
                raw_event: b.raw_event,
                context: ctx(b.task, b.emotion_valence, None),
            },
        ))
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
    headers: HeaderMap,
    Json(b): Json<WeaveSessionBody>,
) -> Result<impl IntoResponse, ApiError> {
    let tenant = b.tenant_id.clone().unwrap_or_else(|| DEFAULT_TENANT.into());
    let grant =
        http_authorize(svc.auth(), &headers, Scope::Write, Some(&tenant)).map_err(map_status)?;
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
        .weave_session(signed_request(
            grant,
            pb::WeaveSessionRequest {
                tenant_id: tenant,
                owner_id: b.owner_id,
                events,
                skip_abstract: b.skip_abstract.unwrap_or(false),
            },
        ))
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
    headers: HeaderMap,
    Json(b): Json<ResonateBody>,
) -> Result<impl IntoResponse, ApiError> {
    let tenant = b.tenant_id.clone().unwrap_or_else(|| DEFAULT_TENANT.into());
    let grant =
        http_authorize(svc.auth(), &headers, Scope::Read, Some(&tenant)).map_err(map_status)?;
    let resp = svc
        .resonate(signed_request(
            grant,
            pb::ResonateRequest {
                tenant_id: tenant,
                owner_id: b.owner_id,
                query: b.query,
                context: ctx(b.task, b.emotion_valence, b.max_hops),
                budget: b.budget.unwrap_or(0),
            },
        ))
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
    headers: HeaderMap,
    Json(b): Json<SearchBody>,
) -> Result<impl IntoResponse, ApiError> {
    let tenant = b.tenant_id.clone().unwrap_or_else(|| DEFAULT_TENANT.into());
    let grant =
        http_authorize(svc.auth(), &headers, Scope::Read, Some(&tenant)).map_err(map_status)?;
    let mut bytes = Vec::with_capacity(b.embedding.len() * 4);
    for v in &b.embedding {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    let resp = svc
        .search(signed_request(
            grant,
            pb::SearchRequest {
                tenant_id: tenant,
                owner_id: b.owner_id,
                query_embedding: bytes,
                top_k: b.top_k.unwrap_or(10),
            },
        ))
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
        .route("/v1/audit", get(audit_events))
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
