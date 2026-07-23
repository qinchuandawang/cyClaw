# 贡献指南

感谢你参与 cyClaw。请先阅读 `README.md`、`SECURITY.md` 和相关模块文档，再开始修改。

## 开发环境

- Rust `1.85` 或更高版本；自动化 CI 使用 Rust `1.96.0` 验证。
- Node.js `24` 和 pnpm `11.9.0`，用于 VS Code 插件。
- JDK `17`，用于 IntelliJ IDEA 插件。
- Windows 构建原生 CLI 时需要 Visual Studio C++ Build Tools。

安装依赖后执行：

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd apps/vscode
pnpm install --frozen-lockfile
pnpm run check
pnpm run compile
```

IDEA 插件验证：

```powershell
cd apps/idea
.\gradlew.bat compileKotlin
```

## 提交要求

- 一个 Pull Request 只解决一个清晰问题，并补充对应测试或说明无法测试的原因。
- 不提交 `.cyclaw/`、API Key、真实项目数据、构建产物、VSIX 或本机 IDE 配置。
- 修改 MCP 工具、权限或自动写入逻辑时，必须说明安全边界和兼容影响。
- 修改 Skills 时，运行 `quick_validate.py` 验证 YAML frontmatter。
- 文档、错误信息和代码注释使用简体中文；标识符使用英文。

## 贡献流程

1. 从 `main` 创建分支。
2. 保持提交可构建、可测试。
3. 在 Pull Request 描述中说明问题、方案、验证命令和限制。
4. 不要在 Issue、提交信息或截图中粘贴密钥、Token、私有路径或客户数据。

## 行为准则

参与本项目即表示接受 [行为准则](CODE_OF_CONDUCT.md)。安全漏洞请按 [安全政策](SECURITY.md) 私下报告，不要公开 Issue。
