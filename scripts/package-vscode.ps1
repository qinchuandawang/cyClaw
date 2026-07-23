param()

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$extensionRoot = Join-Path $repoRoot "apps/vscode"
$binaryDir = Join-Path $extensionRoot "bin/win32-x64"
$sourceBinary = Join-Path $repoRoot "target/release/cyclaw.exe"

Push-Location $repoRoot
try {
  $vcvarsCandidates = @(
    (Join-Path ${env:ProgramFiles} "Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"),
    "C:\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
  )
  $vcvars = $vcvarsCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
  if ($vcvars) {
    $buildCommand = "call `"$vcvars`" >nul 2>nul && cargo build --release -p cyclaw-cli"
    cmd.exe /d /s /c $buildCommand
    if ($LASTEXITCODE -ne 0) {
      throw "使用 MSVC 构建 cyClaw CLI 失败，退出码: $LASTEXITCODE"
    }
  } else {
    cargo build --release -p cyclaw-cli
  }
}
finally {
  Pop-Location
}

New-Item -ItemType Directory -Force -Path $binaryDir | Out-Null
Copy-Item -Force $sourceBinary (Join-Path $binaryDir "cyclaw.exe")

Push-Location $extensionRoot
try {
  pnpm install --frozen-lockfile
  pnpm run check
  pnpm run compile
  pnpm dlx @vscode/vsce package --allow-missing-repository
}
finally {
  Pop-Location
}
