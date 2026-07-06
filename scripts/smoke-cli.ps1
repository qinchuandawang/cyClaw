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
cargo run -p cyclaw-cli -- watch --once --interval 0 --path $ProjectRoot
cargo run -p cyclaw-cli -- inbox list --pending --path $ProjectRoot
cargo run -p cyclaw-cli -- draft generate --include-pending --path $ProjectRoot
cargo run -p cyclaw-cli -- draft list --path $ProjectRoot
cargo run -p cyclaw-cli -- index --path $ProjectRoot
cargo run -p cyclaw-cli -- search "package" --path $ProjectRoot
cargo run -p cyclaw-cli -- status --path $ProjectRoot
