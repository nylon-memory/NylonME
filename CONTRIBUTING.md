# 贡献指南

感谢你对 Nylon 的兴趣！

## 贡献者许可协议（CLA）

本项目要求所有贡献者在首次 PR 时签署 CLA（贡献者许可协议）。
PR 提交后 CLA 机器人会自动引导完成签署，签署一次即可。
CLA 让项目保留对贡献代码进行再许可的权利，这是项目长期可持续发展的保障。

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