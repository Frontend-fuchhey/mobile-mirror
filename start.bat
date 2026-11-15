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

if not exist "%~dp0recordings" mkdir "%~dp0recordings"

:: -------------------------------------------------------------------
:: [STEP 3/3] Launch Mobile Mirror (Automatic USB Mirroring Mode)
:: -------------------------------------------------------------------
echo %ESC%[96m======================================================================%ESC%[0m
echo %ESC%[92;1m  LAUNCHING MOBILE MIRROR%ESC%[0m
echo.
echo %ESC%[97m  AUTOMATIC USB CONNECTION:%ESC%[0m
echo %ESC%[96m    * Plug in your Android phone via USB (with USB Debugging enabled)%ESC%[0m
echo %ESC%[90m      Mobile Mirror automatically detects your phone and starts streaming.%ESC%[0m
echo.
echo %ESC%[97m  LIVE MIRROR HOTKEYS:%ESC%[0m
echo %ESC%[93m    * Press [R] in window : Start / Stop Lossless MP4 Recording%ESC%[0m
echo %ESC%[96m    * Press [M] in window : Toggle Phone Mic Voice Recording ON/OFF%ESC%[0m
echo %ESC%[93m    * Press [Q] or Close  : Disconnect and Exit%ESC%[0m
echo %ESC%[96m======================================================================%ESC%[0m
echo.

"client\target\release\mobile-mirror-client.exe" --port 12345 --record-dir "%~dp0recordings"
set "CLIENT_EXIT_CODE=%errorlevel%"

if "%CLIENT_EXIT_CODE%"=="0" (
    echo.
    echo %ESC%[92;1m[SUCCESS] Mobile Mirror session finished cleanly.%ESC%[0m
) else (
    echo.
    echo %ESC%[93m[INFO] Client exited with code %CLIENT_EXIT_CODE%.%ESC%[0m
)

cd /d "%~dp0"
exit /b %CLIENT_EXIT_CODE%

:error_exit
echo %ESC%[91mSession aborted due to build error.%ESC%[0m
echo.
pause
exit /b 1
