param(
  [ValidateSet("Codex", "Claude", "Both")]
  [string]$Target = "Both",
  [string]$ProjectPath = (Get-Location).Path,
  [switch]$Force
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$sourceRoot = Join-Path $repoRoot "skills"
$projectRoot = (Resolve-Path $ProjectPath).Path
$skills = @(
  "cyclaw-project-knowledge",
  "cyclaw-session-close",
  "cyclaw-doc-audit"
)

function Install-Skills([string]$destinationRoot) {
  New-Item -ItemType Directory -Force -Path $destinationRoot | Out-Null
  foreach ($skill in $skills) {
    $source = Join-Path $sourceRoot $skill
    $destination = Join-Path $destinationRoot $skill
    if (Test-Path $destination) {
      if (-not $Force) {
        throw "Skill 已存在，拒绝覆盖: $destination。确认更新时使用 -Force。"
      }
      Remove-Item -Recurse -Force -LiteralPath $destination
    }
    Copy-Item -Recurse -Force -Path $source -Destination $destination
    Write-Host "已安装 Skill: $destination"
  }
}

if ($Target -in @("Codex", "Both")) {
  Install-Skills (Join-Path $projectRoot ".agents/skills")
}
if ($Target -in @("Claude", "Both")) {
  Install-Skills (Join-Path $projectRoot ".claude/skills")
}

Write-Host "Agent Skills 安装完成。重新打开 Agent 会话后生效。"
