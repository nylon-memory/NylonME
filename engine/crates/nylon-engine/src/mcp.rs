//! MCP (Model Context Protocol) stdio server —— 单二进制内嵌引擎，
//! Claude Code / Cursor / Codex / VS Code Copilot 等 MCP 客户端直接拉起本进程即可拥有长期记忆。
use crate::service::{pb, EngineService};
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router, ErrorData, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Clone)]
pub struct NylonMcp {
    svc: Arc<EngineService>,
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

/// 启动 stdio MCP server（进程内嵌引擎，数据落盘 NYLON_DATA_DIR 或 ~/.nylonme/data）。
pub async fn run_stdio(svc: EngineService) -> Result<(), Box<dyn std::error::Error>> {
    let tenant = std::env::var("NYLON_TENANT").unwrap_or_else(|_| "default".into());
    let owner = std::env::var("NYLON_OWNER").unwrap_or_else(|_| "default".into());
    let server = NylonMcp {
        svc: Arc::new(svc),
        tenant,
        default_owner: owner,
    };
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
