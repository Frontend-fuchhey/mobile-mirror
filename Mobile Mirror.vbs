Set fso = CreateObject("Scripting.FileSystemObject")
currentDir = fso.GetParentFolderName(WScript.ScriptFullName)

Set shell = CreateObject("WScript.Shell")
shell.CurrentDirectory = currentDir

exePath = currentDir & "\client\target\release\mobile-mirror-client.exe"
serverJar = currentDir & "\server\server.jar"
recordDir = currentDir & "\recordings"

If Not fso.FolderExists(recordDir) Then
    fso.CreateFolder(recordDir)
End If

' If artifacts are missing, compile them first (shows build console during compilation)
If Not fso.FileExists(serverJar) Then
    shell.Run "powershell -ExecutionPolicy Bypass -File """ & currentDir & "\server\build.ps1""", 1, True
End If

If Not fso.FileExists(exePath) Then
    shell.Run "cmd /c ""cd /d """ & currentDir & "\client"" && cargo build --release""", 1, True
End If

' Launch Mobile Mirror GUI directly (no terminal/console window)
shell.Run """" & exePath & """ --port 12345 --record-dir """ & recordDir & """", 1, False
