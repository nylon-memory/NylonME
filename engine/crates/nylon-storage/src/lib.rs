//! 持久化图存储：内存图 + WAL 预写日志 + 快照。
//!
//! 对应《尼龙技术架构 v0.2》持久化设计（Week 7-8）：
//! - 写入先落 WAL（append 即 fsync），再改内存图 —— 崩溃后重放 WAL 恢复；
//! - checkpoint() 把当前图状态写成快照并清空 WAL，控制恢复时的重放量；
//! - WAL 重放在第一条 CRC 损坏/不完整记录处截断，容忍崩溃半写。
//!
//! 注：架构长期目标是自研 LSM-Tree（见架构文档存储章节），本 crate 是
//! Phase 1 的嵌入式持久化基线（ RocksDB 单层存储的等效自研替代），
//! API 面向后续存储引擎演进保持稳定。

mod crc32;
mod wal;

use nylon_core::{decode_nodes, encode_nodes, MemoryNode};
use nylon_graph::MemoryGraph;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
pub use wal::DurabilityTicket;
use wal::{Wal, WalOp};

pub const SNAPSHOT_FILE: &str = "graph.snp";
const SNAPSHOT_MAGIC: &[u8; 4] = b"SNP0";

/// 带持久化的记忆图：写路径 WAL 先行（组提交批量刷盘），读路径直接走内存图。
pub struct PersistentGraph {
    graph: MemoryGraph,
    wal: Wal,
    dir: PathBuf,
}

impl PersistentGraph {
    /// 打开（或创建）目录下的持久化图：先加载快照，再重放 WAL。
    pub fn open(dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let mut graph = load_snapshot(&dir.join(SNAPSHOT_FILE))?;
        let (wal, ops) = Wal::open(&dir)?;
        for op in ops {
            apply(&mut graph, op);
        }
        Ok(PersistentGraph { graph, wal, dir })
    }

    /// 只读访问内存图。
    pub fn graph(&self) -> &MemoryGraph {
        &self.graph
    }

    /// 写入节点（WAL 先行入队），返回分片内局部 ID 与持久化票据。
    pub fn add_node(&mut self, node: MemoryNode) -> io::Result<(u32, DurabilityTicket)> {
        let local = self.graph.peek_next_local_id();
        let ticket = self.wal.append(&WalOp::PutNode { id: local, node: node.clone() })?;
        self.graph.add_node_with_id(local, node);
        Ok((local, ticket))
    }

    /// 写入/更新边（WAL-first）。重复边视为权重更新，与内存图语义一致。
    pub fn add_edge(&mut self, from: u32, to: u32, weight: f32) -> io::Result<DurabilityTicket> {
        let ticket = self.wal.append(&WalOp::PutEdge { from, to, weight })?;
        self.graph.add_edge(from, to, weight);
        Ok(ticket)
    }

    /// 更新节点（WAL-first）。节点不存在或已删除时返回 false。
    pub fn update_node(&mut self, local_id: u32, node: MemoryNode) -> io::Result<(bool, DurabilityTicket)> {
        let ticket = self.wal.append(&WalOp::UpdateNode { id: local_id, node: node.clone() })?;
        let applied = self.graph.update_node(local_id, node);
        Ok((applied, ticket))
    }

    /// 逻辑删除节点（WAL-first），物理清理由 compact() 完成。
    pub fn remove_node(&mut self, local_id: u32) -> io::Result<(bool, DurabilityTicket)> {
        let ticket = self.wal.append(&WalOp::RemoveNode { id: local_id })?;
        let applied = self.graph.remove_node(local_id);
        Ok((applied, ticket))
    }

    /// 删除边（WAL-first）。
    pub fn remove_edge(&mut self, from: u32, to: u32) -> io::Result<(bool, DurabilityTicket)> {
        let ticket = self.wal.append(&WalOp::RemoveEdge { from, to })?;
        let applied = self.graph.remove_edge(from, to);
        Ok((applied, ticket))
    }

    /// Delta 合并进 CSR（纯内存操作，不改变逻辑状态，无需记 WAL）。
    pub fn compact(&mut self) {
        self.graph.compact();
    }

    /// 写快照并清空 WAL。快照先写临时文件再 rename，崩溃不会留下半份快照。
    pub fn checkpoint(&mut self) -> io::Result<()> {
        let path = self.dir.join(SNAPSHOT_FILE);
        let tmp = self.dir.join("graph.snp.tmp");
        fs::write(&tmp, encode_snapshot(&self.graph))?;
        // Windows 上 rename 不能覆盖已存在文件，先删旧快照
        if path.exists() {
            fs::remove_file(&path)?;
        }
        fs::rename(&tmp, &path)?;
        self.wal.truncate()
    }
}

