$ErrorActionPreference = "Stop"
$Exe = Join-Path $PSScriptRoot "qos-imager.exe"
$Command = "`$env:QOS_IMAGER_ALLOW_WRITE='1'; & '$($Exe.Replace("'", "''"))'"
Start-Process powershell.exe -Verb RunAs -ArgumentList @(
    "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", $Command
)

