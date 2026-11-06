@echo off
setlocal EnableExtensions EnableDelayedExpansion

title Mobile Mirror - High-Performance Real-Time Screen Mirroring
cd /d "%~dp0"

:: Initialize ANSI color escape sequence
for /f %%a in ('echo prompt $E ^| cmd') do set "ESC=%%a"

echo %ESC%[95m======================================================================%ESC%[0m
echo %ESC%[97;1m          MOBILE MIRROR - REAL-TIME ANDROID STREAMING AND RECORDING    %ESC%[0m
echo %ESC%[95m======================================================================%ESC%[0m
echo.

:: -------------------------------------------------------------------
:: [STEP 1/5] Locate Android Debug Bridge (ADB) and Verify Connection
:: -------------------------------------------------------------------
echo %ESC%[96;1m[STEP 1/5]%ESC%[0m Detecting ADB toolchain and connected Android devices...

set "ADB=adb"
where adb >nul 2>&1
if errorlevel 1 (
    if exist "%LOCALAPPDATA%\Android\Sdk\platform-tools\adb.exe" (
        set "ADB=%LOCALAPPDATA%\Android\Sdk\platform-tools\adb.exe"
        echo %ESC%[93m[INFO] Using ADB from Android SDK: !ADB!%ESC%[0m
    ) else (
        echo.
        echo %ESC%[91;1m[ERROR] Android Debug Bridge [adb] was not found in PATH or Android SDK!%ESC%[0m
        echo Please ensure Android platform-tools are installed or add adb to your system PATH.
        echo.
        goto :error_exit
    )
)

:: Query connected devices
set "DEVICE_FOUND=0"
set "DEVICE_SERIAL="
for /f "skip=1 tokens=1,2" %%A in ('"%ADB%" devices') do (
    if "%%B"=="device" (
        set "DEVICE_FOUND=1"
        set "DEVICE_SERIAL=%%A"
    ) else if "%%B"=="unauthorized" (
        echo.
        echo %ESC%[91;1m[ERROR] Device %%A is UNAUTHORIZED!%ESC%[0m
        echo Please unlock your phone and tap 'Allow USB debugging' on the screen.
        echo.
        goto :error_exit
    ) else if "%%B"=="offline" (
        echo.
        echo %ESC%[91;1m[ERROR] Device %%A is OFFLINE!%ESC%[0m
        echo Please reconnect the USB cable or restart ADB: adb kill-server
        echo.
        goto :error_exit
    )
)

if "!DEVICE_FOUND!"=="0" (
    echo.
    echo %ESC%[91;1m[ERROR] No active Android device detected!%ESC%[0m
    echo Please verify:
    echo   1. Your Android phone is securely connected via USB.
    echo   2. 'Developer Options' and 'USB Debugging' are enabled in Android settings.
    echo   3. 'adb devices' lists your phone in 'device' state.
    echo.
    goto :error_exit
)

echo %ESC%[92m[SUCCESS] Android device detected: !DEVICE_SERIAL!%ESC%[0m
echo.

:: -------------------------------------------------------------------
:: [STEP 2/5] Push Server DEX JAR to Android Device
:: -------------------------------------------------------------------
echo %ESC%[96;1m[STEP 2/5]%ESC%[0m Pushing server DEX archive to /data/local/tmp/...

if not exist "server\server.jar" (
    echo %ESC%[93m[INFO] server.jar not found, compiling Android server from source...%ESC%[0m
    powershell -ExecutionPolicy Bypass -File server\build.ps1
    if errorlevel 1 (
        echo.
        echo %ESC%[91;1m[ERROR] Failed to compile server/server.jar!%ESC%[0m
        goto :error_exit
    )
)

"%ADB%" push "server\server.jar" /data/local/tmp/server.jar
if errorlevel 1 (
    echo.
    echo %ESC%[91;1m[ERROR] Failed to push server.jar to device via ADB!%ESC%[0m
    goto :error_exit
)
echo %ESC%[92m[SUCCESS] server.jar transferred to /data/local/tmp/server.jar%ESC%[0m
echo.

:: -------------------------------------------------------------------
:: [STEP 3/5] Clean Lingering Processes & Configure Port Forwarding
:: -------------------------------------------------------------------
echo %ESC%[96;1m[STEP 3/5]%ESC%[0m Setting up TCP port forwarding [127.0.0.1:12345]...
"%ADB%" forward --remove tcp:12345 >nul 2>&1
"%ADB%" shell "for pid in \$(pidof app_process64 app_process32 app_process 2>/dev/null); do if grep -q 'com.mirror.Server' /proc/\$pid/cmdline 2>/dev/null; then kill -9 \$pid 2>/dev/null; fi; done" >nul 2>&1
"%ADB%" forward tcp:12345 tcp:12345
if errorlevel 1 (
    echo.
    echo %ESC%[91;1m[ERROR] 'adb forward tcp:12345 tcp:12345' failed!%ESC%[0m
    goto :error_exit
)
echo %ESC%[92m[SUCCESS] Port forwarding active: tcp:12345 -^> device:12345%ESC%[0m
echo.

:: -------------------------------------------------------------------
:: [STEP 4/5] Spawn Android Server Daemon in Background
:: -------------------------------------------------------------------
echo %ESC%[96;1m[STEP 4/5]%ESC%[0m Spawning Android server daemon via app_process [UID 2000]...
start "Mobile Mirror Server Daemon" /min "%ADB%" shell "CLASSPATH=/data/local/tmp/server.jar app_process / com.mirror.Server port=12345 bitrate=12000000 fps=60 cbr=true"

:: Wait for server socket to initialize
timeout /t 2 /nobreak >nul
echo %ESC%[92m[SUCCESS] Android server daemon running in background.%ESC%[0m
echo.

:: -------------------------------------------------------------------
:: [STEP 5/5] Launch Rust Desktop Client
:: -------------------------------------------------------------------
echo %ESC%[96;1m[STEP 5/5]%ESC%[0m Launching Rust desktop client [FFmpeg/SDL3]...

if not exist "client\target\release\mobile-mirror-client.exe" (
    echo %ESC%[93m[INFO] Client binary not found, compiling Rust desktop client...%ESC%[0m
    cargo build --release --manifest-path client\Cargo.toml
    if errorlevel 1 (
        echo.
        echo %ESC%[91;1m[ERROR] Failed to compile Rust desktop client!%ESC%[0m
        goto :error_exit
    )
)

if not exist "%~dp0recordings" mkdir "%~dp0recordings"

echo.
echo %ESC%[93m======================================================================%ESC%[0m
echo %ESC%[92;1m  PHONE SCREEN MIRRORING ACTIVE!%ESC%[0m
echo.
echo %ESC%[97m  WINDOW CONTROLS:%ESC%[0m
echo %ESC%[93m    * Press [R] in window  : Start / Stop Screen Recording%ESC%[0m
echo %ESC%[96m    * Press [M] in window  : Toggle Phone Mic Voice Recording ON/OFF%ESC%[0m
echo %ESC%[90m                             (Voice-Only Isolation: Suppresses Background Noise)%ESC%[0m
echo %ESC%[93m    * Press [Q] or Close   : Disconnect and Close Mirror%ESC%[0m
echo.
echo %ESC%[92m  AUDIO MODE: Plays directly from your Phone (never muted) + Saved in MP4%ESC%[0m
echo %ESC%[90m  Recordings directory: %~dp0recordings\%ESC%[0m
echo %ESC%[93m======================================================================%ESC%[0m
echo.

"client\target\release\mobile-mirror-client.exe" --port 12345 --record-dir "%~dp0recordings" --hw-backend nvdec
set "CLIENT_EXIT_CODE=%errorlevel%"

:: -------------------------------------------------------------------
:: Cleanup on Exit
:: -------------------------------------------------------------------
echo.
echo %ESC%[96m[CLEANUP]%ESC%[0m Terminating Android server daemon and releasing port forwarding...
"%ADB%" forward --remove tcp:12345 >nul 2>&1
"%ADB%" shell "for pid in \$(pidof app_process64 app_process32 app_process 2>/dev/null); do if grep -q 'com.mirror.Server' /proc/\$pid/cmdline 2>/dev/null; then kill -9 \$pid 2>/dev/null; fi; done" >nul 2>&1

if "%CLIENT_EXIT_CODE%"=="0" (
    echo %ESC%[92;1m[SUCCESS] Mirroring and recording session completed successfully.%ESC%[0m
) else (
    echo %ESC%[93m[INFO] Client exited with code %CLIENT_EXIT_CODE%.%ESC%[0m
)

cd /d "%~dp0"
echo.
pause
exit /b %CLIENT_EXIT_CODE%

:error_exit
echo %ESC%[91mSession aborted due to error.%ESC%[0m
echo.
pause
exit /b 1
