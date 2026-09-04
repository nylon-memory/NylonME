//! .nylon 二进制编解码（无压缩，小端序）。
//!
//! 对应《尼龙技术架构 v0.2》4.3 节的列式编码思路的简化版：
//! 先保证无损 roundtrip，压缩（Delta/PQ/zstd）留给后续版本。
//!
//! 格式（v1，L2.1 起）：
//! ```text
//! Header:  magic "NYL1" (4B) + node_count (u64)
//! 每节点:  node_id (u64) + tenant_id (str) + owner_id (str) + fact (str)
//!          + emotion_valence (f32) + emotion_intensity (f32)
//!          + created_at (i64) + decay_rate (f32)
//!          + relations (u32 count + str*) + confidence (f32) + mentions_7d (u32)
//!          + tension_baseline (f32) + tension_last_updated (i64)
//!          + embedding (u32 dim + f32*)
//! str = len (u32) + UTF-8 字节
//! ```
//!
//! 兼容：解码同时接受 v0（magic "NYL0"，无 tenant_id 字段），
//! v0 记录的 tenant_id 回填 DEFAULT_TENANT；编码始终写 v1。

use crate::{Filaments, MemoryNode, Tension, DEFAULT_TENANT};

/// v0 格式 magic（无 tenant_id，仅解码兼容）。
pub const MAGIC: &[u8; 4] = b"NYL0";
/// v1 格式 magic（含 tenant_id，L2.1 多租户隔离）。
pub const MAGIC_V1: &[u8; 4] = b"NYL1";

/// 解码错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// magic 头不匹配
    BadMagic,
    /// 数据截断（长度字段超出剩余字节）
    Truncated,
    /// 字符串字段不是合法 UTF-8
    InvalidUtf8,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::BadMagic => write!(f, "bad magic header"),
            DecodeError::Truncated => write!(f, "truncated data"),
            DecodeError::InvalidUtf8 => write!(f, "invalid utf-8 in string field"),
        }
    }
}
impl std::error::Error for DecodeError {}

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn push_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn push_i64(buf: &mut Vec<u8>, v: i64) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn push_f32(buf: &mut Vec<u8>, v: f32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn push_str(buf: &mut Vec<u8>, s: &str) {
    push_u32(buf, s.len() as u32);
    buf.extend_from_slice(s.as_bytes());
}

/// 将一组记忆节点编码为 .nylon 字节流。
pub fn encode_nodes(nodes: &[MemoryNode]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(MAGIC_V1);
    push_u64(&mut buf, nodes.len() as u64);
    for n in nodes {
        let f = &n.filaments;
        push_u64(&mut buf, n.id);
        push_str(&mut buf, &n.tenant_id);
        push_str(&mut buf, &n.owner_id);
        push_str(&mut buf, &f.fact);
        push_f32(&mut buf, f.emotion_valence);
        push_f32(&mut buf, f.emotion_intensity);
        push_i64(&mut buf, f.created_at);
        push_f32(&mut buf, f.decay_rate);
        push_u32(&mut buf, f.relations.len() as u32);
        for r in &f.relations {
            push_str(&mut buf, r);
        }
        push_f32(&mut buf, f.confidence);
        push_u32(&mut buf, f.mentions_7d);
        push_f32(&mut buf, n.tension.baseline);
        push_i64(&mut buf, n.tension.last_updated);
        push_u32(&mut buf, n.embedding.len() as u32);
        for &x in &n.embedding {
            push_f32(&mut buf, x);
        }
    }
    buf
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.pos + n > self.buf.len() {
            return Err(DecodeError::Truncated);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, DecodeError> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, DecodeError> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<String, DecodeError> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::InvalidUtf8)
    }
}

