//! API key 鉴权（Phase 2 L2.2）：租户级三档权限。
//!
//! - 未配置任何 key：开放模式（单机默认），行为与历史版本完全一致；
//! - 配置 key 后：gRPC 请求需携带 `x-api-key` metadata，HTTP 携带
//!   `x-api-key` 头或 `Authorization: Bearer <key>`；
//! - 每把 key 绑定一个租户与权限档位（read < write < admin），
//!   档位决定可调用的 RPC 类别，租户不匹配即拒绝（admin 可用 "*" 通配）。
//!
//! 配置来源（优先级从高到低）：
//! 1. `NYLON_API_KEYS_FILE`：JSON 文件路径；
//! 2. `NYLON_API_KEYS`：内联 JSON。
//!
//! JSON 接受数组或 `{"keys": [...]}`，元素形如：
//! `{"key": "nyl_...", "tenant": "acme", "scope": "write"}`（scope 缺省 read）。

use std::sync::Arc;
use tonic::{Request, Status};

/// 权限档位：只读 < 读写 < 管理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// 只读：Resonate / Search / GetNode
    Read,
    /// 读写：Read + Weave / WeaveSession
    Write,
    /// 管理：Write + 后续管理操作（L2.3），且允许 tenant="*" 通配
    Admin,
}

impl Scope {
    /// 当前档位是否满足所需档位。
    pub fn allows(self, needed: Scope) -> bool {
        self >= needed
    }

    fn parse(s: &str) -> Option<Scope> {
        match s {
            "read" => Some(Scope::Read),
            "write" => Some(Scope::Write),
            "admin" => Some(Scope::Admin),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Write => "write",
            Scope::Admin => "admin",
        }
    }
}

/// 一把 key 的授权结果：绑定租户 + 权限档位。放入 gRPC request extensions，
/// 由 handler 与请求体里的 tenant_id 做最终比对。
#[derive(Debug, Clone)]
pub struct KeyGrant {
    pub tenant: String,
    pub scope: Scope,
}

impl KeyGrant {
    /// 是否覆盖指定租户（admin 通配 "*" 或精确匹配）。
    pub fn covers_tenant(&self, tenant: &str) -> bool {
        self.tenant == "*" || self.tenant == tenant
    }
}

/// key 表：启动时加载，运行期只读（轮换需重启，热加载留给 L2.3）。
#[derive(Debug, Default)]
pub struct ApiKeys {
    grants: std::collections::HashMap<String, KeyGrant>,
}

#[derive(serde::Deserialize)]
struct KeyEntry {
    key: String,
    tenant: String,
    scope: Option<String>,
}

impl ApiKeys {
    /// 从环境加载 key 表；未配置返回 None（开放模式）。
    pub fn from_env() -> Option<Arc<Self>> {
        if let Ok(path) = std::env::var("NYLON_API_KEYS_FILE") {
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("读取 NYLON_API_KEYS_FILE={path} 失败: {e}"));
            return Some(Arc::new(
                Self::parse(&raw).unwrap_or_else(|e| panic!("解析 {path} 失败: {e}")),
            ));
        }
        if let Ok(raw) = std::env::var("NYLON_API_KEYS") {
            return Some(Arc::new(
                Self::parse(&raw).unwrap_or_else(|e| panic!("解析 NYLON_API_KEYS 失败: {e}")),
            ));
        }
        None
    }

    /// 解析 JSON key 表（数组或 {"keys": [...]} 两种形态）。
    pub fn parse(json: &str) -> Result<Self, String> {
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let arr = if let Some(a) = v.as_array() {
            a.clone()
        } else if let Some(a) = v.get("keys").and_then(|k| k.as_array()) {
            a.clone()
        } else {
            return Err("key 表必须是 JSON 数组或 {\"keys\": [...]}".into());
        };
        let mut grants = std::collections::HashMap::new();
        for (i, item) in arr.iter().enumerate() {
            let entry: KeyEntry = serde_json::from_value(item.clone())
                .map_err(|e| format!("第 {} 条 key 记录格式错误: {e}", i + 1))?;
            if entry.key.is_empty() || entry.tenant.is_empty() {
                return Err(format!("第 {} 条记录 key / tenant 不能为空", i + 1));
            }
            let scope = match entry.scope.as_deref() {
                None => Scope::Read,
                Some(s) => {
                    Scope::parse(s).ok_or_else(|| format!("第 {} 条记录 scope 非法: {s}", i + 1))?
                }
            };
            if entry.tenant == "*" && scope != Scope::Admin {
                return Err(format!(
                    "第 {} 条记录：tenant=\"*\" 通配仅允许 admin 档位",
                    i + 1
                ));
            }
            grants.insert(
                entry.key,
                KeyGrant {
                    tenant: entry.tenant,
                    scope,
                },
            );
        }
        if grants.is_empty() {
            return Err("key 表为空（不配置鉴权请直接去掉环境变量）".into());
        }
        Ok(Self { grants })
    }

    /// 用 key 换取授权；未知 key 返回 None。
    pub fn authenticate(&self, key: &str) -> Option<KeyGrant> {
        self.grants.get(key).cloned()
    }

    pub fn len(&self) -> usize {
        self.grants.len()
    }
}

