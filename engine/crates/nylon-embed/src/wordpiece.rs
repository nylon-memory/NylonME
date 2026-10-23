//! 纯 Rust WordPiece 分词器（BERT 系中文模型词表，如 bge-small-zh-v1.5）。
//!
//! 不依赖 tokenizers crate——其传递依赖 esaxx-rs 需要 C++ 编译器，
//! 违背本 crate「纯 Rust 无 C 依赖，移动端可交叉编译」的约束。
//!
//! 算法即 HuggingFace BertTokenizer 的最小可用子集：
//! 1. BasicTokenizer：清洗控制字符、CJK 字符单独成词、按空白/标点切分、英文小写化；
//! 2. WordPiece：贪心最长匹配，续词带 `##` 前缀，无匹配回退 `[UNK]`；
//! 3. 拼装 `[CLS] ... [SEP]`，padding/truncation 到固定 max_len。

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

/// 分词结果（等长 max_len，可直接转 i64 喂模型）。
#[derive(Debug, Clone)]
pub struct WordPieceEncoding {
    pub input_ids: Vec<u32>,
    pub attention_mask: Vec<u32>,
    pub token_type_ids: Vec<u32>,
}

#[derive(Debug)]
pub struct TokenizerError(pub String);

impl fmt::Display for TokenizerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tokenizer: {}", self.0)
    }
}
impl std::error::Error for TokenizerError {}

/// WordPiece 分词器：词表 + 贪心最长匹配。
pub struct WordPieceTokenizer {
    vocab: HashMap<String, u32>,
    unk_id: u32,
    cls_id: u32,
    sep_id: u32,
    pad_id: u32,
    max_len: usize,
}

impl WordPieceTokenizer {
    /// 从 vocab.txt（一行一个 token，行号即 id）构建。
    pub fn from_vocab_file(path: impl AsRef<Path>, max_len: usize) -> Result<Self, TokenizerError> {
        let text = std::fs::read_to_string(path.as_ref())
            .map_err(|e| TokenizerError(format!("read {}: {e}", path.as_ref().display())))?;
        Ok(Self::from_vocab_str(&text, max_len))
    }

    /// 从词表文本构建（便于测试）。
    pub fn from_vocab_str(vocab: &str, max_len: usize) -> Self {
        let mut map = HashMap::with_capacity(4096);
        for (i, line) in vocab.lines().enumerate() {
            let tok = line.trim_end_matches('\r');
            if !tok.is_empty() {
                map.insert(tok.to_string(), i as u32);
            }
        }
        let get = |k: &str| *map.get(k).unwrap_or(&100);
        WordPieceTokenizer {
            unk_id: get("[UNK]"),
            cls_id: get("[CLS]"),
            sep_id: get("[SEP]"),
            pad_id: get("[PAD]"),
            vocab: map,
            max_len,
        }
    }

    pub fn max_len(&self) -> usize {
        self.max_len
    }

    /// 分词并拼装特殊符号，右侧 pad 到 max_len。
    pub fn encode(&self, text: &str) -> WordPieceEncoding {
        let mut ids = Vec::with_capacity(self.max_len.min(64));
        ids.push(self.cls_id);
        for basic in basic_tokenize(text) {
            for piece in self.wordpiece(&basic) {
                ids.push(piece);
                if ids.len() >= self.max_len - 1 {
                    break;
                }
            }
            if ids.len() >= self.max_len - 1 {
                break;
            }
        }
        ids.push(self.sep_id);
        let real = ids.len();
        ids.resize(self.max_len, self.pad_id);
        let mut attention_mask = vec![0u32; self.max_len];
        for m in attention_mask.iter_mut().take(real) {
            *m = 1;
        }
        WordPieceEncoding {
            input_ids: ids,
            attention_mask,
            token_type_ids: vec![0u32; self.max_len],
        }
    }

