@echo off
setlocal EnableExtensions EnableDelayedExpansion

title Mobile Mirror
cd /d "%~dp0"

:: Initialize ANSI color escape sequence
for /f %%a in ('echo prompt $E ^| cmd') do set "ESC=%%a"

echo %ESC%[96m======================================================================%ESC%[0m
echo %ESC%[97;1m              MOBILE MIRROR - 60 FPS USB SCREEN MIRRORING             %ESC%[0m
echo %ESC%[96m======================================================================%ESC%[0m
echo.

:: -------------------------------------------------------------------
:: [STEP 1/3] Ensure Android Server DEX Artifact (server.jar) Exists
:: -------------------------------------------------------------------
if not exist "server\server.jar" (
    echo %ESC%[93m[BUILD] server.jar not found, compiling Android capture server...%ESC%[0m
    powershell -ExecutionPolicy Bypass -File server\build.ps1
    if errorlevel 1 (
        echo.
        echo %ESC%[91;1m[ERROR] Failed to compile server/server.jar!%ESC%[0m
        goto :error_exit
    )
)

:: -------------------------------------------------------------------
:: [STEP 2/3] Ensure Rust SDL3 Desktop Client Binary Exists
:: -------------------------------------------------------------------
if not exist "client\target\release\mobile-mirror-client.exe" (
    echo %ESC%[93m[BUILD] Compiling Rust SDL3 desktop client...%ESC%[0m
    cargo build --release --manifest-path client\Cargo.toml
    if errorlevel 1 (
        echo.
        echo %ESC%[91;1m[ERROR] Failed to compile Rust desktop client!%ESC%[0m
        goto :error_exit
    )
)
