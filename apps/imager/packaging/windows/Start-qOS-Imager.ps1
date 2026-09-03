$ErrorActionPreference = "Stop"
$Exe = Join-Path $PSScriptRoot "qos-imager.exe"
# The GUI remains in the signed-in user's session. It requests UAC only for a
# short-lived writer helper after exact target confirmation.
& $Exe @args
exit $LASTEXITCODE