/// 从 gRPC metadata 提取 x-api-key。
fn grpc_key(req: &Request<()>) -> Option<String> {
    req.metadata()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// gRPC 拦截器：开放模式直接放行；鉴权模式校验 key 有效性，
/// grant 写入 extensions。档位与租户比对在 handler 内完成
///（tonic 0.14 的 Request 不携带 URI，拦截器无法按 RPC 路径分档）。
pub fn grpc_intercept(
    auth: &Option<Arc<ApiKeys>>,
    mut req: Request<()>,
) -> Result<Request<()>, Status> {
    let Some(keys) = auth else {
        return Ok(req);
    };
    let key = grpc_key(&req)
        .ok_or_else(|| Status::unauthenticated("缺少 x-api-key（引擎已启用 API key 鉴权）"))?;
    let grant = keys
        .authenticate(&key)
        .ok_or_else(|| Status::unauthenticated("x-api-key 无效"))?;
    req.extensions_mut().insert(grant);
    Ok(req)
}

/// handler 侧统一鉴权：grant 存在时校验档位 + 请求体 tenant 必须被 key 覆盖。
/// 无 grant（开放模式 / 进程内调用）直接放行。
pub fn authorize(grant: Option<&KeyGrant>, needed: Scope, tenant: &str) -> Result<(), Status> {
    if let Some(g) = grant {
        if !g.scope.allows(needed) {
            return Err(Status::permission_denied(format!(
                "key 档位 {} 不足：该操作需要 {}",
                g.scope.as_str(),
                needed.as_str()
            )));
        }
        if !g.covers_tenant(tenant) {
            return Err(Status::permission_denied(format!(
                "key 绑定租户为 {}，无权访问 tenant={}",
                g.tenant, tenant
            )));
        }
    }
    Ok(())
}

/// HTTP 侧鉴权：x-api-key 头优先，其次 Authorization: Bearer。
/// tenant 为 Some 时同时做租户比对；None（如全局 stats）只校验 key 与档位。
/// 返回 grant（开放模式为 None），错误用 tonic Status 表达以便复用 map_status。
pub fn http_authorize(
    auth: &Option<Arc<ApiKeys>>,
    headers: &axum::http::HeaderMap,
    needed: Scope,
    tenant: Option<&str>,
) -> Result<Option<KeyGrant>, Status> {
    let Some(keys) = auth else {
        return Ok(None);
    };
    let key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| {
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer ").map(|s| s.to_string()))
        })
        .ok_or_else(|| Status::unauthenticated("缺少 API key（x-api-key 头或 Bearer）"))?;
    let grant = keys
        .authenticate(&key)
        .ok_or_else(|| Status::unauthenticated("API key 无效"))?;
    if !grant.scope.allows(needed) {
        return Err(Status::permission_denied(format!(
            "key 档位 {} 不足：该操作需要 {}",
            grant.scope.as_str(),
            needed.as_str()
        )));
    }
    if let Some(t) = tenant {
        if !grant.covers_tenant(t) {
            return Err(Status::permission_denied(format!(
                "key 绑定租户为 {}，无权访问 tenant={t}",
                grant.tenant
            )));
        }
    }
    Ok(Some(grant))
}

