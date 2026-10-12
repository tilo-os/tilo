# shellcheck shell=bash
# The --unlock phase of os/dev/test-nearby-pairing.sh, sourced once B has
# joined A's identity and both are signed in: B's encrypted root opens at boot
# with A's consent (docs/device-reach.md, "Unlocking it after a reboot"). It
# uses that script's vm_a/vm_b/sudo_b/die/note/session_log, $out, $run_b and
# $TILO_USER.
# A created the identity, so it holds the person's root and can release.
#
#   8. B's root moves onto a LUKS2 volume (online, with btrfs replace) whose
#      passphrase the harness knows, and `tilo-disk-encrypt enrol --sibling`
#      adds the sibling slot with B's segment connection as its unlock
#      network.
#   9. B reboots; its initramfs asks A, A approves through `device.unlock`,
#      and B boots without a passphrase.
#  10. Each of these leaves B at the passphrase prompt, with the attempt in
#      A's history (`device unlock:` journal lines); the harness then types
#      the passphrase:
#        deny       A denies B's request.
#        replay     the frames B sent in step 9 are sent to A again.
#        uncertified  B's token carries a boot key A's identity never
#                   certified.
#        revoked    A removes B from the fleet.

vm_a ssh 'command -v tilo-control >/dev/null' \
  || die "--unlock drives A through tilo-control, which only the dev disk carries"
control_a() {
  vm_a ssh "tilo-control --socket /run/user/\$(id -u)/tilo-session/test-control.sock $(printf '%q ' "$@")"
}
unlock_pass=tilo-unlock-e2e
b_root_part=""

# The `device unlock:` lines in A's history with outcome $1 (and reason $2).
a_history() {
  session_log vm_a | grep -F 'device unlock:' | grep -- "outcome=$1" | grep -c -- "${2:-}" || true
}
await_history() {
  local outcome=$1 before=$2 label=$3 reason=${4:-}
  for _ in $(seq 1 60); do
    [ "$(a_history "$outcome" "$reason")" -gt "$before" ] && return 0
    sleep 2
  done
  session_log vm_a | grep -F 'device unlock:' > "$out/unlock-$label-a-history.txt" || true
  die "A's history has no new outcome=$outcome for the $label case"
}
# The six-digit code of B's pending request, once A lists it.
await_pending() {
  local label=$1 code
  for _ in $(seq 1 150); do
    control_a actions enumerate > "$out/unlock-$label-actions.txt" 2>&1 || true
    code="$(grep -oE 'device\.unlock\.approve\.[0-9]{6}' "$out/unlock-$label-actions.txt" | head -1)"
    if [ -n "$code" ]; then
      printf '%s\n' "${code##*.}"
      return 0
    fi
    sleep 2
  done
  die "A never listed B's unlock request ($label)"
}
reboot_b() {
  sudo_b 'systemd-run --on-active=2 --unit=tilo-unlock-test-reboot systemctl reboot' >/dev/null \
    || die "B could not schedule its reboot"
  for _ in $(seq 1 60); do
    vm_b ssh true >/dev/null 2>&1 || return 0
    sleep 1
  done
  die "B did not go down for its reboot"
}
await_ssh_b() {
  for _ in $(seq 1 "$2"); do
    vm_b ssh true >/dev/null 2>&1 && return 0
    sleep 2
  done
  die "B did not answer SSH $1"
}
# B is at the passphrase prompt: it stays unreachable, and the screen shows
# the prompt. Then type the passphrase and wait for B to come up.
at_prompt_then_type() {
  local label=$1
  for _ in $(seq 1 15); do
    ! vm_b ssh true >/dev/null 2>&1 || die "B booted without a passphrase in the $label case"
    sleep 2
  done
  vm_b shot "unlock-$label-prompt" >/dev/null 2>&1 \
    && cp -f "$run_b/unlock-$label-prompt.png" "$out/" 2>/dev/null || true
  for attempt in $(seq 1 6); do
    vm_b type "$unlock_pass" >/dev/null 2>&1
    vm_b keys ret >/dev/null 2>&1
    for _ in $(seq 1 15); do
      vm_b ssh true >/dev/null 2>&1 && break 2
      sleep 2
    done
    [ "$attempt" -lt 6 ] || die "B did not unlock with its passphrase after the $label case"
  done
  # The initramfs journal is part of this boot's: what the attempt saw.
  vm_b ssh 'journalctl -b --no-pager -o short-monotonic -t tilo-sibling-unlock' \
    > "$out/unlock-$label-b-initrd.txt" 2>&1 || true
  note "B stayed at the passphrase prompt ($label) and opened with the passphrase"
}

