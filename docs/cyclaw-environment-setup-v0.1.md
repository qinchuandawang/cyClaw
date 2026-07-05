# cyClaw 环境依赖安装说明 v0.1

日期：2026-07-05

## 1. 目标

本文件记录 cyClaw 当前技术栈所需的本地开发环境，以及本机已经完成的安装和验证结果。

cyClaw 当前技术路线：

```text
Rust Core First
Rust CLI
Tauri + React Desktop
Markdown + SQLite + JSONL
Git Diff + Rule Engine + LLM Hybrid Analysis
MCP Server
```

## 2. 当前机器环境

当前机器已确认的版本：

| 依赖 | 当前版本 |
| --- | --- |
| Rust | `rustc 1.96.0 (ac68faa20 2026-05-25)` |
| Cargo | `cargo 1.96.0 (30a34c682 2026-05-25)` |
| Rust target | `x86_64-pc-windows-msvc` |
| Visual Studio Build Tools | `17.14.35 (June 2026)` |
| Build Tools 安装路径 | `C:\BuildTools` |
| Git | `git version 2.45.1.windows.1` |
| Node.js | `v24.15.0` |
| npm | `11.12.1` |
| pnpm | `11.9.0` |

## 3. 已安装内容

### 3.1 Rust

本机已经安装 Rust stable MSVC 工具链：

```text
stable-x86_64-pc-windows-msvc
```

用于：

- 编译 Rust CLI。
- 编译 Rust core。
- 后续编译 Tauri 后端。
- 后续实现 MCP Server。

### 3.2 Visual Studio Build Tools

本次已安装：

```text
Visual Studio Build Tools 2022
安装路径：C:\BuildTools
工作负载：Microsoft.VisualStudio.Workload.VCTools
```

安装原因：

Rust 的 Windows MSVC target 需要 Visual C++ linker，也就是 `link.exe`。安装前运行 `cargo test` 会报错：

```text
error: linker `link.exe` not found
```

安装 Build Tools 后，`cargo test` 已经通过。

安装方式：

```powershell
$installer = Join-Path $env:TEMP 'vs_BuildTools.exe'
Invoke-WebRequest -Uri 'https://aka.ms/vs/17/release/vs_BuildTools.exe' -OutFile $installer
Start-Process -FilePath $installer -ArgumentList @(
  '--quiet',
  '--wait',
  '--norestart',
  '--nocache',
  '--installPath',
  'C:\BuildTools',
  '--add',
  'Microsoft.VisualStudio.Workload.VCTools',
  '--includeRecommended'
) -Wait
```

### 3.3 Git

Git 已可用，用于：

- 后续读取 `git status`。
- 后续读取 `git diff`。
- 后续实现变更雷达。
- 后续实现 pre-commit 检查。

### 3.4 Node.js / npm / pnpm

Node.js、npm、pnpm 已可用，用于后续：

- Tauri + React 桌面端。
- 前端构建。
- UI 组件开发。
- 后续 VS Code / Cursor 插件。

当前第一阶段 CLI/Core 尚不依赖 Node.js。

## 4. 已完成验证

### 4.1 Rust 格式检查

命令：

```bash
cargo fmt --check
```

结果：

```text
通过
```

### 4.2 Workspace 元数据检查

命令：

```bash
cargo metadata --no-deps --format-version 1
```

结果：

```text
通过
```

### 4.3 Rust 单元测试

命令：

```bash
cargo test
```

结果：

```text
通过
```

测试结果摘要：

```text
cyclaw-core：1 passed
cyclaw-scanner：2 passed
cyclaw-cli：0 tests
doc-tests：通过
```

### 4.4 CLI 初始化验证

命令：

```bash
cargo run -p cyclaw-cli -- init
```

结果：

```text
通过
```

生成文件：

```text
.cyclaw/config.yaml
.cyclaw/project-profile.json
.cyclaw/project.md
```

### 4.5 CLI 扫描验证

命令：

```bash
cargo run -p cyclaw-cli -- scan
```

结果：

```text
通过
```

当前项目扫描结果摘要：

```text
识别语言：Rust
依赖文件：6
文档入口：4
配置文件：0
```

## 5. 推荐的新机器安装顺序

在新的 Windows 开发机器上，建议按以下顺序安装。

### 5.1 安装 Rust

安装 rustup：

```powershell
Invoke-WebRequest -Uri 'https://win.rustup.rs/x86_64' -OutFile "$env:TEMP\rustup-init.exe"
& "$env:TEMP\rustup-init.exe"
```

安装后验证：

```bash
rustc --version
cargo --version
rustup show
```

### 5.2 安装 Visual Studio Build Tools

下载并安装 C++ Build Tools：

```powershell
$installer = Join-Path $env:TEMP 'vs_BuildTools.exe'
Invoke-WebRequest -Uri 'https://aka.ms/vs/17/release/vs_BuildTools.exe' -OutFile $installer
Start-Process -FilePath $installer -ArgumentList @(
  '--quiet',
  '--wait',
  '--norestart',
  '--nocache',
  '--installPath',
  'C:\BuildTools',
  '--add',
  'Microsoft.VisualStudio.Workload.VCTools',
  '--includeRecommended'
) -Wait
```

验证：

```powershell
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
```

### 5.3 安装 Git

安装后验证：

```bash
git --version
```

### 5.4 安装 Node.js 和 pnpm

安装 Node.js 后验证：

```bash
node --version
npm --version
```

安装 pnpm：

```bash
npm install -g pnpm
```

验证：

```bash
pnpm --version
```

## 6. 后续阶段还需要的依赖

当前第一阶段已经满足 Rust CLI/Core 开发。

后续进入 Tauri + React 桌面端时，还需要确认：

| 依赖 | 用途 | 状态 |
| --- | --- | --- |
| WebView2 Runtime | Tauri Windows 桌面渲染 | Windows 现代系统通常自带，后续创建桌面端时验证 |
| Tauri CLI | 桌面端开发和打包 | 后续按项目方式安装 |
| React / Vite | 前端开发 | 后续创建 `apps/desktop` 时安装 |
| SQLite 开发依赖 | 本地索引 | Rust crate 阶段引入后验证 |
| MCP SDK 或协议实现 | MCP Server | 后续阶段选择 |

## 7. 当前注意事项

1. `winget` 当前不可用，所以本机没有通过 winget 安装依赖。
2. 本机使用 MSVC Rust target，不使用 GNU target。
3. `.cyclaw/` 是本地生成目录，已加入 `.gitignore`。
4. `target/` 是 Rust 构建产物，已加入 `.gitignore`。
5. 后续如果安装 Tauri CLI，优先跟随项目依赖管理，不建议先全局安装过多工具。

## 8. 当前可用命令

```bash
cargo fmt --check
cargo test
cargo run -p cyclaw-cli -- init
cargo run -p cyclaw-cli -- scan
```

下一阶段建议进入：

```text
阶段 2：变更雷达
```

也就是实现：

```text
cyclaw diff
```

核心目标：

- 读取 Git status。
- 读取 Git diff。
- 识别 API、Schema、依赖、配置、环境变量变化。
- 生成 `.cyclaw/runs/{run-id}/change-analysis.json`。

