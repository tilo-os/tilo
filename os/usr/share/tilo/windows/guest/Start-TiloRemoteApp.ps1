<#
Fixed RemoteApp wrapper for the one Tilo managed Windows session.

RDS receives only the published application ID. The authenticated SYSTEM
bridge writes its matching short-lived job, so an RDP client cannot add a path,
argv, proxy endpoint, or NetHost grant. The target inherits its own loopback
proxy environment; the shared user's WinINet registry is never changed because
it would merge concurrent leases.
#>
[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$ApplicationId)
$ErrorActionPreference = 'Stop'
if ($ApplicationId -notmatch '^[A-Za-z0-9._-]{1,128}$') { throw 'invalid Tilo RemoteApp operation id' }
$root = 'C:\ProgramData\Tilo\RemoteApp'
$jobPath = Join-Path (Join-Path $root 'rail-jobs') "$ApplicationId.json"
$helper = Join-Path $root 'Tilo-ProcessJob.ps1'
if (-not (Test-Path -LiteralPath $jobPath -PathType Leaf) -or -not (Test-Path -LiteralPath $helper -PathType Leaf)) { throw 'Tilo RemoteApp launch was not prepared by the authenticated bridge' }
. $helper
$job = Get-Content -Raw -LiteralPath $jobPath | ConvertFrom-Json
if ([string]$job.applicationId -cne $ApplicationId) { throw 'Tilo RemoteApp job identity mismatch' }
if ([string]$job.launchNonce -notmatch '^[A-Za-z0-9-]{16,128}$' -or [Int64]$job.expiresAtUnix -le [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()) { throw 'Tilo RemoteApp job expired or was not authenticated' }
$executable = [IO.Path]::GetFullPath([string]$job.executable)
if ($executable -notmatch '^(C:\\Program Files( \(x86\))?\\|C:\\Windows\\)' -or -not (Test-Path -LiteralPath $executable -PathType Leaf)) { throw 'Tilo RemoteApp job executable is invalid' }
$runtimeMarkerPath = $null
if ($null -ne $job.networkProxy) {
    $runtimeMarkerPath = Join-Path $root ("network-runtime-" + [string]$job.launchNonce + '.marker')
    if (-not (Test-Path -LiteralPath $runtimeMarkerPath -PathType Leaf)) { throw 'Tilo network runtime marker is missing' }
}
# Consume the one-time auth job before launching the target. Its former
# 120-second expiry applies only to this handoff; the separate marker defines
# the actual process lifetime.
Remove-Item -LiteralPath $jobPath -Force -ErrorAction Stop
try {
    if ($null -ne $job.networkProxy) {
        # The token is read from the bridge-authenticated, one-use job rather
        # than a RDP request or a permanent proxy setting. Children inherit
        # these values structurally; an app that ignores them remains offline
        # because the appliance has no NAT/default internet route.
        $port = [int]$job.localProxyPort
        if ($port -lt 1 -or $port -gt 65535) { throw 'Tilo network proxy endpoint is invalid' }
        $credential = [string]$job.localProxyCredential
        if ($credential -notmatch '^[A-Za-z0-9_-]{43}$') { throw 'Tilo local proxy credential is invalid' }
        # Standard proxy URL credentials make supporting clients send Basic
        # authentication. A loopback port alone is discoverable and therefore
        # never treated as the cross-app lease boundary.
        $proxyUrl = "http://tilo:$credential@127.0.0.1:$port"
        $env:HTTP_PROXY = $proxyUrl
        $env:HTTPS_PROXY = $proxyUrl
        $env:ALL_PROXY = $proxyUrl
        $env:NO_PROXY = 'localhost,127.0.0.1,::1'
    }
    $null = [Tilo.WindowsGuest.ProcessJob]::RunFocusedApplication($executable, [string]$job.arguments)
} finally {
    # The target's lifetime is the runtime lease lifetime. This exact marker
    # is not a shared session knob and deletion wakes its SYSTEM proxy.
    if ($null -ne $runtimeMarkerPath) { Remove-Item -LiteralPath $runtimeMarkerPath -Force -ErrorAction SilentlyContinue }
}
