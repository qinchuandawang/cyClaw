# cyClaw 发布指南

## 发布前检查

1. 确认根目录 `LICENSE`、`README.md`、`CHANGELOG.md` 和版本号正确。
2. 执行 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings` 和 `cargo test --workspace`。
3. 执行 `pnpm run check`、`pnpm run compile` 与 IDEA `compileKotlin`。
4. 执行 `scripts/evaluate-task-memory.ps1` 和 `scripts/verify-codex-mcp.ps1`。
5. 确认没有 `.env`、API Key、私有路径、`.cyclaw/` 状态或构建产物被 Git 跟踪。
6. 确认 GitHub Actions CI 已通过。

## 版本策略

- GitHub Release 标签采用 `vMAJOR.MINOR.PATCH`，例如 `v0.1.6`。
- Rust workspace 与 IDEA 插件使用同一核心版本。
- VS Code 扩展可因 Marketplace 修订独立递增，但必须在发布说明中注明兼容的 CLI 版本。
- `v0.x` 属于预稳定阶段，MCP 和存储格式可能在小版本中演进。

## 首次发布前的仓库配置

在 GitHub 创建仓库后，维护者必须完成：

1. 将默认分支设为 `main`，开启分支保护和必需 CI 检查。
2. 在仓库 Settings -> Security 启用 Private Vulnerability Reporting。
3. 配置实际仓库 URL、维护者邮箱、VS Code Publisher 和 JetBrains Vendor 信息。
4. 在 VS Code Marketplace 和 Open VSX 创建 Publisher；不要复用个人测试 Publisher。
5. 为 GitHub Actions 授予 `contents: write`，仅允许受保护标签触发 Release。

## 创建发布

```powershell
git checkout main
git pull --ff-only
git tag -a v0.1.6 -m "cyClaw v0.1.6"
git push origin v0.1.6
```

`release.yml` 会构建 Windows、Linux、macOS CLI，Windows x64 VSIX、IDEA ZIP 和 `SHA256SUMS.txt`，然后创建 GitHub Release。发布前应在 Release 草稿中检查产物、校验值和自动生成的说明。

## Marketplace 发布

GitHub Release 成功不代表插件已上架。VS Code Marketplace、Open VSX 与 JetBrains Marketplace 都需要各自的 Publisher/Token。Token 必须保存为 GitHub Actions Secret，绝不能写入仓库或日志。
