#!/usr/bin/env bash
# Static contract for the preinstalled Windows compatibility stack.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
CF="$REPO/os/Containerfile"

fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

for package in wine wine-dxvk gamescope qemu-kvm libvirt-daemon-kvm swtpm edk2-ovmf virtio-win freerdp; do
  grep -q "^    $package \\\\$" "$CF" \
    && ok "$package is part of the immutable image" \
    || bad "$package is absent from the Windows compatibility package transaction"
done

grep -q 'umu-launcher/releases/download/1.4.0/' "$CF" \
  && grep -q 'sha256:138ce4b8843608a257d4bee88191ca78a989778bcefd8abb3c1d1aaac3ac6fb8' "$CF" \
  && ok 'UMU launcher artifact is versioned and checksum-pinned' \
  || bad 'UMU launcher is missing its pinned version or checksum'

grep -q 'umu-proton/releases/download/UMU-Proton-10.0-4/' "$CF" \
  && grep -q 'sha256:62e99e029a18fa313e6fa63d42390918101730a940e3491c54d9d58cab887c69' "$CF" \
  && ok 'default UMU-Proton artifact is versioned and checksum-pinned' \
  || bad 'default UMU-Proton is missing its pinned version or checksum'

grep -q 'COPY --from=windows-compat-artifacts /out/bin/umu-run /usr/libexec/qos-umu-run' "$CF" \
  && ok 'UMU launcher is installed under the qOS-owned execution path' \
  || bad 'UMU launcher is not copied into the final image'

grep -q 'COPY --from=windows-compat-artifacts /out/proton/ /usr/lib/qos/windows-runners/proton/' "$CF" \
  && ok 'pinned Proton runner is installed in the system runner store' \
  || bad 'pinned Proton runner is not copied into the final image'

grep -q 'umu-database/8297d420143bf722387b572fa1b81b5fa22746ac/umu-database.csv' "$CF" \
  && grep -q 'sha256:87271019d53a13299b495287ef45def9078a0d654e5f53f99f67b82fffa034a8' "$CF" \
  && grep -q 'COPY --from=windows-compat-artifacts /out/catalogues/ /usr/share/qos/windows/catalogues/' "$CF" \
  && ok 'UMU compatibility catalogue is commit-pinned, checksummed, and installed' \
  || bad 'UMU compatibility catalogue is not reproducibly installed'

exit "$fail"
