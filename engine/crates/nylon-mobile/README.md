# nylon-mobile

NylonME 记忆引擎的移动端进程内绑定（UniFFI）。Android（Kotlin）与 iOS（Swift）
共用本 crate 编译产物，**进程内直接调用，不开端口、不走 gRPC**。

## 定位与边界

- 只依赖 `nylon-core / nylon-graph / nylon-storage / nylon-embed` 四个纯 Rust 核心库；
  `tonic / axum / rmcp` 等服务端依赖不进入移动端编译产物。
- 付费云端、计费、密钥编排属于闭源商业层（`api/` 网关），**本 crate 永不含计费逻辑**。

## 对外 API（UniFFI）

| 方法 | 说明 |
| --- | --- |
| `MobileEngine.new(config)` | 打开/创建本地记忆库（App 沙箱目录；`embed_model_path`/`embed_vocab_path` 可选启用端上嵌入） |
| `weave(fact, task?, emotion?)` | 编织记忆，返回 node_id |
| `resonate(query, budget)` | 情境共振召回 |
| `get(local_id)` | 读单条记忆 |
| `node_count()` | 记忆条数 |
| `checkpoint()` | 主动落盘（App 退后台时调用） |
| `shutdown()` | compact + 落盘（App 销毁实例前调用；进程被杀由 WAL 兜底） |

## 构建

```powershell
# 宿主检查（Windows）
cd nylon\engine
cargo check -p nylon-mobile
cargo test -p nylon-mobile

# Android 交叉编译（需 NDK；linker 指向 $NDK/toolchains/llvm 下对应 clang）
rustup target add aarch64-linux-android
cargo build -p nylon-mobile --release --target aarch64-linux-android
```

生成 Kotlin/Swift 绑定（骨架后续接入）：

```powershell
cargo install uniffi-bindgen --version 0.29
uniffi-bindgen generate --library target\aarch64-linux-android\release\libnylon_mobile.so --language kotlin --out-dir bindings/
```

## 路线图（骨架 -> 产品）

1. ~~逻辑对齐~~ **已完成**：逻辑抽至 `nylon-service::EngineCore`，服务端（gRPC）与端上共用同一份编织/共振/检索实现，本 crate 为薄绑定；
2. ~~端上嵌入~~ **已完成**：`OnnxEmbedder`（tract 0.23 推理 + 纯 Rust WordPiece 分词，feature `onnx`），
   dims 从模型输出形状自动推断（bge-small-zh-v1.5 为 512）；换模型维度不同需重建记忆库；
   - 模型准备：Xenova/bge-small-zh-v1.5 量化版 + BAAI vocab.txt，量化模型须先剥 value_info
     （`python scripts/prepare_onnx_model.py`，否则 tract 无法调和符号形状）；
   - 宿主语义冒烟：`cargo run -p nylon-embed --example onnx_smoke --features onnx -- <model> <vocab>`；
   - 真机：`cargo build -p nylon-mobile --example ondevice_demo --target aarch64-linux-android --release --features onnx`，
     `ondevice_demo <dir> <1|2> <model.onnx> <vocab.txt>`。
3. ~~生命周期~~ **已完成**：自动 checkpoint（默认 32 写/300s 脏触发，`autoCheckpoint*` 可调）+
   `shutdown()`（compact+落盘）；WAL 恢复开箱即用；Kotlin/Swift 绑定生成与 App 侧
   挂接样例（onStop→checkpoint / onDestroy→shutdown）见 `INTEGRATION.md`；
4. ~~CI~~ **已完成**：ci.yml 新增 Android（NDK clang，aarch64 onnx + x86_64 模拟器）/ iOS
   （device onnx + simulator）构建与绑定新鲜度检查；release.yml 按 tag 产出 .so/xcframework + 绑定包。

## 与服务的差异说明

- 嵌入通道可选：`MobileConfig.embed_model_path` + `embed_vocab_path` 成对提供即启用端上 ONNX 语义通道，否则词面种子 + 图扩散；LLM 通道端上留空；
- 端上为单租户（DEFAULT_TENANT）单 owner；CJK 关系丝抽取已在 core 层实现，两端行为一致。

## CJK 查询分词（两端共用）

中文查询无空格可切，`resonate` 选种时把含汉字的词项展开为**非停用单字 + 相邻二字组**
（IDF 加权，稀有词权重显著更高；`的/了/吗/呢` 等停用字不参与，含停用字的二字组
如「户的」直接跳过，避免撞上「用户的X」句式造成误命中）。无空格 CJK 整串 contains
的旧行为已被取代——`nylon-service::EngineCore` 统一实现，服务端与端上结果一致。

真机回归（荣耀 NTH-AN00 / aarch64，20 条中英混合记忆，WAL 跨进程恢复后）：
8 个中文查询全部 top-1 命中目标记忆（「用户的猫」正确命中橘猫条目，此前被
「用户的X」句式劫持），平均单次共振约 55µs。

端上语义通道回归（同机，feature `onnx` + bge-small-zh-v1.5 量化模型 24MB）：
10 个查询全部 top-1 正确，其中纯语义探针「家里养了宠物吗」（与目标记忆零字面重叠）
正确命中橘猫条目——嵌入通道在端上真实生效。单次共振约 0.3–1.2s（含查询嵌入，
Snapdragon 778G 级 CPU），weave 单条约 0.3–1.1s；WAL 跨进程恢复正常（512 维向量）。
