//! UniFFI 绑定生成 CLI（Kotlin/Swift）。
//!
//! 用法（在 engine/ 下）：
//!   cargo build -p nylon-mobile --release                       # 先出宿主 cdylib
//!   cargo run -p nylon-mobile --bin gen-bindings --features bindgen-cli -- ^
//!       generate --library target/.../nylon_mobile.dll --language kotlin --out-dir bindings/kotlin

fn main() {
    uniffi::uniffi_bindgen_main();
}