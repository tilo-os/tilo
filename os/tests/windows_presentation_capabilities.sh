#!/usr/bin/env bash
# Static integration contracts for RAIL, SPICE guest tools, and GPU probing.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OS_ROOT="${OS_ROOT:-$(cd "$HERE/.." && pwd)}"
REPO_ROOT="$(cd "$OS_ROOT/.." && pwd)"
CONTAINERFILE="$OS_ROOT/Containerfile"
INSTALL="$OS_ROOT/usr/share/tilo/windows/guest/Install-TiloRemoteApp.ps1"
REMOTE_APP="$REPO_ROOT/crates/windows/windows-vm/src/remote_app.rs"
LIBVIRT="$REPO_ROOT/crates/windows/windows-vm/src/libvirt.rs"
GRAPHICS="$REPO_ROOT/crates/windows/windows-vm/src/graphics.rs"
CONTROL="$REPO_ROOT/crates/windows/windows-vm/src/bin/tilo-windows-vm-control.rs"
OWNER="$OS_ROOT/usr/libexec/tilo-windows-owner"
POLICY="$OS_ROOT/usr/share/polkit-1/actions/org.tilo.windows.policy"

fails=0
fail() { printf 'FAIL  %s\n' "$*" >&2; fails=$((fails + 1)); }
ok() { printf 'ok    %s\n' "$*"; }
has() { grep -Fq -- "$2" "$1" && ok "$3" || fail "$3"; }
lacks() { grep -Fq -- "$2" "$1" && fail "$3" || ok "$3"; }

for package in freerdp xorg-x11-server-Xwayland virt-viewer; do
    has "$CONTAINERFILE" "    $package " "image snapshot includes $package"
done
# The side build reaches the image through image-tool-stages, which gathers the side builds.
if grep -Fq 'COPY --from=xwayland-satellite-builder /out/ /xwayland-satellite/' "$CONTAINERFILE" \
   && grep -Fq 'COPY --from=image-tool-stages /xwayland-satellite/usr/bin/xwayland-satellite /usr/bin/xwayland-satellite' "$CONTAINERFILE"; then
    ok 'image includes the patched xwayland-satellite'
else
    fail 'image includes the patched xwayland-satellite'
fi
has "$CONTAINERFILE" 'COPY --from=virtio-win-media /out/virtio-win.iso /tree/usr/share/virtio-win/virtio-win.iso' 'Windows compatibility sysext contains authenticated virtio-win media'

has "$REMOTE_APP" '/app:program:||{}' 'RemoteApp uses the required alias marker'
has "$REMOTE_APP" '+auto-reconnect' 'RAIL reconnects a transient local interruption'
has "$REMOTE_APP" '/auto-reconnect-max-retries:3' 'RAIL reconnect is bounded'
lacks "$REMOTE_APP" '.arg("/dynamic-resolution")' 'desktop dynamic-resolution flag is absent from RAIL'
lacks "$REMOTE_APP" '.arg("+window-drag")' 'desktop window-drag flag is absent from RAIL'

has "$INSTALL" "DriveType = 5" 'guest setup searches only attached CD media'
has "$INSTALL" 'virtio-win-guest-tools.exe' 'guest setup uses Fedora virtio-win media'
has "$INSTALL" 'Get-FileHash -LiteralPath $guestTools -Algorithm SHA256' 'guest setup authenticates the packaged installer'
has "$INSTALL" 'Get-AuthenticodeSignature -LiteralPath $guestTools' 'guest setup verifies the Windows publisher signature'
has "$INSTALL" '[System.Management.Automation.SignatureStatus]::Valid' 'guest setup requires a trusted Authenticode chain'
has "$OS_ROOT/usr/libexec/tilo-windows-provision" 'virtio-win-guest-tools.sha256' 'host pins guest tools to its RPM payload'
has "$INSTALL" "Get-Service -Name 'vdservice'" 'guest setup verifies Windows SPICE vdagent'
has "$INSTALL" 'SkipSpiceGuestTools' 'repair flow can explicitly skip guest integration'
lacks "$INSTALL" 'Invoke-WebRequest' 'guest setup never downloads presentation tools'

has "$LIBVIRT" "<controller type='virtio-serial' index='0'/>" 'domain declares the vdagent transport controller'
has "$LIBVIRT" "name='com.redhat.spice.0'" 'domain declares the vdagent spicevmc channel'
has "$LIBVIRT" "<acceleration accel3d='no'/>" 'Windows fallback does not claim virgl 3D'
has "$LIBVIRT" "<clipboard copypaste='no'/>" 'vdagent cannot enable SPICE clipboard'
has "$LIBVIRT" "<filetransfer enable='no'/>" 'vdagent cannot enable SPICE file transfer'

has "$CONTROL" 'graphics-capabilities' 'controller exposes a pre-provisioning GPU probe'
has "$GRAPHICS" 'sriov_totalvfs' 'probe uses the kernel SR-IOV capability ABI'
has "$GRAPHICS" 'mdev_supported_types' 'probe uses the kernel mediated-device ABI'
has "$GRAPHICS" 'vfio-pci' 'probe distinguishes VFIO-ready partitions'
has "$GRAPHICS" 'automatic_assignment: false' 'probe never equates detection with assignment'
has "$CONTROL" 'sriov_assignment(identifier)' 'attach revalidates the selected VF against the live probe'
has "$CONTROL" 'mediated_assignment(identifier)' 'attach revalidates the selected mdev against the live probe'
has "$LIBVIRT" 'GraphicsRequiresShutoff' 'partition changes require a stopped singleton'
has "$LIBVIRT" '"--config"' 'partition assignment is stable across guest restarts'
has "$OWNER" 'graphics-attach {sriov-vf|mdev}' 'owner helper exposes only typed partition kinds'
has "$POLICY" 'org.tilo.windows.graphics-attach' 'partition attachment requires administrator authorization'

exit "$fails"
