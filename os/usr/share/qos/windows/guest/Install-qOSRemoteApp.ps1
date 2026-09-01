<#
Installs qOS's Windows-side RemoteApp publication and bridge payload.

This uses the official Remote Desktop Services RemoteApp role when the guest is
Windows Server. On client Windows it enables the same documented application
allow-list used by WinApps-style integrations as a best-effort native RAIL
publication. qOS never patches termsrv.dll or installs RDP Wrapper. A client
guest remains eligible for the app-focused SPICE fallback if the native RAIL
surface is unavailable.
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
$focusRoot = 'C:\ProgramData\qOS\FocusedApp'
$bridgeScript = Join-Path $BootstrapRoot 'Start-qOSGuestBridge.ps1'
$focusedScript = Join-Path $BootstrapRoot 'Start-qOSFocusedApp.ps1'
$installerScript = Join-Path $BootstrapRoot 'Start-qOSInstaller.ps1'
$uninstallerScript = Join-Path $BootstrapRoot 'Start-qOSUninstaller.ps1'
$processJobScript = Join-Path $BootstrapRoot 'qOS-ProcessJob.ps1'
$processJobBrokerScript = Join-Path $BootstrapRoot 'Start-qOSProcessJobBroker.ps1'
$secretPath = Join-Path $BootstrapRoot 'qos-bridge.secret'
$configPath = Join-Path $BootstrapRoot 'qos-bridge.json'
foreach ($path in @($bridgeScript, $focusedScript, $installerScript, $uninstallerScript, $processJobScript, $processJobBrokerScript, $secretPath, $configPath, $ApplicationsPath)) {
    if (-not (Test-Path $path -PathType Leaf)) { throw "Missing required qOS guest payload: $path" }
}

# Native RDS RemoteApp is the supported way to get individual windows on
# Server. Client Windows uses the same native allow-list as a best-effort
# integration and can fall back to an app-focused guest surface.
$os = Get-CimInstance -ClassName Win32_OperatingSystem
$isServer = $os.ProductType -eq 3
if ($isServer) {
    $rds = Get-WindowsFeature -Name RDS-RD-Server
    if (-not $rds.Installed) {
        throw 'Install and license the Windows Remote Desktop Services RemoteApp role before enabling qOS publication.'
    }
}

