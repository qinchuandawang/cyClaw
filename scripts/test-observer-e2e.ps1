param(
  [string]$CyclawExe = ""
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($CyclawExe)) {
  cargo build -p cyclaw-cli | Out-Null
  $CyclawExe = Join-Path $PSScriptRoot "..\target\debug\cyclaw.exe"
}

$projectRoot = Join-Path $env:TEMP ("cyclaw-observer-e2e-" + [guid]::NewGuid().ToString("N"))
$logRoot = Join-Path $env:TEMP ("cyclaw-observer-log-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $projectRoot, $logRoot | Out-Null

git -C $projectRoot init | Out-Null
git -C $projectRoot config user.email test@example.com
git -C $projectRoot config user.name "cyClaw Observer Test"
Set-Content -NoNewline -Path (Join-Path $projectRoot "package.json") -Value '{"dependencies":{"react":"18.0.0"}}'
git -C $projectRoot add .
git -C $projectRoot commit -m initial | Out-Null

$stdout = Join-Path $logRoot "observer.stdout.log"
$stderr = Join-Path $logRoot "observer.stderr.log"
$process = Start-Process -FilePath $CyclawExe `
  -ArgumentList @("observer", "run", "--path", $projectRoot, "--debounce-ms", "150", "--maintenance-interval-seconds", "30") `
  -WorkingDirectory $projectRoot `
  -RedirectStandardOutput $stdout `
  -RedirectStandardError $stderr `
  -WindowStyle Hidden `
  -PassThru

try {
  Start-Sleep -Seconds 2
  Set-Content -NoNewline -Path (Join-Path $projectRoot "package.json") -Value '{"dependencies":{"react":"18.0.0","vite":"6.0.0"}}'
  Start-Sleep -Seconds 2

  $reportDir = Join-Path $projectRoot "target\surefire-reports"
  New-Item -ItemType Directory -Force -Path $reportDir | Out-Null
  Set-Content -NoNewline -Path (Join-Path $reportDir "TEST-demo.xml") -Value '<testsuite failures="1"><testcase><failure message="expected failure"/></testcase></testsuite>'
  Start-Sleep -Seconds 2
} finally {
  if (!$process.HasExited) {
    Stop-Process -Id $process.Id -Force
  }
}

$candidates = @(Get-Content (Join-Path $projectRoot ".cyclaw\knowledge-inbox.jsonl") |
  Where-Object { $_.Trim() } |
  ForEach-Object { $_ | ConvertFrom-Json })
$executionEvents = @(Get-Content (Join-Path $projectRoot ".cyclaw\execution-events.jsonl") |
  Where-Object { $_.Trim() } |
  ForEach-Object { $_ | ConvertFrom-Json })
$restart = & $CyclawExe observer run --once --path $projectRoot 2>&1 | Out-String
$ticks = ([regex]::Matches((Get-Content -Raw $stdout), "Observer 补偿结果")).Count

if ($ticks -ne 3) { throw "Observer tick 数异常，期望 3，实际 $ticks。日志：$stdout" }
if (@($candidates | Where-Object source_type -eq "change_analysis").Count -ne 1) { throw "未得到唯一的变更候选" }
if (@($candidates | Where-Object source_type -eq "execution_failure").Count -ne 1) { throw "未得到唯一的失败报告候选" }
if ($executionEvents.Count -ne 1) { throw "执行事件去重失败，期望 1，实际 $($executionEvents.Count)" }
if ($restart -notmatch "变更=否") { throw "重启后未恢复游标或重复处理变化：$restart" }

Write-Host "Observer E2E passed"
Write-Host "临时项目: $projectRoot"
