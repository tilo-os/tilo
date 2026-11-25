# shellcheck shell=bash
# The --unlock phase of os/dev/test-nearby-pairing.sh, sourced once B has
# joined A's identity and both are signed in: B's encrypted root opens at boot
# with A's consent (docs/device-reach.md, "Unlocking it after a reboot"). It
# uses that script's vm_a/vm_b/sudo_b/die/note/session_log, $out, $run_b,
# $device and $TILO_USER.
# A created the identity, so it holds the person's root and can release; B
# holds none of it.
#
#   8. B's root moves onto a LUKS2 volume (online, with btrfs replace) whose
#      passphrase the harness knows, and `tilo-disk-encrypt enrol --sibling`
#      adds the sibling slot with B's segment connection as its unlock
#      network. B's daemon has the person's root certify the boot key, and B
#      keeps none of it, so A asks the person (`devices#certifyKey`) and the
#      harness answers that ask as the person.
#   9. B reboots; its initramfs asks A, the harness approves the sheet
#      (`device.unlock`), and B boots without a passphrase.
#  10. Each of these leaves B at the passphrase prompt, and the boot's own
#      account of the attempt says why; the harness then types the
#      passphrase:
#        deny       the harness declines B's request on A's sheet.
#        replay     the frames B sent in step 9 are sent to A again.
#        uncertified  B's token carries a boot key A's identity never
#                   certified.
#  10c2. B re-enrols with --unattended (where it has a TPM), and the next
#      reboot quotes its accepted PCR 4 + 7 chain so A releases the half with
#      no prompt: a server after a power loss.
#  10e. B takes the shape of an installed Tilo Server's first boot: nobody is
#      signed in, and the installer's kept key is the only authorisation it
#      has. The server's unlock unit runs as tilo-server-unlock.service does:
#      root adds the slot, the daemon's own account asks its daemon, the
#      person answers on A, and root writes the token, removes the install
#      key's slot and shreds it. B then reboots with nobody at its console and
#      A's sheet approves the boot.
#  10d. A revokes B's device key, and B's boot is refused as removed.

vm_a ssh 'command -v tilo-control >/dev/null' \
  || die "--unlock drives A through tilo-control, which only the dev disk carries"
# What waits for the person on A's sheet (`tilo-control questions`), the
# resource that answers or declines each, and the person's answer.
control_a() {
  vm_a ssh "tilo-control --socket /run/user/\$(id -u)/tilo-session/test-control.sock $(printf '%q ' "$@")"
}
unlock_pass=tilo-unlock-e2e
b_root_part=""

