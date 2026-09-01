<#
Runs exactly one bridge-created EXE or MSI job in the existing interactive
Windows session. The SYSTEM bridge owns the job and package; this helper has no
free-form command field and rechecks the immutable size and SHA-256 before use.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$JobPath
)
$ErrorActionPreference = 'Stop'
$sessionRoot = 'C:\ProgramData\qOS\InstallerSession'
$resolvedJob = [IO.Path]::GetFullPath($JobPath)
if ($resolvedJob -notmatch '^C:\\ProgramData\\qOS\\InstallerSession\\operations\\[A-Za-z0-9._-]{16,128}\\job\.json$') {
    throw 'invalid qOS installer job path'
}
$job = Get-Content -Raw -LiteralPath $resolvedJob | ConvertFrom-Json
if ($job.operationId -notmatch '^[A-Za-z0-9._-]{16,128}$' -or $job.kind -notin @('exe', 'msi')) {
    throw 'invalid qOS installer job'
}
$package = [IO.Path]::GetFullPath([string]$job.packagePath)
if ($package -notmatch '^C:\\ProgramData\\qOS\\InstallerSession\\operations\\[A-Za-z0-9._-]{16,128}\\package\.(exe|msi)$') {
    throw 'installer package is outside the managed payload directory'
}
$item = Get-Item -LiteralPath $package
if ([UInt64]$item.Length -ne [UInt64]$job.byteLen) { throw 'installer size changed after acceptance' }
$digest = (Get-FileHash -LiteralPath $package -Algorithm SHA256).Hash.ToLowerInvariant()
if ($digest -ne ([string]$job.sha256).ToLowerInvariant()) { throw 'installer digest changed after acceptance' }

$sessionId = (Get-Process -Id $PID).SessionId
$explorer = @(Get-Process -Name explorer -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -eq $sessionId })
try {
    # The attached SPICE surface shows setup itself, never an incidental guest
    # desktop. Only Explorer in this interactive session is suppressed.
    foreach ($shell in $explorer) { Stop-Process -Id $shell.Id -Force -ErrorAction SilentlyContinue }
    if ($job.kind -eq 'msi') {
        $process = Start-Process -FilePath "$env:WINDIR\System32\msiexec.exe" -ArgumentList @('/i', $package) -Wait -PassThru
    } else {
        # No arguments or shell are accepted. Installer manifests can request
        # UAC normally on the visible app-focused SPICE surface.
        $process = Start-Process -FilePath $package -Wait -PassThru
    }
    exit $process.ExitCode
} finally {
    $sessionExplorer = @(Get-Process -Name explorer -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -eq $sessionId })
    if ($explorer.Count -gt 0 -and $sessionExplorer.Count -eq 0) {
        Start-Process -FilePath "$env:WINDIR\explorer.exe" | Out-Null
    }
}
