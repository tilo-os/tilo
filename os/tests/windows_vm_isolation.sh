#!/usr/bin/env bash
# Static contracts for the host/guest part of the Windows fallback. These are
# intentionally source-level: CI need not boot a licensed Windows Server just
# to discover that a package update opened a route or normalised a desktop.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OS_ROOT="${OS_ROOT:-$(cd "$HERE/.." && pwd)}"
NET="$OS_ROOT/usr/share/tilo/windows/libvirt/tilo-windows-hostonly.xml"
EGRESS_NET="$OS_ROOT/usr/share/tilo/windows/libvirt/tilo-windows-egress.xml"
UNIT="$OS_ROOT/usr/lib/systemd/system/tilo-windows-vm-network.service"
REAPER="$OS_ROOT/usr/lib/systemd/system/tilo-windows-vm-egress-reaper.service"
REAPER_TIMER="$OS_ROOT/usr/lib/systemd/system/tilo-windows-vm-egress-reaper.timer"
REAPER_TIMER_PATH="$OS_ROOT/usr/lib/systemd/system/tilo-windows-vm-egress-reaper.path"
LEASE_PATH="$OS_ROOT/usr/lib/systemd/system/tilo-windows-vm-egress-lease.path"
TMPFILES="$OS_ROOT/usr/lib/tmpfiles.d/tilo.conf"
NETWORK_TOOL="$OS_ROOT/usr/libexec/tilo-windows-vm-network"
PROVISION="$OS_ROOT/usr/libexec/tilo-windows-provision"
RECOVERY="$OS_ROOT/usr/libexec/tilo-windows-recovery-console"
OWNER="$OS_ROOT/usr/libexec/tilo-windows-owner"
VM_CONTROL_SOURCE="$OS_ROOT/../crates/windows/windows-vm/src/bin/tilo-windows-vm-control.rs"
LIBVIRT="$OS_ROOT/../crates/windows/windows-vm/src/libvirt.rs"
INSTALL="$OS_ROOT/usr/share/tilo/windows/guest/Install-TiloRemoteApp.ps1"
BRIDGE="$OS_ROOT/usr/share/tilo/windows/guest/Start-TiloGuestBridge.ps1"
FOCUSED="$OS_ROOT/usr/share/tilo/windows/guest/Start-TiloFocusedApp.ps1"
INSTALLER="$OS_ROOT/usr/share/tilo/windows/guest/Start-TiloInstaller.ps1"
UNINSTALLER="$OS_ROOT/usr/share/tilo/windows/guest/Start-TiloUninstaller.ps1"
PROCESS_JOB="$OS_ROOT/usr/share/tilo/windows/guest/Tilo-ProcessJob.ps1"
PROCESS_JOB_BROKER="$OS_ROOT/usr/share/tilo/windows/guest/Start-TiloProcessJobBroker.ps1"
NETWORK_PROXY="$OS_ROOT/usr/share/tilo/windows/guest/Start-TiloNetworkProxy.ps1"
REMOTE_APP_WRAPPER="$OS_ROOT/usr/share/tilo/windows/guest/Start-TiloRemoteApp.ps1"
COMPAT_DOC="$OS_ROOT/../docs/windows-compatibility.md"
COMPAT_PROTOCOL="$OS_ROOT/../crates/windows/compat-service/src/protocol.rs"
COMPAT_SESSION="$OS_ROOT/../crates/windows/compat-service/src/session.rs"
CONTAINERFILE="$OS_ROOT/Containerfile"

fails=0
fail() { printf 'FAIL  %s\n' "$*" >&2; fails=$((fails + 1)); }
ok() { printf 'ok    %s\n' "$*"; }
need() { [ -r "$1" ] && ok "$(basename "$1") exists" || fail "missing $1"; }
has() { grep -Fq -- "$2" "$1" && ok "$3" || fail "$3"; }
lacks() { grep -Fq -- "$2" "$1" && fail "$3" || ok "$3"; }

if python3 "$HERE/windows_guest_powershell_syntax.py" "$OS_ROOT/usr/share/tilo/windows/guest"; then
    ok 'guest PowerShell structure and fixed-wrapper contracts hold'
else
    fail 'guest PowerShell structure or fixed-wrapper contract failed'
fi