/// 生成一把新 API key（nyl_ 前缀 + 128 位加密随机 hex）。
pub fn generate_key() -> String {
    use rand::Rng;
    let mut buf = [0u8; 16];
    rand::rng().fill(&mut buf);
    format!("nyl_{}", hex_lower(&buf))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Arc<ApiKeys> {
        Arc::new(
            ApiKeys::parse(
                r#"[
                    {"key": "k-read", "tenant": "acme", "scope": "read"},
                    {"key": "k-write", "tenant": "acme", "scope": "write"},
                    {"key": "k-admin", "tenant": "*", "scope": "admin"}
                ]"#,
            )
            .unwrap(),
        )
    }

    #[test]
    fn parse_and_authenticate() {
        let k = keys();
        assert_eq!(k.len(), 3);
        let g = k.authenticate("k-read").unwrap();
        assert_eq!(g.tenant, "acme");
        assert_eq!(g.scope, Scope::Read);
        assert!(k.authenticate("nope").is_none());
    }

    #[test]
    fn wildcard_tenant_requires_admin() {
        assert!(ApiKeys::parse(r#"[{"key":"x","tenant":"*","scope":"write"}]"#).is_err());
        assert!(ApiKeys::parse(r#"[{"key":"x","tenant":"*","scope":"admin"}]"#).is_ok());
    }

    #[test]
    fn missing_scope_defaults_to_read() {
        let k = ApiKeys::parse(r#"{"keys":[{"key":"x","tenant":"t"}]}"#).unwrap();
        assert_eq!(k.authenticate("x").unwrap().scope, Scope::Read);
    }

    #[test]
    fn scope_ordering() {
        assert!(Scope::Read.allows(Scope::Read));
        assert!(!Scope::Read.allows(Scope::Write));
        assert!(Scope::Admin.allows(Scope::Write));
        assert!(Scope::Write.allows(Scope::Read));
    }

    #[test]
    fn open_mode_passes_through() {
        let req = Request::new(());
        assert!(grpc_intercept(&None, req).is_ok());
    }

    #[test]
    fn missing_key_rejected() {
        let auth = Some(keys());
        let req = Request::new(());
        let err = grpc_intercept(&auth, req).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn valid_key_grant_in_extensions() {
        let auth = Some(keys());
        let mut req = Request::new(());
        req.metadata_mut()
            .insert("x-api-key", "k-write".parse().unwrap());
        let req = grpc_intercept(&auth, req).unwrap();
        let grant = req.extensions().get::<KeyGrant>().unwrap();
        assert_eq!(grant.tenant, "acme");
        assert_eq!(grant.scope, Scope::Write);
    }

    #[test]
    fn authorize_checks_scope_and_tenant() {
        let grant = KeyGrant {
            tenant: "acme".into(),
            scope: Scope::Read,
        };
        // 档位不足
        let err = authorize(Some(&grant), Scope::Write, "acme").unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        // 档位够 + 租户匹配
        assert!(authorize(Some(&grant), Scope::Read, "acme").is_ok());
        // 租户不匹配
        let write = KeyGrant {
            tenant: "acme".into(),
            scope: Scope::Write,
        };
        let err = authorize(Some(&write), Scope::Write, "other").unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
        // admin 通配放行
        let admin = KeyGrant {
            tenant: "*".into(),
            scope: Scope::Admin,
        };
        assert!(authorize(Some(&admin), Scope::Write, "other").is_ok());
        // 开放模式放行
        assert!(authorize(None, Scope::Write, "other").is_ok());
    }

    #[test]
    fn generated_key_format() {
        let k = generate_key();
        assert!(k.starts_with("nyl_"));
        assert_eq!(k.len(), 4 + 32);
        assert_ne!(generate_key(), generate_key());
    }
}