# --- 8. B's root on LUKS2 with the sibling slot ----------------------------------
# First boot grows the root partition over the whole disk it finds, so B's
# disk grows only now, while B runs, by as much again as it holds: the root
# then moves onto a new partition of at least its own size at the end. QEMU
# grows the overlay in place (it stays sparse, so only the data actually
# copied takes host space) and tells B's kernel the new capacity. The GPT
# still ends at the old size until its backup header moves. Every step
# records the geometry it saw and fails by name, so a disk without room says
# why.
size_before="$(qemu-img info -U --output=json "$run_b/disk.qcow2" | jq -r '."virtual-size"')"
[[ $size_before =~ ^[0-9]+$ ]] || die "B's overlay reports no size: $size_before"
grown=$((size_before * 2))
vm_b cmd "block_resize tilo-disk $grown" > "$out/unlock-block-resize.txt" 2>&1 \
  || die "QEMU could not grow B's disk: $(cat "$out/unlock-block-resize.txt")"
if grep -qi error "$out/unlock-block-resize.txt"; then
  die "QEMU could not grow B's disk: $(cat "$out/unlock-block-resize.txt")"
fi
sudo_b "set -eu
  tilo_root=7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab
  linux=0FC63DAF-8483-4772-8E79-3D69D8477DE4
  old=\$(lsblk -lnpo NAME,PARTTYPE | awk -v t=\$tilo_root 'tolower(\$2)==t {print \$1}')
  [ -b \"\$old\" ]
  disk=/dev/\$(lsblk -no PKNAME \"\$old\")
  geometry() {
    echo \"--- \$1\"
    echo \"\$disk: \$(blockdev --getsize64 \"\$disk\") bytes as the kernel sees it\"
    sfdisk --dump \"\$disk\"
    sfdisk --list-free \"\$disk\"
  }
  # The kernel learns the new capacity from the device asynchronously.
  for _ in \$(seq 1 30); do
    [ \"\$(blockdev --getsize64 \"\$disk\")\" -ge $grown ] && break
    sleep 1
  done
  geometry 'before moving the backup GPT header'
  size=\$(blockdev --getsize64 \"\$disk\")
  [ \"\$size\" -ge $grown ] || {
    echo \"nearby unlock: B's kernel sees \$size bytes on \$disk, not the grown overlay's $grown\" >&2
    exit 1
  }
  # The root on this disk is mounted, and sfdisk refuses to write a disk in
  # use unless it is told not to re-read it, as the append below is.
  sfdisk --relocate gpt-bak-std --no-reread --no-tell-kernel \"\$disk\" || {
    echo 'nearby unlock: sfdisk could not move the backup GPT header to the end of the grown disk' >&2
    exit 1
  }
  geometry 'after moving the backup GPT header'
  # The new partition holds the root and a LUKS2 header (16 MiB).
  wanted=\$((\$(cat /sys/class/block/\${old##*/}/size) + 32768))
  gap=\$(sfdisk --list-free \"\$disk\" \
    | awk '\$1 ~ /^[0-9]+\$/ && \$2 ~ /^[0-9]+\$/ && \$3 ~ /^[0-9]+\$/ {s = \$3} END {print s + 0}')
  [ \"\$gap\" -ge \"\$wanted\" ] || {
    echo \"nearby unlock: \$disk ends in \$gap free sectors; the encrypted root needs \$wanted\" >&2
    exit 1
  }
  printf ',,%s\n' \"\$linux\" | sfdisk --append --no-reread --no-tell-kernel \"\$disk\" >/dev/null
  partx -a \"\$disk\" 2>/dev/null || partx -u \"\$disk\"
  udevadm settle
  new=\$(lsblk -lnpo NAME,TYPE \"\$disk\" | awk '\$2==\"part\" {p=\$1} END {print p}')
  [ \"\$new\" != \"\$old\" ] && [ -b \"\$new\" ]
  install -d -m 0700 /root/unlock-e2e
  printf '%s' '$unlock_pass' > /root/unlock-e2e/pass
  cryptsetup luksFormat --type luks2 --batch-mode --key-file /root/unlock-e2e/pass \"\$new\"
  cryptsetup open --key-file /root/unlock-e2e/pass \"\$new\" unlock-e2e-root
  mnt=\$(findmnt -rno TARGET -S \"\$old\" | head -1)
  [ -n \"\$mnt\" ]
  devid=\$(btrfs filesystem show \"\$mnt\" | awk -v d=\"\$old\" '\$NF==d {print \$2}')
  btrfs replace start -B -f \"\$devid\" /dev/mapper/unlock-e2e-root \"\$mnt\"
  wipefs -a \"\$old\" >/dev/null
  sfdisk --no-reread --no-tell-kernel --part-type \"\$disk\" \"\${old##*[!0-9]}\" \"\$linux\" >/dev/null
  sfdisk --no-reread --no-tell-kernel --part-type \"\$disk\" \"\${new##*[!0-9]}\" \"\$tilo_root\" >/dev/null
  partx -u \"\$disk\" 2>/dev/null || true
  printf '%s\n' \"\$new\" > /root/unlock-e2e/part
" > "$out/unlock-prepare.txt" 2>&1 || {
  cat "$out/unlock-prepare.txt" >&2
  die "B's root could not move onto LUKS2: $(tail -n 1 "$out/unlock-prepare.txt")"
}
b_root_part="$(sudo_b 'cat /root/unlock-e2e/part')"
[[ $b_root_part == /dev/* ]] || die "B's new root partition is unknown: $b_root_part"
note "B's root is on LUKS2 at $b_root_part"

sudo_b "/usr/libexec/tilo-disk-encrypt enrol $b_root_part --sibling --key-file /root/unlock-e2e/pass \
    --network tilo-nearby-lan --session-uid \$(id -u $TILO_USER)" \
  > "$out/unlock-enrol.txt" 2>&1 || { cat "$out/unlock-enrol.txt" >&2; die "the sibling slot could not be enrolled"; }
sudo_b "cryptsetup luksDump $b_root_part" > "$out/unlock-luksdump.txt"
grep -q 'tilo-sibling-unlock' "$out/unlock-luksdump.txt" || die "B's header carries no sibling-unlock token"
sudo_b "tilo-sibling-unlock status $b_root_part" > "$out/unlock-status.json"
if vm_b ssh 'test -e /dev/tpmrm0'; then sealing=tpm2; else sealing=cleartext; fi
grep -q "\"sealing\": \"$sealing\"" "$out/unlock-status.json" \
  || die "B's material is not sealed as $sealing: $(cat "$out/unlock-status.json")"
note "B enrolled the sibling slot"

# --- 9. approve --------------------------------------------------------------------
approved_before="$(a_history approved)"
reboot_b
code="$(await_pending approve)"
note "A lists B's unlock request with code $code"
control_a actions invoke continuity.devices "device.unlock.approve.$code" --args-json '{"confirm":true}' \
  > "$out/unlock-approve.txt" 2>&1 || { cat "$out/unlock-approve.txt" >&2; die "A could not approve"; }
await_ssh_b "after A approved its unlock" 120
await_history approved "$approved_before" approve
vm_b ssh 'journalctl -b --no-pager -o short-monotonic -t tilo-sibling-unlock' > "$out/unlock-approve-b-initrd.txt" 2>&1 || true
grep -q 'unlocked with the consent' "$out/unlock-approve-b-initrd.txt" \
  || die "B came up, but not through the sibling unlock: $(cat "$out/unlock-approve-b-initrd.txt")"
sudo_b 'cat /run/tilo-sibling-unlock/exchange.json' > "$out/unlock-approved-exchange.json" \
  || die "B kept no transcript of the approved exchange"
vm_b shot unlock-approved >/dev/null 2>&1 && cp -f "$run_b/unlock-approved.png" "$out/" 2>/dev/null || true
note "B booted without a passphrase after A approved"

# --- 10a. deny ---------------------------------------------------------------------
denied_before="$(a_history denied)"
reboot_b
code="$(await_pending deny)"
control_a actions invoke continuity.devices "device.unlock.deny.$code" \
  > "$out/unlock-deny.txt" 2>&1 || { cat "$out/unlock-deny.txt" >&2; die "A could not deny"; }
await_history denied "$denied_before" deny
# --- 10b. replay, while B sits at the prompt of the denied boot ---------------------
refused_before="$(a_history refused)"
vm_a ssh 'cat > /tmp/unlock-replay.json' < "$out/unlock-approved-exchange.json"
vm_a ssh 'tilo-sibling-unlock replay /tmp/unlock-replay.json' > "$out/unlock-replay.txt" 2>&1 || true
grep -qE 'refused: (unknown or reused challenge|this boot was already unlocked)' "$out/unlock-replay.txt" \
  || die "A did not refuse the replayed exchange: $(cat "$out/unlock-replay.txt")"
grep -q released "$out/unlock-replay.txt" && die "A released a half to a replayed exchange"
await_history refused "$refused_before" replay
at_prompt_then_type deny
grep -q 'the owner denied' "$out/unlock-deny-b-initrd.txt" || die "B's attempt did not see the denial"
note "a denied request and a replayed exchange leave B at the prompt"

# --- 10c. a boot key A's identity never certified ---------------------------------
# The token keeps its certificate's shape, but names a fresh boot key that the
# material now holds: the certificate's root signature no longer covers it.
sudo_b "set -eu
  part=$b_root_part
  cd /root/unlock-e2e
  id=\$(cryptsetup luksDump --dump-json-metadata \$part | python3 -c 'import json,sys; t=json.load(sys.stdin)[\"tokens\"]; print(next(k for k,v in t.items() if v[\"type\"]==\"tilo-sibling-unlock\"))')
  cryptsetup token export --token-id \$id \$part > certified.json
  openssl genpkey -algorithm ed25519 -outform DER -out boot.der
  openssl pkey -inform DER -in boot.der -pubout -outform DER -out boot.pub.der
  python3 - <<'PY'
import base64, json
token = json.load(open('certified.json'))
secret = open('boot.der', 'rb').read()[-32:]
public = open('boot.pub.der', 'rb').read()[-32:]
cert = bytearray(base64.b64decode(token['cert']))
assert cert[0] == 1, 'BootKeyCert version'
cert[65:97] = public  # version, user key, device key, then the boot key
token['cert'] = base64.b64encode(bytes(cert)).decode()
assert token['sealing'] == 'cleartext', 'the harness rewrites cleartext material only'
material = json.loads(base64.b64decode(token['material']))
material['boot_secret'] = secret.hex()
token['material'] = base64.b64encode(json.dumps(material).encode()).decode()
json.dump(token, open('uncertified.json', 'w'))
PY
  cryptsetup token remove --token-id \$id \$part
  cryptsetup token import \$part < uncertified.json
" > "$out/unlock-uncertified-setup.txt" 2>&1 || { cat "$out/unlock-uncertified-setup.txt" >&2; die "could not swap in an uncertified boot key"; }
refused_before="$(a_history refused reason=BadCertificate)"
reboot_b
await_history refused "$refused_before" uncertified reason=BadCertificate
at_prompt_then_type uncertified
grep -q 'boot key certificate is not valid' "$out/unlock-uncertified-b-initrd.txt" \
  || die "B's attempt did not see the refusal of its boot key"
sudo_b "set -eu
  part=$b_root_part
  id=\$(cryptsetup luksDump --dump-json-metadata \$part | python3 -c 'import json,sys; t=json.load(sys.stdin)[\"tokens\"]; print(next(k for k,v in t.items() if v[\"type\"]==\"tilo-sibling-unlock\"))')
  cryptsetup token remove --token-id \$id \$part
  cryptsetup token import \$part < /root/unlock-e2e/certified.json
" >/dev/null || die "could not restore B's certified token"
note "an uncertified boot key leaves B at the prompt"

# --- 10d. B removed from the fleet --------------------------------------------------
removed_before="$(a_history removed)"
# Action ids never carry device keys; B's removal is the one titled
# "Remove <B> from your devices".
control_a actions enumerate > "$out/unlock-remove-actions.txt" 2>&1 || true
remove_id="$(grep -oE 'device\.remove\.[A-Za-z0-9_.-]+' "$out/unlock-remove-actions.txt" | sort -u)"
[ "$(wc -l <<<"$remove_id")" = 1 ] && [ -n "$remove_id" ] \
  || die "A does not offer exactly one device to remove: $remove_id"
grep -F "$remove_id" "$out/unlock-remove-actions.txt" | grep -q 'from your devices' \
  || grep -A3 -F "$remove_id" "$out/unlock-remove-actions.txt" | grep -q 'from your devices' \
  || die "A's $remove_id is not the removal of a device"
control_a actions invoke continuity.devices "$remove_id" --args-json '{"confirm":true}' \
  > "$out/unlock-remove.txt" 2>&1 || { cat "$out/unlock-remove.txt" >&2; die "A could not remove B"; }
await_history removed "$removed_before" revoke
refused_before="$(a_history refused reason=Revoked)"
reboot_b
await_history refused "$refused_before" revoked reason=Revoked
at_prompt_then_type revoked
grep -q 'removed from the fleet' "$out/unlock-revoked-b-initrd.txt" \
  || die "B's attempt did not see that it was removed"
session_log vm_a | grep -F 'device unlock:' > "$out/unlock-a-history.txt" || true
note "a device removed from the fleet falls back to the passphrase"
