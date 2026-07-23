param(
  [string]$ServerName = "cyclaw"
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$cyclawExe = Join-Path $repoRoot "target/release/cyclaw.exe"

Push-Location $repoRoot
try {
  cargo build --release -p cyclaw-cli
  if ($LASTEXITCODE -ne 0) {
    throw "构建 cyClaw Release CLI 失败"
  }
}
finally {
  Pop-Location
}

$existing = codex mcp list | Select-String -Pattern "^$([regex]::Escape($ServerName))\s"
if ($existing) {
  codex mcp remove $ServerName
  if ($LASTEXITCODE -ne 0) {
    throw "移除旧 Codex MCP 配置失败: $ServerName"
  }
}

codex mcp add $ServerName -- $cyclawExe mcp
if ($LASTEXITCODE -ne 0) {
  throw "注册 Codex MCP 失败: $ServerName"
}

codex mcp get $ServerName
Write-Host "cyClaw 已注册到 Codex。请新建会话或重启 Codex 以加载 MCP 工具。"
