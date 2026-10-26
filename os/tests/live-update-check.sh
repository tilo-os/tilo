#!/usr/bin/env bash
# A live update on a booted dev VM, as root: stage an image that differs from the booted one by
# PROBE_PATH, then apply it with `tilo-extension live:system`, which mounts the staged
# deployment's composefs image under the digest bootc reports (the kernel's own fs-verity check)
# and merges what changed over /usr. It must apply live, and its state must say so; an update that
# cannot apply live must say it failed and needs a restart.
#
#   live-update-check.sh MIRROR IMAGE PROBE_PATH PROBE_TEXT
#
# MIRROR (host:port/repository) serves IMAGE, an unsigned tag of the dev image repository this VM
# follows, which the policy is told to admit by that tag alone; it is configured as that
# repository's mirror, as a site mirror would be.
# Test tooling, not part of the image: os/dev/vm.sh with-tests copies it into the guest.
set -euo pipefail
mirror=${1:?usage: live-update-check.sh MIRROR IMAGE PROBE_PATH PROBE_TEXT}
image=${2:?}
probe=${3:?}
text=${4:?}
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

rc=0
/usr/libexec/tilo-extension live:system || rc=$?
state=$(/usr/libexec/tilo-extension live:status)
printf 'live:system exited %s; state: %s\n' "$rc" "$state"
flag() { grep -Eq "\"$1\": *$2" <<< "$state"; }
if ((rc == 0)) && flag applied '"system"' && flag failed false; then
  [[ $(cat "$probe" 2>/dev/null) == "$text" ]] || fail "the state says applied, but $probe is not the staged image's"
  printf 'ok    the staged image applied live, and its state records it\n'
  exit 0
fi
if flag failed true && flag reboot_required true; then
  fail 'it did not apply live; the state records failed and that a restart applies it'
fi
fail 'it did not apply live, and the state does not record failed and needs-restart'
