#!/usr/bin/env bash
# Static contracts for the host/guest part of the Windows fallback. These are
# intentionally source-level: CI need not boot a licensed Windows Server just
# to discover that a package update opened a route or normalised a desktop.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OS_ROOT="${OS_ROOT:-$(cd "$HERE/.." && pwd)}"
NET="$OS_ROOT/usr/share/qos/windows/libvirt/qos-windows-hostonly.xml"
EGRESS_NET="$OS_ROOT/usr/share/qos/windows/libvirt/qos-windows-egress.xml"
UNIT="$OS_ROOT/usr/lib/systemd/system/qos-windows-vm-network.service"
NETWORK_TOOL="$OS_ROOT/usr/libexec/qos-windows-vm-network"
PROVISION="$OS_ROOT/usr/libexec/qos-windows-provision"
RECOVERY="$OS_ROOT/usr/libexec/qos-windows-recovery-console"
OWNER="$OS_ROOT/usr/libexec/qos-windows-owner"
VM_CONTROL_SOURCE="$OS_ROOT/../crates/system/windows-vm/src/bin/qos-windows-vm-control.rs"
LIBVIRT="$OS_ROOT/../crates/system/windows-vm/src/libvirt.rs"
INSTALL="$OS_ROOT/usr/share/qos/windows/guest/Install-qOSRemoteApp.ps1"
BRIDGE="$OS_ROOT/usr/share/qos/windows/guest/Start-qOSGuestBridge.ps1"
FOCUSED="$OS_ROOT/usr/share/qos/windows/guest/Start-qOSFocusedApp.ps1"
INSTALLER="$OS_ROOT/usr/share/qos/windows/guest/Start-qOSInstaller.ps1"
UNINSTALLER="$OS_ROOT/usr/share/qos/windows/guest/Start-qOSUninstaller.ps1"
PROCESS_JOB="$OS_ROOT/usr/share/qos/windows/guest/qOS-ProcessJob.ps1"
PROCESS_JOB_BROKER="$OS_ROOT/usr/share/qos/windows/guest/Start-qOSProcessJobBroker.ps1"
COMPAT_DOC="$OS_ROOT/../docs/windows-compatibility.md"
CONTAINERFILE="$OS_ROOT/Containerfile"

fails=0
fail() { printf 'FAIL  %s\n' "$*" >&2; fails=$((fails + 1)); }
ok() { printf 'ok    %s\n' "$*"; }
need() { [ -r "$1" ] && ok "$(basename "$1") exists" || fail "missing $1"; }
has() { grep -Fq -- "$2" "$1" && ok "$3" || fail "$3"; }
lacks() { grep -Fq -- "$2" "$1" && fail "$3" || ok "$3"; }

for file in "$NET" "$EGRESS_NET" "$UNIT" "$NETWORK_TOOL" "$PROVISION" "$RECOVERY" "$OWNER" "$VM_CONTROL_SOURCE" "$INSTALL" "$BRIDGE" "$FOCUSED" "$INSTALLER" "$UNINSTALLER" "$PROCESS_JOB" "$PROCESS_JOB_BROKER" "$COMPAT_DOC"; do need "$file"; done

has "$NET" '<forward mode="none"/>' 'libvirt bridge has no forwarding'
has "$NET" '<qos:windows-hostonly ' 'network has qOS ownership marker'
lacks "$NET" "mode='nat'" 'network cannot silently become NAT'
lacks "$NET" '<route ' 'network has no guest route'
has "$EGRESS_NET" '<forward mode="nat"/>' 'egress network is explicitly NAT-backed'
has "$EGRESS_NET" '<qos:windows-egress ' 'egress network has qOS ownership marker'
has "$NETWORK_TOOL" 'qos-windows-egress' 'network service defines the brokered egress network'
has "$NETWORK_TOOL" 'net-destroy' 'egress network is inactive by default'
has "$NETWORK_TOOL" 'existing $NAME is not a qOS host-only network' 'foreign network collision fails closed'
has "$NETWORK_TOOL" "net-define" 'network is defined through libvirt'
has "$UNIT" 'RestrictAddressFamilies=AF_UNIX' 'network setup is Unix-socket-only'
for directive in NoNewPrivileges=yes CapabilityBoundingSet= ProtectSystem=strict ProtectHome=yes PrivateDevices=yes RestrictNamespaces=yes MemoryDenyWriteExecute=yes; do
    has "$UNIT" "$directive" "network service hardens $directive"
