//! 静态加密接缝（L2.6，企业版）。
//!
//! 社区版只含**接口与格式识别**：加密实现（AES-256-GCM + KEK 托管）在私有仓库
//! `enterprise/nylon-crypto`，企业版二进制在打开存储时注入实现。
//! 设计见《企业版-L2.6-静态加密设计》（私有区 前期设计包/）。
//!
//! 格式约定（向后兼容，互判在文件头）：
//! - 快照：明文 `SNP0` 开头；加密 `SNPE` + 密封块（密封内容仍是一份 SNP0 载荷）
//! - WAL：明文无文件头直接帧流；加密文件头 4 字节 `WALE`，帧结构不变、payload 为密封块
//! - 审计/反馈 JSONL：加密首行 `# NYLE1`，其后每行 base64(密封块)
use std::io;

/// 加密快照魔数（明文是 `SNP0`）。
pub const SNAPSHOT_MAGIC_ENC: &[u8; 4] = b"SNPE";
/// 加密 WAL 文件头（明文 WAL 无文件头）。
pub const WAL_MAGIC_ENC: &[u8; 4] = b"WALE";
/// 加密 JSONL（审计/反馈）首行标记。
pub const JSONL_MAGIC_ENC_LINE: &str = "# NYLE1";

/// 存储层字节编解码接缝：快照载荷、WAL 记录、审计/反馈行共用。
///
/// 实现方义务：`open(seal(x)) == x`；数据损坏、被篡改或密钥错误必须返回 Err，
/// 绝不静默放行（AEAD 的意义就在这里）。
pub trait StorageCipher: Send + Sync {
    /// 密封一段明文字节。
    fn seal(&self, plaintext: &[u8]) -> io::Result<Vec<u8>>;
    /// 解封。失败必须 Err。
    fn open(&self, sealed: &[u8]) -> io::Result<Vec<u8>>;
    /// 算法标识（启动日志用，如 "aes-256-gcm"）。
    fn algorithm(&self) -> &'static str;
}

/// 社区版二进制读到企业版加密数据的统一报错。
pub fn encrypted_store_error(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{what} 由 NylonME 企业版加密存储（L2.6），社区版二进制无法读取。\
             请使用企业版二进制，或联系发行方获取支持。"
        ),
    )
}

// ---------- JSONL 行级加密（audit.jsonl / feedback.jsonl 共用） ----------
//
// 加密文件首行为 JSONL_MAGIC_ENC_LINE 标记，其后每行 = base64(seal(原始行))。
// 明文文件无标记行。两种格式靠首行互判。

use base64::Engine as _;

/// 写一行（加密模式下密封并 base64；明文模式原样）。
pub fn seal_jsonl_line(cipher: Option<&dyn StorageCipher>, line: &str) -> io::Result<String> {
    match cipher {
        None => Ok(line.to_string()),
        Some(c) => Ok(base64::engine::general_purpose::STANDARD.encode(c.seal(line.as_bytes())?)),
    }
}

/// 读一行（加密模式 base64 解码并解封；明文原样）。损坏行返回 None（跳过）。
pub fn open_jsonl_line(cipher: Option<&dyn StorageCipher>, line: &str) -> Option<String> {
    match cipher {
        None => Some(line.to_string()),
        Some(c) => {
            let sealed = base64::engine::general_purpose::STANDARD
                .decode(line)
                .ok()?;
            let plain = c.open(&sealed).ok()?;
            String::from_utf8(plain).ok()
        }
    }
}

/// 判定一份 JSONL 内容是否为加密格式（首行是标记行）。
pub fn jsonl_is_encrypted(content: &str) -> bool {
    content
        .lines()
        .next()
        .map(|l| l.trim() == JSONL_MAGIC_ENC_LINE)
        .unwrap_or(false)
}
