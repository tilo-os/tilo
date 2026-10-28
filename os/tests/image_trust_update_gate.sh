#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
gate="$repo/os/usr/libexec/tilo-image-trust-staged"
deployment="$repo/os/usr/libexec/tilo-deployment"
update_policy_unit="$repo/os/usr/lib/systemd/system/tilo-update-policy@.service"
deployment_update_unit="$repo/os/usr/lib/systemd/system/tilo-deployment-update.service"
deployment_rollback_unit="$repo/os/usr/lib/systemd/system/tilo-deployment-rollback.service"
for required in \
  'NoNewPrivileges=yes' \
  'PrivateDevices=yes' \
  'PrivateNetwork=yes' \
  'ProtectSystem=strict' \
  'ProtectProc=invisible' \
  'CapabilityBoundingSet=' \
  'AmbientCapabilities=' \
  'InaccessiblePaths=-/var/lib/tilo/device-private -/var/lib/tilo/secureboot -/var/lib/tilo/image-trust -/var/lib/tilo/rollback' \
  'SystemCallFilter=@system-service'; do
  grep -Fqx "$required" "$update_policy_unit" || {
    echo "update-policy sandbox is missing: $required" >&2
    exit 1
  }
done
for unit in "$deployment_update_unit" "$deployment_rollback_unit"; do
  for required in \
    'NoNewPrivileges=yes' \
    'PrivateIPC=yes' \
    'ProtectHome=yes' \
    'ProtectKernelModules=yes' \
    'ProtectKernelLogs=yes' \
    'MemoryDenyWriteExecute=yes' \
    'SystemCallArchitectures=native'; do
    grep -Fqx "$required" "$unit" || {
      echo "deployment executor sandbox is missing from $(basename "$unit"): $required" >&2
      exit 1
    }
  done
done
grep -Fqx 'PrivateNetwork=yes' "$deployment_rollback_unit" || {
  echo 'local rollback executor retains unnecessary external network access' >&2
  exit 1
}
for resource_policy in \
  'Nice=10' \
  'CPUWeight=20' \
  'IOWeight=20' \
  'IOSchedulingClass=best-effort' \
  'IOSchedulingPriority=7'; do
  grep -Fqx "$resource_policy" "$deployment_update_unit" || {
    echo "manual update can contend with an interactive workload: $resource_policy" >&2
    exit 1
  }
done
grep -Fq 'tilo-image-trust parse-bootc-status' "$gate"
if grep -Eq 'python(3)?' "$gate"; then
  echo 'staged image trust still delegates authority parsing to Python' >&2
  exit 1
fi
grep -Fq 'tilo-image-trust bootc-status-field "$1"' "$deployment"
grep -Fq 'tilo-image-trust bootc-status-field "$2"' "$deployment"
grep -Fq 'status="$(bootc_status_json)"' "$deployment"
# Every deployment `list` shows comes from one bootc status snapshot, read
# field by field through the Rust parser.
for deployment_kind in booted staged rollback; do
  grep -Fq "print_deployment \"\$esp\" \"\$status\" $deployment_kind" "$deployment"
done
for role_field in image version timestamp composefs; do
  grep -Fq "status_field \"\$status\" \"\$role-$role_field\"" "$deployment"
done
grep -Fq 'status_field "$1" "$2-image"' "$deployment"
grep -Fq 'bootc_field staged-composefs' "$deployment"
if grep -Eq 'python(3)?[[:space:]]+-c|eval[[:space:]]*\(' "$deployment"; then
  echo 'deployment bootc authority parsing still permits Python or eval' >&2
  exit 1
fi
chrony="$repo/os/etc/chrony.conf"
grep -Fqx 'authselectmode require' "$chrony"
grep -Fqx 'minsources 2' "$chrony"
[ "$(grep -Ec '^server [^ ]+ .*\bnts\b' "$chrony")" -ge 3 ]
if grep -E '^server ' "$chrony" | grep -Ev '\bnts\b' >/dev/null; then
  echo 'unauthenticated NTP source can influence image trusted time' >&2
  exit 1
fi
if grep -Eq '^[[:space:]]*(pool|peer|sourcedir|confdir)[[:space:]]' "$chrony"; then
  echo 'dynamic or unaudited Chrony source can influence image trusted time' >&2
  exit 1
fi
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/trust" "$work/boot/EFI/Linux/bootc"
printf '#!/usr/bin/env bash\nexit 3\n' >"$work/bin/successor"
chmod 0755 "$work/bin/successor"
cp "$chrony" "$work/chrony.conf"
chmod 0644 "$work/chrony.conf"
chmod 0700 "$work/trust"
digest="$(printf 'a%.0s' {1..64})"
verity="$(printf 'b%.0s' {1..128})"
printf 'uki\n' >"$work/boot/EFI/Linux/bootc/staged.efi"

