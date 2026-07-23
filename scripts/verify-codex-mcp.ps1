param(
  [string]$ProjectRoot = ""
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($ProjectRoot)) {
  $ProjectRoot = Split-Path -Parent $PSScriptRoot
}

$ProjectRoot = (Resolve-Path $ProjectRoot).Path
$cyclawExe = Join-Path $ProjectRoot "target/debug/cyclaw.exe"
$verificationRoot = Join-Path $ProjectRoot "target/evaluations/codex-mcp-$([guid]::NewGuid().ToString('N'))"

if (-not (Get-Command codex -ErrorAction SilentlyContinue)) {
  throw "codex CLI is not available on PATH"
}

cargo build -p cyclaw-cli

if (-not (Test-Path $cyclawExe)) {
  throw "cyclaw executable missing: $cyclawExe"
}

New-Item -ItemType Directory -Force -Path $verificationRoot | Out-Null
git -C $verificationRoot init --quiet
& $cyclawExe init --path $verificationRoot | Out-Null
& $cyclawExe task begin "MCP 验证任务" --objective "验证任务协议与资源暴露" --path $verificationRoot | Out-Null
& $cyclawExe task decision "MCP 必须使用本地 stdio" --rationale "保持项目隔离并避免公网服务" --path $verificationRoot | Out-Null
& $cyclawExe task reconcile --path $verificationRoot | Out-Null

function New-McpFrame {
  param([string]$Json)
  $length = [System.Text.Encoding]::UTF8.GetByteCount($Json)
  return "Content-Length: $length`r`n`r`n$Json"
}

function Invoke-McpBinary {
  param([string[]]$JsonMessages)

  $message = ($JsonMessages | ForEach-Object { New-McpFrame $_ }) -join ""
  $processInfo = New-Object System.Diagnostics.ProcessStartInfo
  $processInfo.FileName = $cyclawExe
  $processInfo.Arguments = "mcp --path `"$verificationRoot`""
  $processInfo.RedirectStandardInput = $true
  $processInfo.RedirectStandardOutput = $true
  $processInfo.RedirectStandardError = $true
  $processInfo.UseShellExecute = $false

  $process = [System.Diagnostics.Process]::Start($processInfo)
  $stdoutTask = $process.StandardOutput.ReadToEndAsync()
  $stderrTask = $process.StandardError.ReadToEndAsync()
  $process.StandardInput.Write($message)
  $process.StandardInput.Close()

  if (-not $process.WaitForExit(30000)) {
    $process.Kill()
    throw "cyclaw mcp timed out"
  }

  $stdout = $stdoutTask.Result
  $stderr = $stderrTask.Result

  if ($process.ExitCode -ne 0) {
    throw "cyclaw mcp failed: $stderr"
  }

  return $stdout
}

$mcpOutput = Invoke-McpBinary @(
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}',
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}',
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_project_status","arguments":{}}}',
  '{"jsonrpc":"2.0","id":4,"method":"resources/list","params":{}}'
)

foreach ($expected in @(
  '"serverInfo"',
  '"search_project_knowledge"',
  '"get_project_status"',
  '"analyze_changes"',
  '"preview_document_patch"',
  '"apply_document_patch"',
  '"supersede"',
  '"begin_task"',
  '"get_task_context"',
  '"record_decision"',
  '"record_failed_approach"',
  '"checkpoint_task"',
  '"reconcile_project_knowledge"',
  '"close_task"',
  '"list_project_facts"',
  'cyclaw://.cyclaw/memory/facts.jsonl',
  'cyclaw://.cyclaw/tasks/',
  'cyclaw://.cyclaw/reconciliation/',
  '"doctor"',
  '"resources"'
)) {
  if (-not $mcpOutput.Contains($expected)) {
    throw "MCP response missing expected content: $expected"
  }
}

$serverName = "cyclaw-smoke"
codex mcp remove $serverName 2>$null | Out-Null
codex mcp add $serverName -- $cyclawExe mcp | Out-Null
$codexMcp = codex mcp get $serverName
codex mcp remove $serverName | Out-Null

foreach ($expected in @(
  "enabled: true",
  "transport: stdio",
  "command: $cyclawExe",
  "args: mcp"
)) {
  if (-not (($codexMcp -join "`n").Contains($expected))) {
    throw "Codex MCP config missing expected content: $expected"
  }
}

Write-Host "Codex MCP verification passed"
Write-Host ""
Write-Host "Codex config snippet:"
Write-Host "[mcp_servers.cyclaw]"
Write-Host "command = `"$cyclawExe`""
Write-Host "args = [`"mcp`"]"