for file in "$NET" "$EGRESS_NET" "$UNIT" "$REAPER" "$REAPER_TIMER" "$REAPER_TIMER_PATH" "$LEASE_PATH" "$TMPFILES" "$NETWORK_TOOL" "$PROVISION" "$RECOVERY" "$OWNER" "$VM_CONTROL_SOURCE" "$INSTALL" "$BRIDGE" "$FOCUSED" "$INSTALLER" "$UNINSTALLER" "$PROCESS_JOB" "$PROCESS_JOB_BROKER" "$NETWORK_PROXY" "$REMOTE_APP_WRAPPER" "$COMPAT_DOC" "$COMPAT_PROTOCOL" "$COMPAT_SESSION"; do need "$file"; done

has "$NET" '<forward mode="none"/>' 'libvirt bridge has no forwarding'
has "$NET" '<tilo:windows-hostonly ' 'network has Tilo ownership marker'
lacks "$NET" "mode='nat'" 'network cannot silently become NAT'
lacks "$NET" '<route ' 'network has no guest route'
has "$EGRESS_NET" '<forward mode="nat"/>' 'egress network is explicitly NAT-backed'
has "$EGRESS_NET" '<tilo:windows-egress ' 'egress network has Tilo ownership marker'
has "$NETWORK_TOOL" 'tilo-windows-egress' 'network service defines the brokered egress network'
has "$NETWORK_TOOL" 'net-destroy' 'egress network is inactive by default'
has "$REAPER" 'egress-reap' 'root service reaps expired egress leases'
has "$REAPER" 'controller repairs /run/tilo/windows-egress to mode 2750' 'reaper documents its required setgid runtime repair'
lacks "$REAPER" 'RestrictSUIDSGID=yes' 'reaper sandbox permits its required setgid runtime repair'
has "$REAPER" 'egress-reap --until-idle' 'reaper keeps running until the last lease is retired'
has "$REAPER" 'ReadWritePaths=/run/tilo/windows-egress' 'reaper can take the controller lock and retire records under ProtectSystem=strict'
has "$LEASE_PATH" 'DirectoryNotEmpty=/run/tilo/windows-egress/leases' 'a lease record arms the reaper before its NIC is attached'
has "$LEASE_PATH" 'Unit=tilo-windows-vm-egress-reaper.service' 'lease path unit starts the root reaper'
has "$VM_CONTROL_SOURCE" 'scan_lease_records(root)' 'reaper re-reads the lease records on every wake'
has "$VM_CONTROL_SOURCE" 'MAX_SLEEP_SECONDS: u64 = 5' 'reaper wakes within 5 s of any lease expiry, including one passed during suspend'
has "$VM_CONTROL_SOURCE" 'UNRETIRED_ROOT' 'a lease the reaper cannot retire is set aside instead of pinning it'
has "$REAPER" 'StartLimitIntervalSec=0' 'a failing reaper can never be start-limited out of enforcing expiry'
has "$REAPER" 'Restart=on-failure' 'a failed reaper is restarted by its own unit, not the level-triggered path'
has "$REAPER" 'RestartPreventExitStatus=3' 'an idle reaper whose last pass failed is reported, not retried'
has "$VM_CONTROL_SOURCE" 'IDLE_AFTER_FAILURE_EXIT: u8 = 3' 'controller exit status matches the unit restart policy'
has "$TMPFILES" '/run/tilo/windows-egress/unretired 0700' 'set-aside lease records stay root-only'
has "$PROVISION" 'mv -f "$UUID_FILE.new" "$UUID_FILE"' 'domain identity appears complete for the status path unit'
has "$REAPER_TIMER" 'OnUnitActiveSec=30s' 'provisioned appliance status is refreshed periodically'
has "$REAPER_TIMER" 'ConditionPathExists=/var/lib/tilo/windows/domain.uuid' 'status timer is inert without a provisioned appliance'
lacks "$REAPER_TIMER" '[Install]' 'status timer is not enabled on machines without an appliance'
has "$REAPER_TIMER_PATH" 'PathExists=/var/lib/tilo/windows/domain.uuid' 'provisioning starts the status timer without a reboot'
has "$REAPER_TIMER_PATH" 'Unit=tilo-windows-vm-egress-reaper.timer' 'appliance path unit starts the status timer'
has "$VM_CONTROL_SOURCE" 'write_lease' 'egress lease is recorded before NIC attachment'
has "$VM_CONTROL_SOURCE" 'egress-reap' 'controller has a crash-safe lease reaper action'
has "$VM_CONTROL_SOURCE" 'CONTROLLER_LOCK' 'controller has one exclusive lock for lifecycle and egress mutations'
has "$VM_CONTROL_SOURCE" 'advisory_lock::FileLock::exclusive' 'controller holds an advisory exclusive mutation lock'
has "$VM_CONTROL_SOURCE" '"graphics-attach"' 'controller lock covers graphics/domain XML mutation'
has "$VM_CONTROL_SOURCE" '"define"' 'controller lock covers domain definition mutation'
has "$VM_CONTROL_SOURCE" 'if active_lease()?.is_some()' 'egress open rejects any existing lease before creating a NIC'
has "$VM_CONTROL_SOURCE" 'STATUS_SUMMARY' 'controller publishes a sanitized runtime status summary'
has "$VM_CONTROL_SOURCE" 'write_status_summary' 'lifecycle and egress changes atomically refresh status summary'
has "$VM_CONTROL_SOURCE" 'rollback_attached_egress' 'failed egress setup has one rollback path'
has "$VM_CONTROL_SOURCE" 'lease was retained for retry' 'detach failure preserves the reaper/revoke record'
has "$COMPAT_SESSION" 'PortalDocumentAccess::ReadOnly' 'portal Open file is server-fixed read-only'
lacks "$COMPAT_PROTOCOL" 'writable: bool' 'portal Open file has no client-controlled writeback bit'
has "$NETWORK_TOOL" 'existing $NAME is not a Tilo host-only network' 'foreign network collision fails closed'
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
lacks "$RECOVERY" 'tilo-windows-vm-control start' 'viewer cannot acquire VM management authority'
has "$RECOVERY" 'virt-viewer --connect qemu:///system --attach tilo-windows-appliance' 'recovery is an explicit libvirt attachment'
has "$VM_CONTROL_SOURCE" 'LibvirtBackend::new' 'bootstrap reuses the typed libvirt backend'
has "$VM_CONTROL_SOURCE" 'backend.start(&config)' 'bootstrap defines and starts through one idempotent path'
has "$VM_CONTROL_SOURCE" 'egress-open' 'typed controller exposes purpose-bound egress'
has "$VM_CONTROL_SOURCE" 'shutdown|force-shutdown|restart|suspend|resume' 'typed controller exposes lifecycle actions'
has "$LIBVIRT" 'SMBIOS' 'virtual SMBIOS identity remains stable for activation'
has "$LIBVIRT" 'tilo-windows-system' 'virtual disk identity remains stable for activation'
has "$LIBVIRT" '/usr/share/virtio-win/virtio-win.iso' 'setup attaches packaged virtio drivers'
has "$LIBVIRT" '"sdd"' 'setup retires the virtio driver medium'
has "$LIBVIRT" "<controller type='sata'" 'setup media use a Windows-inbox SATA controller'
has "$LIBVIRT" "<target dev='sdd' bus='sata'" 'virtio drivers are readable before virtio-scsi installation'
has "$LIBVIRT" 'virtio-scsi' 'installed system disk remains on virtio-scsi'
has "$PROVISION" 'virtio-win driver ISO is missing' 'provisioning requires packaged virtio drivers'

