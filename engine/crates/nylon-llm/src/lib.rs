//! LLM 接入层（Phase 2）：编织分解与冲突检测依赖的对话模型抽象。
//!
//! - [`HttpChatModel`]：OpenAI 兼容 chat/completions 端点（DeepSeek、本地 ollama 均可）；
//! - [`StubChatModel`]：离线固定应答，供集成测试验证接线。
//!
//! 引擎通过 NYLON_LLM_URL / NYLON_LLM_MODEL / NYLON_LLM_API_KEY 配置；
//! 未配置时 LLM 通道关闭（Weave 回退启发式分解、冲突检测为空）。
use std::time::Duration;

/// 对话模型抽象：system+user 输入，返回结构化 JSON。
#[async_trait::async_trait]
pub trait ChatModel: Send + Sync {
    /// 请求模型输出 JSON 对象（后端不支持 json_object 时退化为文本解析）。
    async fn chat_json(&self, system: &str, user: &str) -> Result<serde_json::Value, LlmError>;
}

#[derive(Debug)]
pub struct LlmError(pub String);

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "llm: {}", self.0)
    }
}
impl std::error::Error for LlmError {}

// ---------- OpenAI 兼容 HTTP 后端 ----------

pub struct HttpChatModel {
    client: reqwest::Client,
    /// 完整的 chat/completions URL（如 https://api.deepseek.com/chat/completions）。
    url: String,
    model: String,
    api_key: Option<String>,
}

#[derive(serde::Serialize)]
struct ChatReq<'a> {
    model: &'a str,
    messages: [Msg<'a>; 2],
    response_format: RespFmt<'a>,
    temperature: f32,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Thinking<'a>>,
}
#[derive(serde::Serialize)]
struct Msg<'a> {
    role: &'a str,
    content: &'a str,
}
#[derive(serde::Serialize)]
struct RespFmt<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
}
#[derive(serde::Serialize)]
struct Thinking<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
}

#[derive(serde::Deserialize)]
struct ChatResp {
    choices: Vec<Choice>,
}
#[derive(serde::Deserialize)]
struct Choice {
    message: MsgOut,
}
#[derive(serde::Deserialize)]
struct MsgOut {
    content: String,
}

impl HttpChatModel {
    pub fn new(url: impl Into<String>, model: impl Into<String>, api_key: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        HttpChatModel {
            client,
            url: url.into(),
            model: model.into(),
            api_key,
        }
    }
}

/// 从响应文本中取出 JSON 对象（容忍 markdown 代码块包裹等常见输出）。
fn parse_json_loose(text: &str) -> Result<serde_json::Value, LlmError> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str(t) {
        return Ok(v);
    }
    // 退化：截取第一个 '{' 到最后一个 '}' 之间的片段
    if let (Some(a), Some(b)) = (t.find('{'), t.rfind('}')) {
        if a < b {
            if let Ok(v) = serde_json::from_str(&t[a..=b]) {
                return Ok(v);
            }
        }
    }
    Err(LlmError(format!(
        "响应不是 JSON: {}",
        &t[t.len().saturating_sub(200)..]
    )))
}

#[async_trait::async_trait]
impl ChatModel for HttpChatModel {
    async fn chat_json(&self, system: &str, user: &str) -> Result<serde_json::Value, LlmError> {
        let req = ChatReq {
            model: &self.model,
            messages: [
                Msg {
                    role: "system",
                    content: system,
                },
                Msg {
                    role: "user",
                    content: user,
                },
            ],
            response_format: RespFmt {
                kind: "json_object",
            },
            temperature: 0.0,
            max_tokens: 1536,
            // NYLON_LLM_THINKING_OFF=1 时请求关闭推理（deepseek-v4-flash 等推理模型会烧光 token 预算导致 JSON 截断）
            thinking: if std::env::var("NYLON_LLM_THINKING_OFF").is_ok() {
                Some(Thinking { kind: "disabled" })
            } else {
                None
            },
        };
        let mut rb = self.client.post(&self.url).json(&req);
        if let Some(key) = &self.api_key {
            rb = rb.bearer_auth(key);
        }
        let resp = rb.send().await.map_err(|e| LlmError(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let n = body.len().min(200);
            return Err(LlmError(format!("HTTP {status}: {}", &body[..n])));
        }
        let parsed: ChatResp = resp.json().await.map_err(|e| LlmError(e.to_string()))?;
        let content = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| LlmError("空 choices".into()))?;
        parse_json_loose(&content)
    }
}

// ---------- 离线 Stub（固定应答） ----------

/// 固定返回预置 JSON，供测试验证接线逻辑。
pub struct StubChatModel {
    canned: serde_json::Value,
}

impl StubChatModel {
    pub fn new(canned: serde_json::Value) -> Self {
        StubChatModel { canned }
    }
}

#[async_trait::async_trait]
impl ChatModel for StubChatModel {
    async fn chat_json(&self, _system: &str, _user: &str) -> Result<serde_json::Value, LlmError> {
        Ok(self.canned.clone())
    }
}

/// 从环境变量构建 LLM 通道。
///
/// NYLON_LLM_URL（完整 chat/completions URL）存在时启用；
/// 可选 NYLON_LLM_MODEL（默认 deepseek-v4-flash）与 NYLON_LLM_API_KEY。
pub fn llm_from_env() -> Option<std::sync::Arc<dyn ChatModel>> {
    let url = std::env::var("NYLON_LLM_URL").ok()?;
    let model = std::env::var("NYLON_LLM_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".into());
    let key = std::env::var("NYLON_LLM_API_KEY").ok();
    Some(std::sync::Arc::new(HttpChatModel::new(url, model, key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stub_returns_canned() {
        let m = StubChatModel::new(serde_json::json!({"conflicts": [0]}));
        let v = m.chat_json("s", "u").await.unwrap();
        assert_eq!(v["conflicts"][0], 0);
    }

    #[test]
    fn parse_json_loose_handles_markdown() {
        // raw string avoids escape issues: input is markdown-wrapped JSON
        let v = parse_json_loose(
            r#"```json
{"a": 1}
```"#,
        )
        .unwrap();
        assert_eq!(v["a"], 1);
        let v = parse_json_loose(r#"{"b": 2}"#).unwrap();
        assert_eq!(v["b"], 2);
        assert!(parse_json_loose("b9;䷧ JSON").is_err());
    }
}
