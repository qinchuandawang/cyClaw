# 依赖与构建环境

## Rust

- 最低支持 Rust `1.85`；GitHub CI 使用 Rust `1.96.0`。
- 主要运行时依赖：`clap`、`serde`、`rusqlite`（bundled SQLite）、`notify`、`pulldown-cmark`、`reqwest`。
- `sha2` 用于 FactEvidence 内容哈希采集与验证。
- `tempfile` 用于 Fact Ledger 和 Fact Patch 的同目录原子替换。
- 使用 `Cargo.lock` 固定解析后的依赖版本。

## 编辑器插件

- VS Code：Node.js `24`、pnpm `11.9.0`、TypeScript `5.7`。
- IntelliJ IDEA：JDK `17`、Gradle Wrapper `8.7`、Kotlin `1.9.25`。

## Windows 原生构建

Windows 从源码构建原生 Rust CLI 时需要 Visual Studio C++ Build Tools 和 Windows SDK。`scripts/package-vscode.ps1` 会尝试自动定位 Visual C++ 环境；GitHub Actions 使用托管 Windows Runner 构建发布包。

## 安全更新

GitHub Dependabot 每周检查 Cargo、npm、Gradle 和 GitHub Actions 依赖。贡献者在更新依赖后应运行完整测试、Clippy 和受影响插件构建。
