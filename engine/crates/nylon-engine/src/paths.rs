//! 数据目录解析（2026-10 根治"换目录启动 = 空库"，公众号用户实踩案例）。
//!
//! serve 模式历史默认 `./nylon-data` 相对**启动时 CWD**：解压 release 到新文件夹
//! 再启动就会读到全新空库（数据没丢，在老目录，但用户视角就是"升级干崩"）。
//! 新策略（v0.4.1 起）：`./nylon-data` 已存在则沿用（老用户无感），否则与 MCP
//! 模式对齐落到 `~/.nylonme/data`；显式 `NYLON_DATA_DIR` 永远优先。

use std::path::Path;

/// serve 数据目录的最终选择。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeDataDir {
    /// 显式 NYLON_DATA_DIR（永远优先）。
    Explicit(String),
    /// ./nylon-data 已存在（目录），向后兼容沿用；调用方应打 warning 建议固定绝对路径。
    LegacyCwd(String),
    /// 全新部署：与 MCP 模式对齐，落 ~/.nylonme/data。
    Home(String),
}

/// 解析 serve 模式数据目录。`cwd` 与 `home` 抽成参数是为了纯函数可测。
pub fn resolve_serve_data_dir(
    env_val: Option<String>,
    cwd: &Path,
    home: Option<String>,
) -> ServeDataDir {
    if let Some(v) = env_val {
        return ServeDataDir::Explicit(v);
    }
    if cwd.join("nylon-data").is_dir() {
        return ServeDataDir::LegacyCwd("./nylon-data".into());
    }
    let home = home.unwrap_or_else(|| ".".into());
    ServeDataDir::Home(format!("{home}/.nylonme/data"))
}

/// 用户家目录（Windows 优先 USERPROFILE，Unix 用 HOME）。
pub fn home_dir() -> Option<String> {
    std::env::var("USERPROFILE")
        .ok()
        .or_else(|| std::env::var("HOME").ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_env_always_wins() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nylon-data")).unwrap();
        let r = resolve_serve_data_dir(
            Some("/var/lib/nylon".into()),
            dir.path(),
            Some("/home/u".into()),
        );
        assert_eq!(r, ServeDataDir::Explicit("/var/lib/nylon".into()));
    }

    #[test]
    fn legacy_cwd_dir_is_honored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nylon-data")).unwrap();
        let r = resolve_serve_data_dir(None, dir.path(), Some("/home/u".into()));
        assert_eq!(r, ServeDataDir::LegacyCwd("./nylon-data".into()));
    }

    #[test]
    fn fresh_deploy_falls_back_to_home() {
        let dir = tempfile::tempdir().unwrap();
        let r = resolve_serve_data_dir(None, dir.path(), Some("/home/u".into()));
        assert_eq!(r, ServeDataDir::Home("/home/u/.nylonme/data".into()));
    }

    #[test]
    fn stray_file_named_nylon_data_does_not_count() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("nylon-data"), b"not a dir").unwrap();
        let r = resolve_serve_data_dir(None, dir.path(), Some("/home/u".into()));
        assert_eq!(r, ServeDataDir::Home("/home/u/.nylonme/data".into()));
    }

    #[test]
    fn no_home_defaults_to_dot() {
        let dir = tempfile::tempdir().unwrap();
        let r = resolve_serve_data_dir(None, dir.path(), None);
        assert_eq!(r, ServeDataDir::Home("./.nylonme/data".into()));
    }
}