    /// 单个基本词的 WordPiece 贪心最长匹配。
    fn wordpiece(&self, token: &str) -> Vec<u32> {
        let chars: Vec<char> = token.chars().collect();
        if chars.len() > 100 {
            return vec![self.unk_id];
        }
        let mut out = Vec::with_capacity(2);
        let mut start = 0;
        while start < chars.len() {
            let mut end = chars.len();
            let mut hit: Option<u32> = None;
            while start < end {
                let mut sub: String = chars[start..end].iter().collect();
                if start > 0 {
                    sub.insert_str(0, "##");
                }
                if let Some(id) = self.vocab.get(&sub) {
                    hit = Some(*id);
                    break;
                }
                end -= 1;
            }
            match hit {
                Some(id) => out.push(id),
                None => return vec![self.unk_id],
            }
            start = end;
        }
        out
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' | '\u{F900}'..='\u{FAFF}'
        | '\u{3000}'..='\u{303F}' | '\u{FF00}'..='\u{FFEF}')
}

fn is_whitespace(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{0005}'..='\u{001F}' | '\u{007F}'..='\u{009F}')
}

fn is_punctuation(c: char) -> bool {
    let cp = c as u32;
    matches!(cp, 33..=47 | 58..=64 | 91..=96 | 123..=126)
        || matches!(c, '\u{3000}'..='\u{303F}' | '\u{FF00}'..='\u{FFEF}')
}

/// BasicTokenizer：CJK 单独成词；空白/标点切分；英文小写化。
fn basic_tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut buf = String::new();
    let flush = |buf: &mut String, tokens: &mut Vec<String>| {
        if !buf.is_empty() {
            tokens.push(std::mem::take(buf));
        }
    };
    for c in text.chars() {
        if (c as u32) == 0 || (c as u32) == 0xFFFD || is_whitespace(c) {
            flush(&mut buf, &mut tokens);
        } else if is_cjk(c) || is_punctuation(c) {
            flush(&mut buf, &mut tokens);
            tokens.push(c.to_string());
        } else {
            buf.extend(c.to_lowercase());
        }
    }
    flush(&mut buf, &mut tokens);
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    const VOCAB: &str = "[PAD]\n[unused]\n##a\n[UNK]\n[CLS]\n[SEP]\n我\n爱\n北\n京\n天\n安\n门\nhello\n##s\n##ing\n，\n";

    #[test]
    fn cjk_chars_become_single_tokens() {
        let tk = WordPieceTokenizer::from_vocab_str(VOCAB, 16);
        let enc = tk.encode("我爱北京天安门");
        // [CLS]=4 + 我6 爱7 北8 京9 天10 安11 门12 + [SEP]=5，其余 pad
        assert_eq!(&enc.input_ids[..9], &[4, 6, 7, 8, 9, 10, 11, 12, 5]);
        assert_eq!(enc.attention_mask.iter().take(9).sum::<u32>(), 9);
        assert_eq!(enc.attention_mask.iter().skip(9).sum::<u32>(), 0);
    }

    #[test]
    fn wordpiece_greedy_longest() {
        let tk = WordPieceTokenizer::from_vocab_str(VOCAB, 16);
        let enc = tk.encode("helloa");
        // hello + ##a
        assert_eq!(&enc.input_ids[..4], &[4, 13, 2, 5]);
    }

    #[test]
    fn unknown_falls_back_to_unk() {
        let tk = WordPieceTokenizer::from_vocab_str(VOCAB, 16);
        let enc = tk.encode("量子");
        assert_eq!(enc.input_ids[1], 3, "[UNK]");
    }

    #[test]
    fn truncation_at_max_len() {
        let tk = WordPieceTokenizer::from_vocab_str(VOCAB, 5);
        let enc = tk.encode("我爱北京天安门，hello");
        assert_eq!(enc.input_ids.len(), 5);
        assert_eq!(enc.input_ids[4], 5, "末位必须是 [SEP]");
    }

    #[test]
    fn real_vocab_layout_ids() {
        // bge/bert 中文词表约定：[PAD]=0 [UNK]=100 [CLS]=101 [SEP]=102
        let vocab = std::iter::once("[PAD]")
            .chain((0..99).map(|_| "[unused]"))
            .chain(["[UNK]", "[CLS]", "[SEP]"])
            .collect::<Vec<_>>()
            .join("\n");
        let tk = WordPieceTokenizer::from_vocab_str(&vocab, 8);
        assert_eq!(tk.pad_id, 0);
        assert_eq!(tk.unk_id, 100);
        assert_eq!(tk.cls_id, 101);
        assert_eq!(tk.sep_id, 102);
    }
}
