#!/usr/bin/env bash
# tilo-security-audit's development posture lists exactly the deviations from
# a production device that are in force (Secure Boot off, enrolled extension
# authorities, a dev-overrides compositor, sshd listening, NOPASSWD sudo,
# unsigned-image admissions in the live container policy): facts on a dev
# image, failures on a production one, and nothing at all on a production
# machine that has none of them.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
audit="$repo/os/usr/libexec/tilo-security-audit"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

sed -n '/^posture_file=/,/^echo "=== end Tilo security audit ==="$/p' "$audit" | sed '$d' > "$work/posture.sh"
grep -q 'deviation()' "$work/posture.sh"

# The unit's privileged snapshot names its files, which the audit reads.
unit="$repo/os/usr/lib/systemd/system/tilo-security-audit.service"
grep -Fq '/run/tilo-security-audit.posture' "$unit"
grep -Fq 'bootc status --json > /run/tilo-security-audit.bootc.json' "$unit"
grep -Fq "posture_file=\"\${TILO_SECURITY_POSTURE:-/run/tilo-security-audit.posture}\"" "$audit"
# The note that claimed a mandatory Cosign policy whatever the live policy said is gone.
! grep -Fq 'official Cosign policy remains mandatory' "$audit"

# A checker that answers as tilo-image-policy-check --unsigned does.
cat > "$work/checker" <<'SH'
#!/usr/bin/env bash
[ "$1" = --unsigned ] || exit 2
cat "$2"
SH
chmod +x "$work/checker"

run() { # flavour, then the fixture directory
  (
    failures=0
    ok()   { echo "[ OK ]   $*"; }
    warn() { echo "[FAIL]   $*"; failures=$((failures + 1)); }
    note() { echo "[info]   $*"; }
    image_flavour="$1"
    sb="$(cat "$2/sb")"
    policy="$2/unsigned"
    image_policy_check="$work/checker"
    export TILO_SECURITY_POSTURE="$2/posture"
    export TILO_SECURITY_AUTHORITIES="$2/authorities"
    export TILO_SECURITY_SHELL="$2/shell"
    export TILO_SECURITY_BOOTC_STATUS="$2/bootc.json"
    # shellcheck disable=SC1091
    . "$work/posture.sh"
    echo "failures=$failures"
  )
}

# A laptop: every deviation in force.
laptop="$work/laptop"
mkdir -p "$laptop/authorities"
echo 0 > "$laptop/sb"
printf '%s\n' '-----BEGIN PUBLIC KEY-----' 'MCowBQYDK2VwAyEAiojj3XQJ8ZX9UtstPLpdcspnCb8dlBIb83SIAbQPb1w=' '-----END PUBLIC KEY-----' > "$laptop/authorities/dev.pub"
printf 'binary tilo compositor: dev-overrides build: developer environment overrides\n' > "$laptop/shell"
printf '%s\n' \
  'listen LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:(("sshd",pid=1,fd=3))' \
  'listen LISTEN 0 128 [::]:22 [::]:* users:(("sshd",pid=1,fd=4))' \
  'listen LISTEN 0 4096 127.0.0.54:53 0.0.0.0:* users:(("systemd-resolve",pid=2,fd=5))' \
  'nopasswd /etc/sudoers.d/tilo-vm-xaver' > "$laptop/posture"
printf '%s\n' "the default rule is [{'type': 'insecureAcceptAnything'}], so an image no scope names is not refused" > "$laptop/unsigned"
out="$(run dev "$laptop")"
expected_fp="$(sed '/^-----/d' "$laptop/authorities/dev.pub" | base64 -d | sha256sum | cut -d' ' -f1)"
for needle in \
  '[dev]    Secure Boot is off (SecureBoot=0)' \
  "[dev]    extension authority enrolled: dev sha256:$expected_fp" \
  "[dev]    the compositor ($laptop/shell) is a dev-overrides build" \
  '[dev]    sshd is listening on 0.0.0.0:22 [::]:22' \
  '[dev]    sudo without a password (NOPASSWD) is granted in /etc/sudoers.d/tilo-vm-xaver' \
  "[dev]    container policy: the default rule is [{'type': 'insecureAcceptAnything'}]" \
  'failures=0'; do
  grep -Fq -- "$needle" <<<"$out" || { echo "dev posture lacks: $needle"; echo "$out"; exit 1; }
done
! grep -Fq 'systemd-resolve' <<<"$out"
# The same machine as a production image fails on each of the six.
out="$(run production "$laptop")"
grep -Fq 'failures=6' <<<"$out" || { echo "$out"; exit 1; }

# A production machine with none of them reports none.
clean="$work/clean"
mkdir -p "$clean/authorities"
echo 1 > "$clean/sb"
printf 'binary\n' > "$clean/shell"
printf '%s\n' 'listen LISTEN 0 4096 127.0.0.54:53 0.0.0.0:* users:(("systemd-resolve",pid=2,fd=5))' > "$clean/posture"
: > "$clean/unsigned"
out="$(run production "$clean")"
[ "$(grep -c '^\[FAIL\]\|^\[dev\]' <<<"$out" || true)" = 0 ] || { echo "$out"; exit 1; }
grep -Fq 'failures=0' <<<"$out"
echo 'security audit development posture: ok'
