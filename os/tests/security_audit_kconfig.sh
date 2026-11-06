#!/usr/bin/env bash
# tilo-security-audit counts a kernel option as compiled only when the config
# sets it (arm64 configs carry no line at all for x86-only or removed options),
# and reads packages from its unit's privileged snapshot.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
audit="$repo/os/usr/libexec/tilo-security-audit"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
sed -n '/^kconfig_off() {$/,/^}$/p' "$audit" > "$work/kconfig.sh"
grep -q 'kconfig_off' "$work/kconfig.sh"
# shellcheck disable=SC1091
. "$work/kconfig.sh"
printf '%s\n' 'CONFIG_DEBUG_FS=y' 'CONFIG_TLS=m' '# CONFIG_KEXEC is not set' > "$work/config"
running_kernel_config="$work/config"
kconfig_off KEXEC
kconfig_off X86_DECODER_SELFTEST
! kconfig_off DEBUG_FS
! kconfig_off TLS
running_kernel_config="$work/missing"
! kconfig_off KEXEC
# Package checks read the unit's privileged snapshot: rpm cannot run under the
# unit's NoNewPrivileges= (SELinux denies its domain transition).
sed -n '/^installed() {$/,/^}$/p' "$audit" > "$work/installed.sh"
# shellcheck disable=SC1091
. "$work/installed.sh"
printf '%s\n' gnutls-utils swtpm-tools > "$work/packages"
packages_file="$work/packages"
installed gnutls-utils
installed gnutls-utils swtpm-tools
! installed gnutls-utils nfs-utils
grep -Fq 'rpm -qa --qf "%%{NAME}\n" > /run/tilo-security-audit.packages' \
  "$repo/os/usr/lib/systemd/system/tilo-security-audit.service"
! grep -E '^[^#]*rpm -q [^a]' "$audit" | grep -v 'rpm -q "$package" >/dev/null 2>&1 || return 1'
# The active LSM list is comma-separated.
grep -Fq 'case ",$active_lsms," in' "$audit"

# --- the image signature decision ---------------------------------------------
# A dev image is a developer device on purpose: its own policy admits the dev
# channel's repository with keys/cosign-dev.pub. The audit must report that as
# a developer-mode notice there, and must keep failing on it everywhere else —
# a release image, and any scope the dev override does not name. The decision
# runs against fixtures, through the audit's own function, so this proves the
# audit and not a copy of its logic.
sed -n '/^check_image_policy() {$/,/^}$/p' "$audit" > "$work/policy.sh"
grep -q 'check_image_policy' "$work/policy.sh" || {
  echo 'the security audit has no check_image_policy decision to test' >&2
  exit 1
}

fixtures="$work/policy-fixture"
mkdir -p "$fixtures/release" "$fixtures/bin"
printf 'TILO_RELEASE_EPOCH=3\nTILO_RELEASE_ISSUER_PREFIX=https://signing.example/release/epoch-\nTILO_RELEASE_IDENTITY_DOMAIN=signing.example\n' \
  > "$fixtures/release/trust.env"
printf 'REKOR\n' > "$fixtures/release/rekor.pub"
printf 'KEY\n' > "$fixtures/cosign.pub"
printf 'DEVKEY\n' > "$fixtures/cosign-dev.pub"
printf 'dev\n' > "$fixtures/flavour-dev"
: > "$fixtures/flavour-release"

# The stub answers as tilo-image-policy-check does: the live binary would be
# needed otherwise, and this test is about the audit's decision, not the
# parser (whose own suite covers that).
cat > "$fixtures/bin/check" <<'SH'
#!/usr/bin/bash
# A dev policy fails the release form and passes the developer one, naming the
# dev key; a weakened release scope fails both.
developer=0
policy=""
for argument in "$@"; do
  [ "$argument" = --developer ] && developer=1 || policy="$argument"
done
# A weakened release scope is a failure whoever asks, exactly as the real
# parser answers: the developer form cannot cover for it.
if grep -Fq '"keyPath": "/etc/pki/other.pub"' "$policy"; then
  echo "$policy: registry.tilo.so/tilo/tilo: keyPath is '/etc/pki/other.pub', not '/usr/share/tilo/cosign.pub'"
  exit 1
