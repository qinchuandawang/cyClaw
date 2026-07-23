param(
  [switch]$RequireIdea = $false
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot

Write-Host "Verifying VS Code / Cursor plugin"
Push-Location (Join-Path $repoRoot "apps/vscode")
try {
  pnpm install --frozen-lockfile
  pnpm run check
  pnpm run compile
}
finally {
  Pop-Location
}

Write-Host "Verifying IntelliJ IDEA plugin"
$ideaDir = Join-Path $repoRoot "apps/idea"
$gradlew = Join-Path $ideaDir "gradlew.bat"
$wrapperJar = Join-Path $ideaDir "gradle/wrapper/gradle-wrapper.jar"

if (Test-Path $wrapperJar) {
  Push-Location $ideaDir
  try {
    & $gradlew buildPlugin
  }
  finally {
    Pop-Location
  }
  exit 0
}

$gradleCommand = Get-Command gradle -ErrorAction SilentlyContinue
if ($gradleCommand) {
  Push-Location $ideaDir
  try {
    gradle wrapper --gradle-version 8.7 --distribution-type bin
    & $gradlew buildPlugin
  }
  finally {
    Pop-Location
  }
  exit 0
}

$message = "IntelliJ IDEA plugin source exists, but Gradle is unavailable and gradle-wrapper.jar is missing. Install Gradle or generate apps/idea/gradle/wrapper/gradle-wrapper.jar, then rerun this script."
if ($RequireIdea) {
  throw $message
}

Write-Warning $message
