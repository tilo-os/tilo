#!/usr/bin/env bash
# Static contract for the preinstalled Windows compatibility stack.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
CF="$REPO/os/Containerfile"
ASSETS="$REPO/os/external-assets/github-assets.tsv"

fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

for package in wine wine-dxvk gamescope qemu-kvm libvirt-daemon-kvm swtpm edk2-ovmf freerdp xwayland-satellite virt-viewer xorriso; do
  grep -q "^    $package \\\\$" "$CF" \
    && ok "$package is part of the immutable image" \
    || bad "$package is absent from the Windows compatibility package transaction"
done

grep -q '^# qos-rpm-snapshot-package: virtio-win$' "$CF" \
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

grep -q 'COPY --from=windows-compat-artifacts /out/bin/umu-run /usr/libexec/qos-umu-run' "$CF" \
  && ok 'UMU launcher is installed under the qOS-owned execution path' \
  || bad 'UMU launcher is not copied into the final image'

grep -q 'COPY --from=windows-compat-artifacts /out/proton/ /usr/lib/qos/windows-runners/proton/' "$CF" \
  && ok 'pinned Proton runner is installed in the system runner store' \
  || bad 'pinned Proton runner is not copied into the final image'

grep -q 'umu-database-8297d420.csv' "$CF" \
  && grep -q $'^87271019d53a13299b495287ef45def9078a0d654e5f53f99f67b82fffa034a8\tumu-database-8297d420.csv\t.*/8297d420143bf722387b572fa1b81b5fa22746ac/umu-database.csv' "$ASSETS" \
  && grep -q 'COPY --from=windows-compat-artifacts /out/catalogues/ /usr/share/qos/windows/catalogues/' "$CF" \
  && ok 'UMU compatibility catalogue is commit-pinned, checksummed, and installed' \
  || bad 'UMU compatibility catalogue is not reproducibly installed'

grep -q 'bottles-programs-d1160b8.tar.gz' "$CF" \
  && grep -q $'^65cd49136305dccaa3d327cc948fa508f8e1d33de3e6c36ba0d9bf4646ca1727\tbottles-programs-d1160b8.tar.gz\t.*/d1160b816ca44a1cc803ab9a0050071517cc1960.tar.gz' "$ASSETS" \
  && grep -q '/out/catalogues/bottles-programs' "$CF" \
  && grep -q 'chmod -R a-w /out/catalogues/bottles-programs' "$CF" \
  && ok 'Bottles program metadata is commit-pinned, read-only, and imported as catalogue data' \
  || bad 'Bottles program metadata pin, read-only store, or catalogue path is missing'

exit "$fail"
