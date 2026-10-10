//! MCP (Model Context Protocol) stdio server —— 单二进制内嵌引擎，
//! Claude Code / Cursor / Codex / VS Code Copilot 等 MCP 客户端直接拉起本进程即可拥有长期记忆。
//!
//! 两种模式：
//! - 本地内嵌（默认）：进程内打开 NYLON_DATA_DIR 数据目录，单机使用；
//! - 远程桥接（设 NYLON_SERVER）：工具调用经 gRPC 转发到远端引擎，
//!   本进程不持有数据，多机共享服务端同一份记忆库。
use crate::service::{pb, EngineService};
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router, ErrorData, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

/// 远端引擎桥：实现与 EngineService 相同的 MemoryEngine trait，
/// 但每个调用都克隆一条廉价 Channel 转发到远端 gRPC 守护进程。
#[derive(Clone)]
pub struct RemoteEngine {
    client: pb::memory_engine_client::MemoryEngineClient<tonic::transport::Channel>,
    /// 远端启用 API key 鉴权（L2.2）时透传的 key（NYLON_API_KEY）。
    api_key: Option<String>,
}

impl RemoteEngine {
    /// 连接远端引擎；接受 "host:port" 或 "http(s)://host:port"（与 SDK/CLI 约定一致）。
    /// https:// 走 TLS（L2.5）：自签证书设 NYLON_TLS_CA 指向 CA PEM，缺省用系统/WebPKI 根。
    /// api_key：远端开启鉴权时随每个请求写入 x-api-key metadata。
    pub async fn connect(
        target: &str,
        api_key: Option<String>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let client = if let Some(rest) = target.strip_prefix("https://") {
            let mut endpoint = tonic::transport::Endpoint::from_shared(format!("https://{rest}"))?;
            let mut tls_cfg = tonic::transport::ClientTlsConfig::new();
            if let Ok(ca_path) = std::env::var("NYLON_TLS_CA") {
                let pem = std::fs::read(&ca_path).map_err(|e| {
                    std::io::Error::new(e.kind(), format!("读取 NYLON_TLS_CA={ca_path} 失败: {e}"))
                })?;
                tls_cfg = tls_cfg.ca_certificate(tonic::transport::Certificate::from_pem(pem));
            }
            endpoint = endpoint.tls_config(tls_cfg)?;
            pb::memory_engine_client::MemoryEngineClient::connect(endpoint).await?
        } else {
            let t = target.strip_prefix("http://").unwrap_or(target);
            pb::memory_engine_client::MemoryEngineClient::connect(format!("http://{t}")).await?
        };
        Ok(Self { client, api_key })
    }

    /// 转发前附加鉴权 metadata。
    fn sign<T>(&self, mut req: tonic::Request<T>) -> tonic::Request<T> {
        if let Some(k) = &self.api_key {
            if let Ok(v) = k.parse() {
                req.metadata_mut().insert("x-api-key", v);
            }
        }
        req
    }
}

#[tonic::async_trait]
impl pb::memory_engine_server::MemoryEngine for RemoteEngine {
    async fn weave(
        &self,
        req: tonic::Request<pb::WeaveRequest>,
    ) -> Result<tonic::Response<pb::WeaveResponse>, tonic::Status> {
        self.client.clone().weave(self.sign(req)).await
    }
    async fn weave_session(
        &self,
        req: tonic::Request<pb::WeaveSessionRequest>,
    ) -> Result<tonic::Response<pb::WeaveSessionResponse>, tonic::Status> {
        self.client.clone().weave_session(self.sign(req)).await
    }
    async fn resonate(
        &self,
        req: tonic::Request<pb::ResonateRequest>,
    ) -> Result<tonic::Response<pb::ResonateResponse>, tonic::Status> {
        self.client.clone().resonate(self.sign(req)).await
    }
    async fn search(
        &self,
        req: tonic::Request<pb::SearchRequest>,
    ) -> Result<tonic::Response<pb::SearchResponse>, tonic::Status> {
        self.client.clone().search(self.sign(req)).await
    }
    async fn get_node(
        &self,
        req: tonic::Request<pb::GetNodeRequest>,
    ) -> Result<tonic::Response<pb::GetNodeResponse>, tonic::Status> {
        self.client.clone().get_node(self.sign(req)).await
    }

    async fn report_feedback(
        &self,
        req: tonic::Request<pb::FeedbackRequest>,
    ) -> Result<tonic::Response<pb::FeedbackResponse>, tonic::Status> {
        self.client.clone().report_feedback(self.sign(req)).await
    }
}