grep -Fqx 'ExecStart=/usr/libexec/tilo-verified-update --quiet' \
  "$repo/os/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/20-tilo-update-policy.conf"
if grep -Eq '^ExecStart=.*--apply' \
    "$repo/os/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/20-tilo-update-policy.conf"; then
  echo 'automatic updates can still force an in-session reboot' >&2
  exit 1
fi
grep -Fq '/usr/libexec/tilo-verified-update ||' "$repo/os/usr/libexec/tilo-deployment"
grep -Fq 'os/usr/libexec/tilo-verified-update' "$repo/os/Containerfile"

cat >"$work/bin/bootc" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ $1 == status ]]; then
  if [[ ${TILO_TEST_BAD_STATUS:-0} == 1 ]]; then printf 'not-json\n'; exit 0; fi
  if [[ ${TILO_TEST_HUGE_STATUS:-0} == 1 ]]; then head -c 1048577 /dev/zero | tr '\0' x; exit 0; fi
  if [[ ${TILO_TEST_NO_STAGED:-0} == 1 ]]; then printf '{"status":{"staged":null}}\n'; exit 0; fi
  printf '{"status":{"staged":{"image":{"imageDigest":"sha256:%s"},"composefs":{"verity":"sha512:%s"}}}}\n' \
    "$TILO_TEST_DIGEST" "$TILO_TEST_VERITY"
else
  printf '%s\n' "$*" >>"$TILO_TEST_BOOTC_CALLS"
fi
EOF
cat >"$work/bin/uki-cmdline" <<EOF
#!/usr/bin/env bash
if [[ \${TILO_TEST_HUGE_CMDLINE:-0} == 1 ]]; then head -c 65537 /dev/zero | tr '\0' x; exit 0; fi
printf '%s\n' "\${TILO_TEST_UKI_CMDLINE:-quiet composefs=sha512:$verity tilo.generation=42}"
EOF
cat >"$work/bin/tilo-image-trust" <<'EOF'
#!/usr/bin/env bash
if [[ $1 == bootc-status-field ]]; then
  [[ $2 == booted-image ]] && printf '%s\n' "${TILO_TEST_FOLLOWING:-registry.tilo.so/tilo/tilo-vm:x86_64-ci}"
  exit 0
