$ErrorActionPreference = "Stop"

$RootDir = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $RootDir

Write-Host "=== [1/2] Building Android DEX Server (server/server.jar) ===" -ForegroundColor Cyan
& powershell -ExecutionPolicy Bypass -File (Join-Path $RootDir "server\build.ps1")

Write-Host "`n=== [2/2] Building Rust Desktop Client (client/) ===" -ForegroundColor Cyan
& cargo test --manifest-path (Join-Path $RootDir "client\Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "Client unit tests failed" }

& cargo build --release --manifest-path (Join-Path $RootDir "client\Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "Client release build failed" }

Write-Host "`n[Success] Both Android server and PC client built cleanly!" -ForegroundColor Green
