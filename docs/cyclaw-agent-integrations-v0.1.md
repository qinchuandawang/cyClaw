# cyClaw Agent 集成方案 v0.1

日期：2026-07-19

## 1. 集成目标

cyClaw 不替代 Codex 或 Claude Code 的编码能力，而是作为独立的项目知识维护层接入其工作流：

```text
文件事件 Watch -> 增量变化识别 -> 知识候选 -> MCP 审阅/草稿/应用 -> 可追溯事件
                                      ^
                                      |
                           Skills 与生命周期 Hooks
```

各组件职责：

| 组件 | 职责 | 是否常驻 |
| --- | --- | --- |
| Watch | 低延迟发现文件变化，生成候选知识 | IDE 使用期间常驻 |
| MCP | 为 Agent 提供结构化查询、审阅、草稿、应用和撤销工具 | 由 Agent 按 stdio 启动 |
| Skills | 规定何时维护知识及标准工具顺序 | 随 Agent 上下文加载 |
| Session Hook | 在会话开始/结束触发轻量知识检查 | 按生命周期执行 |
| Git Hook | 提交前检查 staged 变化是否遗留高价值知识 | 每次提交执行 |

## 2. MCP 配置

Codex 和 Claude Code 均应把当前项目根目录传给 cyClaw：

```text
cyclaw mcp --path <项目根目录>
```

MCP 写操作只经过 Rust Core，不允许任意 Shell 或任意文件写入。核心工具包括：

- `doctor`：检查 Git、初始化状态、权限和活动模型。
- `analyze_changes`：执行一次增量知识分析。
- `get_candidate_detail`、`review_candidate`：查看证据并审阅候选。
- `preview_document_patch`：生成或复用草稿，但不修改目标文档。
- `apply_document_patch`、`revert_document_patch`：应用或撤销受策略保护的文档变更。
- `run_agent`、`set_runtime_strategy`：兼容性的批处理审阅与策略设置，不承担事件采集职责。
- 任务、决策和检查点接口：人工补录或历史兼容，不要求 Coding Agent 调用。
- `reconcile_project_knowledge`：手动诊断入口；独立 Observer 会自动对账。

## 3. Skills

项目提供三个 Skills：

| Skill | 使用时机 |
| --- | --- |
| `cyclaw-project-knowledge` | 需要人工审批或修正 Observer 产出的知识 Patch 时 |
| `cyclaw-session-close` | 仅用于人工审核，不作为编码任务的强制收尾 |
| `cyclaw-doc-audit` | 发布、重大合并、架构评审和定期文档漂移审计 |

安装到目标项目：

```powershell
.\scripts\install-agent-skills.ps1 -Target Both -ProjectPath "D:\path\to\project"
```

Codex 使用 `.agents/skills/`，Claude Code 使用 `.claude/skills/`。脚本默认拒绝覆盖已有 Skill；确认更新时显式传入 `-Force`。

## 4. Codex 集成

Codex 侧只作为读取与审批界面；项目自主性来自单独运行的 Observer：

```text
cyclaw observer run + MCP（只读/审批）+ 可选 Skills
```

独立运行顺序：

```text
Observer 启动补偿 -> 文件/Git/报告事件
-> 候选与证据 -> 自动验证与对账 -> Patch 审批
```

只有真正影响后续工作的约束、决策、失败路径和验证结论才进入 Fact Ledger；普通过程日志不应写入长期记忆。

这里的 Codex 包括当前 ChatGPT/Codex 桌面应用中的编码 Agent。使用本地 stdio MCP，不需要 HTTP 服务、HTTPS 隧道或 OAuth：

```powershell
.\scripts\install-codex-mcp.ps1
```

全局注册只保存 `cyclaw.exe mcp`，不固定项目路径。Codex 从当前工作目录启动 MCP，每个项目继续使用自己的 `.cyclaw/`。新增 MCP 配置只会在新会话或 Codex 重启后加载。

建议在目标项目部署独立 Observer，而不是要求 `AGENTS.md` 驱动 Coding Agent：

```markdown
以独立进程运行 `cyclaw observer run --path <项目根目录>`。Coding Agent 仅在需要时读取候选、预览 Patch 或审批文档变更。
```

不要把 Codex、Claude Code 或 IDE 生命周期 Hook 当作知识采集前提；它们只是可选的管理和展示入口。

## 5. Claude Code 集成

Claude Code 可在项目 `.claude/settings.json` 中接入生命周期 Hook。以下配置假设 `cyclaw` 已加入 `PATH`：

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume|clear|compact",
        "hooks": [
          {
            "type": "command",
            "command": "cyclaw hooks run session-start --path \"$CLAUDE_PROJECT_DIR\""
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "cyclaw hooks run session-stop --path \"$CLAUDE_PROJECT_DIR\""
          }
        ]
      }
    ]
  }
}
```

`SessionStart` 只读取状态和建议；`session-stop` 会分析变化并生成候选，但不会绕过策略直接写项目文档。

不建议把 cyClaw 挂到每次 `PostToolUse`：文件保存已由操作系统事件驱动 Watch 处理，重复执行会增加模型上下文、磁盘写入和日志噪声。只有未运行 IDE Watch 的纯终端流程，才考虑对特定写文件工具增加低频 Hook。

## 6. Git Hook

安装：

```powershell
cyclaw hooks install-git --path .
cyclaw hooks status --path .
```

默认模式只提示，不阻止提交。严格模式：

```powershell
$env:CYCLAW_HOOK_STRICT = "1"
git commit -m "message"
```

`pre-commit` 只分析 staged 文件，并只按本轮相关的高置信度候选决定是否阻止提交，不受工作区其他未暂存文件和历史无关候选影响。

已有非 cyClaw `pre-commit` 时默认拒绝覆盖。当前 `--force` 会替换原 Hook，因此更推荐由现有 Hook 管理器调用：

```sh
cyclaw hooks run pre-commit --path "$(git rev-parse --show-toplevel)"
```

卸载只删除带 `# managed-by-cyclaw` 标记的 Hook：

```powershell
cyclaw hooks uninstall-git --path .
```

## 7. 后续组件优先级

参考 Codex/Claude Code 后，适合 cyClaw 的后续优化顺序：

1. **Hook 组合模式**：支持生成 Hook 片段，而非覆盖现有 Hook，并兼容 Husky、lefthook 和 pre-commit framework。
2. **上下文压缩事件**：会话压缩前自动调用现有 checkpoint 协议，保存决策、限制、失败路径和后续事项。
3. **结构化通知**：Hook 支持 JSON 输出和稳定退出码，让 Agent/IDE 能展示阻塞原因而非解析控制台文本。
4. **对账确认工作流**：把重复、冲突和失效建议转成可预览、可批准、可撤销的 Fact 状态变更。
5. **去重与冷却窗口**：同一 Git 指纹、同一会话和同一候选不重复分析，降低 Stop Hook 重复触发成本。
6. **上下文效果评测**：持续测量召回准确率、无关上下文比例、Token 节省和错误约束注入率。

当前不应优先实现每次工具调用 Hook、任意 Shell Hook 或云端事件总线。这些能力与 cyClaw 的低资源、本地优先和权限克制目标不匹配。
