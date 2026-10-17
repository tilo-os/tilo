#!/usr/bin/env bash
# Static contracts for the narrow polkit/pkexec Windows owner backend.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OS_ROOT="${OS_ROOT:-$(cd "$HERE/.." && pwd)}"
HELPER="$OS_ROOT/usr/libexec/tilo-windows-owner"
VIEWER="$OS_ROOT/usr/libexec/tilo-windows-recovery-console"
POLICY="$OS_ROOT/usr/share/polkit-1/actions/org.tilo.windows.policy"
RULE="$OS_ROOT/etc/polkit-1/rules.d/49-tilo-windows.rules"
CONTAINERFILE="$OS_ROOT/Containerfile"
OWNER_STATUS="$OS_ROOT/../crates/windows/windows-vm/src/owner_status.rs"

fails=0
fail() { printf 'FAIL  %s\n' "$*" >&2; fails=$((fails + 1)); }
ok() { printf 'ok    %s\n' "$*"; }
has() { grep -Fq -- "$2" "$1" && ok "$3" || fail "$3"; }
lacks() { grep -Fq -- "$2" "$1" && fail "$3" || ok "$3"; }
need() { [ -r "$1" ] && ok "$(basename "$1") exists" || fail "missing $1"; }

for file in "$HELPER" "$VIEWER" "$POLICY" "$RULE" "$OWNER_STATUS"; do need "$file"; done
bash -n "$HELPER" && ok 'owner helper has valid shell syntax' || fail 'owner helper shell syntax'
bash -n "$VIEWER" && ok 'recovery viewer has valid shell syntax' || fail 'recovery viewer shell syntax'

has "$HELPER" 'stdin must be an inherited portal file descriptor' 'import contract names inherited portal stdin'
has "$HELPER" 'portal descriptor must name a regular file' 'import rejects pipes and devices'
has "$HELPER" 'MAX_ISO_BYTES=21474836480' 'import size is bounded before staging'
has "$HELPER" 'head -c "$((MAX_ISO_BYTES + 1))" <&0' 'import streams only the inherited descriptor with a hard cap'
has "$HELPER" 'isoinfo -d -i' 'ISO9660 media is parsed before provisioning'
has "$HELPER" 'udfinfo "$image"' 'UDF media is parsed before provisioning'
has "$HELPER" 'mv -- "$temporary" "$staged"' 'validated import is atomically staged'
has "$HELPER" 'prepare --iso "$staged"' 'owner backend reuses singleton provisioner'
lacks "$HELPER" 'prepare --iso "$2"' 'owner backend accepts no caller ISO path'
has "$HELPER" 'exec "$VM_CONTROL" owner-status' 'status is the controller'"'"'s unprivileged owner summary'
has "$OWNER_STATUS" 'phase: String' 'status command emits a typed phase'
has "$OWNER_STATUS" 'catalog_generation: u64' 'status command exposes catalogue generation'
has "$OWNER_STATUS" 'power_state: String' 'status command exposes authoritative domain power state'
has "$OWNER_STATUS" 'temporary_egress: Option<TemporaryEgress>' 'status command exposes temporary egress purpose and expiry'
has "$OWNER_STATUS" 'libc::O_NOFOLLOW' 'owner status summary is opened without following links'
lacks "$OWNER_STATUS" 'Command::new' 'owner status never probes libvirt through the controller'
has "$OWNER_STATUS" 'Failure::Missing' 'missing runtime summary leaves management status usable with unknown power'
lacks "$HELPER" 'python3' 'owner helper runs no Python'
has "$HELPER" 'egress-revoke' 'owner backend offers narrow early egress revocation'
has "$HELPER" 'exec "$VM_CONTROL" egress-revoke' 'owner revocation keeps opaque lease IDs out of UI'
has "$HELPER" 'recovery-start' 'owner backend has a narrow recovery-start action'
has "$HELPER" '"$VM_CONTROL" start' 'recovery start controls only the singleton'
has "$HELPER" 'PKEXEC_UID' 'recovery proof is bound to the polkit caller'
has "$HELPER" 'install -d -m 0700 -o root -g root "$RECOVERY_ROOT"' 'recovery proof directory is root-only'
has "$HELPER" 'mv -- "$token" "$consumed"' 'recovery proof has an atomic single-use transition'
has "$HELPER" 'recovery authorisation has already been consumed or expired' 'recovery replay is explicitly rejected'
has "$POLICY" 'org.tilo.windows.import-iso' 'polkit policy has ISO import action'
has "$POLICY" 'org.tilo.windows.recovery-start' 'polkit policy has recovery action'
has "$POLICY" 'org.tilo.windows.recovery-consume' 'polkit policy has recovery consumption action'
has "$POLICY" 'org.tilo.windows.egress-revoke' 'polkit policy has egress revocation action'
has "$POLICY" 'org.freedesktop.policykit.exec.argv1' 'polkit policy narrows helper argv1'
has "$RULE" 'subject.local && subject.active && subject.isInGroup("wheel")' 'polkit rule requires active local administrator'
has "$RULE" 'polkit.Result.AUTH_ADMIN_KEEP' 'polkit uses the standard administrator auth cache'
has "$POLICY" 'org.freedesktop.policykit.imply' 'recovery proof consumption reuses recovery authorization'
has "$VIEWER" 'pkexec /usr/libexec/tilo-windows-owner consume-recovery' 'viewer atomically consumes owner-issued authorization'
lacks "$VIEWER" 'tilo-windows-vm-control start' 'viewer cannot itself acquire VM-start authority'
has "$CONTAINERFILE" 'tilo-windows-owner' 'image installs owner backend'
has "$CONTAINERFILE" 'org.tilo.windows.policy' 'image installs Windows polkit policy'
has "$CONTAINERFILE" '49-tilo-windows.rules' 'image installs narrow Windows polkit rule'
has "$CONTAINERFILE" 'genisoimage' 'image includes ISO9660 validation tool'
has "$CONTAINERFILE" 'udftools' 'image includes UDF validation tool'

exit "$fails"
