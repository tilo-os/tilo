#!/usr/bin/env bash
# Static contract for the preinstalled Windows compatibility stack.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
CF="$REPO/os/Containerfile"
ASSETS="$REPO/os/external-assets/github-assets.tsv"
KERNEL_CONFIG="$REPO/os/kernel/config-fragments/tilo.config"

fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

grep -Fqx 'CONFIG_NTSYNC=y' "$KERNEL_CONFIG" \
  && ok 'kernel-native ntsync accelerates current Wine/Proton synchronization' \
  || bad 'kernel-native ntsync is absent from the gaming kernel'
grep -Fq '"/dev/ntsync".to_string()' "$REPO/crates/packaging/launcher/src/run.rs" \
  && ok 'game sandboxes receive the kernel ntsync device' \
  || bad 'ntsync is hidden by the game sandbox device namespace'
grep -Fq '"--dev-bind".to_string()' "$REPO/crates/packaging/launcher/src/run.rs" \
  && ok 'native game launch fails visibly instead of silently losing ntsync' \
  || bad 'native game launch silently tolerates a missing ntsync device'
grep -Fq 'let ntsync = PathBuf::from("/dev/ntsync")' \
    "$REPO/crates/windows/compat-service/src/plan.rs" \
  && ok 'UMU/Proton capsules receive the kernel ntsync device' \
  || bad 'ntsync is hidden by the UMU/Proton capsule namespace'
grep -Fq 'bind(&mut argv, "--dev-bind", &ntsync, &ntsync)' \
    "$REPO/crates/windows/compat-service/src/plan.rs" \
  && ok 'Proton launch fails visibly instead of silently losing ntsync' \
  || bad 'Proton launch silently tolerates a missing ntsync device'
grep -Fq '("PROTON_USE_NTSYNC", "1".to_string())' \
    "$REPO/crates/windows/compat-service/src/plan.rs" \
  && ok 'pinned Proton runner is instructed to select ntsync' \
  || bad 'Proton receives ntsync but is not instructed to use it'

grep -Fq "x86_64) vm_firmware=edk2-ovmf; windows_packages='wine wine-dxvk' ;;" "$CF" \
  && ok 'x86-64 image includes the native Wine and DXVK packages' \
  || bad 'x86-64 image lacks its native Wine or DXVK package selection'

for package in qemu-kvm-core qemu-ui-spice-core qemu-char-spice qemu-audio-spice \
  libvirt-daemon-driver-qemu swtpm freerdp xwayland-satellite virt-viewer xorriso; do
  grep -q "^    $package \\\\$" "$CF" \
    && ok "$package is part of the immutable image" \
    || bad "$package is absent from the Windows compatibility package transaction"
done

for selection in 'x86_64) vm_firmware=edk2-ovmf;' 'aarch64) vm_firmware=edk2-aarch64;'; do
  grep -Fq "$selection" "$CF" \
    && ok "native VM firmware selection: $selection" \
    || bad "missing VM firmware selection: $selection"
done
grep -Fq '    "$vm_firmware" \' "$CF" \
  && ok 'selected native VM firmware is installed in the compatibility transaction' \
  || bad 'native VM firmware is absent from the compatibility transaction'

if grep -Eq '^    gamescope \\$' "$CF"; then
  bad 'unused gamescope is installed despite having no trusted launch path'
else
  ok 'unused gamescope is absent from the immutable image'
fi

grep -q '^# tilo-rpm-snapshot-package: virtio-win$' "$CF" \
  && grep -q 'COPY --from=virtio-win-media /out/virtio-win.iso /usr/share/virtio-win/virtio-win.iso' "$CF" \
  && grep -q 'COPY --from=virtio-win-media /out/installer/virtio-win-guest-tools.exe' "$CF" \
  && ok 'minimal authenticated virtio-win media is part of the immutable image' \
  || bad 'virtio-win media is not extracted from its pinned RPM'

for package in pipewire pipewire-pulseaudio pipewire-utils wireplumber; do
  grep -q "^    $package \\\\$" "$CF" \
    && ok "$package provides the brokered Windows audio stack" \
    || bad "$package is absent from the PipeWire/Pulse compatibility proxy"
done

grep -q 'umu-launcher-1.4.0.tar' "$CF" \
  && grep -q $'^138ce4b8843608a257d4bee88191ca78a989778bcefd8abb3c1d1aaac3ac6fb8\tumu-launcher-1.4.0.tar\t.*/releases/download/1.4.0/' "$ASSETS" \
  && ok 'UMU launcher artifact is versioned and checksum-pinned' \
  || bad 'UMU launcher is missing its pinned version or checksum'

grep -q 'UMU-Proton-10.0-4/proton' "$CF" \
  && grep -q $'^62e99e029a18fa313e6fa63d42390918101730a940e3491c54d9d58cab887c69\tumu-proton-10.0-4.tar.gz\t.*/releases/download/UMU-Proton-10.0-4/' "$ASSETS" \
  && ok 'default UMU-Proton artifact is versioned and checksum-pinned' \
  || bad 'default UMU-Proton is missing its pinned version or checksum'

grep -q 'COPY --from=windows-compat-artifacts /out/bin/umu-run /usr/libexec/tilo-umu-run' "$CF" \
  && ok 'UMU launcher is installed under the Tilo-owned execution path' \
  || bad 'UMU launcher is not copied into the final image'

grep -q 'COPY --from=windows-compat-artifacts /out/proton/ /usr/lib/tilo/windows-runners/proton/' "$CF" \
  && ok 'pinned Proton runner is installed in the system runner store' \
  || bad 'pinned Proton runner is not copied into the final image'

grep -q 'umu-database-8297d420.csv' "$CF" \
  && grep -q $'^87271019d53a13299b495287ef45def9078a0d654e5f53f99f67b82fffa034a8\tumu-database-8297d420.csv\t.*/8297d420143bf722387b572fa1b81b5fa22746ac/umu-database.csv' "$ASSETS" \
  && grep -q 'COPY --from=windows-compat-artifacts /out/catalogues/ /usr/share/tilo/windows/catalogues/' "$CF" \
  && ok 'UMU compatibility catalogue is commit-pinned, checksummed, and installed' \
  || bad 'UMU compatibility catalogue is not reproducibly installed'

grep -q 'bottles-programs-d1160b8.tar.gz' "$CF" \
  && grep -q $'^65cd49136305dccaa3d327cc948fa508f8e1d33de3e6c36ba0d9bf4646ca1727\tbottles-programs-d1160b8.tar.gz\t.*/d1160b816ca44a1cc803ab9a0050071517cc1960.tar.gz' "$ASSETS" \
  && grep -q '/out/catalogues/bottles-programs' "$CF" \
  && grep -q 'chmod -R a-w /out/catalogues/bottles-programs' "$CF" \
  && ok 'Bottles program metadata is commit-pinned, read-only, and imported as catalogue data' \
  || bad 'Bottles program metadata pin, read-only store, or catalogue path is missing'

exit "$fail"