lacks "$INSTALL" 'ProductType -ne 3' 'client Windows is not rejected outright'
has "$INSTALL" 'RDS-RD-Server' 'official RDS RemoteApp role is required'
has "$INSTALL" 'TSAppAllowList' 'publication uses Windows RemoteApp allow list'
has "$INSTALL" 'native client RAIL (best effort)' 'client Windows uses native best-effort RAIL'
has "$INSTALL" 'never patches' 'client path does not patch the RDP service'
has "$INSTALL" 'fDisableCdm' 'guest defaults to no drive redirection'
has "$INSTALL" 'fDisableClip' 'guest defaults to no clipboard redirection'
has "$INSTALL" "-RemoteAddress '192.168.251.1'" 'RDP is restricted to host-only endpoint'
has "$INSTALL" "Tilo guest bridge (host-only)" 'bridge firewall rule is host-only'
lacks "$INSTALL" 'Invoke-WebRequest.*RDP' 'guest does not download an RDP patcher'
has "$BRIDGE" 'https://' 'bridge requires TLS'
has "$BRIDGE" 'HMACSHA256' 'bridge authenticates requests'
has "$BRIDGE" 'SHA256]::HashData($cert.RawData)' 'RDP and bridge certificate is pinned with SHA-256'
has "$BRIDGE" 'FixedTimeEquals' 'bridge compares authentication in constant time'
has "$BRIDGE" 'replayed bridge nonce' 'bridge rejects nonce reuse'
has "$INSTALL" 'managed-session.json' 'provisioning binds one persistent managed Windows identity'
has "$INSTALL" 'Language.Parser]::ParseFile' 'guest setup parses every PowerShell payload with the authoritative Windows parser'
has "$INSTALL" "\$managedUser = 'tilo-user'" 'managed session identity is fixed rather than user selected'
has "$INSTALL" 'RandomNumberGenerator' 'managed session credential is generated cryptographically'
has "$INSTALL" 'RotateManagedSessionCredential' 'managed session credential rotation is explicit and idempotent'
has "$INSTALL" 'UserMayNotChangePassword' 'managed identity cannot desynchronise the broker credential'
lacks "$INSTALL" 'Get-Credential' 'provisioning never prompts for the service-session credential'
lacks "$INSTALL" 'ManagedSessionCredential.UserName' 'provisioning has no managed account chooser'
lacks "$INSTALL" 'AutoAdminLogon' 'managed session does not use plaintext Winlogon autologon'
has "$INSTALL" 'New-LocalUser -Name $managedUser' 'persistent managed identity is created only during provisioning'
has "$INSTALL" 'fSingleSessionPerUser' 'Windows is constrained to one session for the managed identity'
has "$INSTALL" 'Start-TiloRemoteApp.ps1' 'published aliases use the fixed Tilo wrapper'
has "$INSTALL" 'Start-TiloNetworkProxy.ps1' 'wrapper receives the guest local proxy helper'
has "$REMOTE_APP_WRAPPER" 'HTTP_PROXY' 'wrapper routes target process trees through loopback proxy'
has "$REMOTE_APP_WRAPPER" 'Consume the one-time auth job' 'wrapper consumes the short-lived authenticated launch job before target execution'
has "$REMOTE_APP_WRAPPER" 'network-runtime-' 'wrapper deletes its nonce-specific runtime marker after target exit'
has "$REMOTE_APP_WRAPPER" 'expiresAtUnix' 'wrapper rejects expired RAIL jobs'
has "$REMOTE_APP_WRAPPER" 'localProxyPort' 'wrapper receives the exact authenticated per-launch loopback port'
has "$REMOTE_APP_WRAPPER" 'localProxyCredential' 'wrapper receives a separate per-lease loopback credential'
has "$REMOTE_APP_WRAPPER" 'tilo:$credential@127.0.0.1' 'wrapper supplies standard Basic proxy URL credentials'
lacks "$REMOTE_APP_WRAPPER" 'netsh winhttp' 'wrapper never sets machine-wide WinHTTP proxy state'
lacks "$REMOTE_APP_WRAPPER" 'Internet Settings' 'wrapper never mutates shared WinINet state across leases'
lacks "$REMOTE_APP_WRAPPER" '18080' 'wrapper never uses a shared fixed loopback proxy port'
has "$NETWORK_PROXY" '192.168.251.1' 'guest proxy can connect only to the host-only gateway'
has "$NETWORK_PROXY" 'Proxy-Authorization: Bearer' 'guest proxy authenticates the short-lived host lease'
has "$NETWORK_PROXY" 'Authorised(head' 'guest proxy requires its local lease credential for ordinary loopback requests'
has "$NETWORK_PROXY" '"Basic "' 'guest proxy requires standard Basic local proxy authentication'
has "$NETWORK_PROXY" 'StripLocalAuthorization' 'plain HTTP forwarding removes the local credential header'
has "$NETWORK_PROXY" 'static bool Same' 'guest proxy compares local credentials without early-exit equality as hardening'
has "$NETWORK_PROXY" 'Add-Type -TypeDefinition' 'guest proxy uses a real CLR concurrent relay, not PowerShell threadpool scriptblocks'
has "$NETWORK_PROXY" 'File.Exists(runtimeMarkerPath)' 'proxy follows its exact runtime marker rather than a shared session state'
has "$NETWORK_PROXY" 'pendingExpiresAtUnix' 'proxy bounds only an unconsumed launch handoff, not app runtime'
has "$NETWORK_PROXY" 'readyPath' 'proxy publishes a listener readiness handshake before launch'
lacks "$NETWORK_PROXY" 'Loopback,18080' 'proxy allocates an independent loopback port per lease'
lacks "$NETWORK_PROXY" 'QueueUserWorkItem({' 'guest proxy does not use a PowerShell scriptblock without a Runspace'
lacks "$NETWORK_PROXY" 'processJobName' 'guest proxy does not overclaim same-user job membership isolation'
lacks "$BRIDGE" 'processJobName' 'bridge does not publish an unusable cross-session job identity'
lacks "$BRIDGE" 'New-LocalUser' 'native RAIL launches never create extra Windows accounts'
has "$BRIDGE" 'Get-ManagedSession' 'native RAIL reuses the provisioned managed identity'
has "$BRIDGE" 'requestedPresentation -ne' 'focused SPICE does not mint RDP credentials'
has "$BRIDGE" 'Start-FocusedApplication' 'SPICE launch invokes the managed guest focus helper'
has "$BRIDGE" 'Start-RailApplication' 'RAIL launch writes an authenticated wrapper operation'
has "$BRIDGE" 'network-proxy-' 'SYSTEM bridge creates a proxy configuration per launch nonce'
has "$BRIDGE" 'RandomNumberGenerator' 'bridge generates an unguessable per-lease loopback credential'
has "$BRIDGE" 'proxy did not become ready' 'bridge refuses launch until the per-lease listener is ready'
has "$BRIDGE" "'Read,Delete'" 'wrapper can consume its exact marker without directory write authority'
has "$BRIDGE" "'Delete'" 'wrapper can delete its exact runtime marker without write authority'
has "$BRIDGE" 'network-runtime-' 'bridge uses a nonce-specific runtime marker separate from the auth job'
lacks "$BRIDGE" 'Ensure-SharedNetworkProxy' 'bridge never replaces another launch proxy with a shared bearer'
has "$BRIDGE" 'Get-NetworkProxyLease' 'bridge validates host-only network lease details'
has "$BRIDGE" 'Remove-ExpiredRailJobs' 'bridge reaps stale RAIL jobs after failed clients'
has "$BRIDGE" 'no managed interactive session exists' 'SPICE fails honestly without a visible session'
has "$BRIDGE" 'app_focused_spice' 'bridge has an explicit SPICE launch request'
lacks "$BRIDGE" 'Tilo-Remove-' 'bridge has no temporary-account cleanup path'
has "$BRIDGE" "LicenseStatus -eq 1" 'guest health reports real Windows activation state'
has "$BRIDGE" 'catalogGeneration = $catalog.generation' 'guest health reports the manifest catalogue generation'
has "$BRIDGE" 'generation = $source.generation' 'guest catalogue preserves its persisted generation'
has "$CONTAINERFILE" 'tilo-windows-vm-network.service' 'image wires network service'
has "$OWNER" 'systemctl start tilo-windows-vm-network.service' 'Windows network is activated only at the privileged VM boundary'
lacks "$CONTAINERFILE" 'systemctl enable tilo-windows-vm-network.service' 'Windows network setup is absent from the normal boot transaction'
has "$CONTAINERFILE" 'systemctl enable tilo-windows-vm-egress-reaper.path tilo-windows-vm-egress-lease.path' 'image enables crash-safe egress lease reaper'
lacks "$CONTAINERFILE" 'systemctl enable tilo-windows-vm-egress-reaper.timer' 'image does not wake every machine for the Windows status timer'
has "$CONTAINERFILE" 'tilo-windows-provision' 'image wires provision tool'
# shellcheck source=lib/containerfile.sh
. "$HERE/lib/containerfile.sh"
if containerfile_installs_output "$CONTAINERFILE" tilo-windows-vm-control \
     /usr/libexec/tilo-windows-vm-control; then
    ok 'image ships the singleton VM controller'
