//! L2.5 TLS 端到端：自签证书 → gRPC + HTTP 双栈真实握手；明文访问被拒。
//! 证书由 rcgen 在测试内现签，不依赖 openssl/网络，CI 全平台可跑。

#[path = "../src/audit.rs"]
mod audit;

#[path = "../src/auth.rs"]
mod auth;

#[path = "../src/http.rs"]
mod http;

#[path = "../src/mcp.rs"]
mod mcp;

#[path = "../src/service.rs"]
mod service;

#[path = "../src/tls.rs"]
mod tls;

use nylon_storage::PersistentGraph;
use service::pb::memory_engine_client::MemoryEngineClient;
use service::pb::memory_engine_server::MemoryEngineServer;
use service::pb::{GetNodeRequest, WeaveRequest};
use service::EngineService;
use tls::TlsMaterial;

/// 现签一张 localhost 自签证书，返回 (TlsMaterial, 证书 PEM)。
fn self_signed() -> (TlsMaterial, Vec<u8>) {
    let rcgen::CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert_pem = cert.pem().into_bytes();
    let key_pem = key_pair.serialize_pem().into_bytes();
    (TlsMaterial::from_pem(cert_pem.clone(), key_pem), cert_pem)
}

fn test_svc(dir: &std::path::Path) -> EngineService {
    let store = PersistentGraph::open(dir).unwrap();
    EngineService::new(store, 8, None, None)
}

/// gRPC over TLS：客户端带 CA 直连成功；明文客户端连 TLS 端口失败。
#[tokio::test]
async fn grpc_tls_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let (material, cert_pem) = self_signed();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let svc = test_svc(dir.path());
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .tls_config(
                tonic::transport::ServerTlsConfig::new().identity(material.tonic_identity()),
            )
            .unwrap()
            .add_service(MemoryEngineServer::new(svc))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    // TLS 客户端：带自签 CA，域名固定 localhost（与证书 SAN 一致）
    let endpoint = tonic::transport::Endpoint::from_shared(format!("https://localhost:{port}"))
        .unwrap()
        .tls_config(
            tonic::transport::ClientTlsConfig::new()
                .ca_certificate(tonic::transport::Certificate::from_pem(cert_pem)),
        )
        .unwrap();
    let mut client = MemoryEngineClient::connect(endpoint).await.unwrap();
    let woven = client
        .weave(WeaveRequest {
            tenant_id: "default".into(),
            owner_id: "tls".into(),
            raw_event: "TLS 通道写入测试".into(),
            context: None,
        })
        .await
        .expect("TLS gRPC weave 应成功")
        .into_inner();
    let got = client
        .get_node(GetNodeRequest {
            tenant_id: "default".into(),
            node_id: woven.node_id,
        })
        .await
        .expect("TLS gRPC get_node 应成功")
        .into_inner();
    assert_eq!(got.filaments.unwrap().fact, "TLS 通道写入测试");

    // 明文客户端打 TLS 端口：握手必然失败（connect 或首个 RPC 报错都算拒绝）
    let plain = MemoryEngineClient::connect(format!("http://127.0.0.1:{port}")).await;
    if let Ok(mut c) = plain {
        let r = c
            .get_node(GetNodeRequest {
                tenant_id: "default".into(),
                node_id: 0,
            })
            .await;
        assert!(r.is_err(), "明文 gRPC 不应能读 TLS 端口");
    }
}

/// HTTP/UI over TLS：rustls 客户端手写 HTTP/1.1 请求，验证 200 + JSON。
#[tokio::test]
async fn http_tls_roundtrip() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::rustls::pki_types::ServerName;
    use tokio_rustls::rustls::{ClientConfig, RootCertStore};

    let dir = tempfile::tempdir().unwrap();
    let (material, cert_pem) = self_signed();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let svc = test_svc(dir.path());
    tokio::spawn(async move {
        http::serve_on(listener, svc, Some(material)).await.unwrap();
    });

    // rustls 客户端：只信这张自签证书
    let mut roots = RootCertStore::empty();
    let der = rustls_pemfile::certs(&mut &cert_pem[..])
        .next()
        .unwrap()
        .unwrap();
    roots.add(der).unwrap();
    let config = ClientConfig::builder_with_provider(
        tokio_rustls::rustls::crypto::ring::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config));

    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let server_name = ServerName::try_from("localhost").unwrap().to_owned();
    let mut tls = connector.connect(server_name, tcp).await.unwrap();
    tls.write_all(
        b"GET /v1/stats?tenant_id=default HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await
    .unwrap();
    let mut buf = Vec::new();
    tls.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    assert!(text.starts_with("HTTP/1.1 200"), "响应应为 200: {text}");
    assert!(
        text.contains("\"embed_dims\":8"),
        "应返回 stats JSON: {text}"
    );

    // 明文 HTTP 打 TLS 端口：不应拿到 200
    let mut plain = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    plain
        .write_all(b"GET /v1/stats HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut pbuf = vec![0u8; 256];
    let n = tokio::time::timeout(std::time::Duration::from_secs(3), plain.read(&mut pbuf))
        .await
        .map(|r| r.unwrap_or(0))
        .unwrap_or(0);
    let ptext = String::from_utf8_lossy(&pbuf[..n]);
    assert!(
        !ptext.starts_with("HTTP/1.1 200"),
        "明文请求不应拿到 200: {ptext}"
    );
}

/// from_env 配置校验：只设一个变量必须报错（fail fast）。
#[test]
fn tls_env_requires_both() {
    // 保存/恢复环境，避免污染同进程其他测试
    let old_cert = std::env::var("NYLON_TLS_CERT").ok();
    let old_key = std::env::var("NYLON_TLS_KEY").ok();
    std::env::remove_var("NYLON_TLS_KEY");
    std::env::set_var("NYLON_TLS_CERT", "whatever.pem");
    let r = tls::from_env();
    match (old_cert, old_key) {
        (c, k) => {
            match c {
                Some(v) => std::env::set_var("NYLON_TLS_CERT", v),
                None => std::env::remove_var("NYLON_TLS_CERT"),
            }
            match k {
                Some(v) => std::env::set_var("NYLON_TLS_KEY", v),
                None => std::env::remove_var("NYLON_TLS_KEY"),
            }
        }
    }
    assert!(r.is_err(), "只设 NYLON_TLS_CERT 必须报错");

    // 证书/私钥不匹配：validate 应拒绝（用两张独立自签证书拼）
    let c1 = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let k2 = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let mixed = TlsMaterial::from_pem(
        c1.cert.pem().into_bytes(),
        k2.key_pair.serialize_pem().into_bytes(),
    );
    assert!(tls::validate(&mixed).is_err(), "不匹配的证书/私钥应被拒");
}
