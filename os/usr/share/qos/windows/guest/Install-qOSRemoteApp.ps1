<#
Installs qOS's Windows-side RemoteApp publication and bridge payload.

This intentionally supports only a user-supplied Windows Server installation
with the official Remote Desktop Services RemoteApp role. Consumer Windows
editions do not provide a supported RemoteApp host; qOS will not patch
termsrv.dll or ship RDP Wrapper just to hide a desktop.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateScript({ Test-Path $_ -PathType Container })]
    [string]$BootstrapRoot,
    [Parameter(Mandatory = $true)]
    [ValidateScript({ Test-Path $_ -PathType Leaf })]
    [string]$ApplicationsPath
)

$ErrorActionPreference = 'Stop'
$root = 'C:\ProgramData\qOS\RemoteApp'
$bridgeScript = Join-Path $BootstrapRoot 'Start-qOSGuestBridge.ps1'
$secretPath = Join-Path $BootstrapRoot 'qos-bridge.secret'
$configPath = Join-Path $BootstrapRoot 'qos-bridge.json'
foreach ($path in @($bridgeScript, $secretPath, $configPath, $ApplicationsPath)) {
    if (-not (Test-Path $path -PathType Leaf)) { throw "Missing required qOS guest payload: $path" }
}

# Native RDS RemoteApp is the supported way to get individual windows. Fail
# closed rather than presenting a concealed ordinary desktop from a client SKU.
$os = Get-CimInstance -ClassName Win32_OperatingSystem
if ($os.ProductType -ne 3) {
    throw 'qOS needs a Windows Server ISO with the supported RDS RemoteApp role; client Windows is recovery-only.'
}
$rds = Get-WindowsFeature -Name RDS-RD-Server
if (-not $rds.Installed) {
    throw 'Install and license the Windows Remote Desktop Services RemoteApp role before enabling qOS publication.'
}

$apps = Get-Content -Raw -LiteralPath $ApplicationsPath | ConvertFrom-Json
if ($null -eq $apps -or $null -eq $apps.applications) { throw 'Application manifest has no applications array.' }
foreach ($app in $apps.applications) {
    if ($app.id -notmatch '^[A-Za-z0-9._-]{1,128}$') { throw "Invalid app id: $($app.id)" }
    if ([string]::IsNullOrWhiteSpace($app.name) -or [string]::IsNullOrWhiteSpace($app.path)) { throw "Incomplete application: $($app.id)" }
    $resolved = [IO.Path]::GetFullPath($app.path)
    if (-not (Test-Path $resolved -PathType Leaf)) { throw "Published executable does not exist: $resolved" }
    if ($resolved -notmatch '^(C:\\Program Files( \(x86\))?\\|C:\\Windows\\)') { throw "Refusing executable outside a managed Windows location: $resolved" }
}

