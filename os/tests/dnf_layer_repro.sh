#!/usr/bin/env bash
# An RPM installed at two different real times must leave identical package
# database bytes when the image build clock is fixed. DNF diagnostics are
# removed in the same layer without touching package metadata.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT

cc -std=c11 -Wall -Wextra -Werror -O2 -fPIC -shared \
  "$repo/os/scripts/build-clock.c" -o "$work/build-clock.so"

mkdir -p "$work/rpmbuild"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
cat >"$work/rpmbuild/SPECS/tilo-clock-probe.spec" <<'SPEC'
Name: tilo-clock-probe
Version: 1
Release: 1
Summary: Deterministic RPM install clock probe
License: MIT
BuildArch: noarch
Prefix: /usr/share/tilo-clock-probe
%description
Local build test only.
%install
mkdir -p %{buildroot}/usr/share/tilo-clock-probe
printf 'probe\n' > %{buildroot}/usr/share/tilo-clock-probe/value
%files
/usr/share/tilo-clock-probe/value
SPEC
rpmbuild -bb --quiet --define "_topdir $work/rpmbuild" \
  "$work/rpmbuild/SPECS/tilo-clock-probe.spec"
package="$work/rpmbuild/RPMS/noarch/tilo-clock-probe-1-1.noarch.rpm"
test -s "$package"

epoch=1790520508
for run in 1 2; do
  root="$work/root$run"
  mkdir -p "$root/rpmdb" "$root/usr/lib/sysimage/libdnf5" "$root/var/log"
  SOURCE_DATE_EPOCH="$epoch" LD_PRELOAD="$work/build-clock.so" \
    rpm --dbpath "$root/rpmdb" --initdb
  SOURCE_DATE_EPOCH="$epoch" LD_PRELOAD="$work/build-clock.so" \
    rpm --dbpath "$root/rpmdb" --prefix "$work/files" --nodeps -i "$package"
  times="$(rpm --dbpath "$root/rpmdb" -q \
    --qf '%{INSTALLTIME} %{INSTALLTID}' tilo-clock-probe)"
  test "$times" = "$epoch $epoch"
  rpm --dbpath "$root/rpmdb" --verifydb
  test "$(rpm --dbpath "$root/rpmdb" -qf \
    "$work/files/value")" = 'tilo-clock-probe-1-1.noarch'

  # Simulate DNF's real-time SQLite history and log. The cleanup may delete
  # neither rpmdb nor the libdnf5 package-state files used by later DNF calls.
  printf 'packages=tilo-clock-probe\n' >"$root/usr/lib/sysimage/libdnf5/packages.toml"
  printf 'real-time-%s\n' "$run" >"$root/usr/lib/sysimage/libdnf5/transaction_history.sqlite"
  printf 'real-time-%s\n' "$run" >"$root/var/log/dnf5.log"
  mkdir -p "$root/var/cache/ldconfig"
  printf 'inode-%s\n' "$run" >"$root/var/cache/ldconfig/aux-cache"
  sh "$repo/os/scripts/clean-dnf-layer-state.sh" "$root"
  test ! -e "$root/usr/lib/sysimage/libdnf5/transaction_history.sqlite"
  test ! -e "$root/var/log/dnf5.log"
  test ! -e "$root/var/cache/ldconfig/aux-cache"
  test "$(cat "$root/usr/lib/sysimage/libdnf5/packages.toml")" = \
    'packages=tilo-clock-probe'
  test -s "$root/rpmdb/rpmdb.sqlite"
  rm -rf -- "$work/files"
done

first="$(sha256sum "$work/root1/rpmdb/rpmdb.sqlite" | cut -d' ' -f1)"
second="$(sha256sum "$work/root2/rpmdb/rpmdb.sqlite" | cut -d' ' -f1)"
test "$first" = "$second" || {
  echo "RPM database differs with fixed build clock: $first != $second" >&2
  exit 1
}

# Without the preload, RPM must report the actual transaction time. This
# negative control proves that the comparison above exercises the clock shim.
mkdir -p "$work/unfixed/rpmdb"
rpm --dbpath "$work/unfixed/rpmdb" --initdb
rpm --dbpath "$work/unfixed/rpmdb" --prefix "$work/files" --nodeps -i "$package"
unfixed="$(rpm --dbpath "$work/unfixed/rpmdb" -q \
  --qf '%{INSTALLTIME} %{INSTALLTID}' tilo-clock-probe)"
test "$unfixed" != "$epoch $epoch"
test "$(sha256sum "$work/unfixed/rpmdb/rpmdb.sqlite" | cut -d' ' -f1)" != "$first"

