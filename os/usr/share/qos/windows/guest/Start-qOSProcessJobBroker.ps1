<#
Elevated process-job broker for a typed installer or bound EXE uninstaller
whose application manifest requires administrator rights. Its only variable
launch input is the strict administrator-created operation job path. It
revalidates the full job after UAC before suspended process creation.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$JobPath
)

$ErrorActionPreference = 'Stop'
$resolvedJob = [IO.Path]::GetFullPath($JobPath)
if ($resolvedJob -notmatch '^C:\\ProgramData\\qOS\\InstallerSession\\operations\\(?<operation>[A-Za-z0-9._-]{16,128})\\job\.json$') {
    throw 'invalid qOS elevated installer job path'
}
$pathOperationId = [string]$Matches.operation

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'qOS installer process-job broker did not receive UAC elevation'
}

$job = Get-Content -Raw -LiteralPath $resolvedJob | ConvertFrom-Json
if (
    [string]$job.operationId -cne $pathOperationId -or
    [string]$job.operationId -notmatch '^[A-Za-z0-9._-]{16,128}$'
) {
    throw 'invalid typed qOS elevated process job'
}

$processJobHelper = Join-Path $PSScriptRoot 'qOS-ProcessJob.ps1'
if (-not (Test-Path -LiteralPath $processJobHelper -PathType Leaf)) {
    throw 'qOS elevated process containment helper is missing'
}
. $processJobHelper

if ([string]$job.operationKind -eq 'installation') {
    if ([string]$job.kind -ne 'exe' -or [string]$job.sha256 -notmatch '^[A-Fa-f0-9]{64}$') {
        throw 'invalid typed qOS elevated installer job'
    }
    $package = [IO.Path]::GetFullPath([string]$job.packagePath)
    if ($package -notmatch '^C:\\ProgramData\\qOS\\InstallerSession\\operations\\(?<operation>[A-Za-z0-9._-]{16,128})\\package\.exe$') {
        throw 'elevated installer package is outside the managed operation directory'
    }
    if ([string]$Matches.operation -cne $pathOperationId) {
        throw 'elevated installer package belongs to a different operation'
    }
    $item = Get-Item -LiteralPath $package
    if ([UInt64]$item.Length -ne [UInt64]$job.byteLen) {
        throw 'elevated installer size changed after acceptance'
    }
    $digest = (Get-FileHash -LiteralPath $package -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($digest -cne ([string]$job.sha256).ToLowerInvariant()) {
        throw 'elevated installer digest changed after acceptance'
    }
    $result = [qOS.WindowsGuest.ProcessJob]::RunExecutableInstaller($package)
} elseif ([string]$job.operationKind -eq 'uninstallation' -and [string]$job.kind -eq 'exe') {
    $contract = Get-qOSValidatedExecutableUninstallContract $job
    $result = [qOS.WindowsGuest.ProcessJob]::RunExecutableUninstaller(
        $contract.executablePath,
        [string[]]@($contract.arguments)
    )
} else {
    throw 'qOS elevated process-job broker received an unsupported operation'
}
exit $result.ExitCode
