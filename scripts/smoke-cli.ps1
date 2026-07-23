param(
  [string]$ProjectRoot = ""
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($ProjectRoot)) {
  $ProjectRoot = Join-Path $env:TEMP ("cyclaw-smoke-" + [guid]::NewGuid().ToString("N"))
  New-Item -ItemType Directory -Force -Path $ProjectRoot | Out-Null
  git -C $ProjectRoot init | Out-Null
  git -C $ProjectRoot config user.email test@example.com
  git -C $ProjectRoot config user.name "Test User"
  Set-Content -Path (Join-Path $ProjectRoot "package.json") -Value '{"dependencies":{"react":"latest"}}'
  git -C $ProjectRoot add .
  git -C $ProjectRoot commit -m initial | Out-Null
  Set-Content -Path (Join-Path $ProjectRoot "package.json") -Value '{"dependencies":{"react":"latest","vite":"latest"}}'
  Set-Content -Path (Join-Path $ProjectRoot ".env.example") -Value "API_URL=http://localhost"
}

Write-Host "Smoke project: $ProjectRoot"

cargo run -p cyclaw-cli -- init --path $ProjectRoot
cargo run -p cyclaw-cli -- scan --path $ProjectRoot
cargo run -p cyclaw-cli -- watch --once --path $ProjectRoot
cargo run -p cyclaw-cli -- inbox list --pending --path $ProjectRoot
cargo run -p cyclaw-cli -- draft generate --include-pending --path $ProjectRoot
cargo run -p cyclaw-cli -- draft list --path $ProjectRoot
cargo run -p cyclaw-cli -- index --path $ProjectRoot
cargo run -p cyclaw-cli -- search "package" --path $ProjectRoot
cargo run -p cyclaw-cli -- status --path $ProjectRoot
cargo run -p cyclaw-cli -- model add deepseek --base-url https://api.deepseek.com --model deepseek-v4-flash --api-key-env DEEPSEEK_API_KEY --path $ProjectRoot
cargo run -p cyclaw-cli -- model list --path $ProjectRoot
cargo run -p cyclaw-cli -- agent run --once --no-model --path $ProjectRoot
cargo run -p cyclaw-cli -- agent runs list --limit 5 --path $ProjectRoot
cargo run -p cyclaw-cli -- agent runs clean --keep 5 --path $ProjectRoot
cargo run -p cyclaw-cli -- policy show --path $ProjectRoot
cargo run -p cyclaw-cli -- policy check .cyclaw/agent-runs/demo.json --level local_knowledge_write --path $ProjectRoot
cargo run -p cyclaw-cli -- policy check docs/dependencies.md --level docs_write --path $ProjectRoot
cargo run -p cyclaw-cli -- policy enable-docs-apply --path $ProjectRoot
cargo run -p cyclaw-cli -- events list --limit 5 --path $ProjectRoot

function New-McpFrame {
  param([string]$Json)
  $length = [System.Text.Encoding]::UTF8.GetByteCount($Json)
  return "Content-Length: $length`r`n`r`n$Json"
}

function Invoke-CyclawMcp {
  param([string[]]$JsonMessages)

  $message = ($JsonMessages | ForEach-Object { New-McpFrame $_ }) -join ""
  $processInfo = New-Object System.Diagnostics.ProcessStartInfo
  $processInfo.FileName = "cargo"
  $processInfo.Arguments = "run -p cyclaw-cli -- mcp --path `"$ProjectRoot`""
  $processInfo.RedirectStandardInput = $true
  $processInfo.RedirectStandardOutput = $true
  $processInfo.RedirectStandardError = $true
  $processInfo.UseShellExecute = $false

  $process = [System.Diagnostics.Process]::Start($processInfo)
  $stdoutTask = $process.StandardOutput.ReadToEndAsync()
  $stderrTask = $process.StandardError.ReadToEndAsync()
  $process.StandardInput.Write($message)
  $process.StandardInput.Close()

  if (-not $process.WaitForExit(120000)) {
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

$mcpOutput = Invoke-CyclawMcp @(
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}',
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}',
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_project_status","arguments":{}}}',
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_project_profile","arguments":{}}}',
  '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"search_project_knowledge","arguments":{"query":"package","limit":5}}}',
  '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"list_pending_knowledge","arguments":{}}}',
  '{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"list_document_patches","arguments":{}}}',
  '{"jsonrpc":"2.0","id":8,"method":"resources/list","params":{}}',
  '{"jsonrpc":"2.0","id":9,"method":"resources/read","params":{"uri":"cyclaw://.cyclaw/project.md"}}'
)

foreach ($expected in @(
  '"serverInfo"',
  '"search_project_knowledge"',
  '"get_project_status"',
  '"get_project_profile"',
  '"list_pending_knowledge"',
  '"list_document_patches"',
  '"resources"',
  'cyclaw://.cyclaw/project.md'
)) {
  if (-not $mcpOutput.Contains($expected)) {
    throw "cyclaw mcp response missing expected content: $expected"
  }
}

Write-Host "MCP tools/resources smoke passed"

$pendingJson = Get-Content -Encoding UTF8 -Path (Join-Path $ProjectRoot ".cyclaw/knowledge-inbox.jsonl") | Select-Object -First 1 | ConvertFrom-Json
cargo run -p cyclaw-cli -- inbox accept $pendingJson.id --path $ProjectRoot
cargo run -p cyclaw-cli -- draft generate --candidate $pendingJson.id --path $ProjectRoot
$patchId = "patch_$($pendingJson.id)"
cargo run -p cyclaw-cli -- draft apply $patchId --path $ProjectRoot

if (-not (Test-Path (Join-Path $ProjectRoot $pendingJson.recommended_doc))) {
  throw "expected applied document missing: $($pendingJson.recommended_doc)"
}

Write-Host "CLI accept/draft/apply smoke passed"