#[derive(Clone)]
pub struct NylonMcp {
    svc: Arc<dyn pb::memory_engine_server::MemoryEngine>,
    tenant: String,
    default_owner: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WeaveArgs {
    /// 要持久化的事实：一句自成一体的话（带名字/数字/路径），不要写密钥或密码
    pub fact: String,
    /// 记忆归属（项目或用户 slug），缺省用环境变量 NYLON_OWNER 或 "default"
    pub owner: Option<String>,
    /// 主题标签，帮助相关记忆自动建立关联
    pub task: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResonateArgs {
    /// 查询：自然语言问题或关键词
    pub query: String,
    /// 记忆归属（项目或用户 slug），缺省用环境变量 NYLON_OWNER 或 "default"
    pub owner: Option<String>,
    /// 返回条数上限，默认 8
    pub budget: Option<u32>,
    /// 联想扩散深度：0=仅精准命中不扩散；缺省按引擎默认（多跳联想）
    pub max_hops: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetArgs {
    /// 节点 ID（resonate 返回的第一列）
    pub node_id: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FeedbackArgs {
    /// 当时没答好的原始查询（用户问了什么）
    pub query: String,
    /// 失败类型：down（差评/没答到点上）| wrong（答错了）| insufficient（记忆信息不足）。默认 down
    pub rating: Option<String>,
    /// 可选补充：正确答案是什么/缺什么信息（帮助引擎定向反思）
    pub comment: Option<String>,
    /// 记忆归属（项目或用户 slug），缺省用环境变量 NYLON_OWNER 或 "default"
    pub owner: Option<String>,
}

#[tool_router]
impl NylonMcp {
    #[tool(
        description = "把一条持久记忆织入引擎（事实/决策/偏好/环境信息）。事实要自成一体、包含关键名字与数字；绝不写入密钥或密码。"
    )]
    async fn memory_weave(
        &self,
        Parameters(args): Parameters<WeaveArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = pb::WeaveRequest {
            tenant_id: self.tenant.clone(),
            owner_id: args
                .owner
                .clone()
                .unwrap_or_else(|| self.default_owner.clone()),
            raw_event: args.fact,
            context: Some(pb::ContextSpectrum {
                task: args.task,
                emotion_valence: None,
                device: None,
                max_hops: None,
            }),
        };
        let resp =
            pb::memory_engine_server::MemoryEngine::weave(&*self.svc, tonic::Request::new(req))
                .await
                .map_err(|e| ErrorData::internal_error(e.message().to_string(), None))?
                .into_inner();
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "NODE_ID={} LINKED={:?}",
            resp.node_id, resp.linked_nodes
        ))]))
    }

    #[tool(
        description = "按情境共振召回相关记忆：从种子节点沿关系图自适应扩散。任务开始时用它回忆历史决策与坑。"
    )]
    async fn memory_resonate(
        &self,
        Parameters(args): Parameters<ResonateArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = pb::ResonateRequest {
            tenant_id: self.tenant.clone(),
            owner_id: args
                .owner
                .clone()
                .unwrap_or_else(|| self.default_owner.clone()),
            query: args.query,
            context: Some(pb::ContextSpectrum {
                task: None,
                emotion_valence: None,
                device: None,
                max_hops: args.max_hops,
            }),
            budget: args.budget.unwrap_or(8),
            // budget 的既有语义即"返回条数上限"（见工具描述），同步到 top_k（issue #3）
            top_k: args.budget.unwrap_or(8),
        };
        let resp =
            pb::memory_engine_server::MemoryEngine::resonate(&*self.svc, tonic::Request::new(req))
                .await
                .map_err(|e| ErrorData::internal_error(e.message().to_string(), None))?
                .into_inner();
        if resp.activated.is_empty() {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "（没有相关记忆）",
            )]));
        }
        let mut lines = Vec::new();
        for n in &resp.activated {
            let fact = n
                .filaments
                .as_ref()
                .map(|f| f.fact.clone())
                .unwrap_or_default();
            lines.push(format!("{}\t{:.3}\t{}", n.node_id, n.resonance, fact));
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(
            lines.join("\n"),
        )]))
    }

    #[tool(description = "按节点 ID 读取一条记忆的完整内容。")]
    async fn memory_get(
        &self,
        Parameters(args): Parameters<GetArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = pb::GetNodeRequest {
            tenant_id: self.tenant.clone(),
            node_id: args.node_id,
        };
        let resp =
            pb::memory_engine_server::MemoryEngine::get_node(&*self.svc, tonic::Request::new(req))
                .await
                .map_err(|e| ErrorData::internal_error(e.message().to_string(), None))?
                .into_inner();
        let fact = resp
            .filaments
            .map(|f| f.fact)
            .unwrap_or_else(|| "(节点不存在)".into());
        Ok(CallToolResult::success(vec![ContentBlock::text(fact)]))
    }

    #[tool(
        description = "报告一次基于记忆的失败回答（差评/答错/信息不足）。引擎会持久化记录，并在空闲反思时针对该失败定向补全推断。当你发现记忆检索没帮上忙或答错时主动调用。"
    )]
    async fn memory_feedback(
        &self,
        Parameters(args): Parameters<FeedbackArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = pb::FeedbackRequest {
            tenant_id: self.tenant.clone(),
            owner_id: args
                .owner
                .clone()
                .unwrap_or_else(|| self.default_owner.clone()),
            query: args.query,
            rating: args.rating.unwrap_or_else(|| "down".into()),
            comment: args.comment.unwrap_or_default(),
            shown_node_ids: Vec::new(),
        };
        pb::memory_engine_server::MemoryEngine::report_feedback(
            &*self.svc,
            tonic::Request::new(req),
        )
        .await
        .map_err(|e| ErrorData::internal_error(e.message().to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            "已记录。引擎将在空闲反思时针对该失败定向补全（feedback-driven reflection）。",
        )]))
    }
}

