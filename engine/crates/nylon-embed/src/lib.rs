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
    input: &'a [String],
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
        let mut req = self.client.post(&endpoint).json(&EmbedReq {
            model: &self.model,
            input: texts,
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
}
