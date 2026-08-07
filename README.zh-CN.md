# NylonME — 尼龙记忆引擎

> 面向 AI Agent 的单机记忆引擎：记忆丝多维修织 · 网状记忆图 · 张力遗忘 · 情境共振
> 状态：早期开发（Phase 1，API 尚不稳定）

NylonME 把一条记忆建模为多股"丝"（事实/情感/时序/关系/置信/频次）的编织体，记忆节点之间以加权边构成网状图而非层级树。检索不是 top-k 相似度匹配，而是"情境共振"：从种子节点出发，按关联强度 × 情境匹配 × 实时张力在图上扩散激活，并受全局激活预算约束（防止高扇出节点扩散爆炸）。不使用的记忆会沿张力遗忘曲线自然沉降，而不是无限堆积。

## 快速开始

```bash
cd engine
cargo test          # 运行全部单元测试
cargo run -p nylon-engine   # 运行自检演示
```

演示输出（种子 = "机票"，任务情境 = "出差"）：

```
情境共振（种子=机票, 任务=出差）:
  node 0: resonance=0.919  用户问机票
  node 1: resonance=0.627  出差偏好：靠窗座位
  node 2: resonance=0.310  酒店偏好：近地铁
  node 3: resonance=0.247  上次出差：2026-06 上海
```

## 仓库结构

```
NylonME/
├── proto/            # nylon/v1 gRPC 契约（Weave / Resonate / Search / GetNode）
└── engine/           # Rust workspace
    └── crates/
        ├── nylon-core    # 记忆丝数据模型 + 张力遗忘公式（logistic 归一化）
        ├── nylon-graph   # CSR 主图 + Delta 缓冲 + 共振遍历（优先级队列 + 全局预算）
        ├── nylon-vector  # 向量索引抽象（当前为暴力余弦基线，HNSW 开发中）
        └── nylon-engine  # 引擎入口（gRPC 服务化进行中）
```

## 当前状态与路线图

已完成：核心数据模型、CSR + Delta 增量图、共振遍历、向量检索基线。
进行中：HNSW 索引、gRPC 服务化（tonic）、RocksDB 持久化、Python SDK。

## 贡献

欢迎贡献！提交 PR 前请阅读 [CONTRIBUTING.md](CONTRIBUTING.md)。
注意：本项目要求签署 CLA（贡献者许可协议），PR 提交后会有机器人引导签署。

## 许可证

[Apache License 2.0](LICENSE)。"NylonME" 名称与标识是项目商标，许可证不授权商标使用（见 [NOTICE](NOTICE)）。