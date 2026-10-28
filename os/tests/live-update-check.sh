#!/usr/bin/env bash
# A live update on a booted dev VM, as root: stage an image that differs from the booted one by
# PROBE_PATH, then apply it with `tilo-extension live:system`, which mounts the staged
# deployment's composefs image under the digest bootc reports (the kernel's own fs-verity check)
# and merges what changed over /usr. It must apply live, and its state must say so; an update that
# cannot apply live must say it failed and needs a restart.
#
#   live-update-check.sh [--or-restart] MIRROR IMAGE PROBE_PATH PROBE_SHA256
#
# MIRROR (host:port/repository) serves IMAGE, an unsigned tag of the dev image repository this VM
# follows, which the policy is told to admit by that tag alone; it is configured as that
# repository's mirror, as a site mirror would be. PROBE_PATH is a file of IMAGE whose content has
# PROBE_SHA256 and differs from the booted image's. With --or-restart an update that records that
# it failed to apply live and that a restart applies it passes too, as does staging on an image
# that predates live updates.
# Test tooling, not part of the image: os/dev/vm.sh with-tests copies it into the guest.
set -euo pipefail
or_restart=false
if [[ ${1:-} == --or-restart ]]; then or_restart=true; shift; fi
mirror=${1:?usage: live-update-check.sh [--or-restart] MIRROR IMAGE PROBE_PATH PROBE_SHA256}
image=${2:?}
probe=${3:?}
sha256=${4:?}
repository=${image%:*}
fail() { printf 'FAIL  live update: %s\n' "$*" >&2; exit 1; }

cat > /etc/containers/registries.conf.d/90-live-update-check.conf <<CONF
[[registry]]
prefix = "$repository"
location = "$repository"
[[registry.mirror]]
location = "$mirror"
insecure = true
CONF
# The dev channel takes only images main signed; this test image is unsigned, so the policy
# admits exactly its tag, the most specific scope, and nothing else.
python3 - "$image" <<'PY'
import json, sys
path = '/etc/containers/policy.json'
policy = json.load(open(path))
policy['transports']['docker'][sys.argv[1]] = [{'type': 'insecureAcceptAnything'}]
json.dump(policy, open(path, 'w'), indent=2)
PY

# bootc's composefs backend mounts the ESP read-only and then cannot stage over it; writable
# until the next boot, as tilo-vm leaves it.
if findmnt -n -o FSTYPE,OPTIONS /boot 2>/dev/null | grep -q '^vfat *ro,'; then mount -o remount,rw /boot; fi
bootc switch "$image" || fail "bootc did not stage $image"
bootc status --format json | grep -q '"staged": *{' || fail 'bootc reports no staged deployment'

# An image older than live updates applies a staged one on restart only.
if ! /usr/libexec/tilo-extension live:status >/dev/null 2>&1; then
  [[ $or_restart == true ]] || fail 'this image cannot apply an update live (no live:status)'
  printf 'ok    this image applies updates on restart only (no live:system); a restart applies it\n'
  exit 0
fi
rc=0
/usr/libexec/tilo-extension live:system || rc=$?
state=$(/usr/libexec/tilo-extension live:status)
printf 'live:system exited %s; state: %s\n' "$rc" "$state"
flag() { grep -Eq "\"$1\": *$2" <<< "$state"; }
if ((rc == 0)) && flag applied '"system"' && flag failed false; then
  [[ $(sha256sum "$probe" 2>/dev/null | cut -d' ' -f1) == "$sha256" ]] || fail "the state says applied, but $probe is not the staged image's"
  printf 'ok    the staged image applied live, and its state records it\n'
  exit 0
fi
if flag failed true && flag reboot_required true; then
  if [[ $or_restart == true ]]; then
    printf 'ok    it did not apply live, and its state records that a restart applies it\n'
    exit 0
  fi
  fail 'it did not apply live; the state records failed and that a restart applies it'
fi
fail 'it did not apply live, and the state does not record failed and needs-restart'
