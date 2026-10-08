//! 嵌入向量接入层（Phase 2 语义通道）。
//!
//! - [`HttpEmbedder`]：OpenAI 兼容 /v1/embeddings 端点（自建 TEI/ollama/第三方 API 均可）；
//! - [`StubEmbedder`]：确定性离线实现，字符 n-gram 哈希入桶，供无网开发与集成测试。
//!
//! 引擎通过 NYLON_EMBED_URL / NYLON_EMBED_MODEL / NYLON_EMBED_DIMS 配置；
//! 未配置时嵌入通道关闭（行为与 Phase 1 一致）。

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;

/// 嵌入器抽象：文本批 → 等维向量批。
#[async_trait::async_trait]
pub trait Embedder: Send + Sync {
    /// 返回每条文本的嵌入向量（长度均为 dims()）。
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError>;
    fn dims(&self) -> usize;
}

#[derive(Debug)]
pub struct EmbedError(pub String);

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "embed: {}", self.0)
    }
}
impl std::error::Error for EmbedError {}

// ---------- OpenAI 兼容 HTTP 后端 ----------

pub struct HttpEmbedder {
    client: reqwest::Client,
    url: String,
    model: String,
    api_key: Option<String>,
    dims: usize,
}

#[derive(serde::Serialize)]
struct EmbedReq<'a> {
    model: &'a str,
    input: &'a [std::borrow::Cow<'a, str>],
}

/// 单条输入的字节上限（默认 4500，可用 NYLON_EMBED_MAX_BYTES 覆盖）。
/// 实测约束（2026-10-08）：ollama 的 llama-server 嵌入路径以 ubatch=2048 为
/// 硬上限——即使 -c/OLLAMA_CONTEXT_LENGTH=8192，>2048 token 的单条输入仍
/// 报 400 "input length exceeds the context length"。4500 字节 ≈ 英文
/// 1150-1500 tokens / CJK 1500 chars（≤~1800 tokens），各语种均安全。
/// 使用更大上下文/批量部署（TEI/vLLM 等）时应调高该值。
/// 嵌入语义集中在文本前部，截断代价远小于编织失败。
fn max_embed_bytes() -> usize {
    std::env::var("NYLON_EMBED_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4_500)
}

/// 按 UTF-8 字符边界截断到字节上限；未超限则零拷贝借用。
fn truncate_for_embed(text: &str, cap: usize) -> std::borrow::Cow<'_, str> {
    if text.len() <= cap {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut end = cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    eprintln!(
        "[embed] 输入 {} 字节超过上限 {}，已截断（尾部不参与嵌入，原文仍完整落库）",
        text.len(),
        cap
    );
    std::borrow::Cow::Borrowed(&text[..end])
}
#[derive(serde::Deserialize)]
struct EmbedResp {
    data: Vec<EmbedItem>,
}
#[derive(serde::Deserialize)]
struct EmbedItem {
    embedding: Vec<f32>,
}

impl HttpEmbedder {
    /// url 为端点根（如 http://127.0.0.1:8080），内部拼 /v1/embeddings。
    pub fn new(
        url: impl Into<String>,
        model: impl Into<String>,
        dims: usize,
        api_key: Option<String>,
    ) -> Self {
        HttpEmbedder {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            url: url.into(),
            model: model.into(),
            api_key,
            dims,
        }
    }
}

#[async_trait::async_trait]
impl Embedder for HttpEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let endpoint = self.url.trim_end_matches('/').to_string() + "/v1/embeddings";
        let cap = max_embed_bytes();
        let inputs: Vec<std::borrow::Cow<'_, str>> =
            texts.iter().map(|t| truncate_for_embed(t, cap)).collect();
        let mut req = self.client.post(&endpoint).json(&EmbedReq {
            model: &self.model,
            input: &inputs,
        });
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await.map_err(|e| EmbedError(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let n = body.len().min(200);
            return Err(EmbedError(format!("HTTP {status}: {}", &body[..n])));
        }
        let parsed: EmbedResp = resp.json().await.map_err(|e| EmbedError(e.to_string()))?;
        if parsed.data.len() != texts.len() {
            return Err(EmbedError(format!(
                "返回 {} 条，期望 {} 条",
                parsed.data.len(),
                texts.len()
            )));
        }
        for item in &parsed.data {
            if item.embedding.len() != self.dims {
                return Err(EmbedError(format!(
                    "维度 {} 与配置 {} 不符",
                    item.embedding.len(),
                    self.dims
                )));
            }
        }
        Ok(parsed.data.into_iter().map(|d| d.embedding).collect())
    }
    fn dims(&self) -> usize {
        self.dims
    }
}