done

has "$PROVISION" "must never be passed on argv" 'provisioner rejects host-side licence handling by contract'
has "$PROVISION" "an appliance already exists" 'provisioner refuses a second appliance'
has "$PROVISION" 'qemu-img create' 'provisioner creates qcow2 only once'
has "$PROVISION" 'xorriso -as mkisofs' 'guest payload is delivered without a host share'
has "$PROVISION" '"license":"unknown"' 'host state records no licence value'
has "$PROVISION" '"$VM_CONTROL" start' 'provisioning starts the defined headless singleton'
has "$RECOVERY" '--owner-authorized' 'desktop console has an explicit recovery gate'
has "$RECOVERY" 'refuses to run a Wayland GUI as root' 'desktop viewer refuses root execution'
has "$OWNER" '"$VM_CONTROL" start' 'owner backend starts a powered-off singleton idempotently'
has "$RECOVERY" 'consume-recovery' 'viewer requires a root-owned one-shot owner authorisation proof'
lacks "$RECOVERY" 'qos-windows-vm-control start' 'viewer cannot acquire VM management authority'
has "$RECOVERY" 'virt-viewer --connect qemu:///system --attach qos-windows-appliance' 'recovery is an explicit libvirt attachment'
has "$VM_CONTROL_SOURCE" 'LibvirtBackend::new' 'bootstrap reuses the typed libvirt backend'
has "$VM_CONTROL_SOURCE" 'backend.start(&config)' 'bootstrap defines and starts through one idempotent path'
has "$VM_CONTROL_SOURCE" 'egress-open' 'typed controller exposes purpose-bound egress'
has "$VM_CONTROL_SOURCE" 'shutdown|force-shutdown|restart|suspend|resume' 'typed controller exposes lifecycle actions'
has "$LIBVIRT" 'SMBIOS' 'virtual SMBIOS identity remains stable for activation'
has "$LIBVIRT" 'qos-windows-system' 'virtual disk identity remains stable for activation'