fi
if [[ $1 == parse-bootc-status ]]; then
  status="$(cat)"
  [[ $status == *'"status"'* ]] || exit 1
  if [[ $status == *'"staged":null'* ]]; then printf 'NONE\n'; exit 0; fi
  digest="$(sed -n 's/.*"imageDigest":"\([^"]*\)".*/\1/p' <<<"$status")"
  verity="$(sed -n 's/.*"verity":"\([^"]*\)".*/\1/p' <<<"$status")"
  printf '%s\n%s\n' "$digest" "$verity"
  exit 0
fi
if [[ $1 == renew-under-update-lock ]]; then
  printf '%s\n' "$*" >"$TILO_TEST_VERIFY_ARGS.renew"
  [[ ${TILO_TEST_REJECT_RENEWAL:-0} != 1 ]]
  exit $?
fi
printf '%s\n' "$*" >"$TILO_TEST_VERIFY_ARGS"
[[ ${TILO_TEST_REJECT:-0} != 1 ]]
EOF
# The channel names what the update follows (tilo-update-channel next); nothing: the origin.
cat >"$work/bin/channel" <<'EOF'
#!/usr/bin/env bash
[[ $1 == next && -n ${TILO_TEST_CHANNEL_NEXT:-} ]] && printf '%s\n' "$TILO_TEST_CHANNEL_NEXT"
exit 0
EOF
cat >"$work/bin/chronyc" <<'EOF'
#!/usr/bin/env bash
[[ $* == '-n waitsync 1 0.1' && ${TILO_TEST_CHRONY_UNSYNC:-0} != 1 ]]
EOF
cat >"$work/bin/image-trust-gate" <<'EOF'
#!/usr/bin/env bash
exec bash "$TILO_TEST_GATE_SOURCE" "$@"
EOF
chmod +x "$work/bin/"*

run_gate() {
  PATH="$work/bin:$PATH" TILO_IMAGE_TRUST_DIR="$work/trust" \
    TILO_IMAGE_TRUST_OWNER_UID="$(id -u)" \
    TILO_IMAGE_TRUST_BOOT="$work/boot" TILO_IMAGE_TRUST_BOOTC="$work/bin/bootc" \
    TILO_IMAGE_TRUST_UKI_CMDLINE="$work/bin/uki-cmdline" \
    TILO_IMAGE_TRUST_CHRONYC="$work/bin/chronyc" \
    TILO_IMAGE_TRUST_CHRONY_CONFIG="$work/chrony.conf" \
    TILO_TEST_DIGEST="$digest" TILO_TEST_VERITY="${TILO_TEST_VERITY_OVERRIDE-$verity}" \
    TILO_TEST_BOOTC_CALLS="$work/bootc.calls" \
    TILO_TEST_VERIFY_ARGS="$work/verify.args" \
    bash "$repo/os/usr/libexec/tilo-image-trust-staged" "$@"
}

run_update() {
  PATH="$work/bin:$PATH" TILO_IMAGE_TRUST_DIR="$work/trust" \
    TILO_IMAGE_TRUST_OWNER_UID="$(id -u)" \
    TILO_IMAGE_TRUST_BOOT="$work/boot" TILO_IMAGE_TRUST_BOOTC="$work/bin/bootc" \
    TILO_IMAGE_TRUST_UKI_CMDLINE="$work/bin/uki-cmdline" \
    TILO_IMAGE_TRUST_CHRONYC="$work/bin/chronyc" \
    TILO_IMAGE_TRUST_CHRONY_CONFIG="$work/chrony.conf" \
    TILO_IMAGE_TRUST_UPDATE_LOCK="${TILO_IMAGE_TRUST_UPDATE_LOCK:-$work/update.lock}" \
    TILO_IMAGE_TRUST_SUCCESSOR="$work/bin/successor" \
    TILO_IMAGE_TRUST_CHANNEL="$work/bin/channel" \
    TILO_IMAGE_TRUST_GATE="$work/bin/image-trust-gate" \
    TILO_TEST_GATE_SOURCE="$repo/os/usr/libexec/tilo-image-trust-staged" \
    TILO_TEST_DIGEST="$digest" TILO_TEST_VERITY="${TILO_TEST_VERITY_OVERRIDE-$verity}" \
    TILO_TEST_BOOTC_CALLS="$work/bootc.calls" TILO_TEST_VERIFY_ARGS="$work/verify.args" \
    bash "$repo/os/usr/libexec/tilo-verified-update" "$@"
}

# No marker means the official Cosign-only policy remains active and this gate
# performs no owner-chain work.
run_gate
test ! -e "$work/verify.args"
run_update --quiet
grep -Fqx 'upgrade --quiet' "$work/bootc.calls"
rm -f "$work/bootc.calls"

# A channel naming the image bootc already follows upgrades it; naming another (the device chose
# another channel, or a developer pinned a build) switches to it, through the same path.
TILO_TEST_CHANNEL_NEXT=registry.tilo.so/tilo/tilo-vm:x86_64-ci run_update --quiet
grep -Fqx 'upgrade --quiet' "$work/bootc.calls"
rm -f "$work/bootc.calls"
TILO_TEST_CHANNEL_NEXT=registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci-x86_64 run_update --quiet
grep -Fqx 'switch --quiet registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci-x86_64' "$work/bootc.calls"
rm -f "$work/bootc.calls"

# Arming takes this same lock. Even before the marker exists, an update must
# wait rather than race past the owner-chain decision while artifacts publish.
exec 8>"$work/update.lock"
flock -x 8
run_update --quiet &
blocked_update=$!
sleep 0.1
if [ -e "$work/bootc.calls" ]; then
  echo 'pre-arming update bypassed the trust transaction lock' >&2
  exit 1
fi
flock -u 8
wait "$blocked_update"
grep -Fqx 'upgrade --quiet' "$work/bootc.calls"
rm -f "$work/bootc.calls"

printf 'armed\n' >"$work/trust/required"
touch "$work/trust/root.pub" \
  "$work/trust/revocations.bin" "$work/trust/endorsement.bin"
chmod 0600 "$work/trust/"*
run_gate --require-staged >/dev/null
grep -Fq "verify-and-record $work/trust/root.pub $work/trust/revocations.bin $work/trust/endorsement.bin sha256:$digest 42" "$work/verify.args"
grep -Fq "$work/trust/generation.state $work/trust/time.state" "$work/verify.args"

if TILO_TEST_CHRONY_UNSYNC=1 run_gate --require-staged >/dev/null 2>&1; then
  echo 'unsynchronized RTC was accepted as trusted endorsement time' >&2
  exit 1
fi
cp "$work/chrony.conf" "$work/chrony.conf.valid"
printf 'server attacker.invalid iburst\n' >>"$work/chrony.conf"
if run_gate --require-staged >/dev/null 2>&1; then
  echo 'unauthenticated runtime time source was accepted for image trust' >&2
  exit 1
fi
mv "$work/chrony.conf.valid" "$work/chrony.conf"
chmod 0644 "$work/chrony.conf"

# Signed successor metadata must match the ACTUAL staged digest and UKI,
# remain fresh after download, and renew only before exact owner verification.
expires=$(( $(date +%s) + 600 ))
run_gate --successor "sha256:$digest" 42 "$expires" 8 "$work/update.lock" >/dev/null
grep -Fq "renew-under-update-lock $work/trust /var/lib/tilo/device-private/keypair.bin sha256:$digest 42" "$work/verify.args.renew"
grep -Fq "verify-and-record $work/trust/root.pub" "$work/verify.args"
rm "$work/verify.args.renew"
for invalid in expired generation digest; do
  selected_digest="sha256:$digest"; selected_generation=42; selected_expiry="$expires"
  case "$invalid" in
    expired) selected_expiry="$(date +%s)" ;;
    generation) selected_generation=43 ;;
    digest) selected_digest="sha256:$(printf 'f%.0s' {1..64})" ;;
  esac
  if run_gate --successor "$selected_digest" "$selected_generation" "$selected_expiry" 8 "$work/update.lock" >/dev/null 2>&1; then
    echo "invalid successor $invalid was accepted" >&2
    exit 1
  fi
  [ ! -e "$work/verify.args.renew" ]
done
if TILO_TEST_REJECT_RENEWAL=1 run_gate --successor "sha256:$digest" 42 "$expires" 8 "$work/update.lock" >/dev/null 2>&1; then
  echo 'refused owner renewal was activated' >&2
  exit 1
fi
rm "$work/verify.args.renew"
if TILO_TEST_CHRONY_UNSYNC=1 run_gate --trusted-time >/dev/null 2>&1; then
  echo 'successor discovery used unauthenticated time' >&2
  exit 1
fi

run_update --quiet >/dev/null
sed -n '1p' "$work/bootc.calls" | grep -Fqx 'upgrade --download-only --quiet'
sed -n '2p' "$work/bootc.calls" | grep -Fqx 'upgrade --from-downloaded --quiet'
rm -f "$work/bootc.calls"

run_update --apply --quiet >/dev/null
sed -n '1p' "$work/bootc.calls" | grep -Fqx 'upgrade --download-only --quiet'
sed -n '2p' "$work/bootc.calls" | grep -Fqx 'upgrade --from-downloaded --apply --quiet'
rm -f "$work/bootc.calls"

# With owner trust armed a channel switch is gated like an upgrade.
TILO_TEST_CHANNEL_NEXT=registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci-x86_64 run_update --quiet >/dev/null
sed -n '1p' "$work/bootc.calls" | grep -Fqx 'switch --download-only --quiet registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci-x86_64'
sed -n '2p' "$work/bootc.calls" | grep -Fqx 'switch --from-downloaded --quiet'
rm -f "$work/bootc.calls"

TILO_TEST_NO_STAGED=1 run_update --quiet >/dev/null
grep -Fqx 'upgrade --download-only --quiet' "$work/bootc.calls"
[ "$(wc -l <"$work/bootc.calls")" -eq 1 ]
rm -f "$work/bootc.calls"

if TILO_TEST_REJECT=1 run_update --quiet >/dev/null 2>&1; then
  echo 'rejected owner image endorsement was activated' >&2
  exit 1
fi
grep -Fqx 'upgrade --download-only --quiet' "$work/bootc.calls"
[ "$(wc -l <"$work/bootc.calls")" -eq 1 ]

if TILO_TEST_BAD_STATUS=1 run_gate >/dev/null 2>&1; then
  echo 'malformed bootc status bypassed the configured owner trust gate' >&2
  exit 1
fi
if TILO_TEST_HUGE_STATUS=1 run_gate >/dev/null 2>&1; then
  echo 'oversized bootc status bypassed the bounded trust parser' >&2
  exit 1
fi
if TILO_TEST_HUGE_CMDLINE=1 run_gate --require-staged >/dev/null 2>&1; then
  echo 'oversized UKI command line bypassed the bounded trust parser' >&2
  exit 1
fi
if TILO_TEST_VERITY_OVERRIDE= run_gate --require-staged >/dev/null 2>&1; then
  echo 'missing staged composefs digest bypassed owner trust' >&2
  exit 1
fi

if TILO_TEST_REJECT=1 run_gate --require-staged >/dev/null 2>&1; then
  echo 'rejected owner image endorsement was accepted' >&2
  exit 1
fi

if TILO_TEST_UKI_CMDLINE="composefs=sha512:$verity composefs=sha512:$verity tilo.generation=42" \
    run_gate --require-staged >/dev/null 2>&1; then
  echo 'duplicate composefs arguments bypassed staged-image binding' >&2
  exit 1
fi
if TILO_TEST_UKI_CMDLINE="composefs=sha512:$verity tilo.generation=42 tilo.generation=43" \
    run_gate --require-staged >/dev/null 2>&1; then
  echo 'duplicate generation arguments bypassed staged-image binding' >&2
  exit 1
fi
ln "$work/boot/EFI/Linux/bootc/staged.efi" "$work/boot/EFI/Linux/bootc/staged-alias.efi"
if run_gate --require-staged >/dev/null 2>&1; then
  echo 'multiply-linked staged UKI bypassed image trust' >&2
  exit 1
fi
rm "$work/boot/EFI/Linux/bootc/staged-alias.efi"
chmod 0666 "$work/boot/EFI/Linux/bootc/staged.efi"
if run_gate --require-staged >/dev/null 2>&1; then
  echo 'group/world-writable staged UKI bypassed image trust' >&2
  exit 1
fi
chmod 0644 "$work/boot/EFI/Linux/bootc/staged.efi"
chmod 0777 "$work/boot"
if run_gate --require-staged >/dev/null 2>&1; then
  echo 'unsafe boot-store permissions bypassed image trust' >&2
  exit 1
fi
chmod 0755 "$work/boot"
chmod 0775 "$work/boot/EFI/Linux"
if run_gate --require-staged >/dev/null 2>&1; then
  echo 'writable UKI parent directory bypassed image trust' >&2
  exit 1
fi
chmod 0755 "$work/boot/EFI/Linux"
mv "$work/boot/EFI/Linux/bootc" "$work/escaped-bootc"
ln -s "$work/escaped-bootc" "$work/boot/EFI/Linux/bootc"
if run_gate --require-staged >/dev/null 2>&1; then
  echo 'staged UKI escaping through a parent symlink bypassed image trust' >&2
  exit 1
fi
rm "$work/boot/EFI/Linux/bootc"
mv "$work/escaped-bootc" "$work/boot/EFI/Linux/bootc"

printf 'wrong\n' >"$work/trust/required"
if run_update --quiet >/dev/null 2>&1; then
  echo 'malformed owner-trust marker bypassed verification' >&2
  exit 1
fi
rm -f "$work/trust/required"
ln -s "$work/missing-marker-target" "$work/trust/required"
if run_update --quiet >/dev/null 2>&1; then
  echo 'dangling owner-trust marker bypassed verification' >&2
  exit 1
fi
rm -f "$work/trust/required"
printf 'armed\n' >"$work/trust/required"
chmod 0600 "$work/trust/required"

chmod 0755 "$work/trust"
if run_gate --require-staged >/dev/null 2>&1; then
  echo 'owner image trust accepted an insecure trust directory' >&2
  exit 1
fi

chmod 0700 "$work/trust"
for reserved in --download-only --download-only=true --from-downloaded --from-downloaded=true --apply=true; do
  if run_update "$reserved" --quiet >/dev/null 2>&1; then
    echo "reserved update phase argument was accepted: $reserved" >&2
    exit 1
  fi
done
ln -s "$work/attacker-lock" "$work/update-symlink.lock"
if TILO_IMAGE_TRUST_UPDATE_LOCK="$work/update-symlink.lock" run_update --quiet \
    >/dev/null 2>&1; then
  echo 'owner image update accepted a symlink transaction lock' >&2
  exit 1
fi

chmod 0644 "$work/update.lock"
if run_update --quiet >/dev/null 2>&1; then
  echo 'owner image update accepted a shared transaction lock' >&2
  exit 1
fi
chmod 0600 "$work/update.lock"
ln "$work/update.lock" "$work/update-lock-alias"
printf 'lock sentinel\n' >"$work/update.lock"
if run_update --quiet >/dev/null 2>&1; then
  echo 'owner image update accepted a multiply-linked transaction lock' >&2
  exit 1
fi
grep -Fqx 'lock sentinel' "$work/update-lock-alias" || {
  echo 'update lock was truncated before its hard-link count was rejected' >&2
  exit 1
}

printf 'ok: staged updates enforce the optional owner identity image chain\n'
printf 'ok: rejected endorsements fail closed before update activation\n'
printf 'ok: download, verification, rollback state, and activation share one lock\n'