#[tool_handler]
impl ServerHandler for NylonMcp {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info.name = "nylonme-memory".into();
        info.server_info.version = env!("CARGO_PKG_VERSION").into();
        info.instructions = Some(
            "NylonME 长期记忆引擎（情境共振检索）。工作流：任务开始时调 memory_resonate 回忆相关历史；\
             获得重要决策/事实/坑时调 memory_weave 沉淀（一句自成一体的话，不含密钥）。\
             owner 用于隔离不同项目的记忆。"
                .into(),
        );
        info
    }
}

/// 本地内嵌模式：进程内嵌引擎，数据落盘 NYLON_DATA_DIR 或 ~/.nylonme/data。
pub async fn run_stdio(svc: EngineService) -> Result<(), Box<dyn std::error::Error>> {
    run(Arc::new(svc)).await
}

/// 远程桥接模式：MCP 工具调用全部转发到 NYLON_SERVER 指向的远端引擎，
/// 本进程不持有任何记忆数据——多机共享服务端同一份记忆库。
pub async fn run_stdio_remote(remote: RemoteEngine) -> Result<(), Box<dyn std::error::Error>> {
    run(Arc::new(remote)).await
}

async fn run(
    svc: Arc<dyn pb::memory_engine_server::MemoryEngine>,
) -> Result<(), Box<dyn std::error::Error>> {
    let tenant = std::env::var("NYLON_TENANT").unwrap_or_else(|_| "default".into());
    let owner = std::env::var("NYLON_OWNER").unwrap_or_else(|_| "default".into());
    let server = NylonMcp {
        svc,
        tenant,
        default_owner: owner,
    };
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

// ---------- Streamable HTTP 模式（/mcp 端点，路径 B：客户端零二进制） ----------

use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};

impl NylonMcp {
    pub fn new(
        svc: Arc<dyn pb::memory_engine_server::MemoryEngine>,
        tenant: String,
        default_owner: String,
    ) -> Self {
        Self {
            svc,
            tenant,
            default_owner,
        }
    }
}

/// 构建挂到 HTTP 服务器上的 /mcp 服务：与 stdio 模式共享同一套工具定义，
/// 但直接调用进程内引擎（不走 gRPC 转发）。
///
/// - 无会话状态（legacy_session_mode=false）：每个请求自包含，多客户端
///   并发互不干扰，重启不丢会话；
/// - json_response=true：简单工具调用直接回 JSON，不开 SSE 流；
/// - Host 校验：默认关闭（端点已有 key 鉴权，见 http.rs 的 /mcp 中间件）；
///   公网部署时用 NYLON_MCP_ALLOWED_HOSTS="host1,host2" 收紧。
pub fn streamable_http_service(
    svc: EngineService,
) -> StreamableHttpService<NylonMcp, LocalSessionManager> {
    let tenant = std::env::var("NYLON_TENANT").unwrap_or_else(|_| "default".into());
    let owner = std::env::var("NYLON_OWNER").unwrap_or_else(|_| "default".into());
    // non_exhaustive 结构体：不能字面量构造，先 default 再改字段
    let mut config = StreamableHttpServerConfig::default();
    config.legacy_session_mode = false;
    config.json_response = true;
    match std::env::var("NYLON_MCP_ALLOWED_HOSTS") {
        Ok(v) => {
            config.allowed_hosts = v.split(',').map(|s| s.trim().to_string()).collect();
        }
        Err(_) => {
            config = config.disable_allowed_hosts();
        }
    }
    let svc: Arc<dyn pb::memory_engine_server::MemoryEngine> = Arc::new(svc);
    StreamableHttpService::new(
        move || Ok(NylonMcp::new(svc.clone(), tenant.clone(), owner.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    )
}
