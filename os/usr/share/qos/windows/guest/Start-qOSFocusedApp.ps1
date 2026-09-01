<#
Runs one qOS-approved Windows application in the already logged-in,
interactive guest session used by the app-focused SPICE fallback.

This file is invoked by a temporary Scheduled Task with InteractiveToken. It
does not accept an executable path from the task command line: the SYSTEM-side
bridge writes a private job file after resolving the immutable application
manifest. Explorer is stopped only while the selected application owns the
managed surface, then restored in finally for recovery and normal use.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateScript({ Test-Path $_ -PathType Leaf })]
    [string]$JobPath
)

$ErrorActionPreference = 'Stop'
$job = Get-Content -Raw -LiteralPath $JobPath | ConvertFrom-Json
if ($null -eq $job -or [string]::IsNullOrWhiteSpace([string]$job.executable)) {
    throw 'qOS focused-application job is incomplete.'
}
$executable = [IO.Path]::GetFullPath([string]$job.executable)
if ($executable -notmatch '^(C:\\Program Files( \(x86\))?\\|C:\\Windows\\)') {
    throw 'qOS focused-application executable is outside a managed Windows location.'
}
if (-not (Test-Path $executable -PathType Leaf)) { throw "qOS application is missing: $executable" }

$sessionId = (Get-Process -Id $PID).SessionId
$explorer = @(
    Get-Process -Name explorer -ErrorAction SilentlyContinue |
        Where-Object { $_.SessionId -eq $sessionId }
)
try {
    # A whole-surface SPICE fallback should look like the selected application,
    # not a second Windows desktop. Stop only Explorer instances visible to the
    # interactive qOS user; the SYSTEM bridge and recovery console are separate.
    foreach ($process in $explorer) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    }
    $arguments = [string]$job.arguments
    if ([string]::IsNullOrEmpty($arguments)) {
        $process = Start-Process -FilePath $executable -PassThru
    } else {
        $process = Start-Process -FilePath $executable -ArgumentList $arguments -PassThru
    }
    Wait-Process -Id $process.Id
} finally {
    # Reconstitute Explorer only when it was present before this managed
    # surface. This makes closing the fallback return the guest to its normal
    # recovery environment without leaving a shellless user session behind.
    $sessionExplorer = @(
        Get-Process -Name explorer -ErrorAction SilentlyContinue |
            Where-Object { $_.SessionId -eq $sessionId }
    )
    if ($explorer.Count -gt 0 -and $sessionExplorer.Count -eq 0) {
        Start-Process -FilePath "$env:WINDIR\explorer.exe" | Out-Null
    }
    if ($null -ne $job.taskName -and [string]$job.taskName -match '^qOS-Focus-[A-Za-z0-9-]{8,64}$') {
        Unregister-ScheduledTask -TaskName ([string]$job.taskName) -Confirm:$false -ErrorAction SilentlyContinue
    }
    Remove-Item -LiteralPath $JobPath -Force -ErrorAction SilentlyContinue
}
