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
$processJobHelper = Join-Path $PSScriptRoot 'qOS-ProcessJob.ps1'
if (-not (Test-Path -LiteralPath $processJobHelper -PathType Leaf)) {
    throw 'qOS process containment helper is missing.'
}
. $processJobHelper

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
        # The helper owns quoting; /i and the verified package are a fixed MSI
        # contract, not a bridge-supplied command line.
        $result = [qOS.WindowsGuest.ProcessJob]::RunMsiInstaller(
            "$env:WINDIR\System32\msiexec.exe",
            $package
        )
    } else {
        # No package arguments or shell command are accepted. Try the limited
        # token first. ERROR_ELEVATION_REQUIRED means CreateProcess created
        # nothing, so it remains safe to ask UAC to elevate qOS's fixed broker
        # and let that broker do suspended creation and job assignment.
        try {
            $result = [qOS.WindowsGuest.ProcessJob]::RunExecutableInstaller($package)
        } catch {
            $exception = $_.Exception
            $elevationRequired = $false
            while ($null -ne $exception) {
                if ($exception.GetType().FullName -eq 'qOS.WindowsGuest.ProcessElevationRequiredException') {
                    $elevationRequired = $true
                    break
                }
                $exception = $exception.InnerException
            }
            if (-not $elevationRequired) { throw }

            $broker = Join-Path $PSScriptRoot 'Start-qOSProcessJobBroker.ps1'
            if (-not (Test-Path -LiteralPath $broker -PathType Leaf)) {
                throw 'qOS elevated installer broker is missing.'
            }
            $windowsPowerShell = "$env:WINDIR\System32\WindowsPowerShell\v1.0\powershell.exe"
            $brokerArguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy RemoteSigned -File `"$broker`" -JobPath `"$resolvedJob`""
            try {
                # The only variable broker argument is the operation job path,
                # already constrained above to an administrator-owned path
                # with a 16-128 character opaque directory component.
                $brokerProcess = Start-Process -FilePath $windowsPowerShell -Verb RunAs -ArgumentList $brokerArguments -Wait -PassThru
            } catch {
                throw "qOS installer UAC consent was canceled or the elevated process-job broker could not start: $($_.Exception.Message)"
            }
            $result = [PSCustomObject]@{ ExitCode = $brokerProcess.ExitCode }
        }
    }
    exit $result.ExitCode
} finally {
    $sessionExplorer = @(Get-Process -Name explorer -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -eq $sessionId })
    if ($explorer.Count -gt 0 -and $sessionExplorer.Count -eq 0) {
        Start-Process -FilePath "$env:WINDIR\explorer.exe" | Out-Null
    }
}