$apps = Get-Content -Raw -LiteralPath $ApplicationsPath | ConvertFrom-Json
if ($null -eq $apps -or $null -eq $apps.applications) { throw 'Application manifest has no applications array.' }
$generationText = [string]$apps.generation
if ($generationText -notmatch '^[0-9]{1,19}$') { throw 'Application manifest has an invalid generation.' }
try { $null = [UInt64]$generationText }
catch { throw 'Application manifest generation is out of range.' }
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
$focusAcl = New-Object Security.AccessControl.DirectorySecurity
$focusAcl.SetAccessRuleProtection($true, $false)
foreach ($principal in @('SYSTEM', 'BUILTIN\Administrators')) {
    $rule = New-Object Security.AccessControl.FileSystemAccessRule($principal, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    [void]$focusAcl.AddAccessRule($rule)
}
New-Item -ItemType Directory -Force -Path $focusRoot | Out-Null
Set-Acl -LiteralPath $focusRoot -AclObject $focusAcl
$acl = New-Object Security.AccessControl.DirectorySecurity
$acl.SetAccessRuleProtection($true, $false)
foreach ($principal in @('SYSTEM', 'BUILTIN\Administrators')) {
    $rule = New-Object Security.AccessControl.FileSystemAccessRule($principal, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    [void]$acl.AddAccessRule($rule)
}
Set-Acl -LiteralPath $root -AclObject $acl
Copy-Item -LiteralPath $bridgeScript -Destination (Join-Path $root 'Start-qOSGuestBridge.ps1') -Force
Copy-Item -LiteralPath $focusedScript -Destination (Join-Path $focusRoot 'Start-qOSFocusedApp.ps1') -Force
Copy-Item -LiteralPath $processJobScript -Destination (Join-Path $focusRoot 'qOS-ProcessJob.ps1') -Force
Copy-Item -LiteralPath $secretPath -Destination (Join-Path $root 'bridge.secret') -Force
Copy-Item -LiteralPath $configPath -Destination (Join-Path $root 'bridge.json') -Force
Copy-Item -LiteralPath $ApplicationsPath -Destination (Join-Path $root 'published-applications.json') -Force

# User-selected installers are copied into this VM-local directory only after
# the authenticated bridge verifies their declared size and SHA-256. It holds
# no bridge key or publication manifest. The active managed user receives
# read/execute only when a one-use operation is started.
$installerRoot = 'C:\ProgramData\qOS\InstallerSession'
New-Item -ItemType Directory -Force -Path $installerRoot | Out-Null
Set-Acl -LiteralPath $installerRoot -AclObject $focusAcl
foreach ($child in @('staging', 'payloads', 'operations')) {
    New-Item -ItemType Directory -Force -Path (Join-Path $installerRoot $child) | Out-Null
}
Copy-Item -LiteralPath $installerScript -Destination (Join-Path $installerRoot 'Start-qOSInstaller.ps1') -Force
Copy-Item -LiteralPath $uninstallerScript -Destination (Join-Path $installerRoot 'Start-qOSUninstaller.ps1') -Force
Copy-Item -LiteralPath $processJobScript -Destination (Join-Path $installerRoot 'qOS-ProcessJob.ps1') -Force
Copy-Item -LiteralPath $processJobBrokerScript -Destination (Join-Path $installerRoot 'Start-qOSProcessJobBroker.ps1') -Force
$uninstallRecordsPath = Join-Path $root 'uninstall-records.json'
if (-not (Test-Path -LiteralPath $uninstallRecordsPath -PathType Leaf)) {
    [IO.File]::WriteAllText($uninstallRecordsPath, '{"schemaVersion":2,"records":[]}', [Text.UTF8Encoding]::new($false))
}

$allowList = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Terminal Server\TSAppAllowList'
$base = Join-Path $allowList 'Applications'
New-Item -Path $base -Force | Out-Null
foreach ($app in $apps.applications) {
    $key = Join-Path $base $app.id
    New-Item -Path $key -Force | Out-Null
    New-ItemProperty -Path $key -Name Name -PropertyType String -Value $app.name -Force | Out-Null
    New-ItemProperty -Path $key -Name Path -PropertyType String -Value ([IO.Path]::GetFullPath($app.path)) -Force | Out-Null
    New-ItemProperty -Path $key -Name CommandLine -PropertyType String -Value ([string]$app.arguments) -Force | Out-Null
    New-ItemProperty -Path $key -Name ShowInTSWA -PropertyType DWord -Value 0 -Force | Out-Null
}

# Windows client SKUs do not expose the Server Manager RDS role, but the
# terminal-services application allow-list is still the native contract used
# by WinApps-like clients. This is deliberately best-effort: if the local RDP
# service does not accept RAIL, qOS chooses the app-focused SPICE backend. No
# unsupported binary patching or RDP Wrapper is involved.
New-ItemProperty -Path $allowList -Name fDisabledAllowList -PropertyType DWord -Value 0 -Force | Out-Null

# Do not make a host filesystem or clipboard grant by accident. Per-session
# redirects are supplied by FreeRDP only after qOS's normal workspace broker
# has materialised a short lease.
$ts = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services'
New-Item -Path $ts -Force | Out-Null
New-ItemProperty -Path $ts -Name fDisableCdm -PropertyType DWord -Value 1 -Force | Out-Null
New-ItemProperty -Path $ts -Name fDisableClip -PropertyType DWord -Value 1 -Force | Out-Null
New-ItemProperty -Path $ts -Name fDisableAudioCapture -PropertyType DWord -Value 1 -Force | Out-Null
New-ItemProperty -Path $ts -Name UserAuthentication -PropertyType DWord -Value 1 -Force | Out-Null
New-ItemProperty -Path $ts -Name fSingleSessionPerUser -PropertyType DWord -Value 0 -Force | Out-Null

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
$mode = if ($isServer) { 'official RDS RemoteApp' } else { 'native client RAIL (best effort)' }
Write-Output "qOS application publication installed using $mode. The Windows desktop remains a recovery-only surface."