# An operator can run the same probe against the exact Fedora 44/RPM 6 base
# already cached locally, without building or downloading a full OS image:
# TILO_RPM6_IMAGE=<local-image-id> bash os/tests/dnf_layer_repro.sh
if [ -n "${TILO_RPM6_IMAGE:-}" ]; then
  for run in 1 2; do
    podman run --rm --pull=never --network=none \
      -v "$work:/probe:Z" \
      -e SOURCE_DATE_EPOCH="$epoch" -e LD_PRELOAD=/probe/build-clock.so \
      --entrypoint /bin/bash "$TILO_RPM6_IMAGE" -euc '
        run="$1"
        rpm --dbpath "/probe/rpm6db$run" --initdb
        rpm --dbpath "/probe/rpm6db$run" --prefix /probe/files --nodeps \
          -i /probe/rpmbuild/RPMS/noarch/tilo-clock-probe-1-1.noarch.rpm
        test "$(rpm --dbpath "/probe/rpm6db$run" -q \
          --qf "%{INSTALLTIME} %{INSTALLTID}" tilo-clock-probe)" = \
          "$SOURCE_DATE_EPOCH $SOURCE_DATE_EPOCH"
        rpm --dbpath "/probe/rpm6db$run" --verifydb
        rm -rf /probe/files
      ' bash "$run"
  done
  first6="$(sha256sum "$work/rpm6db1/rpmdb.sqlite" | cut -d' ' -f1)"
  second6="$(sha256sum "$work/rpm6db2/rpmdb.sqlite" | cut -d' ' -f1)"
  test "$first6" = "$second6" || {
    echo "RPM 6 database differs with fixed build clock: $first6 != $second6" >&2
    exit 1
  }
fi

# The final image stage must use the wrapper before the first DNF command and
# invoke finalization after its last DNF command; each intermediate layer is
# otherwise already contaminated before final cleanup can run.
python3 - "$repo/os/Containerfile" <<'PY'
import re
import sys

source = open(sys.argv[1], encoding='utf-8').read()
rootfs = source.split('FROM ${BASE_REF} AS packages\n', 1)[1].split('FROM rootfs AS split\n', 1)[0]
wrapper = rootfs.index('COPY --chmod=0755 os/scripts/dnf-fixed-time.sh /usr/local/bin/dnf')
first_dnf = rootfs.index('dnf install -y --setopt=reposdir=/repo')
assert wrapper < first_dnf
assert rootfs.index('ARG FINALIZE_RPMDB=') < first_dnf
assert rootfs.index('COPY --chmod=0755 os/scripts/dnf-fixed-time.sh /usr/local/bin/rpm') < first_dnf
assert rootfs.index('/usr/local/bin/dnf /usr/local/bin/rpm /usr/libexec/tilo/build-clock.so') > first_dnf
assert 'clean-dnf-layer-state' in source.split('FROM ${BASE_REF} AS packages\n', 1)[1]
from pathlib import Path
assert '/usr/libexec/tilo/clean-dnf-layer-state' in (
    Path(sys.argv[1]).parent / 'scripts/finalize-rpmdb.sh').read_text(encoding='utf-8')
# A WAL-mode rpmdb cannot be opened on the read-only /usr of the booted image.
finalize = (Path(sys.argv[1]).parent / 'scripts/finalize-rpmdb.sh').read_text(encoding='utf-8')
assert 'PRAGMA journal_mode=DELETE' in finalize
assert finalize.index('PRAGMA journal_mode=DELETE') > finalize.rindex('rpm -qf')

# Each rootfs RUN that performs a DNF transaction must finalize before its
# layer snapshot. The separate uki-tools stage never enters the rootfs image.
instructions = re.split(r'(?m)^(?=[A-Z][A-Z-]* )', rootfs)
transactions = 0
for instruction in instructions:
    active = '\n'.join(line for line in instruction.splitlines()
                       if not line.lstrip().startswith('#'))
    if active.startswith('RUN ') and re.search(r'\bdnf (?:install|swap|--disablerepo)', active):
        transactions += 1
        assert '${FINALIZE_RPMDB}' in active, active[:120]
# Every rpm transaction rewrites the whole database, so each RUN that holds
# one ships a full rpmdb.sqlite in its layer (#4055: twenty layers carried
# 957 MB of them). The package transactions share one RUN; the kernel swap,
# which needs files copied after them, is the other.
assert transactions == 2, (
    f'{transactions} rootfs RUNs run dnf; each ships its own copy of the rpm database. '
    'Add a transaction to the package RUN in packages instead.')
PY
echo 'fixed DNF/RPM transaction time, package ownership and layer cleanup: ok'
