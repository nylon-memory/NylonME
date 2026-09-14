//! nylon-mobile：NylonME 记忆引擎的移动端进程内绑定（UniFFI）。
//!
//! 定位：Android（Kotlin/JNI）与 iOS（Swift）共用本 crate 编译出的
//! cdylib/staticlib，进程内直接调用 weave / resonate，**不开端口、不走 gRPC**。
//!
//! 逻辑主体在 nylon-service 的 EngineCore（与 gRPC 服务端共用同一份
//! 编织/共振/检索实现）；本文件只做 UniFFI 类型映射与调用转发。
//!
//! 生命周期提示：App 退后台/低内存时调用 `checkpoint()`，防进程被杀丢 WAL。

use std::sync::Arc;

use nylon_core::DEFAULT_TENANT;
use nylon_graph::ContextSpectrum;
use nylon_service::{CoreError, EngineCore, WeaveLlmMode};
use nylon_storage::PersistentGraph;

uniffi::setup_scaffolding!();

/// 引擎配置（App 侧构造一次，进程内单例）。
#[derive(uniffi::Record)]
pub struct MobileConfig {
    /// 数据目录（App 私有沙箱路径，如 filesDir/nylon）。
    pub data_dir: String,
    /// 记忆归属者 ID（单用户 App 固定一个即可；多账号切换时换 owner）。
    pub owner: String,
}

/// resonate 返回的激活记忆。
#[derive(uniffi::Record)]
#[derive(Debug)]
pub struct ActivatedMemory {
    pub local_id: u32,
    pub fact: String,
    pub relations: Vec<String>,
    /// 共振强度（图扩散得分）。
    pub resonance: f32,
    /// 当前张力（含时间衰减与频次强化）。
    pub tension: f32,
}

/// get 返回的记忆详情。
#[derive(uniffi::Record)]
pub struct MobileMemory {
    pub local_id: u32,
    pub fact: String,
    pub relations: Vec<String>,
    pub created_at: i64,
    pub tension: f32,
}

/// 移动端错误（映射为 Kotlin/Swift 异常）。
#[derive(uniffi::Error, Debug)]
pub enum MobileError {
    Io { message: String },
    Locked { message: String },
    NotFound { message: String },
}

impl std::fmt::Display for MobileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MobileError::Io { message } => write!(f, "io: {message}"),
            MobileError::Locked { message } => write!(f, "locked: {message}"),
            MobileError::NotFound { message } => write!(f, "not found: {message}"),
        }
    }
}

impl std::error::Error for MobileError {}

fn map_err(e: CoreError) -> MobileError {
    match e {
        CoreError::NotFound(m) => MobileError::NotFound { message: m },
        CoreError::Invalid(m) => MobileError::Io { message: format!("invalid argument: {m}") },
        CoreError::Internal(m) => MobileError::Io { message: m },
    }
}

/// 移动端记忆引擎句柄。App 侧单例持有，线程安全。
///
/// 内部持有单线程 tokio runtime 驱动 EngineCore 的异步方法（WAL 持久化
/// 等待在 blocking 线程池执行，不占用调用线程做 IO）；对外保持同步 API，
/// Kotlin/Swift 侧无需引入协程/Task 即可调用。
#[derive(uniffi::Object)]
pub struct MobileEngine {
    core: EngineCore,
    owner: String,
    rt: tokio::runtime::Runtime,
}

