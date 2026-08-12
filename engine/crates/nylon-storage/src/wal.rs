//! 预写日志（WAL）：追加式操作日志 + 组提交（group commit）。
//!
//! 记录帧格式：`[payload_len u32][crc32 u32][payload]`。
//! 重放时在第一条 CRC 不匹配或不完整的记录处截断（崩溃半写恢复）。
//!
//! 写路径（Phase 2 起）：单独的写线程独占文件句柄；调用方把编码后的记录
//! 通过通道递交，立即返回一张 [`DurabilityTicket`]。写线程把累积的一批记录一次性
//! 写入并只做一次 fsync，然后统一应答该批的所有票据——并发写共享同一次刷盘。
//! 调用方在应答客户端前等待自己最后一张票据，持久化语义不变；
//! 但内存图可能短暂超前于已刷盘状态（未等票的读会看到尚未持久化的数据）。
//! 读取器进程崩溃只会丢失“未应答”的写入，与客户端观察一致。
use crate::crc32::crc32;
use nylon_core::{decode_nodes, encode_nodes, MemoryNode};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

pub const WAL_FILE: &str = "wal.log";
/// 单条记录阐值上限 64MB（防损坏数据导致巨额分配）
const MAX_RECORD: usize = 64 * 1024 * 1024;
/// 单次 fsync 最多合并的记录数。
const MAX_BATCH: usize = 1024;

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
    if nodes.len() == 1 {
        nodes.into_iter().next()
    } else {
        None
    }
}

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn encode_op(op: &WalOp) -> Vec<u8> {
    let mut buf = Vec::new();
    match op {
        WalOp::PutNode { id, node } | WalOp::UpdateNode { id, node } => {
            buf.push(if matches!(op, WalOp::PutNode { .. }) {
                1
            } else {
                5
            });
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
    let mut r = Reader {
        buf: payload,
        pos: 0,
    };
    let tag = *r.take(1)?.first()?;
    match tag {
        1 | 5 => {
            let id = r.u32()?;
            let len = r.u32()? as usize;
            let node = decode_node(r.take(len)?)?;
            Some(if tag == 1 {
                WalOp::PutNode { id, node }
            } else {
                WalOp::UpdateNode { id, node }
            })
        }
        2 => Some(WalOp::RemoveNode { id: r.u32()? }),
        3 => Some(WalOp::PutEdge {
            from: r.u32()?,
            to: r.u32()?,
            weight: r.f32()?,
        }),
        4 => Some(WalOp::RemoveEdge {
            from: r.u32()?,
            to: r.u32()?,
        }),
        _ => None,
    }
}

/// 持久化票据：一条 WAL 记录已刷盘的凭证。
///
/// 通道保序：等待某个节点的最后一张票据，即覆盖其之前提交的所有记录。
/// 丢弃票据 = 不等待该记录的刷盘确认（适用于后续还会等更靠后票据的场景）。
pub struct DurabilityTicket {
    state: Arc<(Mutex<Option<io::Result<()>>>, Condvar)>,
}

/// 写线程使用的应答端。
struct Ack {
    state: Arc<(Mutex<Option<io::Result<()>>>, Condvar)>,
}

impl Ack {
    fn pair() -> (Ack, DurabilityTicket) {
        let state = Arc::new((Mutex::new(None), Condvar::new()));
        (
            Ack {
                state: state.clone(),
            },
            DurabilityTicket { state },
        )
    }
    fn reply(self, r: io::Result<()>) {
        let (lock, cv) = &*self.state;
        *lock.lock().unwrap() = Some(r);
        cv.notify_one();
    }
}

impl DurabilityTicket {
    /// 阻塞等待该记录（及其之前的全部记录）刷盘完成。
    pub fn wait(self) -> io::Result<()> {
        let (lock, cv) = &*self.state;
        let mut guard = lock.lock().unwrap();
        while guard.is_none() {
            guard = cv.wait(guard).unwrap();
        }
        guard.take().unwrap()
    }
}

/// 写线程的工作单元。
enum WalMsg {
    /// 已编码的帧（含长度与 CRC）。
    Append { frame: Vec<u8>, ack: Ack },
    /// 在此之前的记录全部刷盘后清空日志（快照完成后调用）。
    Truncate { ack: Ack },
}

fn frame(op: &WalOp) -> Vec<u8> {
    let payload = encode_op(op);
    let mut rec = Vec::with_capacity(payload.len() + 8);
    rec.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    rec.extend_from_slice(&crc32(&payload).to_le_bytes());
    rec.extend_from_slice(&payload);
    rec
}

/// 重放全部有效记录；在第一条损坏/不完整记录处截断文件并返回已验证的前缀。
fn replay_file(file: &mut File) -> io::Result<Vec<WalOp>> {
    let mut buf = Vec::new();
    file.seek(SeekFrom::Start(0))?;
    file.read_to_end(&mut buf)?;
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
        file.set_len(valid_end as u64)?; // 截断损坏尾部
        file.sync_all()?;
    }
    Ok(ops)
}

fn writer_loop(mut file: File, fsync: bool, rx: Receiver<WalMsg>) {
    while let Ok(first) = rx.recv() {
        match first {
            WalMsg::Truncate { ack } => {
                let r = file.set_len(0).and_then(|_| file.sync_all());
                ack.reply(r);
            }
            WalMsg::Append { frame: f0, ack: a0 } => {
                // 组提交：掘起当前已排队的记录，一次写入 + 一次 fsync
                let mut frames = vec![f0];
                let mut acks = vec![a0];
                while frames.len() < MAX_BATCH {
                    match rx.try_recv() {
                        Ok(WalMsg::Append { frame, ack }) => {
                            frames.push(frame);
                            acks.push(ack);
                        }
                        Ok(WalMsg::Truncate { ack }) => {
                            flush(&mut file, fsync, &mut frames, &mut acks);
                            let r = file.set_len(0).and_then(|_| file.sync_all());
                            ack.reply(r);
                        }
                        Err(_) => break,
                    }
                }
                flush(&mut file, fsync, &mut frames, &mut acks);
            }
        }
    }
    // 通道断开（Wal 被 drop）：队列已清空，退出。
}

fn flush(file: &mut File, fsync: bool, frames: &mut Vec<Vec<u8>>, acks: &mut Vec<Ack>) {
    if frames.is_empty() {
        return;
    }
    let mut result: io::Result<()> = Ok(());
    for f in frames.iter() {
        if let Err(e) = file.write_all(f) {
            result = Err(e);
            break;
        }
    }
    if result.is_ok() && fsync {
        if let Err(e) = file.sync_all() {
            result = Err(e);
        }
    }
    for ack in acks.drain(..) {
        let r = match &result {
            Ok(()) => Ok(()),
            Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
        };
        ack.reply(r);
    }
    frames.clear();
}

/// WAL 文件句柄（前端：通道 + 写线程）。
pub struct Wal {
    tx: Option<Sender<WalMsg>>,
    handle: Option<JoinHandle<()>>,
}

impl Wal {
    /// 打开（或创建）日志，重放并截断损坏尾部，然后启动写线程。
    ///
    /// 诊断旋钮 `NYLON_WAL_NO_FSYNC=1` 可关闭 fsync，仅用于基准 profiling（区分
    /// fsync 成本），生产环境严禁使用。
    pub fn open(dir: &Path) -> io::Result<(Wal, Vec<WalOp>)> {
        let path = dir.join(WAL_FILE);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let ops = replay_file(&mut file)?;
        file.seek(SeekFrom::End(0))?;
        let fsync = std::env::var_os("NYLON_WAL_NO_FSYNC").is_none();
        let (tx, rx) = channel();
        let handle = std::thread::Builder::new()
            .name("nylon-wal-writer".into())
            .spawn(move || writer_loop(file, fsync, rx))?;
        Ok((
            Wal {
                tx: Some(tx),
                handle: Some(handle),
            },
            ops,
        ))
    }

    /// 提交一条操作，立即返回持久化票据（不阻塞等刷盘）。
    pub fn append(&self, op: &WalOp) -> io::Result<DurabilityTicket> {
        let (ack, ticket) = Ack::pair();
        let tx = self.tx.as_ref().expect("wal open");
        tx.send(WalMsg::Append {
            frame: frame(op),
            ack,
        })
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "wal 写线程已退出"))?;
        Ok(ticket)
    }

    /// 清空日志（快照完成后调用）；阻塞等待此前记录全部落盘后截断。
    pub fn truncate(&self) -> io::Result<()> {
        let (ack, ticket) = Ack::pair();
        let tx = self.tx.as_ref().expect("wal open");
        tx.send(WalMsg::Truncate { ack })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "wal 写线程已退出"))?;
        ticket.wait()
    }
}

impl Drop for Wal {
    fn drop(&mut self) {
        // 断开通道：写线程在处理完队列中剩余记录后退出（退出即保证已刷盘）。
        drop(self.tx.take());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