lacks "$INSTALL" 'ProductType -ne 3' 'client Windows is not rejected outright'
has "$INSTALL" 'RDS-RD-Server' 'official RDS RemoteApp role is required'
has "$INSTALL" 'TSAppAllowList' 'publication uses Windows RemoteApp allow list'
has "$INSTALL" 'native client RAIL (best effort)' 'client Windows uses native best-effort RAIL'
has "$INSTALL" 'never patches' 'client path does not patch the RDP service'
has "$INSTALL" 'fDisableCdm' 'guest defaults to no drive redirection'
has "$INSTALL" 'fDisableClip' 'guest defaults to no clipboard redirection'
has "$INSTALL" "-RemoteAddress '192.168.251.1'" 'RDP is restricted to host-only endpoint'
has "$INSTALL" "qOS guest bridge (host-only)" 'bridge firewall rule is host-only'
lacks "$INSTALL" 'Invoke-WebRequest.*RDP' 'guest does not download an RDP patcher'
has "$BRIDGE" 'https://' 'bridge requires TLS'
has "$BRIDGE" 'HMACSHA256' 'bridge authenticates requests'
has "$BRIDGE" 'SHA256]::HashData($cert.RawData)' 'RDP and bridge certificate is pinned with SHA-256'
has "$BRIDGE" 'FixedTimeEquals' 'bridge compares authentication in constant time'
has "$BRIDGE" 'replayed bridge nonce' 'bridge rejects nonce reuse'
has "$BRIDGE" 'New-LocalUser' 'each native RAIL launch receives a fresh account'
has "$BRIDGE" 'requestedPresentation -ne' 'focused SPICE does not mint RDP credentials'
has "$BRIDGE" 'Start-FocusedApplication' 'SPICE launch invokes the managed guest focus helper'
has "$BRIDGE" 'no managed interactive session exists' 'SPICE fails honestly without a visible session'
has "$BRIDGE" 'app_focused_spice' 'bridge has an explicit SPICE launch request'
has "$BRIDGE" 'RandomNumberGenerator]::Create' 'launch password uses Windows PowerShell-compatible cryptographic RNG'
has "$BRIDGE" 'New-ScheduledTaskTrigger -Once -At $when' 'credential cleanup avoids locale-dependent schtasks dates'
has "$BRIDGE" 'qOS-Remove-' 'temporary launch accounts are reaped'
has "$BRIDGE" "LicenseStatus -eq 1" 'guest health reports real Windows activation state'
has "$BRIDGE" 'catalogGeneration = $catalog.generation' 'guest health reports the manifest catalogue generation'
has "$BRIDGE" 'generation = $source.generation' 'guest catalogue preserves its persisted generation'
has "$CONTAINERFILE" 'qos-windows-vm-network.service' 'image wires network service'
has "$CONTAINERFILE" 'qos-windows-provision' 'image wires provision tool'
has "$CONTAINERFILE" '/out/qos-windows-vm-control /usr/libexec/qos-windows-vm-control' 'image ships the singleton VM controller'
has "$CONTAINERFILE" 'virt-viewer' 'image wires explicit recovery viewer'
has "$PROVISION" 'Start-qOSFocusedApp.ps1' 'guest tools carry the focused-app helper'
has "$PROVISION" 'Start-qOSInstaller.ps1' 'guest tools carry the managed installer helper'
has "$PROVISION" 'Start-qOSUninstaller.ps1' 'guest tools carry the typed uninstaller helper'
has "$PROVISION" 'qOS-ProcessJob.ps1' 'guest tools carry the shared native process-job helper'
has "$PROVISION" 'Start-qOSProcessJobBroker.ps1' 'guest tools carry the typed UAC process-job broker'
has "$INSTALL" "Join-Path \$focusRoot 'qOS-ProcessJob.ps1'" 'guest install places the job helper beside focused launches'
has "$INSTALL" "Join-Path \$installerRoot 'qOS-ProcessJob.ps1'" 'guest install places the job helper in the protected installer root'
has "$BRIDGE" "Join-Path \$operationDirectory 'Start-qOSProcessJobBroker.ps1'" 'installer operation receives its fixed local UAC broker'
has "$FOCUSED" 'Stop-Process -Id' 'focused fallback suppresses Explorer while running'
has "$FOCUSED" 'Start-Process -FilePath "$env:WINDIR\explorer.exe"' 'focused fallback restores Explorer'
has "$FOCUSED" 'RunFocusedApplication' 'focused fallback waits through the shared process-tree job'
lacks "$FOCUSED" 'Wait-Process' 'focused fallback never waits only for the bootstrap PID'
has "$PROCESS_JOB" 'CreateSuspended = 0x00000004' 'managed process starts with its primary thread suspended'
has "$PROCESS_JOB" 'AssignProcessToJobObject(job, process.hProcess)' 'suspended process is assigned to the qOS job before execution'
has "$PROCESS_JOB" 'ResumeThread(process.hThread)' 'managed process resumes only after job assignment'
has "$PROCESS_JOB" 'JobObjectLimitKillOnJobClose = 0x00002000' 'abandoned helpers tear down their contained process tree'
has "$PROCESS_JOB" 'accounting.ActiveProcesses == 0' 'Explorer restoration waits for the whole job to become empty'
has "$PROCESS_JOB" 'The process is already in an incompatible parent job.' 'incompatible nested jobs fail closed before resume'
lacks "$PROCESS_JOB" 'const uint JobObjectLimitBreakaway' 'qOS does not permit explicit job breakaway'
has "$PROCESS_JOB" 'CommandLineToArgvW(string commandLine' 'EXE uninstall discovery uses the native Windows argv parser'
has "$PROCESS_JOB" 'MaxUninstallArguments = 32' 'EXE uninstall argument count is bounded'
has "$PROCESS_JOB" 'MaxUninstallArgumentChars = 4096' 'each EXE uninstall argument is bounded'
has "$PROCESS_JOB" 'MaxUninstallCommandChars = 16384' 'aggregate EXE uninstall arguments are bounded'
lacks "$PROCESS_JOB" 'ShellExecuteEx' 'native process helper never launches the vendor command through ShellExecute'
has "$PROCESS_JOB" '[IO.FileAttributes]::ReparsePoint' 'EXE uninstall paths reject reparse points'
has "$PROCESS_JOB" 'qOS executable uninstall path has an untrusted owner' 'EXE uninstall paths require a trusted owner'
has "$PROCESS_JOB" 'grants write access to an untrusted principal' 'EXE uninstall paths reject untrusted write ACEs'
has "$PROCESS_JOB" "\$root.StartsWith((\$programRoot + '\\')" 'EXE uninstall roots must be product-specific Program Files children'
has "$BRIDGE" 'Require-UploadAuth' 'installer upload has a metadata-bound HMAC contract'
has "$BRIDGE" 'installer upload needs an exact content length' 'installer upload requires an exact size'
has "$BRIDGE" 'TransformFinalBlock' 'guest recomputes installer SHA-256 while streaming'
has "$BRIDGE" '[IO.File]::Move($partial, $final)' 'installer becomes visible only by atomic same-volume rename'
has "$BRIDGE" '$maximumInstallerBytes = [UInt64]17179869184' 'guest import matches the 16 GiB portal bound'
has "$BRIDGE" '$maximumStagedBytes = [UInt64]34359738368' 'aggregate staged installer bytes are bounded'
has "$BRIDGE" 'Invoke-InstallerCleanup' 'expired and completed installer resources are reaped'
has "$BRIDGE" "-LogonType InteractiveToken -RunLevel Limited" 'installer starts in the existing limited interactive session'
has "$BRIDGE" 'beforeExecutableHashes' 'candidate discovery is scoped to installation changes'
has "$BRIDGE" '[IO.File]::Replace' 'administrator publication state advances atomically'
has "$BRIDGE" 'expectedGeneration' 'publication changes use optimistic generation checks'
has "$BRIDGE" "'Traverse', 'None', 'None'" 'installer parent ACL grants traversal only'
lacks "$BRIDGE" "ReadAndExecute', 'ContainerInherit,ObjectInherit'.*\$installerRoot" 'staged payload root is not recursively exposed'
has "$INSTALLER" 'Get-FileHash -LiteralPath $package -Algorithm SHA256' 'interactive helper rechecks accepted package digest'
has "$INSTALLER" 'RunMsiInstaller' 'MSI helper has one typed fixed invocation'
has "$INSTALLER" 'RunExecutableInstaller($package)' 'EXE helper invokes only the accepted file without arguments'
lacks "$INSTALLER" 'Start-Process -FilePath $package' 'installer package cannot bypass suspended job assignment'
has "$INSTALLER" '-Verb RunAs' 'requireAdministrator EXEs show standard Windows UAC for the fixed qOS broker'
has "$INSTALLER" 'UAC consent was canceled' 'UAC cancellation is reported explicitly'
has "$INSTALLER" 'Where-Object { $_.SessionId -eq $sessionId }' 'installer hides Explorer only in its managed session'
lacks "$INSTALLER" 'Invoke-Expression' 'installer helper has no free-form command evaluator'
has "$PROCESS_JOB_BROKER" "[string]\$job.kind -ne 'exe'" 'elevated broker accepts only the typed EXE operation'
has "$PROCESS_JOB_BROKER" '[string]$Matches.operation -cne $pathOperationId' 'elevated broker binds package and job to one operation directory'
has "$PROCESS_JOB_BROKER" 'Get-FileHash -LiteralPath $package -Algorithm SHA256' 'elevated broker rechecks the accepted digest after UAC'
has "$PROCESS_JOB_BROKER" 'RunExecutableInstaller($package)' 'elevated broker uses suspended native creation without package arguments'
lacks "$PROCESS_JOB_BROKER" 'Invoke-Expression' 'elevated broker has no free-form command evaluator'
has "$BRIDGE" 'uninstallAvailable = $canUninstall' 'authenticated catalogue persists uninstall availability'
has "$BRIDGE" "operationKind = 'uninstallation'" 'uninstall has a distinct typed operation'
has "$BRIDGE" 'Reset-UninstallRecord' 'failed uninstall operations become safely retryable'
has "$BRIDGE" 'Complete-UninstallPublication' 'successful uninstall retracts its publication'
has "$INSTALL" '{"schemaVersion":2,"records":[]}' 'new uninstall stores use the tagged v2 schema'
has "$BRIDGE" '[int]$store.schemaVersion -ne 1' 'legacy MSI-only uninstall stores remain readable'
has "$BRIDGE" "kind = 'msi'" 'legacy MSI records migrate to the tagged kind'
has "$BRIDGE" '[qOS.WindowsGuest.ProcessJob]::ParseUninstallCommandLine($uninstallString)' 'registry EXE uninstall commands are parsed but never executed raw'
has "$BRIDGE" 'Get-qOSValidatedExecutableUninstallContract' 'EXE uninstall discovery and persistence enforce the bound contract'
has "$BRIDGE" 'executableSha256 = $validatedUninstaller.executableSha256' 'administrator uninstall records persist the exact EXE digest'
has "$BRIDGE" "operationKind = 'uninstallation'" 'uninstall jobs retain a typed operation discriminator'
has "$UNINSTALLER" 'RunMsiUninstaller' 'uninstaller accepts only a parsed MSI product code'
has "$UNINSTALLER" 'Get-qOSValidatedExecutableUninstallContract $job' 'EXE uninstaller revalidates root, ACL, path, argv, and digest at execution'
has "$UNINSTALLER" 'RunExecutableUninstaller' 'EXE uninstaller uses the direct typed process-job argv method'
has "$UNINSTALLER" '-Verb RunAs' 'requireAdministrator EXE uninstallers use the fixed visible UAC broker'
has "$PROCESS_JOB_BROKER" "[string]\$job.operationKind -eq 'uninstallation'" 'elevated broker accepts only typed EXE uninstall jobs'
has "$PROCESS_JOB_BROKER" 'Get-qOSValidatedExecutableUninstallContract $job' 'elevated broker repeats EXE binding and digest validation after UAC'
has "$PROCESS_JOB_BROKER" 'RunExecutableUninstaller' 'elevated broker invokes the EXE uninstaller without a shell'
lacks "$UNINSTALLER" "-ArgumentList @('/x'" 'uninstaller cannot bypass its contained typed method'
has "$UNINSTALLER" 'Where-Object { $_.SessionId -eq $sessionId }' 'uninstaller hides Explorer only in its managed session'
lacks "$UNINSTALLER" 'UninstallString' 'registry uninstall command strings are never executed'
lacks "$UNINSTALLER" 'Invoke-Expression' 'uninstaller has no free-form command evaluator'
has "$COMPAT_DOC" 'The raw `UninstallString` is neither stored nor run.' 'EXE uninstall acceptance and raw-command rejection are documented'

