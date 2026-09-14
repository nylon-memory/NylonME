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
| `MobileEngine.new(config)` | 打开/创建本地记忆库（App 沙箱目录） |
| `weave(fact, task?, emotion?)` | 编织记忆，返回 node_id |
| `resonate(query, budget)` | 情境共振召回 |
| `get(local_id)` | 读单条记忆 |
| `node_count()` | 记忆条数 |
| `checkpoint()` | 主动落盘（App 退后台时调用） |

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
2. **端上嵌入**：新增 `OnnxEmbedder`（tract + 量化小模型），dims 须与索引维度一致，换模型需重嵌；
3. **生命周期**：checkpoint 挂接 App onPause/onMemoryWarning；WAL 恢复开箱即用；
4. **CI**：release workflow 增加 Android/iOS target 与绑定产物发布。

## 与服务的差异说明

- 嵌入/LLM 通道默认留空（端上模型接入前，共振走词面种子 + 图扩散）；
- 端上为单租户（DEFAULT_TENANT）单 owner；CJK 关系丝抽取已在 core 层实现，两端行为一致。
