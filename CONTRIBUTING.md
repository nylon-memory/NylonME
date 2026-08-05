# 贡献指南

感谢你对 Nylon 的兴趣！

## 贡献者许可协议（CLA）

本项目要求所有贡献者在首次 PR 时签署 CLA（贡献者许可协议）。
协议全文见 [CLA.md](CLA.md)（中英双语），核心条款包括：授予项目方**再许可（relicense）**权利——这是项目保留许可证演进通道（如未来必要时切换 BSL）的法律基础。
PR 提交后 CLA 机器人会自动引导完成签署，签署一次即可。

## 开发流程

1. Fork 并创建特性分支
2. 提交前本地通过：
   ```bash
   cd engine
   cargo fmt --all --check
   cargo clippy --workspace -- -D warnings
   cargo test --workspace
   ```
3. 提交信息使用 Conventional Commits（如 `feat:`, `fix:`, `chore:`）
4. 提交 PR，CI 全绿 + review 通过后合并

## 代码约定

- Rust：edition 2021，`cargo fmt` 默认配置，clippy 零警告
- 注释与文档使用中文或英文均可，公开 API 必须有文档注释
- 设计决策请先在 Issue 中讨论，避免直接提交大型重构 PR