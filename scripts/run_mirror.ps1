param(
    [int]$Port = 12345,
    [int]$Bitrate = 12000000,
    [int]$Fps = 60,
    [int]$MaxSize = 0,
    [string]$RecordPath = "recordings/recording.mp4",
    [string]$HwBackend = "nvdec"
)

$ErrorActionPreference = "Stop"
$RootDir = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$ServerJar = Join-Path $RootDir "server\server.jar"
$ClientBin = Join-Path $RootDir "client\target\release\mobile-mirror-client.exe"

# Locate ADB
$Adb = "adb"
if (-not (Get-Command $Adb -ErrorAction SilentlyContinue)) {
    $SdkAdb = Join-Path $env:LOCALAPPDATA "Android\Sdk\platform-tools\adb.exe"
    if (Test-Path $SdkAdb) { $Adb = $SdkAdb }
}

if (-not (Test-Path $ServerJar) -or -not (Test-Path $ClientBin)) {
    Write-Host "[Run] Building server and client artifacts first..." -ForegroundColor Yellow
    & powershell -ExecutionPolicy Bypass -File (Join-Path $RootDir "scripts\build_all.ps1")
}

Write-Host "[Run] Pushing server.jar to /data/local/tmp/server.jar..." -ForegroundColor Cyan
& $Adb push $ServerJar /data/local/tmp/server.jar

Write-Host "[Run] Forwarding TCP port $Port -> device:$Port..." -ForegroundColor Cyan
& $Adb forward "tcp:$Port" "tcp:$Port"

Write-Host "[Run] Starting com.mirror.Server under ADB shell (UID 2000)..." -ForegroundColor Cyan
$ServerJob = Start-Job -ScriptBlock {
    param($AdbCmd, $P, $Br, $F, $Ms)
    & $AdbCmd shell "CLASSPATH=/data/local/tmp/server.jar app_process / com.mirror.Server port=$P bitrate=$Br fps=$F max_size=$Ms cbr=true"
} -ArgumentList $Adb, $Port, $Bitrate, $Fps, $MaxSize

Start-Sleep -Milliseconds 600

try {
    Write-Host "[Run] Launching PC Desktop Client..." -ForegroundColor Green
    & $ClientBin --addr "127.0.0.1:$Port" --record $RecordPath --hw $HwBackend
} finally {
    Stop-Job $ServerJob -ErrorAction SilentlyContinue
    Remove-Job $ServerJob -Force -ErrorAction SilentlyContinue
    & $Adb forward --remove "tcp:$Port" 2>$null
}