else
    fail 'image ships the singleton VM controller'
fi
has "$CONTAINERFILE" 'virt-viewer' 'image wires explicit recovery viewer'
has "$PROVISION" 'Start-TiloFocusedApp.ps1' 'guest tools carry the focused-app helper'
has "$PROVISION" 'Start-TiloInstaller.ps1' 'guest tools carry the managed installer helper'
has "$PROVISION" 'Start-TiloUninstaller.ps1' 'guest tools carry the typed uninstaller helper'
has "$PROVISION" 'Tilo-ProcessJob.ps1' 'guest tools carry the shared native process-job helper'
has "$PROVISION" 'Start-TiloProcessJobBroker.ps1' 'guest tools carry the typed UAC process-job broker'
has "$INSTALL" "Join-Path \$focusRoot 'Tilo-ProcessJob.ps1'" 'guest install places the job helper beside focused launches'
has "$INSTALL" "Join-Path \$installerRoot 'Tilo-ProcessJob.ps1'" 'guest install places the job helper in the protected installer root'
has "$BRIDGE" "Join-Path \$operationDirectory 'Start-TiloProcessJobBroker.ps1'" 'installer operation receives its fixed local UAC broker'
has "$FOCUSED" 'Stop-Process -Id' 'focused fallback suppresses Explorer while running'
has "$FOCUSED" 'Start-Process -FilePath "$env:WINDIR\explorer.exe"' 'focused fallback restores Explorer'
has "$FOCUSED" 'RunFocusedApplication' 'focused fallback waits through the shared process-tree job'
lacks "$FOCUSED" 'Wait-Process' 'focused fallback never waits only for the bootstrap PID'
has "$PROCESS_JOB" 'CreateSuspended = 0x00000004' 'managed process starts with its primary thread suspended'
has "$PROCESS_JOB" 'AssignProcessToJobObject(job, process.hProcess)' 'suspended process is assigned to the Tilo job before execution'
has "$PROCESS_JOB" 'ResumeThread(process.hThread)' 'managed process resumes only after job assignment'
has "$PROCESS_JOB" 'JobObjectLimitKillOnJobClose = 0x00002000' 'abandoned helpers tear down their contained process tree'
has "$PROCESS_JOB" 'accounting.ActiveProcesses == 0' 'Explorer restoration waits for the whole job to become empty'
has "$PROCESS_JOB" 'The process is already in an incompatible parent job.' 'incompatible nested jobs fail closed before resume'
lacks "$PROCESS_JOB" 'const uint JobObjectLimitBreakaway' 'Tilo does not permit explicit job breakaway'
has "$PROCESS_JOB" 'CommandLineToArgvW(string commandLine' 'EXE uninstall discovery uses the native Windows argv parser'
has "$PROCESS_JOB" 'MaxUninstallArguments = 32' 'EXE uninstall argument count is bounded'
has "$PROCESS_JOB" 'MaxUninstallArgumentChars = 4096' 'each EXE uninstall argument is bounded'
has "$PROCESS_JOB" 'MaxUninstallCommandChars = 16384' 'aggregate EXE uninstall arguments are bounded'
lacks "$PROCESS_JOB" 'ShellExecuteEx' 'native process helper never launches the vendor command through ShellExecute'
has "$PROCESS_JOB" '[IO.FileAttributes]::ReparsePoint' 'EXE uninstall paths reject reparse points'
has "$PROCESS_JOB" 'Tilo executable uninstall path has an untrusted owner' 'EXE uninstall paths require a trusted owner'
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
has "$INSTALLER" '-Verb RunAs' 'requireAdministrator EXEs show standard Windows UAC for the fixed Tilo broker'
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
has "$BRIDGE" '[Tilo.WindowsGuest.ProcessJob]::ParseUninstallCommandLine($uninstallString)' 'registry EXE uninstall commands are parsed but never executed raw'
has "$BRIDGE" 'Get-TiloValidatedExecutableUninstallContract' 'EXE uninstall discovery and persistence enforce the bound contract'
has "$BRIDGE" 'executableSha256 = $validatedUninstaller.executableSha256' 'administrator uninstall records persist the exact EXE digest'
has "$BRIDGE" "operationKind = 'uninstallation'" 'uninstall jobs retain a typed operation discriminator'
has "$UNINSTALLER" 'RunMsiUninstaller' 'uninstaller accepts only a parsed MSI product code'
has "$UNINSTALLER" 'Get-TiloValidatedExecutableUninstallContract $job' 'EXE uninstaller revalidates root, ACL, path, argv, and digest at execution'
has "$UNINSTALLER" 'RunExecutableUninstaller' 'EXE uninstaller uses the direct typed process-job argv method'
has "$UNINSTALLER" '-Verb RunAs' 'requireAdministrator EXE uninstallers use the fixed visible UAC broker'
has "$PROCESS_JOB_BROKER" "[string]\$job.operationKind -eq 'uninstallation'" 'elevated broker accepts only typed EXE uninstall jobs'
has "$PROCESS_JOB_BROKER" 'Get-TiloValidatedExecutableUninstallContract $job' 'elevated broker repeats EXE binding and digest validation after UAC'
has "$PROCESS_JOB_BROKER" 'RunExecutableUninstaller' 'elevated broker invokes the EXE uninstaller without a shell'
lacks "$UNINSTALLER" "-ArgumentList @('/x'" 'uninstaller cannot bypass its contained typed method'
has "$UNINSTALLER" 'Where-Object { $_.SessionId -eq $sessionId }' 'uninstaller hides Explorer only in its managed session'
lacks "$UNINSTALLER" 'UninstallString' 'registry uninstall command strings are never executed'
lacks "$UNINSTALLER" 'Invoke-Expression' 'uninstaller has no free-form command evaluator'
has "$COMPAT_DOC" 'The raw `UninstallString` is neither stored nor run.' 'EXE uninstall acceptance and raw-command rejection are documented'

