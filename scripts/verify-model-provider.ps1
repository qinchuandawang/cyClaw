param(
  [string]$ProjectRoot = "",
  [string]$Provider = "deepseek",
  [string]$BaseUrl = "https://api.deepseek.com",
  [string]$Model = "deepseek-v4-flash",
  [string]$ApiKeyEnv = "DEEPSEEK_API_KEY"
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($ProjectRoot)) {
  $ProjectRoot = Split-Path -Parent $PSScriptRoot
}

$ProjectRoot = (Resolve-Path $ProjectRoot).Path

if ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($ApiKeyEnv))) {
  throw "环境变量未设置: $ApiKeyEnv"
}

cargo run -p cyclaw-cli -- model add $Provider --base-url $BaseUrl --model $Model --api-key-env $ApiKeyEnv --path $ProjectRoot
cargo run -p cyclaw-cli -- model list --path $ProjectRoot
cargo run -p cyclaw-cli -- model test $Provider --prompt "请回答：cyClaw model gateway ok" --path $ProjectRoot

Write-Host "Model provider verification passed"
