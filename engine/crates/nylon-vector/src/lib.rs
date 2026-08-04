//! 向量检索接口。
//! 当前为暴力余弦实现（单机正确性基线）；
//! 自研 HNSW（Top-100 < 10ms @ 百万级）为路线图交付物 1.2。

/// 向量索引抽象：便于从暴力实现平滑切换到 HNSW。
pub trait VectorIndex {
    fn add(&mut self, id: u32, vector: &[f32]);
    /// 返回按余弦相似度降序的 Top-K：(id, similarity)
    fn search(&self, query: &[f32], k: usize) -> Vec<(u32, f32)>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// 暴力 Top-K：O(n·d)，作为正确性基准与小规模场景兜底。
#[derive(Debug, Default)]
pub struct BruteForceIndex {
    dims: usize,
    ids: Vec<u32>,
    data: Vec<f32>,
}

impl BruteForceIndex {
    pub fn new(dims: usize) -> Self {
        BruteForceIndex { dims, ids: Vec::new(), data: Vec::new() }
    }
}

impl VectorIndex for BruteForceIndex {
    fn add(&mut self, id: u32, vector: &[f32]) {
        assert_eq!(vector.len(), self.dims, "向量维度不匹配");
        self.ids.push(id);
        self.data.extend_from_slice(vector);
    }

    fn search(&self, query: &[f32], k: usize) -> Vec<(u32, f32)> {
        assert_eq!(query.len(), self.dims, "查询向量维度不匹配");
        let mut scored: Vec<(u32, f32)> = self
            .ids
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, cosine(query, &self.data[i * self.dims..(i + 1) * self.dims])))
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(k);
        scored
    }

    fn len(&self) -> usize {
        self.ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_k_returns_most_similar() {
        let mut idx = BruteForceIndex::new(2);
        idx.add(1, &[1.0, 0.0]);
        idx.add(2, &[0.9, 0.1]);
        idx.add(3, &[0.0, 1.0]);
        let out = idx.search(&[1.0, 0.0], 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, 1);
        assert_eq!(out[1].0, 2);
        assert!(out[0].1 > 0.99);
    }
}