# SYSTEM owns the bridge key. Normal Windows programs, including every
# RemoteApp account, never receive read access to this long-lived bootstrap
# key. The host copy is root-only in /var/lib/qos/windows/bridge.
New-Item -ItemType Directory -Force -Path $root | Out-Null
$acl = New-Object Security.AccessControl.DirectorySecurity
$acl.SetAccessRuleProtection($true, $false)
foreach ($principal in @('SYSTEM', 'BUILTIN\Administrators')) {
    $rule = New-Object Security.AccessControl.FileSystemAccessRule($principal, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    [void]$acl.AddAccessRule($rule)
}
Set-Acl -LiteralPath $root -AclObject $acl
Copy-Item -LiteralPath $bridgeScript -Destination (Join-Path $root 'Start-qOSGuestBridge.ps1') -Force
Copy-Item -LiteralPath $secretPath -Destination (Join-Path $root 'bridge.secret') -Force
Copy-Item -LiteralPath $configPath -Destination (Join-Path $root 'bridge.json') -Force
Copy-Item -LiteralPath $ApplicationsPath -Destination (Join-Path $root 'published-applications.json') -Force

$base = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Terminal Server\TSAppAllowList\Applications'
New-Item -Path $base -Force | Out-Null
foreach ($app in $apps.applications) {
    $key = Join-Path $base $app.id
    New-Item -Path $key -Force | Out-Null
    New-ItemProperty -Path $key -Name Name -PropertyType String -Value $app.name -Force | Out-Null
    New-ItemProperty -Path $key -Name Path -PropertyType String -Value ([IO.Path]::GetFullPath($app.path)) -Force | Out-Null
    New-ItemProperty -Path $key -Name CommandLine -PropertyType String -Value ([string]$app.arguments) -Force | Out-Null
    New-ItemProperty -Path $key -Name ShowInTSWA -PropertyType DWord -Value 0 -Force | Out-Null
}

# Do not make a host filesystem or clipboard grant by accident. Per-session
# redirects are supplied by FreeRDP only after qOS's normal workspace broker
# has materialised a short lease.
$ts = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services'
New-Item -Path $ts -Force | Out-Null
New-ItemProperty -Path $ts -Name fDisableCdm -PropertyType DWord -Value 1 -Force | Out-Null
New-ItemProperty -Path $ts -Name fDisableClip -PropertyType DWord -Value 1 -Force | Out-Null
New-ItemProperty -Path $ts -Name fDisableAudioCapture -PropertyType DWord -Value 1 -Force | Out-Null
New-ItemProperty -Path $ts -Name UserAuthentication -PropertyType DWord -Value 1 -Force | Out-Null

# Never allow RDP from arbitrary LAN peers. The libvirt bridge has no forward,
# and this additionally admits only its host-side address.
Enable-NetFirewallRule -DisplayGroup 'Remote Desktop'
Get-NetFirewallRule -DisplayGroup 'Remote Desktop' | Get-NetFirewallAddressFilter | Set-NetFirewallAddressFilter -RemoteAddress '192.168.251.1'
Set-ItemProperty -Path 'HKLM:\System\CurrentControlSet\Control\Terminal Server' -Name fDenyTSConnections -Value 0
$bridgeRule = Get-NetFirewallRule -DisplayName 'qOS guest bridge (host-only)' -ErrorAction SilentlyContinue
if ($null -eq $bridgeRule) {
    New-NetFirewallRule -DisplayName 'qOS guest bridge (host-only)' -Direction Inbound -Action Allow -Protocol TCP -LocalPort 49888 -RemoteAddress '192.168.251.1' | Out-Null
} else {
    $bridgeRule | Get-NetFirewallAddressFilter | Set-NetFirewallAddressFilter -RemoteAddress '192.168.251.1'
}
$rdpTcp = 'HKLM:\SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp'
# The bridge creates this VM-local certificate before it begins listening. The
# RDP listener uses the same identity, so qOS can pin one SHA-256 certificate
# fingerprint for both control and RemoteApp data planes.
if (Test-Path $rdpTcp) {
    $bridgeCert = Get-ChildItem Cert:\LocalMachine\My | Where-Object { $_.FriendlyName -eq 'qOS guest bridge' } | Select-Object -First 1
    if ($null -eq $bridgeCert) {
        $bridgeCert = New-SelfSignedCertificate -DnsName 'qos-windows-appliance' -CertStoreLocation Cert:\LocalMachine\My -FriendlyName 'qOS guest bridge' -KeyAlgorithm RSA -KeyLength 3072 -HashAlgorithm SHA256 -NotAfter (Get-Date).AddYears(5)
    }
    if ($null -ne $bridgeCert) { Set-ItemProperty -Path $rdpTcp -Name SSLCertificateSHA1Hash -Value $bridgeCert.Thumbprint }
}

$taskName = 'qOS Guest RemoteApp Bridge'
# The payload lives in a SYSTEM/admin-only directory copied from the owner
# mounted tools ISO. RemoteSigned rejects Internet-marked replacement scripts;
# AllSigned would reject qOS's deliberately keyless, offline guest payload.
$action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy RemoteSigned -File `"$root\Start-qOSGuestBridge.ps1`""
$trigger = New-ScheduledTaskTrigger -AtStartup
$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Principal $principal -Force | Out-Null
Start-ScheduledTask -TaskName $taskName
Write-Output 'qOS RemoteApp publication installed. The Windows desktop remains a recovery-only surface.'
