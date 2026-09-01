<# Runs one bridge-discovered typed MSI or bound EXE product removal. #>
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

$resolvedJob = [IO.Path]::GetFullPath($JobPath)
if ($resolvedJob -notmatch '^C:\\ProgramData\\qOS\\InstallerSession\\operations\\(?<operation>[A-Za-z0-9._-]{16,128})\\job\.json$') {
    throw 'invalid qOS uninstaller job path'
}
$pathOperationId = [string]$Matches.operation
$job = Get-Content -Raw -LiteralPath $resolvedJob | ConvertFrom-Json
if (
    [string]$job.operationKind -ne 'uninstallation' -or
    [string]$job.operationId -cne $pathOperationId -or
    [string]$job.operationId -notmatch '^[A-Za-z0-9._-]{16,128}$' -or
    [string]$job.kind -notin @('msi', 'exe')
) {
    throw 'invalid typed qOS removal job'
}
$sessionId = (Get-Process -Id $PID).SessionId
$explorer = @(Get-Process -Name explorer -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -eq $sessionId })
try {
    foreach ($shell in $explorer) { Stop-Process -Id $shell.Id -Force -ErrorAction SilentlyContinue }
    if ([string]$job.kind -eq 'msi') {
        if ([string]$job.productCode -notmatch '^\{[A-Fa-f0-9]{8}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{12}\}$') {
            throw 'invalid typed Windows Installer removal job'
        }
        # The product code was parsed from an administrator-owned uninstall
        # record. The invocation remains exactly msiexec /x {GUID}.
        $result = [qOS.WindowsGuest.ProcessJob]::RunMsiUninstaller(
            "$env:WINDIR\System32\msiexec.exe",
            [string]$job.productCode
        )
    } else {
        # The registry command string is gone. Revalidate the persisted root,
        # ACL chain, exact executable digest, and bounded argv immediately
        # before direct native creation.
        $contract = Get-qOSValidatedExecutableUninstallContract $job
        try {
            $result = [qOS.WindowsGuest.ProcessJob]::RunExecutableUninstaller(
                $contract.executablePath,
                [string[]]@($contract.arguments)
            )
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
                throw 'qOS elevated uninstaller broker is missing.'
            }
            $windowsPowerShell = "$env:WINDIR\System32\WindowsPowerShell\v1.0\powershell.exe"
            $brokerArguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy RemoteSigned -File `"$broker`" -JobPath `"$resolvedJob`""
            try {
                # The strict operation path is the only variable broker input;
                # the elevated broker independently repeats every binding and
                # digest check after the user grants UAC consent.
                $brokerProcess = Start-Process -FilePath $windowsPowerShell -Verb RunAs -ArgumentList $brokerArguments -Wait -PassThru
            } catch {
                throw "qOS uninstaller UAC consent was canceled or the elevated process-job broker could not start: $($_.Exception.Message)"
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
