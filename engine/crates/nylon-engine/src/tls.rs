//! L2.5 传输层加密（TLS，rustls/ring 纯 Rust 栈，Windows GNU 工具链可编）。
//!
//! 设 `NYLON_TLS_CERT` + `NYLON_TLS_KEY`（PEM 文件路径）即对 gRPC 与 HTTP/UI 网关
//! 同时启用 TLS；两个变量必须同时设置，缺一直接报错退出（fail fast，杜绝半加密状态）。
//! 默认关闭：不设变量则行为与旧版本完全一致（明文，仅监听回环时可接受）。
//!
//! 客户端约定：URL 用 `https://` 前缀即走 TLS；自签证书时设 `NYLON_TLS_CA` 指向 CA PEM。
use std::io;

/// 从环境加载的 TLS 材料（PEM 原文 + 来源路径，便于日志与排障）。
#[derive(Clone)]
pub struct TlsMaterial {
    cert_pem: Vec<u8>,
    key_pem: Vec<u8>,
    cert_path: String,
    key_path: String,
}

/// 从 `NYLON_TLS_CERT` / `NYLON_TLS_KEY` 读取证书与私钥。
/// 返回 Ok(None) 表示未配置（默认明文）；只设其一为配置错误。
pub fn from_env() -> io::Result<Option<TlsMaterial>> {
    let cert = std::env::var("NYLON_TLS_CERT").ok();
    let key = std::env::var("NYLON_TLS_KEY").ok();
    match (cert, key) {
        (None, None) => Ok(None),
        (Some(cert_path), Some(key_path)) => {
            let cert_pem = std::fs::read(&cert_path).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("读取 NYLON_TLS_CERT={cert_path} 失败: {e}"),
                )
            })?;
            let key_pem = std::fs::read(&key_path).map_err(|e| {
                io::Error::new(e.kind(), format!("读取 NYLON_TLS_KEY={key_path} 失败: {e}"))
            })?;
            Ok(Some(TlsMaterial {
                cert_pem,
                key_pem,
                cert_path,
                key_path,
            }))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NYLON_TLS_CERT 与 NYLON_TLS_KEY 必须同时设置（当前只设置了其一）",
        )),
    }
}

impl TlsMaterial {
    /// 直接由 PEM 内容构造（测试与程序化部署用）。
    #[allow(dead_code)] // bin 里只走 from_env；from_pem 供集成测试用
    pub fn from_pem(cert_pem: Vec<u8>, key_pem: Vec<u8>) -> Self {
        Self {
            cert_pem,
            key_pem,
            cert_path: "<memory>".into(),
            key_path: "<memory>".into(),
        }
    }

    pub fn cert_path(&self) -> &str {
        &self.cert_path
    }

    /// tonic gRPC 服务端身份。
    pub fn tonic_identity(&self) -> tonic::transport::Identity {
        tonic::transport::Identity::from_pem(&self.cert_pem, &self.key_pem)
    }

    /// rustls 服务端配置。显式使用 ring provider：aws-lc-rs 在 Windows GNU 工具链下
    /// 无法正常构建，release 的 windows-x64 目标是 x86_64-pc-windows-gnu。
    pub fn rustls_server_config(&self) -> io::Result<tokio_rustls::rustls::ServerConfig> {
        use tokio_rustls::rustls::ServerConfig;
        let certs = rustls_pemfile::certs(&mut &self.cert_pem[..])
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("解析证书 PEM 失败（{}）: {e}", self.cert_path),
                )
            })?;
        if certs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("证书文件 {} 中没有 PEM 证书块", self.cert_path),
            ));
        }
        let key = rustls_pemfile::private_key(&mut &self.key_pem[..])
            .map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("解析私钥 PEM 失败（{}）: {e}", self.key_path),
                )
            })?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("私钥文件 {} 中没有 PEM 私钥块", self.key_path),
                )
            })?;
        ServerConfig::builder_with_provider(
            tokio_rustls::rustls::crypto::ring::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("TLS 协议配置: {e}")))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("证书/私钥不匹配或私钥不受支持: {e}"),
            )
        })
    }
}

/// PEM 解析前置校验：部署时尽早暴露"证书路径配错/格式不对"，而不是等到第一个连接。
pub fn validate(material: &TlsMaterial) -> io::Result<()> {
    material.rustls_server_config().map(|_| ())
}
