#!/usr/bin/env bash
# Fail the image when systemd would have to break an ordering cycle at boot.
#
# systemd resolves a cycle by deleting one job in it and carrying on, so the
# only symptom is whichever unit it chose not running: once it was
# systemd-tmpfiles-setup, leaving /var/tmp 0755, no root SSH key and /tmp/.X11-unix
# owned by the greeter. `systemd-analyze verify` builds the same start
# transaction the boot does and prints "Found ordering cycle" for each one.
#
# A negative control runs first: a verifier that cannot see a deliberate cycle
# here would pass every image, so it fails the build instead.
set -euo pipefail

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

cat > "$scratch/tilo-ordering-probe-a.service" <<'UNIT'
[Unit]
DefaultDependencies=no
Wants=tilo-ordering-probe-b.service
After=tilo-ordering-probe-b.service
[Service]
Type=oneshot
ExecStart=/usr/bin/true
UNIT
cat > "$scratch/tilo-ordering-probe-b.service" <<'UNIT'
[Unit]
DefaultDependencies=no
After=tilo-ordering-probe-a.service
[Service]
Type=oneshot
ExecStart=/usr/bin/true
UNIT
if ! SYSTEMD_LOG_LEVEL=info systemd-analyze verify --man=no \
    "$scratch/tilo-ordering-probe-a.service" 2>&1 | grep -q 'Found ordering cycle'; then
  echo "verify-boot-ordering: systemd-analyze verify did not report a deliberate cycle;" >&2
  echo "it cannot check this image's boot ordering" >&2
  exit 1
fi

found=0
for target in default.target sysinit.target; do
  report=$(SYSTEMD_LOG_LEVEL=info systemd-analyze verify --man=no "$target" 2>&1 || true)
  if grep -E 'Found ordering cycle|deleted to break ordering cycle' <<<"$report" >&2; then
    found=1
  fi
done
if [ "$found" -ne 0 ]; then
  echo "verify-boot-ordering: the boot transaction has an ordering cycle (above)" >&2
  exit 1
fi
echo "verify-boot-ordering: default.target and sysinit.target start without an ordering cycle"