/// 把一条 WAL 操作应用到内存图（重放用，幂等安全）。
fn apply(graph: &mut MemoryGraph, op: WalOp) {
    match op {
        WalOp::PutNode { id, node } => graph.add_node_with_id(id, node),
        WalOp::UpdateNode { id, node } => {
            graph.update_node(id, node);
        }
        WalOp::RemoveNode { id } => {
            graph.remove_node(id);
        }
        WalOp::PutEdge { from, to, weight } => graph.add_edge(from, to, weight),
        WalOp::RemoveEdge { from, to } => {
            graph.remove_edge(from, to);
        }
    }
}

// ---------- 快照编解码 ----------
// 格式：SNP0 | next_local_id u32 | node_count u32
//       | (local_id u32, payload_len u32, .nylon 编码节点)* 
//       | edge_count u32 | (from u32, to u32, weight f32)*

fn encode_snapshot(graph: &MemoryGraph) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(SNAPSHOT_MAGIC);
    buf.extend_from_slice(&graph.peek_next_local_id().to_le_bytes());
    let nodes: Vec<(u32, MemoryNode)> =
        graph.live_nodes().map(|(id, n)| (id, n.clone())).collect();
    buf.extend_from_slice(&(nodes.len() as u32).to_le_bytes());
    for (id, node) in &nodes {
        buf.extend_from_slice(&id.to_le_bytes());
        let nb = encode_nodes(std::slice::from_ref(node));
        buf.extend_from_slice(&(nb.len() as u32).to_le_bytes());
        buf.extend_from_slice(&nb);
    }
    let edges = graph.edges();
    buf.extend_from_slice(&(edges.len() as u32).to_le_bytes());
    for (s, t, w) in edges {
        buf.extend_from_slice(&s.to_le_bytes());
        buf.extend_from_slice(&t.to_le_bytes());
        buf.extend_from_slice(&w.to_le_bytes());
    }
    buf
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.pos.checked_add(n)? > self.buf.len() {
            return None;
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn load_snapshot(path: &Path) -> io::Result<MemoryGraph> {
    let mut graph = MemoryGraph::new();
    let mut buf = Vec::new();
    match fs::File::open(path) {
        Ok(mut f) => {
            f.read_to_end(&mut buf)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(graph),
        Err(e) => return Err(e),
    }
    if buf.len() < 12 || &buf[..4] != SNAPSHOT_MAGIC {
        return Err(invalid("bad snapshot header"));
    }
    let mut c = Cursor { buf: &buf, pos: 4 };
    let next_local_id = c.u32().ok_or_else(|| invalid("truncated snapshot"))?;
    let node_count = c.u32().ok_or_else(|| invalid("truncated snapshot"))? as usize;
    for _ in 0..node_count {
        let id = c.u32().ok_or_else(|| invalid("truncated snapshot node"))?;
        let len = c.u32().ok_or_else(|| invalid("truncated snapshot node"))? as usize;
        let payload = c.take(len).ok_or_else(|| invalid("truncated snapshot node"))?;
        let nodes = decode_nodes(payload).map_err(|e| invalid(&format!("snapshot node decode: {e}")))?;
        if nodes.len() != 1 {
            return Err(invalid("snapshot node payload count mismatch"));
        }
        graph.add_node_with_id(id, nodes.into_iter().next().unwrap());
    }
    // ID 分配器必须恢复到快照时刻的值（尾部可能有已物理删除的 ID）
    graph.restore_next_local_id(next_local_id);
    let edge_count = c.u32().ok_or_else(|| invalid("truncated snapshot edges"))? as usize;
    for _ in 0..edge_count {
        let from = c.u32().ok_or_else(|| invalid("truncated snapshot edge"))?;
        let to = c.u32().ok_or_else(|| invalid("truncated snapshot edge"))?;
        let w = c.f32().ok_or_else(|| invalid("truncated snapshot edge"))?;
        graph.add_edge(from, to, w);
    }
    Ok(graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nylon_core::{Filaments, Tension};
    use std::io::Write;

    fn node(id: u64, fact: &str) -> MemoryNode {
        MemoryNode {
            id,
            owner_id: "test".into(),
            filaments: Filaments {
                fact: fact.into(),
                emotion_valence: 0.5,
                emotion_intensity: 0.5,
                created_at: 1000 + id as i64,
                decay_rate: 0.01,
                relations: vec!["测试".into()],
                confidence: 0.9,
                mentions_7d: 1,
            },
            tension: Tension { baseline: 0.8, last_updated: 0 },
            embedding: vec![1.0, 2.0, 3.0],
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("nylon-storage-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    /// checkpoint 后重开：状态一致，且新写入的 ID 从快照位置继续分配。
    #[test]
    fn reopen_after_checkpoint() {
        let dir = temp_dir("checkpoint");
        {
            let mut pg = PersistentGraph::open(&dir).unwrap();
            let (a, _ticket_a) = pg.add_node(node(1, "甲")).unwrap();
            let (b, _ticket_b) = pg.add_node(node(2, "乙")).unwrap();
            pg.add_edge(a, b, 0.9).unwrap();
            pg.checkpoint().unwrap();
        }
        let mut pg = PersistentGraph::open(&dir).unwrap();
        assert_eq!(pg.graph().node_count(), 2);
        assert_eq!(pg.graph().get_node(0).unwrap().filaments.fact, "甲");
        assert_eq!(pg.graph().edges(), vec![(0, 1, 0.9)]);
        // ID 单调：新节点应拿到 2 而不是复用 0
        let (c, _ticket_c) = pg.add_node(node(3, "丙")).unwrap();
        assert_eq!(c, 2);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 不 checkpoint 直接退出：全靠 WAL 重放恢复。
    #[test]
    fn crash_recovery_without_checkpoint() {
        let dir = temp_dir("crash");
        {
            let mut pg = PersistentGraph::open(&dir).unwrap();
            for i in 0..3u64 {
                pg.add_node(node(i, &format!("节点{i}"))).unwrap();
            }
            pg.add_edge(0, 1, 0.5).unwrap();
            pg.add_edge(1, 2, 0.6).unwrap();
            // 无 checkpoint，模拟崩溃退出
        }
        let pg = PersistentGraph::open(&dir).unwrap();
        assert_eq!(pg.graph().node_count(), 3);
        assert_eq!(pg.graph().edges().len(), 2);
        assert_eq!(pg.graph().get_node(2).unwrap().filaments.fact, "节点2");
        let _ = fs::remove_dir_all(&dir);
    }

    /// WAL 尾部写入垃圾字节（模拟崩溃半写）：重放应在损坏处截断，已有记录不受影响。
    #[test]
    fn wal_tail_corruption_truncated() {
        let dir = temp_dir("corrupt");
        {
            let mut pg = PersistentGraph::open(&dir).unwrap();
            pg.add_node(node(1, "完好的")).unwrap();
            pg.add_node(node(2, "也在")).unwrap();
        }
        // 追加半条损坏记录
        let wal_path = dir.join(wal::WAL_FILE);
        fs::OpenOptions::new().append(true).open(&wal_path).unwrap()
            .write_all(&[0xDE, 0xAD, 0xBE]).unwrap();
        let pg = PersistentGraph::open(&dir).unwrap();
        assert_eq!(pg.graph().node_count(), 2);
        // 损坏尾部已被截断，继续写入不受影响
        drop(pg);
        let mut pg = PersistentGraph::open(&dir).unwrap();
        pg.add_node(node(3, "截断后再写")).unwrap();
        drop(pg);
        let pg = PersistentGraph::open(&dir).unwrap();
        assert_eq!(pg.graph().node_count(), 3);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 删除操作持久化：删节点 + 删边后 checkpoint，重开不复活。
    #[test]
    fn removals_persist() {
        let dir = temp_dir("remove");
        {
            let mut pg = PersistentGraph::open(&dir).unwrap();
            pg.add_node(node(1, "留")).unwrap();
            pg.add_node(node(2, "删")).unwrap();
            pg.add_node(node(3, "也留")).unwrap();
            pg.add_edge(0, 1, 0.5).unwrap();
            pg.add_edge(0, 2, 0.7).unwrap();
            assert!(pg.remove_node(1).unwrap().0);
            assert!(pg.remove_edge(0, 2).unwrap().0);
            pg.checkpoint().unwrap();
        }
        let pg = PersistentGraph::open(&dir).unwrap();
        assert_eq!(pg.graph().node_count(), 2);
        assert!(pg.graph().get_node(1).is_none());
        assert!(pg.graph().edges().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// 更新操作持久化：update_node 与边权重更新在重开后生效。
    #[test]
    fn updates_persist() {
        let dir = temp_dir("update");
        {
            let mut pg = PersistentGraph::open(&dir).unwrap();
            pg.add_node(node(1, "旧事实")).unwrap();
            pg.add_node(node(2, "目标")).unwrap();
            pg.add_edge(0, 1, 0.3).unwrap();
            let mut updated = node(1, "新事实");
            updated.filaments.confidence = 0.5;
            assert!(pg.update_node(0, updated).unwrap().0);
            pg.add_edge(0, 1, 0.95).unwrap(); // 重复边 = 权重更新
            pg.checkpoint().unwrap();
        }
        let pg = PersistentGraph::open(&dir).unwrap();
        let n = pg.graph().get_node(0).unwrap();
        assert_eq!(n.filaments.fact, "新事实");
        assert_eq!(n.filaments.confidence, 0.5);
        assert_eq!(pg.graph().edges(), vec![(0, 1, 0.95)]);
        let _ = fs::remove_dir_all(&dir);
    }
}
