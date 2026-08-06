//! 尼龙核心数据模型：记忆丝（Filaments）与张力遗忘（Tension Forgetting）。
//!
//! 对应《尼龙记忆模型 v0.2》2.2 / 2.4 节：
//! T(t) = T0 * e^(-λt) * F(freq) * β(context)，F 为 logistic 归一化。

/// 记忆丝：一条记忆的多维度编织体。
#[derive(Debug, Clone, PartialEq)]
pub struct Filaments {
    /// 事实丝：记忆内容本身
    pub fact: String,
    /// 情感丝：效价 [-1, 1]
    pub emotion_valence: f32,
    /// 情感丝：强度 [0, 1]
    pub emotion_intensity: f32,
    /// 时序丝：创建时间（Unix 秒）
    pub created_at: i64,
    /// 时序丝：遗忘速率 λ（1/天）。事实丝 ~0.01，情感丝 ~0.1（初值，待 A/B 调优）
    pub decay_rate: f32,
    /// 关系丝：关联标签（如 "工作"、"编程"）
    pub relations: Vec<String>,
    /// 置信丝：可靠程度 [0, 1]
    pub confidence: f32,
    /// 频次丝：近 7 天被提及次数
    pub mentions_7d: u32,
}

/// 张力状态。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tension {
    /// 基础张力 T0 ∈ [0, 1]
    pub baseline: f32,
    /// 上次张力更新时间（Unix 秒）
    pub last_updated: i64,
}

/// 记忆节点。
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryNode {
    pub id: u64,
    pub owner_id: String,
    pub filaments: Filaments,
    pub tension: Tension,
    pub embedding: Vec<f32>,
}

/// 频率强化项：logistic 归一化，F ∈ (1, 2)。
/// 线性项会让高频记忆全部顶满上限而失去区分度（v0.2 修正）。
pub fn freq_boost(mentions_7d: u32) -> f32 {
    2.0 / (1.0 + (-0.1 * mentions_7d as f32).exp())
}

/// 实时张力计算：T(t) = T0 * e^(-λt) * F(freq) * β(context)，上限 1.0。
pub fn compute_tension(node: &MemoryNode, now: i64, context_boost: f32) -> f32 {
    let days = (now - node.tension.last_updated).max(0) as f32 / 86_400.0;
    let decay = (-node.filaments.decay_rate * days).exp();
    let t = node.tension.baseline * decay * freq_boost(node.filaments.mentions_7d)
        * context_boost.clamp(0.0, 2.0);
    t.min(1.0)
}

pub mod wire;
pub use wire::{decode_nodes, encode_nodes, DecodeError};

#[cfg(test)]
mod tests {
    use super::*;

    fn node(decay_rate: f32, mentions: u32) -> MemoryNode {
        MemoryNode {
            id: 1,
            owner_id: "alice".into(),
            filaments: Filaments {
                fact: "用户喜欢 Python".into(),
                emotion_valence: 0.8,
                emotion_intensity: 0.85,
                created_at: 0,
                decay_rate,
                relations: vec!["编程".into()],
                confidence: 0.9,
                mentions_7d: mentions,
            },
            tension: Tension { baseline: 0.5, last_updated: 0 },
            embedding: vec![],
        }
    }

    #[test]
    fn tension_decays_over_time() {
        let n = node(0.1, 0);
        let t0 = compute_tension(&n, 0, 1.0);
        let t7 = compute_tension(&n, 7 * 86_400, 1.0);
        assert!(t7 < t0, "7 天后张力应衰减: {t0} -> {t7}");
    }

    #[test]
    fn frequency_boosts_but_is_bounded() {
        let cold = compute_tension(&node(0.01, 0), 0, 1.0);
        let hot = compute_tension(&node(0.01, 50), 0, 1.0);
        assert!(hot > cold, "高频记忆张力应更高");
        assert!(hot <= 1.0, "张力上限 1.0");
        // logistic 有界：mentions 从 50 到 5000 提升应很小
        let extreme = compute_tension(&node(0.01, 5000), 0, 1.0);
        assert!((extreme - hot).abs() < 0.05, "频率项应饱和: {hot} vs {extreme}");
    }

    #[test]
    fn fact_filament_forgets_slower_than_emotion() {
        let fact = compute_tension(&node(0.01, 0), 30 * 86_400, 1.0);
        let emotion = compute_tension(&node(0.1, 0), 30 * 86_400, 1.0);
        assert!(fact > emotion, "事实丝应比情感丝遗忘得慢");
    }
}