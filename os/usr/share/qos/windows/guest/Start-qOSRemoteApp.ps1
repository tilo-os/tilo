<#
Fixed RemoteApp wrapper for the one qOS managed Windows session.

RDS receives only the published application ID. The authenticated SYSTEM
bridge writes its matching short-lived job, so an RDP client cannot add a path,
argv, proxy endpoint, or NetHost grant. The target inherits loopback proxy
environment only from this process tree; no user or machine-wide proxy is set.
#>
[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$ApplicationId)
$ErrorActionPreference = 'Stop'
if ($ApplicationId -notmatch '^[A-Za-z0-9._-]{1,128}$') { throw 'invalid qOS RemoteApp operation id' }
$root = 'C:\ProgramData\qOS\RemoteApp'
$jobPath = Join-Path (Join-Path $root 'rail-jobs') "$ApplicationId.json"
$helper = Join-Path $root 'qOS-ProcessJob.ps1'
$proxyHelper = Join-Path $root 'Start-qOSNetworkProxy.ps1'
if (-not (Test-Path -LiteralPath $jobPath -PathType Leaf) -or -not (Test-Path -LiteralPath $helper -PathType Leaf)) { throw 'qOS RemoteApp launch was not prepared by the authenticated bridge' }
. $helper
$internetSettings = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings'
$restoreKey = 'HKCU:\Software\qOS'
$restoreName = 'WindowsNetworkLeaseRestore'
function Restore-qOSWinInetProxy {
    $saved = (Get-ItemProperty -Path $restoreKey -Name $restoreName -ErrorAction SilentlyContinue).$restoreName
    if ([string]::IsNullOrWhiteSpace([string]$saved)) { return }
    try {
        $snapshot = $saved | ConvertFrom-Json
        foreach ($name in @('ProxyEnable', 'ProxyServer', 'ProxyOverride', 'AutoConfigURL')) {
            $item = $snapshot.$name
            if ($null -ne $item -and [bool]$item.present) { Set-ItemProperty -Path $internetSettings -Name $name -Value $item.value -Force }
            else { Remove-ItemProperty -Path $internetSettings -Name $name -ErrorAction SilentlyContinue }
        }
    } finally { Remove-ItemProperty -Path $restoreKey -Name $restoreName -ErrorAction SilentlyContinue }
}
function Set-qOSWinInetLeaseProxy {
    # This is HKCU for the one managed session, never machine WinHTTP. Save
    # exact presence/value state first so a crash is repaired by the next
    # wrapper invocation before it consumes another authenticated operation.
    Restore-qOSWinInetProxy
    $current = Get-ItemProperty -Path $internetSettings
    $snapshot = @{}
    foreach ($name in @('ProxyEnable', 'ProxyServer', 'ProxyOverride', 'AutoConfigURL')) {
        $property = $current.PSObject.Properties[$name]
        $snapshot[$name] = @{ present = ($null -ne $property); value = if ($null -eq $property) { $null } else { $property.Value } }
    }
    New-Item -Path $restoreKey -Force | Out-Null
    Set-ItemProperty -Path $restoreKey -Name $restoreName -Value ($snapshot | ConvertTo-Json -Compress) -Force
    Set-ItemProperty -Path $internetSettings -Name ProxyEnable -Value 1 -Type DWord -Force
    Set-ItemProperty -Path $internetSettings -Name ProxyServer -Value '127.0.0.1:18080' -Type String -Force
    Remove-ItemProperty -Path $internetSettings -Name AutoConfigURL -ErrorAction SilentlyContinue
    Add-Type -TypeDefinition '[DllImport("wininet.dll", SetLastError=true)] public static extern bool InternetSetOption(IntPtr hInternet, int dwOption, IntPtr lpBuffer, int dwBufferLength);' -Name qOSWinInetRefresh -Namespace Native -ErrorAction SilentlyContinue
    [Native.qOSWinInetRefresh]::InternetSetOption([IntPtr]::Zero, 39, [IntPtr]::Zero, 0) | Out-Null
    [Native.qOSWinInetRefresh]::InternetSetOption([IntPtr]::Zero, 37, [IntPtr]::Zero, 0) | Out-Null
}
$job = Get-Content -Raw -LiteralPath $jobPath | ConvertFrom-Json
if ([string]$job.applicationId -cne $ApplicationId) { throw 'qOS RemoteApp job identity mismatch' }
if ([string]$job.launchNonce -notmatch '^[A-Za-z0-9-]{16,128}$' -or [Int64]$job.expiresAtUnix -le [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()) { throw 'qOS RemoteApp job expired or was not authenticated' }
$executable = [IO.Path]::GetFullPath([string]$job.executable)
if ($executable -notmatch '^(C:\\Program Files( \(x86\))?\\|C:\\Windows\\)' -or -not (Test-Path -LiteralPath $executable -PathType Leaf)) { throw 'qOS RemoteApp job executable is invalid' }
$proxy = $null
try {
    if ($null -ne $job.networkProxy) {
        # The token is read from the bridge-authenticated, one-use job rather
        # than a RDP request or a permanent proxy setting. Children inherit
        # these values structurally; an app that ignores them remains offline
        # because the appliance has no NAT/default internet route.
        $proxy = Start-Process -FilePath 'powershell.exe' -ArgumentList @('-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'RemoteSigned', '-File', $proxyHelper, '-JobPath', $jobPath) -PassThru -WindowStyle Hidden
        Start-Sleep -Milliseconds 100
        Set-qOSWinInetLeaseProxy
        $env:HTTP_PROXY = 'http://127.0.0.1:18080'
        $env:HTTPS_PROXY = 'http://127.0.0.1:18080'
        $env:ALL_PROXY = 'http://127.0.0.1:18080'
        $env:NO_PROXY = 'localhost,127.0.0.1,::1'
    }
    $null = [qOS.WindowsGuest.ProcessJob]::RunFocusedApplication($executable, [string]$job.arguments)
} finally {
    if ($null -ne $proxy -and -not $proxy.HasExited) { Stop-Process -Id $proxy.Id -Force -ErrorAction SilentlyContinue }
    Restore-qOSWinInetProxy
    # One RAIL operation consumes exactly one bridge job. This is also the
    # deterministic guest-side lease revocation path when the target exits.
    Remove-Item -LiteralPath $jobPath -Force -ErrorAction SilentlyContinue
}