// ---------- 确定性 Stub（离线开发/测试） ----------

/// 字符 n-gram 哈希入桶的伪嵌入：共享 n-gram 越多的文本向量越相似。
/// 确定性（同输入同输出），无需网络与模型文件。
pub struct StubEmbedder {
    dims: usize,
}

impl StubEmbedder {
    pub fn new(dims: usize) -> Self {
        StubEmbedder { dims }
    }
}

#[async_trait::async_trait]
impl Embedder for StubEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts.iter().map(|t| stub_embed(t, self.dims)).collect())
    }
    fn dims(&self) -> usize {
        self.dims
    }
}

fn stub_embed(text: &str, dims: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; dims];
    let lower = text.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    for n in [2usize, 3] {
        if chars.len() < n {
            continue;
        }
        for w in chars.windows(n) {
            let mut h = DefaultHasher::new();
            for c in w {
                h.write_u32(*c as u32);
            }
            let hv = h.finish();
            v[(hv as usize) % dims] += 1.0;
        }
    }
    // L2 归一化，配合余弦相似度
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// 从环境变量构建嵌入器。
///
/// 设置了 NYLON_EMBED_URL 时启用 HTTP 后端（OpenAI 兼容 /v1/embeddings），
/// 可选 NYLON_EMBED_MODEL（默认 bge-m3）与 NYLON_EMBED_API_KEY；
/// 未设置时返回 None，嵌入通道关闭。
pub fn embedder_from_env(dims: usize) -> Option<std::sync::Arc<dyn Embedder>> {
    let url = std::env::var("NYLON_EMBED_URL").ok()?;
    let model = std::env::var("NYLON_EMBED_MODEL").unwrap_or_else(|_| "bge-m3".into());
    let key = std::env::var("NYLON_EMBED_API_KEY").ok();
    Some(std::sync::Arc::new(HttpEmbedder::new(
        url, model, dims, key,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stub_is_deterministic_and_semanticish() {
        let emb = StubEmbedder::new(64);
        let a = emb.embed(&["出差订机票".to_string()]).await.unwrap();
        let b = emb.embed(&["出差订机票".to_string()]).await.unwrap();
        assert_eq!(a, b, "同输入必须同输出");
        let c = emb.embed(&["出差订酒店".to_string()]).await.unwrap();
        let d = emb.embed(&["量子力学期末考试".to_string()]).await.unwrap();
        let sim = |x: &[f32], y: &[f32]| x.iter().zip(y).map(|(p, q)| p * q).sum::<f32>();
        assert!(
            sim(&a[0], &c[0]) > sim(&a[0], &d[0]),
            "共享 n-gram 多的文本应更相似"
        );
    }

    #[tokio::test]
    async fn stub_dims_respected() {
        let emb = StubEmbedder::new(8);
        let out = emb.embed(&["hello".into(), "world".into()]).await.unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|v| v.len() == 8));
    }

    #[test]
    fn truncate_respects_cap_and_char_boundary() {
        // 短文本零拷贝借用
        let short = "hello memory".to_string();
        assert!(matches!(
            truncate_for_embed(&short, 100),
            std::borrow::Cow::Borrowed(_)
        ));
        // ASCII 超长截到 cap
        let long_ascii = "a".repeat(30_000);
        let out = truncate_for_embed(&long_ascii, 20_000);
        assert_eq!(out.len(), 20_000);
        // CJK（3 字节/字符）截断必须落在字符边界，且不超过 cap
        let long_cjk = "记".repeat(10_000); // 30_000 字节
        let out = truncate_for_embed(&long_cjk, 20_000);
        assert!(out.len() <= 20_000);
        assert_eq!(out.len() % 3, 0, "截断必须落在 UTF-8 字符边界");
        assert!(out.chars().all(|c| c == '记'));
    }
}
