//! 尼龙记忆引擎 — 库入口。
//!
//! 模块全部公开，供两类二进制复用：
//! - 社区版 `nylon-engine`（本 crate 的 main.rs）：cipher 恒为 None；
//! - 企业版 `nylon-engine-ent`（私有仓库）：通过 [`run_serve_with`] 注入
//!   静态加密实现（L2.6），其余服务装配逻辑完全一致。

pub mod audit;
pub mod auth;
pub mod http;
pub mod mcp;
pub mod paths;
pub mod service;
pub mod tls;

use std::sync::Arc;

use nylon_storage::{PersistentGraph, StorageCipher};

/// 装配并运行 gRPC + HTTP 双栈服务（原 main.rs 的 serve 分支）。
///
/// `cipher` 为企业版静态加密实现（L2.6）：Some 时快照/WAL/审计/反馈日志
/// 全部密封落盘；None 为社区版明文行为。数据目录的解析与警告由调用方负责
/// （社区版在 main.rs 里做，企业版可自行决定）。
pub async fn run_serve_with(
    addr: &str,
    data: &str,
    dims: usize,
    cipher: Option<Arc<dyn StorageCipher>>,
) {
    let store = PersistentGraph::open_with(data, cipher.clone()).expect("open persistent store");
    let llm = nylon_llm::llm_from_env();
    let embedder = nylon_embed::embedder_from_env(dims);
    let keys = auth::ApiKeys::from_env();
    if let Some(k) = &keys {
        println!("API key 鉴权已启用（{} 把 key，L2.2）", k.len());
    }
    // L2.5 TLS：NYLON_TLS_CERT/KEY 同时设置才对 gRPC + HTTP 双栈启用；缺一直接退出
    let tls = tls::from_env().unwrap_or_else(|e| {
        eprintln!("TLS 配置错误: {e}");
        std::process::exit(2);
    });
    if let Some(t) = &tls {
        tls::validate(t).unwrap_or_else(|e| {
            eprintln!("TLS 证书/私钥无效: {e}");
            std::process::exit(2);
        });
        println!("TLS 已启用（L2.5，证书 {}）", t.cert_path());
    }
    let svc = service::EngineService::new(store, dims, embedder, llm)
        .with_auth(keys.clone())
        .with_audit(audit::Audit::start_with(
            std::path::Path::new(data),
            cipher.clone(),
        ))
        .with_cipher(cipher);
    // 嵌入通道启动探测（issue #4）：未配置/端点不可达都醒目提示，不再静默退化
    svc.probe_embedder().await;
    let sock = addr.parse().expect("invalid listen addr");
    // HTTP 网关（REST + 社区版 Web UI）默认 127.0.0.1:50052，NYLON_HTTP_ADDR=off 关闭
    let http_addr = std::env::var("NYLON_HTTP_ADDR").unwrap_or_else(|_| "127.0.0.1:50052".into());
    let scheme = if tls.is_some() { "grpcs" } else { "grpc" };
    println!("nylon-engine gRPC listening on {addr} ({scheme}, data={data}, dims={dims})");
    // 周期 checkpoint：快照落盘并截断 WAL，控制恢复重放量 + 支持热备份（L2.4）
    let ckpt_secs: u64 = std::env::var("NYLON_CHECKPOINT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    if ckpt_secs > 0 {
        let svc_ckpt = svc.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(ckpt_secs)).await;
                match svc_ckpt.checkpoint("") {
                    Ok(()) => println!("[checkpoint] 快照已落盘，WAL 已截断"),
                    Err(e) => eprintln!("[checkpoint] 失败: {e}"),
                }
            }
        });
    }
    let mut grpc_builder = tonic::transport::Server::builder();
    if let Some(t) = &tls {
        grpc_builder = grpc_builder
            .tls_config(tonic::transport::ServerTlsConfig::new().identity(t.tonic_identity()))
            .expect("gRPC TLS 配置");
    }
    let grpc = grpc_builder
        .add_service(
            service::pb::memory_engine_server::MemoryEngineServer::with_interceptor(
                svc.clone(),
                move |req: tonic::Request<()>| auth::grpc_intercept(&keys, req),
            ),
        )
        .serve(sock);
    if http_addr == "off" {
        grpc.await.expect("gRPC server");
    } else {
        let http = http::serve(svc, &http_addr, tls);
        tokio::join!(async { grpc.await.expect("gRPC server") }, async {
            http.await.expect("http server")
        },);
    }
}
