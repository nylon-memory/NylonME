//! 审计事件流（Phase 2 L2.3）：append-only 操作日志，UI/REST 可查询。
//!
//! - 每个 RPC 成功或鉴权拒绝都会落一条事件：时间戳 + action + tenant/owner + 细节；
//! - 写路径：内存环形缓冲（供 UI 快速查询）+ JSONL 文件追加（持久化全量历史），
//!   后台任务批量 flush，RPC 路径零磁盘 IO；
//! - 启动时从 JSONL 尾部预载最近事件，重启后 UI 不留白；
//! - `NYLON_AUDIT=off` 整体关闭；文件轮转留给后续版本（当前 JSONL 单调追加）。

use serde::Serialize;
use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// 一条审计事件。
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
pub struct AuditEvent {
    pub ts: i64,
    pub action: String,
    pub tenant: String,
    pub owner: String,
    pub detail: String,
}

/// 审计汇：emit 非阻塞（channel 入队即返），查询走内存环形缓冲。
#[derive(Clone)]
pub struct Audit {
    tx: tokio::sync::mpsc::UnboundedSender<AuditEvent>,
    recent: Arc<Mutex<VecDeque<AuditEvent>>>,
}

/// 环形缓冲容量（UI 查询窗口）。
const RING_CAP: usize = 5000;
/// 启动预载读取的文件尾部窗口。
const PRELOAD_BYTES: u64 = 256 * 1024;

impl Audit {
    /// 启动审计流：JSONL 落在数据目录 audit.jsonl。
    /// `NYLON_AUDIT=off` 时返回 None（完全关闭）。
    pub fn start(data_dir: &Path) -> Option<Self> {
        let disabled = std::env::var("NYLON_AUDIT")
            .map(|v| v == "off")
            .unwrap_or(false);
        Self::start_inner(data_dir, disabled)
    }

    /// 与启动逻辑分离的开关（测试可直接驱动，避免进程级 env 竞态）。
    fn start_inner(data_dir: &Path, disabled: bool) -> Option<Self> {
        if disabled {
            return None;
        }
        let path = data_dir.join("audit.jsonl");
        let recent = Arc::new(Mutex::new(preload(&path, RING_CAP)));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AuditEvent>();
        let ring = Arc::clone(&recent);
        tokio::spawn(async move {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap_or_else(|e| panic!("打开审计日志 {} 失败: {e}", path.display()));
            let mut flush_tick = tokio::time::interval(std::time::Duration::from_secs(1));
            flush_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    ev = rx.recv() => {
                        let Some(ev) = ev else { break };
                        if let Ok(line) = serde_json::to_string(&ev) {
                            let _ = file.write_all(line.as_bytes());
                            let _ = file.write_all(b"\n");
                        }
                        let mut g = ring.lock().unwrap_or_else(|p| p.into_inner());
                        if g.len() >= RING_CAP {
                            g.pop_front();
                        }
                        g.push_back(ev);
                    }
                    _ = flush_tick.tick() => {
                        let _ = file.flush();
                    }
                }
            }
            let _ = file.flush();
        });
        Some(Self { tx, recent })
    }

    /// 记录一条事件（非阻塞；后台任务落盘 + 入环）。
    pub fn emit(&self, action: &str, tenant: &str, owner: &str, detail: String) {
        let ev = AuditEvent {
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            action: action.to_string(),
            tenant: tenant.to_string(),
            owner: owner.to_string(),
            detail,
        };
        // 接收端关闭（引擎停机中）时静默丢弃
        let _ = self.tx.send(ev);
    }

    /// 查询最近事件（最新在前）。tenant/owner/action 为可选精确过滤。
    pub fn query(
        &self,
        tenant: Option<&str>,
        owner: Option<&str>,
        action: Option<&str>,
        limit: usize,
    ) -> Vec<AuditEvent> {
        let g = self.recent.lock().unwrap_or_else(|p| p.into_inner());
        g.iter()
            .rev()
            .filter(|e| tenant.is_none_or(|t| e.tenant == t))
            .filter(|e| owner.is_none_or(|o| e.owner == o))
            .filter(|e| action.is_none_or(|a| e.action == a))
            .take(limit)
            .cloned()
            .collect()
    }
}

/// 启动预载：读文件尾部窗口，跳过第一条可能不完整的行，解析剩余 JSONL。
fn preload(path: &Path, cap: usize) -> VecDeque<AuditEvent> {
    let mut out = VecDeque::new();
    let Ok(meta) = std::fs::metadata(path) else {
        return out;
    };
    let size = meta.len();
    if size == 0 {
        return out;
    }
    let start = size.saturating_sub(PRELOAD_BYTES);
    let Ok(mut file) = std::fs::File::open(path) else {
        return out;
    };
    use std::io::{Read, Seek};
    if file.seek(std::io::SeekFrom::Start(start)).is_err() {
        return out;
    }
    let mut buf = String::new();
    if file.read_to_string(&mut buf).is_err() {
        return out;
    }
    for (i, line) in buf.lines().enumerate() {
        // 窗口中间切入时第一条行残缺，跳过
        if start > 0 && i == 0 {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<AuditEvent>(line) {
            if out.len() >= cap {
                out.pop_front();
            }
            out.push_back(ev);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn emit_and_query_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let audit = Audit::start(dir.path()).unwrap();
        audit.emit("weave", "t1", "alice", "node=3 linked=2".into());
        audit.emit("resonate", "t1", "bob", "hits=5".into());
        audit.emit("weave", "t2", "carol", "node=1".into());
        // 等后台任务消费
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let all = audit.query(None, None, None, 10);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].action, "weave"); // 最新在前
        assert_eq!(all[0].tenant, "t2");

        let t1 = audit.query(Some("t1"), None, None, 10);
        assert_eq!(t1.len(), 2);
        let alice_weaves = audit.query(Some("t1"), Some("alice"), Some("weave"), 10);
        assert_eq!(alice_weaves.len(), 1);
        assert_eq!(alice_weaves[0].detail, "node=3 linked=2");

        // JSONL 已落盘
        let raw = std::fs::read_to_string(dir.path().join("audit.jsonl")).unwrap();
        assert_eq!(raw.lines().count(), 3);
    }

    #[tokio::test]
    async fn preload_restores_recent_events() {
        let dir = tempfile::tempdir().unwrap();
        {
            let audit = Audit::start(dir.path()).unwrap();
            audit.emit("weave", "t1", "alice", "第一次运行".into());
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        } // 模拟进程退出
        let audit = Audit::start(dir.path()).unwrap();
        let all = audit.query(None, None, None, 10);
        assert_eq!(all.len(), 1, "重启后应从 JSONL 预载历史事件");
        assert_eq!(all[0].detail, "第一次运行");
    }

    #[tokio::test]
    async fn disabled_by_env() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Audit::start_inner(dir.path(), true).is_none());
        assert!(Audit::start_inner(dir.path(), false).is_some());
    }
}
