# 端上接入指南（Android / iOS）

`MobileEngine` 是进程内引擎：不开端口、不走网络。本指南讲三件事——
**生命周期挂接**、**模型文件部署**、**崩溃数据安全契约**。

## 数据安全契约（先读这个）

| 事件 | 引擎行为 | App 要做什么 |
| --- | --- | --- |
| weave 写入 | WAL-first + fsync（组提交），写成功即已持久 | 什么都不用做 |
| 进程被杀（OOM/系统回收） | 下次启动自动重放 WAL 恢复 | 什么都不用做 |
| WAL 无限增长 | 自动 checkpoint：默认每 32 次写或 300s（脏时）触发 | 可用 `autoCheckpointWrites`/`autoCheckpointSecs` 调整 |
| App 退后台 | — | **建议**调 `checkpoint()`，缩短下次冷启动的 WAL 重放 |
| App 主动销毁引擎 | — | 调 `shutdown()`（compact + checkpoint）后再释放引用 |

WAL 已保证不丢数据；checkpoint 只影响**恢复速度**，不影响正确性。

## Android 接入

### 产物部署

```
app/src/main/jniLibs/arm64-v8a/libnylon_mobile.so   # aarch64 release 产物
app/src/main/jniLibs/x86_64/libnylon_mobile.so      # 模拟器（可选）
app/src/main/assets/nylon/model_quantized.onnx      # 量化模型（须先剥 value_info）
app/src/main/assets/nylon/vocab.txt
```

模型从 assets 解压到沙箱（assets 是只读流，tract 需要文件路径）：

```kotlin
fun deployModel(context: Context): Pair<String, String> {
    val dir = File(context.filesDir, "nylon").apply { mkdirs() }
    val model = File(dir, "model.onnx")
    val vocab = File(dir, "vocab.txt")
    if (!model.exists()) context.assets.open("nylon/model_quantized.onnx").use { i -> model.outputStream().use { i.copyTo(it) } }
    if (!vocab.exists()) context.assets.open("nylon/vocab.txt").use { i -> vocab.outputStream().use { i.copyTo(it) } }
    return model.absolutePath to vocab.absolutePath
}
```

### 生命周期挂接（ProcessLifecycleOwner，App 级一次）

```kotlin
class MemoryEngineHolder(context: Context) : DefaultLifecycleObserver {
    private val appContext = context.applicationContext
    private var engine: MobileEngine? = null

    @Synchronized
    fun engine(): MobileEngine = engine ?: run {
        val (model, vocab) = deployModel(appContext)
        MobileEngine(
            MobileConfig(
                dataDir = File(appContext.filesDir, "nylon/db").absolutePath,
                owner = "me",                       // 单用户固定即可
                embedModelPath = model,             // 不需要语义通道就传 null（两个都 null）
                embedVocabPath = vocab,
                autoCheckpointWrites = null,        // 默认 32
                autoCheckpointSecs = null,          // 默认 300
            )
        ).also { engine = it }
    }

    override fun onStop(owner: LifecycleOwner) {    // App 退后台
        engine?.checkpoint()
    }

    fun onTrimMemory(level: Int) {
        if (level >= ComponentCallbacks2.TRIM_MEMORY_BACKGROUND) engine?.checkpoint()
    }

    fun destroy() {                                  // 主动释放（如退出登录）
        engine?.shutdown()
        engine = null
    }
}

// Application.onCreate:
// ProcessLifecycleOwner.get().lifecycle.addObserver(holder)
```

## iOS 接入

- 产物：`libnylon_mobile.a`（staticlib，aarch64-apple-ios + 模拟器 target）打成 xcframework；
  绑定见 `bindings/swift/`（nylon_mobile.swift + FFI modulemap）。
- 模型随 Bundle 分发，首次启动拷入 `Application Support/nylon/`。

```swift
final class MemoryEngineHolder {
    private var engine: MobileEngine?

    func engine() throws -> MobileEngine { /* 同 Android 逻辑，略 */ }

    @objc func didEnterBackground() { try? engine?.checkpoint() }
    @objc func willTerminate()      { try? engine?.shutdown() }
}
// NotificationCenter: UIApplication.didEnterBackgroundNotification / willTerminateNotification
```

## 绑定再生成

```powershell
cd nylon\engine
cargo build -p nylon-mobile                                   # 出宿主 cdylib（元数据来源）
cargo run -p nylon-mobile --bin gen-bindings --features bindgen-cli -- `
    generate --library target\x86_64-pc-windows-gnu\debug\nylon_mobile.dll `
    --language kotlin --out-dir crates\nylon-mobile\bindings\kotlin
# --language swift 同理。ktlint/swift-format 缺失仅影响格式化，不影响正确性。
```