/// 解码 .nylon 字节流为一组记忆节点。
pub fn decode_nodes(buf: &[u8]) -> Result<Vec<MemoryNode>, DecodeError> {
    let mut r = Reader { buf, pos: 0 };
    let magic = r.take(4)?;
    let has_tenant = if magic == MAGIC_V1 {
        true
    } else if magic == MAGIC {
        false // v0：无 tenant 字段，回填默认租户
    } else {
        return Err(DecodeError::BadMagic);
    };
    let count = r.u64()? as usize;
    let mut nodes = Vec::with_capacity(count.min(1 << 20));
    for _ in 0..count {
        let id = r.u64()?;
        let tenant_id = if has_tenant {
            r.string()?
        } else {
            DEFAULT_TENANT.to_string()
        };
        let owner_id = r.string()?;
        let fact = r.string()?;
        let emotion_valence = r.f32()?;
        let emotion_intensity = r.f32()?;
        let created_at = r.i64()?;
        let decay_rate = r.f32()?;
        let rel_count = r.u32()? as usize;
        let mut relations = Vec::with_capacity(rel_count.min(1 << 16));
        for _ in 0..rel_count {
            relations.push(r.string()?);
        }
        let confidence = r.f32()?;
        let mentions_7d = r.u32()?;
        let baseline = r.f32()?;
        let last_updated = r.i64()?;
        let dim = r.u32()? as usize;
        let mut embedding = Vec::with_capacity(dim.min(1 << 16));
        for _ in 0..dim {
            embedding.push(r.f32()?);
        }
        nodes.push(MemoryNode {
            id,
            tenant_id,
            owner_id,
            filaments: Filaments {
                fact,
                emotion_valence,
                emotion_intensity,
                created_at,
                decay_rate,
                relations,
                confidence,
                mentions_7d,
            },
            tension: Tension {
                baseline,
                last_updated,
            },
            embedding,
        });
    }
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(fact: &str, relations: &[&str], embedding: Vec<f32>) -> MemoryNode {
        MemoryNode {
            id: 42,
            tenant_id: "acme".into(),
            owner_id: "alice".into(),
            filaments: Filaments {
                fact: fact.into(),
                emotion_valence: 0.75,
                emotion_intensity: 0.5,
                created_at: 1_754_400_000,
                decay_rate: 0.01,
                relations: relations.iter().map(|s| s.to_string()).collect(),
                confidence: 0.9,
                mentions_7d: 7,
            },
            tension: Tension {
                baseline: 0.8,
                last_updated: 1_754_400_000,
            },
            embedding,
        }
    }

    #[test]
    fn roundtrip_preserves_all_filaments() {
        let nodes = vec![
            sample("用户喜欢 Python", &["编程", "工作"], vec![0.1, 0.2, 0.3]),
            sample("上次出差：上海，住了哪家酒店来着？", &["出差"], vec![]),
            sample(
                "空关系丝",
                &[],
                (0..768).map(|i| i as f32 * 0.001).collect(),
            ),
        ];
        let bytes = encode_nodes(&nodes);
        let back = decode_nodes(&bytes).unwrap();
        assert_eq!(back, nodes, "roundtrip 应完全保真");
    }

    #[test]
    fn empty_set_roundtrip() {
        let bytes = encode_nodes(&[]);
        assert_eq!(decode_nodes(&bytes).unwrap(), vec![]);
    }

    #[test]
    fn rejects_bad_magic() {
        assert_eq!(decode_nodes(b"XXXX1234"), Err(DecodeError::BadMagic));
    }

    /// v0 旧格式（无 tenant 字段）必须能解码，tenant 回填 DEFAULT_TENANT。
    #[test]
    fn decodes_v0_with_default_tenant() {
        // 手工构造 v0 字节流：magic NYL0 + 单节点（owner 之后没有 tenant 字段）
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        push_u64(&mut buf, 1);
        push_u64(&mut buf, 7); // node_id
        push_str(&mut buf, "bob"); // owner_id
        push_str(&mut buf, "旧数据");
        push_f32(&mut buf, 0.1);
        push_f32(&mut buf, 0.2);
        push_i64(&mut buf, 1000);
        push_f32(&mut buf, 0.01);
        push_u32(&mut buf, 1);
        push_str(&mut buf, "旧标签");
        push_f32(&mut buf, 0.9);
        push_u32(&mut buf, 3);
        push_f32(&mut buf, 0.8);
        push_i64(&mut buf, 1000);
        push_u32(&mut buf, 0); // 空 embedding
        let nodes = decode_nodes(&buf).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].tenant_id, DEFAULT_TENANT);
        assert_eq!(nodes[0].owner_id, "bob");
        assert_eq!(nodes[0].filaments.fact, "旧数据");
    }

    /// v1 编码的 tenant 在 roundtrip 中保真。
    #[test]
    fn v1_roundtrip_preserves_tenant() {
        let nodes = vec![sample("跨租户数据", &["隔离"], vec![0.5])];
        let bytes = encode_nodes(&nodes);
        assert_eq!(&bytes[..4], MAGIC_V1);
        let back = decode_nodes(&bytes).unwrap();
        assert_eq!(back[0].tenant_id, "acme");
    }

    #[test]
    fn rejects_truncated() {
        let bytes = encode_nodes(&[sample("截断测试", &["a"], vec![1.0])]);
        let cut = &bytes[..bytes.len() - 3];
        assert_eq!(decode_nodes(cut), Err(DecodeError::Truncated));
    }
}