VM_ROOT="$(cd "$OS_ROOT/../crates/windows/windows-vm" 2>/dev/null && pwd)"
REMOTE_APP="$VM_ROOT/src/remote_app.rs"
COMMAND_RUNNER="$VM_ROOT/src/command.rs"
PRESENTATION="$VM_ROOT/src/presentation.rs"
LIBVIRT="$VM_ROOT/src/libvirt.rs"
SESSION_WIRING="$VM_ROOT/../../../crates/session/shell/src/shell/windows_compat.rs"
need "$REMOTE_APP"
need "$PRESENTATION"
need "$LIBVIRT"
need "$COMMAND_RUNNER"
need "$SESSION_WIRING"
has "$REMOTE_APP" 'OsString::from("xfreerdp")' 'RAIL uses the upstream FreeRDP client'
lacks "$REMOTE_APP" 'binary: OsString::from("wlfreerdp")' 'RAIL does not assume a nonexistent Wayland wrapper'
has "$REMOTE_APP" 'AppFocusedSpiceLauncher' 'SPICE is an explicit app-focused fallback'
has "$REMOTE_APP" 'qemu:///system' 'SPICE attaches through local libvirt only'
has "$COMMAND_RUNNER" 'bounded_command::run_prepared(' 'VM and RemoteApp helpers have bounded execution'
lacks "$COMMAND_RUNNER" 'wait_with_output()' 'VM broker cannot allocate unbounded helper output'
has "$LIBVIRT" 'Duration::from_secs(300)' 'libvirt control operations have a short hard deadline'
has "$REMOTE_APP" '/clipboard:direction-to:all,files-to:off' 'clipboard needs an explicit grant without ambient file exposure'
has "$REMOTE_APP" '"-home-drive"' 'RAIL never redirects the owner home directory'
has "$REMOTE_APP" '/cert:fingerprint:sha256:' 'RAIL pins the authenticated guest certificate'
has "$REMOTE_APP" '--auto-resize=always' 'focused SPICE follows Tilo tile resizing when its guest agent is present'
has "$PRESENTATION" 'ApplicationPresentation::AppFocusedSpice' 'presentation fallback is typed'
lacks "$PRESENTATION" 'ApplicationPresentation::Recovery' 'recovery is not an application route'
has "$LIBVIRT" '/var/lib/tilo/windows/install-media.iso' 'prepared Windows media is attached read-only'
has "$LIBVIRT" '/var/lib/tilo/windows/tilo-guest-tools.iso' 'guest tools are attached read-only'
has "$LIBVIRT" 'retire_installation_media' 'authenticated enrollment retires setup media'
has "$LIBVIRT" "<clipboard copypaste='no'/>" 'SPICE clipboard stays disabled independently of guest state'
has "$LIBVIRT" "<filetransfer enable='no'/>" 'SPICE file transfer stays disabled independently of guest state'
has "$LIBVIRT" "name='com.redhat.spice.0'" 'SPICE exposes the bounded resize/cursor agent channel'
has "$LIBVIRT" '"--live",' 'setup media is removed from the running guest'
has "$LIBVIRT" '"--config",' 'setup media is removed from persistent configuration'
has "$SESSION_WIRING" 'LiveVmDelegate::with_shared_session_tracker' 'session composes the live singleton VM delegate'
has "$SESSION_WIRING" 'HttpsGuestBridge::new' 'session uses the authenticated HTTPS guest bridge'
lacks "$SESSION_WIRING" '        UnavailableVmDelegate,' 'session does not hardcode VM unavailability'

exit "$fails"
