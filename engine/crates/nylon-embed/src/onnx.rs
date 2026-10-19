//! 端上 ONNX 嵌入器：tract 推理引擎 + 纯 Rust WordPiece 分词。
//!
//! 选型理由：tract 与分词器均为纯 Rust（无 C/C++ 依赖），满足移动端
//! 交叉编译约束（对照：tokenizers crate 传递依赖 esaxx-rs 需要 C++，
//! ort 需要预编译 onnxruntime C 库，两者在 Android/iOS 交叉编译下都麻烦）。
//!
//! 模型约定：BERT 系 encoder（如 BAAI/bge-small-zh-v1.5 导出的 ONNX），
//! 输入 input_ids/attention_mask/token_type_ids（i64，[1, max_len]），
//! 输出 last_hidden_state（f32，[1, max_len, dims]）。

use std::collections::HashMap;
use std::path::Path;

use tract_onnx::prelude::*;
use tract_onnx::tract_core::internal::SimplePlan;

use crate::wordpiece::WordPieceTokenizer;
use crate::{EmbedError, Embedder};

/// 池化方式：bge 官方用 CLS；mean 对句向量任务更稳。
#[derive(Debug, Clone, Copy, Default)]
pub enum Pooling {
    #[default]
    Cls,
    Mean,
}

type Runnable = Arc<SimplePlan<TypedFact, Box<dyn TypedOp>>>;

/// 端上 ONNX 嵌入器。load 后线程安全，可共享（tract plan 为 Send+Sync）。
pub struct OnnxEmbedder {
    model: Runnable,
    tokenizer: WordPieceTokenizer,
    /// 模型输入名 → 模型输入序号（输入顺序随导出工具变化，按名对齐）。
    input_order: HashMap<String, usize>,
    dims: usize,
    pooling: Pooling,
}

impl OnnxEmbedder {
    /// model_path：ONNX 文件；vocab_path：vocab.txt（行号即 token id）。
    /// max_len 为固定输入长度（右侧 padding），移动端建议 128（延迟/内存权衡）。
    /// dims 从模型输出形状推断（[1, seq, dims] 末维），调用方无需先知。
    pub fn new(
        model_path: impl AsRef<Path>,
        vocab_path: impl AsRef<Path>,
        max_len: usize,
        pooling: Pooling,
    ) -> Result<Self, EmbedError> {
        let tokenizer = WordPieceTokenizer::from_vocab_file(vocab_path, max_len)
            .map_err(|e| EmbedError(e.to_string()))?;

        let mut infer = tract_onnx::onnx()
            .model_for_path(model_path.as_ref())
            .map_err(|e| EmbedError(format!("load {}: {e}", model_path.as_ref().display())))?;

        // 记录输入名顺序（先收集为拥有值，避免借用与后续 move 冲突）
        let mut input_order = HashMap::new();
        let outlets: Vec<OutletId> = infer
            .input_outlets()
            .map_err(|e| EmbedError(format!("inputs: {e}")))?
            .to_vec();
        let names: Vec<String> = outlets
            .iter()
            .enumerate()
            .map(|(i, outlet)| {
                infer.outlet_label(*outlet)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("input_{i}"))
            })
            .collect();
        for (i, name) in names.into_iter().enumerate() {
            input_order.insert(name, i);
            infer = infer
                .with_input_fact(
                    i,
                    InferenceFact::dt_shape(i64::datum_type(), tvec!(1, max_len as i64)),
                )
                .map_err(|e| EmbedError(format!("input fact {i}: {e}")))?;
        }

        let model = infer
            .into_optimized()
            .and_then(|m| m.into_runnable())
            .map_err(|e| EmbedError(format!("compile: {e:?}")))?;

        // 从输出事实推断隐藏维（[batch, seq, dims] 末维）
        let out_fact = model
            .model()
            .output_fact(0)
            .map_err(|e| EmbedError(format!("output fact: {e}")))?;
        let dims = out_fact
            .shape
            .last()
            .and_then(|d| d.to_i64().ok())
            .map(|d| d as usize)
            .ok_or_else(|| EmbedError("无法从模型输出形状推断 dims".into()))?;

        Ok(OnnxEmbedder {
            model,
            tokenizer,
            input_order,
            dims,
            pooling,
        })
    }

    fn embed_one(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let max_len = self.tokenizer.max_len();
        let enc = self.tokenizer.encode(text);
        let ids: Vec<i64> = enc.input_ids.iter().map(|&v| v as i64).collect();
        let mask: Vec<i64> = enc.attention_mask.iter().map(|&v| v as i64).collect();
        let types: Vec<i64> = enc.token_type_ids.iter().map(|&v| v as i64).collect();

        // 按模型声明的输入名装配（导出工具可能改变输入顺序或省略 token_type_ids）；
        // 无输入名时回退标准顺序 [input_ids, attention_mask, token_type_ids]。
        let data_for = |name: &str, pos: usize| -> Vec<i64> {
            match name {
                "input_ids" => ids.clone(),
                "attention_mask" => mask.clone(),
                "token_type_ids" => types.clone(),
                _ => [ids.clone(), mask.clone(), types.clone()][pos.min(2)].clone(),
            }
        };
        let n_inputs = self.input_order.len().max(1);
        let mut inputs: TVec<TValue> = TVec::new();
        for pos in 0..n_inputs {
            let name = self
                .input_order
                .iter()
                .find(|(_, &i)| i == pos)
                .map(|(n, _)| n.as_str())
                .unwrap_or("");
            let data = data_for(name, pos);
            let tensor = Tensor::from_shape(&[1, max_len], &data)
                .map_err(|e| EmbedError(format!("tensor {name}: {e}")))?;
            inputs.push(tensor.into());
        }

        let outputs = self
            .model
            .run(inputs)
            .map_err(|e| EmbedError(format!("run: {e}")))?;
        let hidden = outputs[0]
            .to_plain_array_view::<f32>()
            .map_err(|e| EmbedError(format!("output: {e}")))?;
        if hidden.ndim() != 3 || hidden.shape()[2] != self.dims {
            return Err(EmbedError(format!(
                "输出形状 {:?} 与预期 [1, seq, {}] 不符",
                hidden.shape(),
                self.dims
            )));
        }
        let seq = hidden.shape()[1];
        let mut vec = vec![0.0f32; self.dims];
        match self.pooling {
            Pooling::Cls => {
                vec.copy_from_slice(hidden.slice(tract_ndarray::s![0, 0, ..]).as_slice().ok_or_else(|| EmbedError("cls slice".into()))?);
            }
            Pooling::Mean => {
                let mut wsum = 0.0f32;
                for t in 0..seq {
                    let w = enc.attention_mask[t] as f32;
                    if w > 0.0 {
                        wsum += w;
                        for (d, acc) in vec.iter_mut().enumerate() {
                            *acc += hidden[[0, t, d]] * w;
                        }
                    }
                }
                if wsum > 0.0 {
                    for acc in &mut vec {
                        *acc /= wsum;
                    }
                }
            }
        }
        // L2 归一化，配合余弦相似度
        let norm = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut vec {
                *x /= norm;
            }
        }
        Ok(vec)
    }
}

#[async_trait::async_trait]
impl Embedder for OnnxEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        // 逐条推理（移动端单次 weave/resonate 均为 1 条，CPU 耗时 <100ms，无需批处理）
        texts.iter().map(|t| self.embed_one(t)).collect()
    }
    fn dims(&self) -> usize {
        self.dims
    }
}
