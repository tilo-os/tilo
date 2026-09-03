$ErrorActionPreference = "Stop"
$Exe = Join-Path $PSScriptRoot "qos-imager.exe"
if ($args | Where-Object { $_ -eq "--shots" -or $_ -like "--shots=*" -or $_ -eq "--shots-dir" -or $_ -like "--shots-dir=*" }) {
    & $Exe @args
    exit $LASTEXITCODE
}
$Command = "`$env:QOS_IMAGER_ALLOW_WRITE='1'; & '$($Exe.Replace("'", "''"))'"
Start-Process powershell.exe -Verb RunAs -ArgumentList @(
    "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", $Command
)