# The resource of A's pending question of `kind`, once its sheet shows; the
# listing it was read from is kept as evidence of what asked.
await_question() {
  local kind=$1 label=$2 listed="$out/unlock-$2-questions.json" resource=""
  for _ in $(seq 1 180); do
    control_a questions --json > "$listed" 2>&1 || true
    resource="$(jq -r --arg kind "$kind" \
      'map(select(.kind == $kind))[0].resource // empty' "$listed" 2>/dev/null || true)"
    if [ -n "$resource" ]; then
      printf '%s\n' "$resource"
      return 0
    fi
    sleep 1
  done
  die "A never showed the person a $kind question ($label); see unlock-$label-questions.json"
}
# The person's ask on A to certify a boot key of B's, once it waits for them: its invocation
# and the workspace it waits in, as A's workspace invocations list it. The listing is kept as
# evidence of what asked.
await_certify_ask() {
  local label=$1 listed="$out/unlock-$1-asked.json" found=""
  for _ in $(seq 1 180); do
    vm_a ssh 'tilo read invocations/workspace --json' > "$listed" 2>/dev/null || true
    found="$(jq -r '[.items // [] | .[]
        | select(.status == "waiting")
        | select((.asked.action // .action // "") | endswith("#certifyKey"))][0]
        | select(.) | "\(.id) \(.asked.workspace)"' "$listed" 2>/dev/null || true)"
    if [ -n "$found" ]; then
      printf '%s\n' "$found"
      return 0
    fi
    sleep 1
  done
  die "A never asked the person to certify B's boot key ($label); see unlock-$label-asked.json"
}
# The person answers that ask on A, as themselves: approve, or decline.
resolve_a() {
  local verdict=$1 label=$2 invocation workspace
  read -r invocation workspace <<<"$(await_certify_ask "$label")"
  note "A asks the person to certify B's boot key ($label): $invocation"
  vm_a ssh "tilo do --in-person 'invocations/workspace/$invocation#resolve' resolution=$verdict -w '$workspace'" \
      > "$out/unlock-$label-resolve.txt" 2>&1 \
    || { cat "$out/unlock-$label-resolve.txt" >&2; die "the person's ask on A could not be answered ($label)"; }
}
# The person answers A's sheet `resource`: allow it, or decline it. The
# resource is the question; `answer` and `cancel` are its own actions.
answer_a() {
  local verdict=$1 resource=$2 action=answer
  [ "$verdict" = allow ] || action=cancel
  { control_a raw "$(action_frame so.tilo.session "$resource" "$action")" \
      > "$out/unlock-$verdict-sheet.txt" 2>&1 \
      && action_completed "$out/unlock-$verdict-sheet.txt"; } \
    || { cat "$out/unlock-$verdict-sheet.txt" >&2; die "A's sheet could not be answered ($verdict)"; }
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
  vm_b ssh 'journalctl -b --no-pager -o short-monotonic -u 'tilo-unlock-sibling@*'' \
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

# B keeps none of the person's root, so its daemon asks A, where the person is asked: the
# setup waits for them in the background while the harness answers as the person.
( sudo_b "/usr/libexec/tilo-disk-encrypt enrol $b_root_part --sibling --key-file /root/unlock-e2e/pass \
    --network tilo-nearby-lan" \
    > "$out/unlock-enrol.txt" 2>&1 ) &
enrol_pid=$!
resolve_a approve enrol
wait "$enrol_pid" || { cat "$out/unlock-enrol.txt" >&2; die "the sibling slot could not be enrolled"; }
sudo_b "cryptsetup luksDump $b_root_part" > "$out/unlock-luksdump.txt"
grep -q 'tilo-sibling-unlock' "$out/unlock-luksdump.txt" || die "B's header carries no sibling-unlock token"
sudo_b "/usr/libexec/tilo/tilo-auth sibling status $b_root_part" > "$out/unlock-status.json"
if vm_b ssh 'test -e /dev/tpmrm0'; then sealing=tpm2; else sealing=cleartext; fi
grep -q "\"sealing\": \"$sealing\"" "$out/unlock-status.json" \
  || die "B's material is not sealed as $sealing: $(cat "$out/unlock-status.json")"
note "B enrolled the sibling slot"

# --- 9. approve --------------------------------------------------------------------
reboot_b
approve_ask="$(await_question network.device-unlock approve)"
note "A shows B's unlock request: $approve_ask"
answer_a allow "$approve_ask"
await_ssh_b "after A approved its unlock" 120
vm_b ssh 'journalctl -b --no-pager -o short-monotonic -u 'tilo-unlock-sibling@*'' > "$out/unlock-approve-b-initrd.txt" 2>&1 || true
grep -q 'unlocked with the consent' "$out/unlock-approve-b-initrd.txt" \
  || die "B came up, but not through the sibling unlock: $(cat "$out/unlock-approve-b-initrd.txt")"
sudo_b 'cat /run/tilo-sibling-unlock/exchange.json' > "$out/unlock-approved-exchange.json" \
  || die "B kept no transcript of the approved exchange"
vm_b shot unlock-approved >/dev/null 2>&1 && cp -f "$run_b/unlock-approved.png" "$out/" 2>/dev/null || true
note "B booted without a passphrase after A approved"

# --- 10a. deny ---------------------------------------------------------------------
reboot_b
deny_ask="$(await_question network.device-unlock deny)"
answer_a deny "$deny_ask"
# --- 10b. replay, while B sits at the prompt of the denied boot ---------------------
vm_a ssh 'cat > /tmp/unlock-replay.json' < "$out/unlock-approved-exchange.json"
vm_a ssh 'sudo /usr/libexec/tilo/tilo-auth sibling replay /tmp/unlock-replay.json' > "$out/unlock-replay.txt" 2>&1 || true
grep -qE 'refused: (unknown or reused challenge|this boot was already unlocked)' "$out/unlock-replay.txt" \
  || die "A did not refuse the replayed exchange: $(cat "$out/unlock-replay.txt")"
grep -q released "$out/unlock-replay.txt" && die "A released a half to a replayed exchange"
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
# A refuses the unknown boot key before asking anyone, so no sheet stands:
# the boot is left at its own prompt, saying why.
reboot_b
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

# --- 10c2. unattended: B's boot chain is accepted, so nobody is asked ---------------
# A server reboots with nobody to approve it. B re-enrols asking for this
# machine's current PCR 4 + 7 chain to be accepted: its session publishes a
# root-signed acceptance, and every boot B then quotes that chain with its TPM
# and A releases the half without a question. Needs a TPM in B; without one the
# case is reported, never silently skipped.
if vm_b ssh 'test -e /dev/tpmrm0'; then
  ( sudo_b "/usr/libexec/tilo/tilo-auth sibling remove $b_root_part --key-file /root/unlock-e2e/pass \
      && /usr/libexec/tilo-disk-encrypt enrol $b_root_part --sibling --unattended \
        --key-file /root/unlock-e2e/pass --network tilo-nearby-lan" \
    > "$out/unlock-unattended-enrol.txt" 2>&1 ) &
  unattended_pid=$!
  resolve_a approve unattended
  wait "$unattended_pid" \
    || { cat "$out/unlock-unattended-enrol.txt" >&2; die "B could not re-enrol for an unattended unlock"; }
  sudo_b "/usr/libexec/tilo/tilo-auth sibling status $b_root_part" > "$out/unlock-unattended-status.json"
  reboot_b
  await_ssh_b "without a prompt after its unattended unlock" 120
  vm_b ssh 'journalctl -b --no-pager -o short-monotonic -u 'tilo-unlock-sibling@*'' \
    > "$out/unlock-unattended-b-initrd.txt" 2>&1 || true
  grep -q 'unlocked with the consent' "$out/unlock-unattended-b-initrd.txt" \
    || die "B came up, but not through the sibling unlock: $(cat "$out/unlock-unattended-b-initrd.txt")"
  sudo_b 'cat /run/tilo-sibling-unlock/exchange.json' > "$out/unlock-unattended-exchange.json" \
    || die "B kept no transcript of the unattended exchange"
  grep -q '"attestation": "quoted"' "$out/unlock-unattended-exchange.json" \
    || die "B's unattended exchange carried no TPM quote: $(cat "$out/unlock-unattended-exchange.json")"
  # Nobody clicked anything: B came up inside its timeout, so A released the
  # half on the quote like the sibling it is.
  note "B booted unattended on its accepted boot chain, with no approval on A"
else
  note "B has no TPM; the unattended case needs one and did not run"
fi

# --- 10e. a server's first boot sets up its own unlock ------------------------------
# The install keeps the throwaway key inside the encrypted root when the plan
# says another of the person's devices approves the boot. Once the server's
# daemon runs, tilo-server-unlock.service spends it; until then its status
# calls the unlock pending. This is that path on B, with nobody signed in and
# the unit's parts as it runs them: root's around the daemon's own account's.
sudo_b "/usr/libexec/tilo/tilo-auth sibling remove $b_root_part --key-file /root/unlock-e2e/pass" \
  > "$out/unlock-server-remove.txt" 2>&1 \
  || { cat "$out/unlock-server-remove.txt" >&2; die "B could not drop its sibling slot before the server case"; }
# A throwaway slot stands in for the one the installer would have kept: the
# setup needs an existing passphrase of the volume, and this is it.
sudo_b "set -eu
  umask 0077
  head -c 64 /dev/urandom > /root/unlock-e2e/install.key
  cryptsetup luksAddKey --batch-mode --key-file /root/unlock-e2e/pass \
    --new-keyfile /root/unlock-e2e/install.key $b_root_part
  rm -rf /var/lib/tilo/sibling-enrol
  install -d -m 0700 /var/lib/tilo/sibling-enrol
  cp /root/unlock-e2e/install.key /var/lib/tilo/sibling-enrol/luks.key
  chmod 0600 /var/lib/tilo/sibling-enrol/luks.key
  printf 'luks_uuid=%s\nkey_file=%s\n' \
    \"\$(cryptsetup luksUUID $b_root_part)\" /var/lib/tilo/sibling-enrol/luks.key \
    > /var/lib/tilo/sibling-enrol/enrol.conf
  chmod 0600 /var/lib/tilo/sibling-enrol/enrol.conf
  install -d -m 0700 /var/lib/tilo-server
  : > /var/lib/tilo-server/sibling-unlock-pending
" > "$out/unlock-server-setup.txt" 2>&1 \
  || { cat "$out/unlock-server-setup.txt" >&2; die "B could not take the server's first-boot shape"; }
slots_before="$(sudo_b "cryptsetup luksDump $b_root_part" | awk '/^Keyslots:/{k=1;next} /^[^ ]/{k=0} k && $2==\"luks2\"{n++} END{print n+0}')"
# Nobody is signed in. B's daemon is the system unit's, so the daemon's own account here is
# tilo where a server's is tilo-server; the rest is the server's unit as it runs.
sudo_b "loginctl terminate-user $TILO_USER" >/dev/null 2>&1 || true
( sudo_b "systemd-run --wait --collect --unit=tilo-server-unlock-e2e \
    -p User=tilo -p RuntimeDirectory=tilo-server-unlock -p RuntimeDirectoryMode=0700 \
    -p 'ExecStartPre=+/usr/libexec/tilo-server-unlock prepare' \
    -p 'ExecStopPost=+/usr/libexec/tilo-server-unlock finish' \
    /usr/libexec/tilo-server-unlock register" \
    > "$out/unlock-server-unit.txt" 2>&1 ) &
unit_pid=$!
resolve_a approve server
wait "$unit_pid" || { cat "$out/unlock-server-unit.txt" >&2; die "B's unlock unit did not finish"; }
sudo_b "journalctl --no-pager -o cat -u tilo-server-unlock-e2e" > "$out/unlock-server-journal.txt" 2>&1 || true
grep -q 'the unlock is set up with the person.s devices, and the install key is removed' \
    "$out/unlock-server-journal.txt" \
  || die "B's unlock unit did not report a finished setup: $(cat "$out/unlock-server-journal.txt")"
sudo_b "cryptsetup luksDump $b_root_part" > "$out/unlock-server-luksdump.txt"
grep -q 'tilo-sibling-unlock' "$out/unlock-server-luksdump.txt" \
  || die "the server-shaped enrolment left no sibling token"
sudo_b "test ! -e /var/lib/tilo/sibling-enrol/luks.key \
    && test ! -e /var/lib/tilo/sibling-enrol/enrol.conf \
    && test ! -e /var/lib/tilo-server/sibling-unlock-pending" \
  || die "the install key, its file or the pending status survived the enrolment"
slots_after="$(awk '/^Keyslots:/{k=1;next} /^[^ ]/{k=0} k && $2==\"luks2\"{n++} END{print n+0}' "$out/unlock-server-luksdump.txt")"
[ "$slots_after" -eq "$slots_before" ] \
  || die "the install key's slot was not replaced one-for-one ($slots_before -> $slots_after)"
# Reboot with nobody at the console, and approve on A's sheet like any
# sibling boot.
reboot_b
approve_ask="$(await_question network.device-unlock server)"
note "A shows the server-shaped B's unlock request: $approve_ask"
answer_a allow "$approve_ask"
await_ssh_b "after A approved the server-shaped unlock" 120
sudo_b "journalctl -b --no-pager -o short-monotonic -u 'tilo-unlock-sibling@*'" \
  > "$out/unlock-server-b-initrd.txt" 2>&1 || true
grep -q 'unlocked with the consent' "$out/unlock-server-b-initrd.txt" \
  || die "the server-shaped B came up, but not through the sibling unlock: $(cat "$out/unlock-server-b-initrd.txt")"
note "a server-shaped first boot enrolled its sibling slot and booted with A's approval"

# --- 10d. B removed from the fleet --------------------------------------------------
# The person's own device list is where a device is revoked, by its key: the
# same step Settings' "Remove from your devices" takes.
remove_args="$(jq -cn --arg device "$device" '{device: $device, reason: "retired", confirm: true}')"
{ control_a raw "$(action_frame tilo.device devices revoke "$remove_args")" \
    > "$out/unlock-remove.txt" 2>&1 && action_completed "$out/unlock-remove.txt"; } || { cat "$out/unlock-remove.txt" >&2; die "A could not revoke B"; }
# A refusal on the fleet is decided before the person is asked, so no sheet
# stands: the boot says it was removed and waits for the passphrase.
reboot_b
at_prompt_then_type revoked
grep -q 'removed from the fleet' "$out/unlock-revoked-b-initrd.txt" \
  || die "B's attempt did not see that it was removed"
session_log vm_a | grep -F 'device unlock:' > "$out/unlock-a-history.txt" || true
note "a device removed from the fleet falls back to the passphrase"
