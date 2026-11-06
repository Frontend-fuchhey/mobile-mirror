$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $ScriptDir

# Auto-discover Android SDK
$SdkRoot = $env:ANDROID_HOME
if (-not $SdkRoot) { $SdkRoot = $env:ANDROID_SDK_ROOT }
if (-not $SdkRoot) {
    $DefaultSdk = Join-Path $env:LOCALAPPDATA "Android\Sdk"
    if (Test-Path $DefaultSdk) {
        $SdkRoot = $DefaultSdk
    }
}

if (-not $SdkRoot -or -not (Test-Path $SdkRoot)) {
    throw "Android SDK not found. Set ANDROID_HOME or ANDROID_SDK_ROOT."
}

$PlatformDir = Get-ChildItem (Join-Path $SdkRoot "platforms") -Directory | Sort-Object Name -Descending | Select-Object -First 1
$BuildToolsDir = Get-ChildItem (Join-Path $SdkRoot "build-tools") -Directory | Sort-Object Name -Descending | Select-Object -First 1

if (-not $PlatformDir -or -not $BuildToolsDir) {
    throw "Could not find Android platform or build-tools in $SdkRoot"
}

$AndroidJar = Join-Path $PlatformDir.FullName "android.jar"
$D8Bat = Join-Path $BuildToolsDir.FullName "d8.bat"

Write-Host "[Build] Using Android Platform : $($PlatformDir.Name)" -ForegroundColor Cyan
Write-Host "[Build] Using Build-Tools      : $($BuildToolsDir.Name)" -ForegroundColor Cyan

$BuildDir = Join-Path $ScriptDir "build"
$ClassesDir = Join-Path $BuildDir "classes"
$OutputJar = Join-Path $ScriptDir "server.jar"

if (Test-Path $BuildDir) {
    Remove-Item $BuildDir -Recurse -Force
}
New-Item -ItemType Directory -Path $ClassesDir -Force | Out-Null

$JavaFiles = Get-ChildItem (Join-Path $ScriptDir "src\main\java") -Recurse -Filter "*.java" | ForEach-Object { $_.FullName }

Write-Host "[Build] Compiling $($JavaFiles.Count) Java source files..." -ForegroundColor Yellow
& javac --release 11 -cp $AndroidJar -d $ClassesDir $JavaFiles
if ($LASTEXITCODE -ne 0) {
    throw "javac compilation failed with exit code $LASTEXITCODE"
}

$ClassFiles = Get-ChildItem $ClassesDir -Recurse -Filter "*.class" | ForEach-Object { $_.FullName }

Write-Host "[Build] Converting JVM bytecode to DEX archive (server.jar)..." -ForegroundColor Yellow
& $D8Bat --min-api 21 --lib $AndroidJar --output $OutputJar $ClassFiles
if ($LASTEXITCODE -ne 0) {
    throw "d8 dexing failed with exit code $LASTEXITCODE"
}

if (Test-Path $BuildDir) {
    Remove-Item $BuildDir -Recurse -Force
}

$JarItem = Get-Item $OutputJar
Write-Host "[Build] Successfully built $($JarItem.FullName) ($($JarItem.Length) bytes)" -ForegroundColor Green