VM_ROOT="$(cd "$OS_ROOT/../crates/system/windows-vm" 2>/dev/null && pwd)"
REMOTE_APP="$VM_ROOT/src/remote_app.rs"
PRESENTATION="$VM_ROOT/src/presentation.rs"
LIBVIRT="$VM_ROOT/src/libvirt.rs"
SESSION_WIRING="$VM_ROOT/../../../shell/src/shell/windows_compat.rs"
need "$REMOTE_APP"
need "$PRESENTATION"
need "$LIBVIRT"
need "$SESSION_WIRING"
has "$REMOTE_APP" 'OsString::from("xfreerdp")' 'RAIL uses the upstream FreeRDP client'
lacks "$REMOTE_APP" 'binary: OsString::from("wlfreerdp")' 'RAIL does not assume a nonexistent Wayland wrapper'
has "$REMOTE_APP" 'AppFocusedSpiceLauncher' 'SPICE is an explicit app-focused fallback'
has "$REMOTE_APP" 'qemu:///system' 'SPICE attaches through local libvirt only'
has "$PRESENTATION" 'ApplicationPresentation::AppFocusedSpice' 'presentation fallback is typed'
lacks "$PRESENTATION" 'ApplicationPresentation::Recovery' 'recovery is not an application route'
has "$LIBVIRT" '/var/lib/qos/windows/install-media.iso' 'prepared Windows media is attached read-only'
has "$LIBVIRT" '/var/lib/qos/windows/qos-guest-tools.iso' 'guest tools are attached read-only'
has "$LIBVIRT" 'retire_installation_media' 'authenticated enrollment retires setup media'
has "$LIBVIRT" '"--live",' 'setup media is removed from the running guest'
has "$LIBVIRT" '"--config",' 'setup media is removed from persistent configuration'
has "$SESSION_WIRING" 'LiveVmDelegate::new' 'session composes the live singleton VM delegate'
has "$SESSION_WIRING" 'HttpsGuestBridge::new' 'session uses the authenticated HTTPS guest bridge'
lacks "$SESSION_WIRING" '        UnavailableVmDelegate,' 'session does not hardcode VM unavailability'

exit "$fails"
