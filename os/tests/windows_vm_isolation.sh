#!/usr/bin/env bash
# Static contracts for the host/guest part of the Windows fallback. These are
# intentionally source-level: CI need not boot a licensed Windows Server just
# to discover that a package update opened a route or normalised a desktop.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OS_ROOT="${OS_ROOT:-$(cd "$HERE/.." && pwd)}"
NET="$OS_ROOT/usr/share/qos/windows/libvirt/qos-windows-hostonly.xml"
UNIT="$OS_ROOT/usr/lib/systemd/system/qos-windows-vm-network.service"
NETWORK_TOOL="$OS_ROOT/usr/libexec/qos-windows-vm-network"
PROVISION="$OS_ROOT/usr/libexec/qos-windows-provision"
RECOVERY="$OS_ROOT/usr/libexec/qos-windows-recovery-console"
VM_CONTROL_SOURCE="$OS_ROOT/../crates/system/windows-vm/src/bin/qos-windows-vm-control.rs"
INSTALL="$OS_ROOT/usr/share/qos/windows/guest/Install-qOSRemoteApp.ps1"
BRIDGE="$OS_ROOT/usr/share/qos/windows/guest/Start-qOSGuestBridge.ps1"
FOCUSED="$OS_ROOT/usr/share/qos/windows/guest/Start-qOSFocusedApp.ps1"
CONTAINERFILE="$OS_ROOT/Containerfile"

fails=0
fail() { printf 'FAIL  %s\n' "$*" >&2; fails=$((fails + 1)); }
ok() { printf 'ok    %s\n' "$*"; }
need() { [ -r "$1" ] && ok "$(basename "$1") exists" || fail "missing $1"; }
has() { grep -Fq -- "$2" "$1" && ok "$3" || fail "$3"; }
lacks() { grep -Fq -- "$2" "$1" && fail "$3" || ok "$3"; }

for file in "$NET" "$UNIT" "$NETWORK_TOOL" "$PROVISION" "$RECOVERY" "$VM_CONTROL_SOURCE" "$INSTALL" "$BRIDGE" "$FOCUSED"; do need "$file"; done

has "$NET" '<forward mode="none"/>' 'libvirt bridge has no forwarding'
has "$NET" '<qos:windows-hostonly ' 'network has qOS ownership marker'
lacks "$NET" "mode='nat'" 'network cannot silently become NAT'
lacks "$NET" '<route ' 'network has no guest route'
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
has "$RECOVERY" '/usr/libexec/qos-windows-vm-control start' 'recovery starts a powered-off singleton idempotently'
has "$RECOVERY" 'virt-viewer --connect qemu:///system --attach qos-windows-appliance' 'recovery is an explicit libvirt attachment'
has "$VM_CONTROL_SOURCE" 'LibvirtBackend::new' 'bootstrap reuses the typed libvirt backend'
has "$VM_CONTROL_SOURCE" 'backend.start(&config)' 'bootstrap defines and starts through one idempotent path'

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
has "$FOCUSED" 'Stop-Process -Id' 'focused fallback suppresses Explorer while running'
has "$FOCUSED" 'Start-Process -FilePath "$env:WINDIR\explorer.exe"' 'focused fallback restores Explorer'

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