fi
if grep -Fq '"keyPath": "/usr/share/tilo/cosign-dev.pub"' "$policy"; then
  if [ "$developer" -eq 1 ]; then
    echo "$policy: Tilo image scopes require the /usr/share/tilo/cosign.pub sigstore signature, logged in Rekor; registry.tilo.so/tilo/tilo-vm is verified with the development key /usr/share/tilo/cosign-dev.pub, which a developer-built image admits in its own place"
    exit 0
  fi
  echo "$policy: registry.tilo.so/tilo/tilo-vm: keyPath is '/usr/share/tilo/cosign-dev.pub', not '/usr/share/tilo/cosign.pub'"
  exit 1
fi
echo "$policy: Tilo image scopes require the /usr/share/tilo/cosign.pub sigstore signature, logged in Rekor"
SH
chmod 0755 "$fixtures/bin/check"

policy_dev="$fixtures/policy-dev.json"
printf '{"transports":{"docker":{"registry.tilo.so/tilo/tilo-vm":{"keyPath": "/usr/share/tilo/cosign-dev.pub"}}}}\n' > "$policy_dev"
policy_release="$fixtures/policy-release.json"
printf '{"transports":{"docker":{}}}\n' > "$policy_release"
policy_weakened="$fixtures/policy-weakened.json"
printf '{"transports":{"docker":{"registry.tilo.so/tilo/tilo-vm":{"keyPath": "/usr/share/tilo/cosign-dev.pub"},"registry.tilo.so/tilo/tilo":{"keyPath": "/etc/pki/other.pub"}}}}\n' > "$policy_weakened"

# decide POLICY FLAVOUR-FILE PINNED-KEY: prints "<status> <verdict>".
decide() {
  (
    set +eu
    # shellcheck disable=SC1091
    . "$work/policy.sh"
    policy="$1" image_flavour_file="$2" pinned_key="$3" \
      release_dir="$fixtures/release" dev_key="$fixtures/cosign-dev.pub" \
      image_policy_check="$fixtures/bin/check"
    if verdict="$(check_image_policy)"; then
      printf '0 %s' "$verdict"
    else
      printf '%s %s' "$?" "$verdict"
    fi
  )
}

status_and_report="$(decide "$policy_release" "$fixtures/flavour-release" "$fixtures/cosign.pub")"
case "$status_and_report" in
  0*"the /usr/share/tilo/cosign.pub sigstore signature"*) ok_release=1 ;;
  *) ok_release=0 ;;
esac

status_and_report="$(decide "$policy_dev" "$fixtures/flavour-dev" "$fixtures/cosign.pub")"
case "$status_and_report" in
  2*"development key"*) ok_dev_note=1 ;;
  *) ok_dev_note=0 ;;
esac

# The same policy on a release image is still a failure.
status_and_report="$(decide "$policy_dev" "$fixtures/flavour-release" "$fixtures/cosign.pub")"
case "$status_and_report" in
  1*) ok_release_fails=1 ;;
  *) ok_release_fails=0 ;;
esac

# A dev image cannot smuggle a weakened release scope through the dev key.
status_and_report="$(decide "$policy_weakened" "$fixtures/flavour-dev" "$fixtures/cosign.pub")"
case "$status_and_report" in
  1*) ok_dev_weakened=1 ;;
  *) ok_dev_weakened=0 ;;
esac

# A dev image whose development key is not installed fails: the override names
# a key that has to be there.
status_and_report="$( (
  set +eu
  # shellcheck disable=SC1091
  . "$work/policy.sh"
  policy="$policy_dev" image_flavour_file="$fixtures/flavour-dev" pinned_key="$fixtures/cosign.pub" \
    release_dir="$fixtures/release" dev_key="$fixtures/missing-dev.pub" \
    image_policy_check="$fixtures/bin/check"
  if verdict="$(check_image_policy)"; then
    printf '0 %s' "$verdict"
  else
    printf '%s %s' "$?" "$verdict"
  fi
) )"
case "$status_and_report" in
  1*"missing or empty"*) ok_dev_key_missing=1 ;;
  *) ok_dev_key_missing=0 ;;
esac

if [ "${ok_release:-0}" = 1 ] && [ "$ok_release_fails" = 1 ] && [ "$ok_dev_note" = 1 ] \
   && [ "$ok_dev_weakened" = 1 ] && [ "$ok_dev_key_missing" = 1 ]; then
  echo 'security audit image signature decision: ok'
else
  printf 'security audit image signature decision: FAIL (release=%s release-fails=%s dev-note=%s dev-weakened=%s dev-key=%s)\n' \
    "$ok_release" "$ok_release_fails" "$ok_dev_note" "$ok_dev_weakened" "$ok_dev_key_missing" >&2
  exit 1
fi
echo 'security audit kernel option checks: ok'
