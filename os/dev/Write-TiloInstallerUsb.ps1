[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
    [string]$Image,

    [Parameter(Mandatory)]
    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
    [string]$Acceptance,

    [Parameter(Mandatory)]
    [ValidateRange(0, 999)]
    [int]$DiskNumber,

    [Parameter(Mandatory)]
    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Container })]
    [string]$HandoffDirectory,

    [string]$PythonPath = 'python.exe',

    [switch]$PlanOnly
)

<#
.SYNOPSIS
Creates a reviewed plan and, only after an exact typed confirmation, writes an
accepted Tilo installer raw image to one removable Windows disk.

.DESCRIPTION
This is a thin native-Windows convenience wrapper around
write-verified-installer-media.py.  It intentionally does not manufacture the
confirmation phrase: the operator must read the generated plan and type the
bound phrase before any disk is changed.  The Python writer owns all media
identity, offlining, byte read-back, and success-evidence checks.
#>

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'Run this wrapper from native Windows PowerShell, not WSL.'
}

$principal = [Security.Principal.WindowsPrincipal]::new(
    [Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Open an Administrator PowerShell before using this command.'
}

$scriptPath = Join-Path $PSScriptRoot 'write-verified-installer-media.py'
if (-not (Test-Path -LiteralPath $scriptPath -PathType Leaf)) {
    throw "Missing guarded media writer: $scriptPath"
}

$resolvedImage = (Resolve-Path -LiteralPath $Image -ErrorAction Stop).Path
$resolvedAcceptance = (Resolve-Path -LiteralPath $Acceptance -ErrorAction Stop).Path
$resolvedHandoff = (Resolve-Path -LiteralPath $HandoffDirectory -ErrorAction Stop).Path
$planPath = Join-Path $resolvedHandoff 'lenovo-media-plan.json'
$evidencePath = Join-Path $resolvedHandoff 'lenovo-media-write.json'

if ((Test-Path -LiteralPath $planPath) -or (Test-Path -LiteralPath $evidencePath)) {
    throw "Refusing to reuse existing plan or evidence. Review and move $planPath and $evidencePath first."
}

$planArguments = @(
    $scriptPath, 'plan',
    '--image', $resolvedImage,
    '--acceptance', $resolvedAcceptance,
    '--disk-number', $DiskNumber,
    '--evidence', $evidencePath,
    '--output', $planPath
)
& $PythonPath @planArguments
if ($LASTEXITCODE -ne 0) {
    throw "The guarded writer rejected the plan (exit $LASTEXITCODE)."
}

$plan = Get-Content -LiteralPath $planPath -Raw | ConvertFrom-Json
Write-Host ''
Write-Host 'Review this exact removable target before continuing:' -ForegroundColor Yellow
$plan.target | Select-Object number, model, unique_id, size_bytes, bus_type | Format-List | Out-Host
Write-Host "Required typed confirmation: $($plan.required_confirmation)" -ForegroundColor Yellow

if ($PlanOnly) {
    Write-Host "Plan written to $planPath. No disk was modified."
    return
}

$typed = Read-Host 'Type the required confirmation exactly to erase that disk'
if ($typed -cne $plan.required_confirmation) {
    throw 'Confirmation did not exactly match the reviewed plan. No disk was modified.'
}

& $PythonPath $scriptPath 'write' '--plan' $planPath '--confirmation' $typed
if ($LASTEXITCODE -ne 0) {
    throw "The guarded writer did not complete successfully (exit $LASTEXITCODE). Review $evidencePath before retrying."
}

Write-Host "Verified installer-media evidence written to $evidencePath" -ForegroundColor Green
