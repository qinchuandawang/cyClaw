param()

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$cyclawExe = Join-Path $repoRoot "target/debug/cyclaw.exe"
$evaluationRoot = Join-Path $repoRoot ("target/evaluations/task-memory-" + [guid]::NewGuid().ToString("N"))

Push-Location $repoRoot
try {
  cargo build -p cyclaw-cli
  if ($LASTEXITCODE -ne 0) {
    throw "构建 cyClaw CLI 失败"
  }
}
finally {
  Pop-Location
}

New-Item -ItemType Directory -Force -Path $evaluationRoot | Out-Null
git -C $evaluationRoot init --quiet
git -C $evaluationRoot config user.email "cyclaw-eval@example.com"
git -C $evaluationRoot config user.name "cyClaw Evaluation"
Set-Content -Path (Join-Path $evaluationRoot "refund.md") -Value "# Refund State`n" -Encoding utf8
git -C $evaluationRoot add .
git -C $evaluationRoot commit --quiet -m "initial"

& $cyclawExe init --path $evaluationRoot | Out-Null
& $cyclawExe task begin "退款约束确认" --objective "确认退款状态机不可逆约束" --related-file refund.md --path $evaluationRoot | Out-Null
& $cyclawExe task decision "退款完成后不得重新进入处理中" --rationale "避免重复退款" --evidence refund.md --confidence 95 --path $evaluationRoot | Out-Null
& $cyclawExe task failure "完成状态直接回退到处理中" --reason "会触发重复退款" --evidence refund.md --path $evaluationRoot | Out-Null
& $cyclawExe task close "已确认退款状态约束" --path $evaluationRoot | Out-Null

& $cyclawExe task begin "修改退款状态机" --objective "调整退款处理中状态" --related-file refund.md --path $evaluationRoot | Out-Null
$contextJson = & $cyclawExe task context --query "退款处理中" --budget-tokens 1000 --path $evaluationRoot
$context = $contextJson | ConvertFrom-Json
$statements = @($context.facts.facts | ForEach-Object { $_.fact.statement })
$recalledDecision = [bool]($statements | Where-Object { $_ -like "*不得重新进入处理中*" })
$recalledFailure = [bool]($statements | Where-Object { $_ -like "*失败方案*重复退款*" })

& $cyclawExe task decision "项目退款完成后不得重新进入处理中" --rationale "保持状态机不可逆" --evidence refund.md --confidence 90 --path $evaluationRoot | Out-Null
& $cyclawExe task reconcile --path $evaluationRoot | Out-Null
$reportPath = Get-ChildItem (Join-Path $evaluationRoot ".cyclaw/reconciliation") -Filter *.json |
  Sort-Object LastWriteTime -Descending |
  Select-Object -First 1 -ExpandProperty FullName
$report = Get-Content $reportPath -Raw | ConvertFrom-Json

$result = [ordered]@{
  evaluation_root = $evaluationRoot
  recalled_decision = $recalledDecision
  recalled_failed_approach = $recalledFailure
  context_fact_count = @($context.facts.facts).Count
  context_document_count = @($context.documents).Count
  estimated_tokens = $context.estimated_tokens
  budget_tokens = $context.budget_tokens
  duplicate_count = $report.duplicate_count
  conflict_count = $report.conflict_count
  stale_count = $report.stale_count
}

if (-not $recalledDecision -or -not $recalledFailure) {
  throw "跨会话任务记忆召回失败：$($result | ConvertTo-Json -Compress)"
}
if ($report.duplicate_count -lt 1) {
  throw "知识对账未发现预期重复事实：$($result | ConvertTo-Json -Compress)"
}

$result | ConvertTo-Json
