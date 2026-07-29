# Launch a debug nebula instance for ui_probe and print its PID.
param([string]$ExePath = "D:\temp_build\nebula\target\debug\nebula.exe")
$p = Start-Process $ExePath -ArgumentList '--working-directory','D:\temp_build' -PassThru
Start-Sleep -Seconds 7
$p.Refresh()
Write-Output ("pid=" + $p.Id)
