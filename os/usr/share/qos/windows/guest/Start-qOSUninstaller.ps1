<# Runs one bridge-discovered Windows Installer product removal. #>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$JobPath
)
$ErrorActionPreference = 'Stop'
$resolvedJob = [IO.Path]::GetFullPath($JobPath)
if ($resolvedJob -notmatch '^C:\\ProgramData\\qOS\\InstallerSession\\operations\\[A-Za-z0-9._-]{16,128}\\job\.json$') {
    throw 'invalid qOS uninstaller job path'
}
$job = Get-Content -Raw -LiteralPath $resolvedJob | ConvertFrom-Json
if ($job.operationId -notmatch '^[A-Za-z0-9._-]{16,128}$' -or $job.productCode -notmatch '^\{[A-Fa-f0-9]{8}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{12}\}$') {
    throw 'invalid typed Windows Installer removal job'
}
$sessionId = (Get-Process -Id $PID).SessionId
$explorer = @(Get-Process -Name explorer -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -eq $sessionId })
try {
    foreach ($shell in $explorer) { Stop-Process -Id $shell.Id -Force -ErrorAction SilentlyContinue }
    # The product code was parsed from an administrator-owned uninstall record.
    # No registry command line, executable path, or client argument is run.
    $process = Start-Process -FilePath "$env:WINDIR\System32\msiexec.exe" -ArgumentList @('/x', [string]$job.productCode) -Wait -PassThru
    exit $process.ExitCode
} finally {
    $sessionExplorer = @(Get-Process -Name explorer -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -eq $sessionId })
    if ($explorer.Count -gt 0 -and $sessionExplorer.Count -eq 0) {
        Start-Process -FilePath "$env:WINDIR\explorer.exe" | Out-Null
    }
}
