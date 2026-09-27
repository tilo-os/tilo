#!/usr/bin/env bash
# Prove bootc lint transient-state cleanup and the greetd portal mask tmpfiles
# declaration without assembling a full OS image (issue #71).
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cleaner="$repo/os/scripts/clean-bootc-transient-state.py"
tmpfiles="$repo/os/usr/lib/tmpfiles.d/tilo.conf"
containerfile="$repo/os/Containerfile"
fail=0

ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

if [ ! -f "$cleaner" ]; then
  bad "missing $cleaner"
  exit 1
fi
if [ ! -f "$tmpfiles" ]; then
  bad "missing $tmpfiles"
  exit 1
fi

portal_mask_line='L  /var/lib/greetd/.config/systemd/user/xdg-desktop-portal.service  -  -  -  -  /dev/null'
if grep -Fqx "$portal_mask_line" "$tmpfiles" \
   && grep -Eq '^d[[:space:]]+/var/lib/greetd[[:space:]]+0755[[:space:]]+greetd[[:space:]]+greetd' "$tmpfiles"; then
  ok 'greetd xdg-desktop-portal mask is declared for first-boot tmpfiles'
else
  bad 'greetd portal mask is not declared in tmpfiles.d/tilo.conf'
fi

if grep -Fq 'clean-bootc-transient-state.py' "$containerfile" \
   && grep -Fq 'python3 /usr/local/libexec/tilo-clean-bootc-transient-state.py' "$containerfile" \
   && awk '
        /tilo-clean-bootc-transient-state[.]py/ { seen_clean = 1 }
        seen_clean && /bootc container lint/ { found = 1 }
        END { exit found ? 0 : 1 }
      ' "$containerfile"; then
  ok 'rootfs Containerfile clears transient state immediately before bootc lint'
else
  bad 'rootfs Containerfile does not run transient-state cleanup before bootc lint'
fi

if grep -Fq "target=/usr/local/libexec/tilo-clean-bootc-transient-state.py,ro" "$containerfile" \
   && awk '
      /^RUN rm -f \/etc\/resolv.conf && printf / { placeholder = NR }
      /python3 \/usr\/local\/libexec\/tilo-clean-bootc-transient-state[.]py/ { clean = NR }
      /^COPY --from=runtime-resolver-link \/out\/ \/etc\/$/ { restored = NR }
      /^FROM rootfs AS split$/ { split_line = NR }
      END { exit (placeholder && clean && restored && split_line && placeholder < clean && clean < restored && restored < split_line) ? 0 : 1 }
   ' "$containerfile"; then
  ok 'build DNS mount is outside /run during cleanup; runtime resolver is restored before sealing'
else
  bad 'rootfs resolver ordering may pin /run during cleanup or alter the sealed image'
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
root="$work/rootfs"
mkdir -p \
  "$root/run/NetworkManager" \
  "$root/run/__pycache__" \
  "$root/run/cups/certs" \
  "$root/tmp/build-scratch" \
  "$root/var/log" \
  "$root/var/lib/dnf" \
  "$root/var/lib/authselect" \
  "$root/var/lib/greetd/.config/systemd/user" \
  "$root/var/cache/libX11/compose" \
  "$root/var/cache/libdnf5" \
  "$root/var/cache/libvirt" \
  "$root/var/cache/ldconfig" \
  "$root/var/cache/swcatalog/cache"

printf 'dnf log\n' >"$root/var/log/dnf5.log"
printf 'lock\n' >"$root/var/lib/dnf/system-repo.lock"
printf 'checksum\n' >"$root/var/lib/authselect/checksum"
printf 'aux\n' >"$root/var/cache/ldconfig/aux-cache"
printf 'catalog\n' >"$root/var/cache/swcatalog/cache/C-os-catalog.xb"
printf 'pyc\n' >"$root/run/__pycache__/hardware_profile.cpython-314.pyc"
ln -s /dev/null "$root/var/lib/greetd/.config/systemd/user/xdg-desktop-portal.service"

python3 "$cleaner" "$root"

still_present=()
for path in \
  run/NetworkManager \
  run/__pycache__ \
  run/cups \
  tmp/build-scratch \
  var/log/dnf5.log \
  var/lib/dnf/system-repo.lock \
  var/lib/authselect/checksum \
  var/cache/ldconfig/aux-cache \
  var/cache/swcatalog/cache/C-os-catalog.xb \
  var/cache/libX11 \
  var/cache/libdnf5 \
  var/cache/libvirt \
  var/lib/greetd/.config/systemd/user/xdg-desktop-portal.service
 do
  if [ -e "$root/$path" ] || [ -L "$root/$path" ]; then
    still_present+=("$path")
  fi
done

if [ "${#still_present[@]}" -eq 0 ] && [ -d "$root/var" ] && [ -d "$root/run" ]; then
  ok 'fixture rootfs loses bootc lint transient paths while keeping /var and /run'
else
  bad "cleanup left transient paths: ${still_present[*]:-none}; var/run missing?"
fi

# Build-time mounts under /run (the image build's /run mounts, the cleaner's
# own bind mount) are not image content and cannot be removed: the cleaner
# must leave them and still empty everything around them.
mounted="$work/mounted"
mkdir -p "$mounted/var/log" "$mounted/run/NetworkManager" "$mounted/run/cups/certs" "$mounted/run/stale"
printf 'log\n' >"$mounted/var/log/dnf5.log"
printf 'cert\n' >"$mounted/run/cups/certs/0"
: >"$mounted/run/cleaner.py"
if unshare -rm true 2>/dev/null; then
  if unshare -rm sh -c '
      mount -t tmpfs tmpfs "$1/run/NetworkManager" &&
      mkdir "$1/run/stale/mnt" && mount -t tmpfs tmpfs "$1/run/stale/mnt" &&
      mount --bind "$2" "$1/run/cleaner.py" &&
      python3 "$2" "$1" &&
      mountpoint -q "$1/run/NetworkManager" && mountpoint -q "$1/run/stale/mnt" &&
      [ ! -e "$1/run/cups" ] && [ ! -e "$1/var/log" ]
    ' sh "$mounted" "$cleaner"; then
    ok 'cleanup leaves build-time mount points under /run and empties the rest'
  else
    bad 'cleanup failed on, or removed, build-time mount points under /run'
  fi
else
  ok 'skipped mount-point check: no unprivileged mount namespace here'
fi

# Negative control: a check that cannot observe /var must fail, not skip.
if python3 "$cleaner" "$work/missing-root" >/dev/null 2>&1; then
  bad 'cleanup skipped a rootfs without /var'
else
  ok 'cleanup fails closed when /var is missing'
fi

exit "$fail"
