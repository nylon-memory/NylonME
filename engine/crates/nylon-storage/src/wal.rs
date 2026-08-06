//! 预写日志（WAL）：追加式操作日志。
//! 记录帧格式：`[payload_len u32][crc32 u32][payload]`，追加即 fsync。
//! 重放时在第一条 CRC 不匹配或不完整的记录处截断（崩溃半写恢复）。

use crate::crc32::crc32;
use nylon_core::{decode_nodes, encode_nodes, MemoryNode};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const WAL_FILE: &str = "wal.log";
/// 单条记录防御上限 64MB（防损坏数据导致巨额分配）
const MAX_RECORD: usize = 64 * 1024 * 1024;

/// 持久化操作。
#[derive(Debug, Clone, PartialEq)]
pub enum WalOp {
    PutNode { id: u32, node: MemoryNode },
    RemoveNode { id: u32 },
    PutEdge { from: u32, to: u32, weight: f32 },
    RemoveEdge { from: u32, to: u32 },
    UpdateNode { id: u32, node: MemoryNode },
}

fn encode_node(node: &MemoryNode) -> Vec<u8> {
    encode_nodes(std::slice::from_ref(node))
}

fn decode_node(buf: &[u8]) -> Option<MemoryNode> {
    let nodes = decode_nodes(buf).ok()?;
    if nodes.len() == 1 { nodes.into_iter().next() } else { None }
}

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn encode_op(op: &WalOp) -> Vec<u8> {
    let mut buf = Vec::new();
    match op {
        WalOp::PutNode { id, node } | WalOp::UpdateNode { id, node } => {
            buf.push(if matches!(op, WalOp::PutNode { .. }) { 1 } else { 5 });
            push_u32(&mut buf, *id);
            let nb = encode_node(node);
            push_u32(&mut buf, nb.len() as u32);
            buf.extend_from_slice(&nb);
        }
        WalOp::RemoveNode { id } => {
            buf.push(2);
            push_u32(&mut buf, *id);
        }
        WalOp::PutEdge { from, to, weight } => {
            buf.push(3);
            push_u32(&mut buf, *from);
            push_u32(&mut buf, *to);
            buf.extend_from_slice(&weight.to_le_bytes());
        }
        WalOp::RemoveEdge { from, to } => {
            buf.push(4);
            push_u32(&mut buf, *from);
            push_u32(&mut buf, *to);
        }
    }
    buf
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.pos + n > self.buf.len() {
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

fn decode_op(payload: &[u8]) -> Option<WalOp> {
    let mut r = Reader { buf: payload, pos: 0 };
    let tag = *r.take(1)?.first()?;
    match tag {
        1 | 5 => {
            let id = r.u32()?;
            let len = r.u32()? as usize;
            let node = decode_node(r.take(len)?)?;
            Some(if tag == 1 { WalOp::PutNode { id, node } } else { WalOp::UpdateNode { id, node } })
        }
        2 => Some(WalOp::RemoveNode { id: r.u32()? }),
        3 => Some(WalOp::PutEdge { from: r.u32()?, to: r.u32()?, weight: r.f32()? }),
        4 => Some(WalOp::RemoveEdge { from: r.u32()?, to: r.u32()? }),
        _ => None,
    }
}

/// WAL 文件句柄。
pub struct Wal {
    file: File,
    /// 是否每次 append 后 fsync。诊断旋钮 NYLON_WAL_NO_FSYNC=1 可关闭，
    /// 仅用于基准 profiling（区分 fsync 成本），生产环境严禁使用。
    fsync: bool,
}

impl Wal {
    pub fn open(dir: &Path) -> io::Result<Wal> {
        let path = dir.join(WAL_FILE);
        let file = OpenOptions::new().read(true).write(true).create(true).open(path)?;
        let fsync = std::env::var_os("NYLON_WAL_NO_FSYNC").is_none();
        Ok(Wal { file, fsync })
    }

    /// 追加一条操作并 fsync（先写日志后改内存，保证崩溃可恢复）。
    pub fn append(&mut self, op: &WalOp) -> io::Result<()> {
        let payload = encode_op(op);
        let mut rec = Vec::with_capacity(payload.len() + 8);
        rec.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        rec.extend_from_slice(&crc32(&payload).to_le_bytes());
        rec.extend_from_slice(&payload);
        self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(&rec)?;
        if self.fsync {
            self.file.sync_all()?;
        }
        Ok(())
    }

    /// 重放全部有效记录；在第一条损坏/不完整记录处截断文件并返回已验证的前缀。
    pub fn replay(&mut self) -> io::Result<Vec<WalOp>> {
        let mut buf = Vec::new();
        self.file.seek(SeekFrom::Start(0))?;
        self.file.read_to_end(&mut buf)?;
        let mut ops = Vec::new();
        let mut pos = 0usize;
        let mut valid_end = 0usize;
        while pos + 8 <= buf.len() {
            let len = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
            let crc = u32::from_le_bytes(buf[pos + 4..pos + 8].try_into().unwrap());
            if len > MAX_RECORD || pos + 8 + len > buf.len() {
                break; // 长度异常或记录不完整：崩溃半写
            }
            let payload = &buf[pos + 8..pos + 8 + len];
            if crc32(payload) != crc {
                break; // CRC 不匹配：损坏
            }
            match decode_op(payload) {
                Some(op) => ops.push(op),
                None => break,
            }
            pos += 8 + len;
            valid_end = pos;
        }
        if valid_end < buf.len() {
            self.file.set_len(valid_end as u64)?; // 截断损坏尾部
            self.file.sync_all()?;
        }
        Ok(ops)
    }

    /// 清空日志（快照完成后调用）。
    pub fn truncate(&mut self) -> io::Result<()> {
        self.file.set_len(0)?;
        self.file.sync_all()
    }
}