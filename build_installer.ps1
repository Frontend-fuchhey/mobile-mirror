<#
.SYNOPSIS
    Builds Mobile Mirror (Rust release client + Android server.jar) and packages
    all runtime dependencies into a single Windows installer: dist\MobileMirror-Setup.exe

.EXAMPLE
    .\build_installer.ps1 -Version "1.0.0"
#>
param(
    [string]$Version = "1.0.0",
    [switch]$SkipRustBuild,
    [switch]$RebuildServer,
    [string]$AdbDir = ""
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$DistDir = Join-Path $RepoRoot "dist"
$StageDir = Join-Path $DistDir "stage"

Write-Host "=== Mobile Mirror Windows Installer Builder (v$Version) ===" -ForegroundColor Cyan

# 1. Verify or build Android server.jar
$ServerJar = Join-Path $RepoRoot "server\server.jar"
if ($RebuildServer -or -not (Test-Path $ServerJar)) {
    Write-Host "[1/5] Building Android server.jar..." -ForegroundColor Yellow
    $ServerBuildScript = Join-Path $RepoRoot "server\build.ps1"
    & powershell -ExecutionPolicy Bypass -File $ServerBuildScript
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $ServerJar)) {
        throw "Failed to build server/server.jar."
    }
} else {
    Write-Host "[1/5] Using existing server\server.jar ($((Get-Item $ServerJar).Length) bytes)" -ForegroundColor Green
}

# 2. Build Rust release client
$ClientManifest = Join-Path $RepoRoot "client\Cargo.toml"
$ClientExe = Join-Path $RepoRoot "client\target\release\mobile-mirror-client.exe"
if (-not $SkipRustBuild) {
    Write-Host "[2/5] Compiling Rust release binary (cargo build --release)..." -ForegroundColor Yellow
    & cargo build --release --manifest-path $ClientManifest
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $ClientExe)) {
        throw "Cargo release build failed."
    }
} elseif (-not (Test-Path $ClientExe)) {
    throw "SkipRustBuild was set, but $ClientExe does not exist."
} else {
    Write-Host "[2/5] Using existing release binary: $ClientExe" -ForegroundColor Green
}

# 3. Prepare clean staging directory
Write-Host "[3/5] Staging application binaries and runtime DLLs into $StageDir..." -ForegroundColor Yellow
if (Test-Path $StageDir) {
    Remove-Item -Recurse -Force $StageDir
}
New-Item -ItemType Directory -Force -Path $StageDir | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $StageDir "recordings") | Out-Null

# Main executable (renamed to user-friendly "Mobile Mirror.exe")
Copy-Item -Path $ClientExe -Destination (Join-Path $StageDir "Mobile Mirror.exe") -Force

# Pre-compiled Android DEX server
Copy-Item -Path $ServerJar -Destination (Join-Path $StageDir "server.jar") -Force

# Runtime DLLs (SDL3 + FFmpeg)
$ClientLibDir = Join-Path $RepoRoot "client\lib"
if (-not (Test-Path $ClientLibDir)) {
    throw "Missing runtime DLL directory: $ClientLibDir"
}
$Dlls = Get-ChildItem -Path $ClientLibDir -Filter "*.dll" -File
if ($Dlls.Count -eq 0) {
    throw "No runtime DLLs found in $ClientLibDir"
}
foreach ($dll in $Dlls) {
    Copy-Item -Path $dll.FullName -Destination (Join-Path $StageDir $dll.Name) -Force
    Write-Host "  + Bundled $($dll.Name)" -ForegroundColor DarkGray
}

# Locate and bundle ADB + WinUSB DLLs
function Find-AdbDirectory {
    param([string]$ExplicitDir)
    $candidates = @()
    if ($ExplicitDir) { $candidates += $ExplicitDir }
    $cmd = Get-Command "adb.exe" -ErrorAction SilentlyContinue
    if ($cmd) { $candidates += (Split-Path -Parent $cmd.Source) }
    if ($env:ANDROID_HOME) { $candidates += (Join-Path $env:ANDROID_HOME "platform-tools") }
    if ($env:ANDROID_SDK_ROOT) { $candidates += (Join-Path $env:ANDROID_SDK_ROOT "platform-tools") }
    if ($env:LOCALAPPDATA) { $candidates += (Join-Path $env:LOCALAPPDATA "Android\Sdk\platform-tools") }

    foreach ($dir in $candidates) {
        if ($dir -and (Test-Path (Join-Path $dir "adb.exe"))) {
            return $dir
        }
    }
    return $null
}

$ResolvedAdbDir = Find-AdbDirectory -ExplicitDir $AdbDir
if (-not $ResolvedAdbDir) {
    throw "Could not locate adb.exe. Install Android platform-tools or pass -AdbDir <path>."
}

foreach ($adbFile in @("adb.exe", "AdbWinApi.dll", "AdbWinUsbApi.dll")) {
    $src = Join-Path $ResolvedAdbDir $adbFile
    if (-not (Test-Path $src)) {
        throw "Required ADB runtime file missing: $src"
    }
    Copy-Item -Path $src -Destination (Join-Path $StageDir $adbFile) -Force
    Write-Host "  + Bundled $adbFile" -ForegroundColor DarkGray
}

# 4. Locate Inno Setup Compiler (ISCC.exe)
Write-Host "[4/5] Locating Inno Setup Compiler (ISCC.exe)..." -ForegroundColor Yellow
$IsccCandidates = @(
    (Get-Command "ISCC.exe" -ErrorAction SilentlyContinue | ForEach-Object { $_.Source }),
    (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe"),
    "C:\Program Files (x86)\Inno Setup 6\ISCC.exe",
    "C:\Program Files\Inno Setup 6\ISCC.exe"
)
$IsccPath = $IsccCandidates | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $IsccPath) {
    throw "Inno Setup 6 (ISCC.exe) not found. Install it via: winget install JRSoftware.InnoSetup"
}
Write-Host "  Using ISCC: $IsccPath" -ForegroundColor Green

# 5. Compile MobileMirror-Setup.exe
Write-Host "[5/5] Compiling dist\MobileMirror-Setup.exe..." -ForegroundColor Yellow
$IssPath = Join-Path $RepoRoot "installer.iss"
& $IsccPath "/DAppVersion=$Version" "/DStageDir=$StageDir" "/DOutputDir=$DistDir" $IssPath
if ($LASTEXITCODE -ne 0) {
    throw "Inno Setup compilation failed with exit code $LASTEXITCODE."
}

$SetupExe = Join-Path $DistDir "MobileMirror-Setup.exe"
if (-not (Test-Path $SetupExe)) {
    throw "Expected output installer not found at $SetupExe"
}

$SizeMB = [math]::Round((Get-Item $SetupExe).Length / 1MB, 2)
Write-Host ""
Write-Host "SUCCESS: Built $SetupExe ($SizeMB MB)" -ForegroundColor Green