#[uniffi::export]
impl MobileEngine {
    /// 打开（或创建）本地记忆库。
    #[uniffi::constructor]
    pub fn new(config: MobileConfig) -> Result<Arc<Self>, MobileError> {
        let store = PersistentGraph::open(&config.data_dir).map_err(|e| MobileError::Io {
            message: format!("open {}: {e}", config.data_dir),
        })?;
        // 端上默认无嵌入/LLM 通道：heuristic 编织 + 词面/图扩散共振；
        // 端上嵌入模型（OnnxEmbedder）接入后在此注入。
        let core = EngineCore::new(store, nylon_service::DEFAULT_EMBED_DIMS, None, None);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| MobileError::Io { message: format!("runtime: {e}") })?;
        Ok(Arc::new(Self {
            core,
            owner: config.owner,
            rt,
        }))
    }

    /// 编织一条记忆。task/emotion 为可选情境丝；返回 node_id。
    pub fn weave(
        &self,
        fact: String,
        task: Option<String>,
        emotion_valence: Option<f32>,
    ) -> Result<u64, MobileError> {
        if fact.trim().is_empty() {
            return Err(MobileError::Io { message: "fact is empty".into() });
        }
        let ctx = (task.is_some() || emotion_valence.is_some()).then(|| ContextSpectrum {
            task,
            emotion_valence,
            max_hops: None,
        });
        self.rt
            .block_on(async {
                self.core
                    .weave_one(
                        DEFAULT_TENANT,
                        &self.owner,
                        fact.trim(),
                        ctx,
                        WeaveLlmMode::Full,
                    )
                    .await
            })
            .map(|o| o.node_id)
            .map_err(map_err)
    }

    /// 情境共振召回。
    pub fn resonate(&self, query: String, budget: u32) -> Result<Vec<ActivatedMemory>, MobileError> {
        self.rt
            .block_on(async {
                self.core
                    .resonate(DEFAULT_TENANT, &self.owner, &query, None, budget)
                    .await
            })
            .map(|out| {
                out.activated
                    .into_iter()
                    .map(|a| ActivatedMemory {
                        local_id: a.node_id as u32,
                        fact: a.filaments.fact,
                        relations: a.filaments.relations,
                        resonance: a.resonance,
                        tension: a.current_tension,
                    })
                    .collect()
            })
            .map_err(map_err)
    }

    /// 按局部 ID 读取记忆详情（owner 不符按不存在处理）。
    pub fn get(&self, local_id: u32) -> Result<MobileMemory, MobileError> {
        let out = self
            .core
            .get_node(DEFAULT_TENANT, local_id as u64)
            .map_err(map_err)?;
        if out.owner_id != self.owner {
            return Err(MobileError::NotFound {
                message: format!("node {local_id} (owner {})", self.owner),
            });
        }
        Ok(MobileMemory {
            local_id,
            fact: out.filaments.fact,
            relations: out.filaments.relations,
            created_at: out.filaments.created_at,
            tension: out.current_tension,
        })
    }

    /// 当前 owner 的存活记忆数。
    pub fn node_count(&self) -> Result<u64, MobileError> {
        Ok(self.core.node_count_for(DEFAULT_TENANT, &self.owner))
    }

    /// 主动 checkpoint：App 退后台/低内存时调用，防进程被杀丢 WAL。
    pub fn checkpoint(&self) -> Result<(), MobileError> {
        self.core.checkpoint().map_err(map_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_engine() -> (Arc<MobileEngine>, String) {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = format!("{}/nylon-mobile-test-{unique}", std::env::temp_dir().display());
        let e = MobileEngine::new(MobileConfig { data_dir: dir.clone(), owner: "tester".into() })
            .unwrap();
        (e, dir)
    }

    #[test]
    fn weave_and_resonate_roundtrip() {
        let (e, dir) = temp_engine();
        let id = e.weave("User prefers oat milk latte".into(), None, None).unwrap();
        assert!(e.get(id as u32).is_ok(), "weave 返回的 id 应可读回");
        let hits = e.resonate("oat milk".into(), 5).unwrap();
        assert!(!hits.is_empty());
        assert!(hits.iter().any(|h| h.fact.contains("oat milk")));
        e.checkpoint().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cjk_weave_and_resonate() {
        let (e, dir) = temp_engine();
        e.weave("我明天要去杭州出差见客户".into(), None, None).unwrap();
        let hits = e.resonate("杭州出差".into(), 5).unwrap();
        assert!(
            hits.iter().any(|h| h.fact.contains("杭州")),
            "中文关系丝（CJK 二字组）应支撑召回: {hits:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

