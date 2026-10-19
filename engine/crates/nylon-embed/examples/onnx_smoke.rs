//! ONNX 嵌入器冒烟测试：加载真实模型，验证语义相似度排序。
//!
//! 用法: onnx_smoke <model.onnx> <vocab.txt>
//! 期望: 「用户的猫」与「用户养了一只三岁的橘猫」相似度显著高于与「量子力学」的相似度。

use nylon_embed::{Embedder, OnnxEmbedder, Pooling};

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: onnx_smoke <model.onnx> <vocab.txt>");
        std::process::exit(2);
    }
    let t0 = std::time::Instant::now();
    let emb = OnnxEmbedder::new(&args[1], &args[2], 128, Pooling::Cls)
        .unwrap_or_else(|e| {
            eprintln!("load failed: {e}");
            std::process::exit(1);
        });
    println!("model loaded in {:?}, dims={}", t0.elapsed(), emb.dims());

    let texts = [
        "用户的猫",                             // query
        "用户养了一只三岁的橘猫，叫咪咪",       // 语义相关
        "我家咪咪最近胖了一圈",                 // 相关（猫=咪咪，无字面重叠）
        "用户每天早上七点起床跑步三公里",       // 同前缀不同语义
        "量子力学期末考试重点",                 // 无关
    ];
    let t1 = std::time::Instant::now();
    let vecs = emb
        .embed(&texts.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .await
        .expect("embed");
    println!("embedded {} texts in {:?}", texts.len(), t1.elapsed());
    for (i, v) in vecs.iter().enumerate() {
        assert_eq!(v.len(), emb.dims(), "dims");
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((n - 1.0).abs() < 1e-3, "应已 L2 归一化, got {n}");
        println!("sim(query, {:?}) = {:.4}", texts[i], cosine(&vecs[0], v));
    }
}