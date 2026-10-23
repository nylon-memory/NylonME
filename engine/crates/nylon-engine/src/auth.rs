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

    pub fn parse(s: &str) -> Option<Scope> {
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

/// key 表：启动时加载；来自文件时支持热加载——authenticate 时检查文件 mtime，
/// 变了自动重读（`nylon-engine keys add/revoke` 后无需重启引擎）。
/// 内联 JSON（NYLON_API_KEYS）配置不热加载。
#[derive(Debug, Default)]
pub struct ApiKeys {
    grants: std::sync::RwLock<std::collections::HashMap<String, KeyGrant>>,
    /// key 表文件路径（热加载用）；内联 JSON 配置时为 None
    file: Option<std::path::PathBuf>,
    loaded_mtime: std::sync::RwLock<Option<std::time::SystemTime>>,
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
            let p = std::path::PathBuf::from(&path);
            return Some(Arc::new(
                Self::load_or_bootstrap(&p).unwrap_or_else(|e| panic!("key 表 {path}: {e}")),
            ));
        }
        if let Ok(raw) = std::env::var("NYLON_API_KEYS") {
            return Some(Arc::new(
                Self::parse(&raw).unwrap_or_else(|e| panic!("解析 NYLON_API_KEYS 失败: {e}")),
            ));
        }
        None
    }

    /// 加载 key 表文件；文件不存在时自动生成一把 admin key 落盘并打印一次
    /// （首次启动引导，GitLab/MinIO 同款模式）——管理员拿到这把 key 后，
    /// 用 `nylon-engine keys add` 给其他人发 key（热加载，无需重启）。
    pub fn load_or_bootstrap(path: &std::path::Path) -> Result<Self, String> {
        if !path.exists() {
            let admin = generate_key();
            let entries = serde_json::json!([{"key": admin, "tenant": "*", "scope": "admin"}]);
            write_keys_file(path, &entries)?;
            eprintln!("================================================================");
            eprintln!("NylonME 鉴权初始化：已生成管理员 key（仅显示这一次，请妥善保存）");
            eprintln!("  {admin}");
            eprintln!("key 表文件: {}", path.display());
            eprintln!("给同事发 key: nylon-engine keys add --tenant <租户> --scope write");
            eprintln!("================================================================");
        }
        let raw = std::fs::read_to_string(path).map_err(|e| format!("读取失败: {e}"))?;
        let grants = Self::parse_grants(&raw).map_err(|e| format!("解析失败: {e}"))?;
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        Ok(Self {
            grants: std::sync::RwLock::new(grants),
            file: Some(path.to_path_buf()),
            loaded_mtime: std::sync::RwLock::new(mtime),
        })
    }

    /// 解析 JSON key 表（数组或 {"keys": [...]} 两种形态）。
    pub fn parse(json: &str) -> Result<Self, String> {
        Ok(Self {
            grants: std::sync::RwLock::new(Self::parse_grants(json)?),
            file: None,
            loaded_mtime: std::sync::RwLock::new(None),
        })
    }

    fn parse_grants(json: &str) -> Result<std::collections::HashMap<String, KeyGrant>, String> {
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
        Ok(grants)
    }

    /// 用 key 换取授权；未知 key 返回 None。
    pub fn authenticate(&self, key: &str) -> Option<KeyGrant> {
        self.maybe_reload();
        self.grants.read().unwrap().get(key).cloned()
    }

    pub fn len(&self) -> usize {
        self.grants.read().unwrap().len()
    }

    /// 文件源热加载：mtime 变了就重读；解析失败保留旧表
    /// （绝不让鉴权因为一个写到一半的文件失效）。
    fn maybe_reload(&self) {
        let Some(f) = &self.file else { return };
        let mtime = std::fs::metadata(f).and_then(|m| m.modified()).ok();
        if mtime == *self.loaded_mtime.read().unwrap() {
            return;
        }
        match std::fs::read_to_string(f)
            .ok()
            .and_then(|raw| Self::parse_grants(&raw).ok())
        {
            Some(grants) => {
                let n = grants.len();
                *self.grants.write().unwrap() = grants;
                *self.loaded_mtime.write().unwrap() = mtime;
                eprintln!("[auth] key 表已热加载（{n} 把 key）");
            }
            None => eprintln!("[auth] key 表 {} 重读失败，保留旧表", f.display()),
        }
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

// ---------- key 表文件管理（nylon-engine keys 子命令；离线操作，热加载生效） ----------

/// 原子写 key 表（tmp + rename；Unix 下设 0600）。
pub fn write_keys_file(path: &std::path::Path, entries: &serde_json::Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    let tmp = path.with_extension("tmp");
    let body = serde_json::to_string_pretty(entries).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body).map_err(|e| format!("写临时文件失败: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("原子替换失败: {e}"))?;
    Ok(())
}

/// 读 key 表原始条目（文件不存在视为空表——keys add 的创建路径）。
fn read_keys_entries(path: &std::path::Path) -> Result<Vec<serde_json::Value>, String> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("读取失败: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("不是合法 JSON: {e}"))?;
    if let Some(a) = v.as_array() {
        return Ok(a.clone());
    }
    if let Some(a) = v.get("keys").and_then(|k| k.as_array()) {
        return Ok(a.clone());
    }
    Err("key 表必须是 JSON 数组或 {\"keys\": [...]}".into())
}

/// 新增一把 key（缺省自动生成），返回完整 key（只在此刻完整显示一次）。
pub fn keys_add(
    path: &std::path::Path,
    tenant: &str,
    scope: &str,
    explicit_key: Option<String>,
) -> Result<String, String> {
    if Scope::parse(scope).is_none() {
        return Err(format!("scope 非法: {scope}（可选 read/write/admin）"));
    }
    if tenant == "*" && scope != "admin" {
        return Err("tenant=\"*\" 通配仅允许 admin 档位".into());
    }
    let key = explicit_key.unwrap_or_else(generate_key);
    if key.is_empty() {
        return Err("key 不能为空".into());
    }
    let mut entries = read_keys_entries(path)?;
    if entries
        .iter()
        .any(|e| e.get("key").and_then(|k| k.as_str()) == Some(key.as_str()))
    {
        return Err("key 已存在".into());
    }
    entries.push(serde_json::json!({"key": key, "tenant": tenant, "scope": scope}));
    write_keys_file(path, &serde_json::Value::Array(entries))?;
    Ok(key)
}

/// 列出 key（打码：前 12 位 + …），返回 (打码 key, tenant, scope)。
pub fn keys_list(path: &std::path::Path) -> Result<Vec<(String, String, String)>, String> {
    let entries = read_keys_entries(path)?;
    Ok(entries
        .iter()
        .map(|e| {
            let key = e.get("key").and_then(|k| k.as_str()).unwrap_or("");
            let masked = format!("{}…", &key[..key.len().min(12)]);
            (
                masked,
                e.get("tenant")
                    .and_then(|t| t.as_str())
                    .unwrap_or("?")
                    .to_string(),
                e.get("scope")
                    .and_then(|s| s.as_str())
                    .unwrap_or("read")
                    .to_string(),
            )
        })
        .collect())
}

/// 按完整 key 或唯一前缀吊销，返回被吊销的 key。
pub fn keys_revoke(path: &std::path::Path, key_or_prefix: &str) -> Result<String, String> {
    let entries = read_keys_entries(path)?;
    let matches: Vec<_> = entries
        .iter()
        .filter(|e| {
            e.get("key")
                .and_then(|k| k.as_str())
                .map(|k| k == key_or_prefix || k.starts_with(key_or_prefix))
                .unwrap_or(false)
        })
        .collect();
    match matches.len() {
        0 => Err(format!("没有匹配 {key_or_prefix} 的 key")),
        1 => {
            let doomed = matches[0]
                .get("key")
                .and_then(|k| k.as_str())
                .unwrap_or("")
                .to_string();
            let kept: Vec<_> = entries
                .into_iter()
                .filter(|e| e.get("key").and_then(|k| k.as_str()) != Some(doomed.as_str()))
                .collect();
            if kept.is_empty() {
                return Err(
                    "不能吊销最后一把 key（真想开放模式请删掉 NYLON_API_KEYS_FILE 配置）".into(),
                );
            }
            write_keys_file(path, &serde_json::Value::Array(kept))?;
            Ok(doomed)
        }
        n => Err(format!(
            "前缀 {key_or_prefix} 匹配到 {n} 把 key，请给出更长前缀"
        )),
    }
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

    fn temp_keys_path(tag: &str) -> std::path::PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("nylon-keys-{tag}-{}-{n}.json", std::process::id()))
    }

    #[test]
    fn bootstrap_creates_admin_key_file() {
        let p = temp_keys_path("bootstrap");
        assert!(!p.exists());
        let keys = ApiKeys::load_or_bootstrap(&p).unwrap();
        assert_eq!(keys.len(), 1);
        // 文件里只有一把 admin key，且能认证
        let raw = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let key = v[0]["key"].as_str().unwrap();
        assert_eq!(v[0]["scope"].as_str().unwrap(), "admin");
        let g = keys.authenticate(key).unwrap();
        assert_eq!(g.scope, Scope::Admin);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn hot_reload_after_keys_add_and_revoke() {
        let p = temp_keys_path("hotreload");
        let keys = ApiKeys::load_or_bootstrap(&p).unwrap();
        // 新增：文件被 keys add 改写后，不用重建 ApiKeys 就能认证新 key
        let new_key = keys_add(&p, "acme", "write", None).unwrap();
        assert!(keys.authenticate(&new_key).is_some());
        // 吊销：新 key 立即失效，初始 admin 仍在
        let removed = keys_revoke(&p, &new_key).unwrap();
        assert_eq!(removed, new_key);
        assert!(keys.authenticate(&new_key).is_none());
        assert_eq!(keys.len(), 1);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn revoke_ambiguous_prefix_rejected() {
        let p = temp_keys_path("revoke");
        keys_add(&p, "t", "write", Some("nyl_aaaa1111".into())).unwrap();
        keys_add(&p, "t", "write", Some("nyl_aaaa2222".into())).unwrap();
        assert!(keys_revoke(&p, "nyl_aaaa").is_err()); // 前缀不唯一
        assert_eq!(keys_revoke(&p, "nyl_aaaa1").unwrap(), "nyl_aaaa1111");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn keys_add_validates_scope_and_wildcard() {
        let p = temp_keys_path("validate");
        assert!(keys_add(&p, "t", "superuser", None).is_err());
        assert!(keys_add(&p, "*", "write", None).is_err());
        assert!(keys_add(&p, "*", "admin", None).is_ok());
        std::fs::remove_file(&p).ok();
    }
}
