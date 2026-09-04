#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
common="$repo/os/dev/_common.sh"
vm="$repo/os/dev/vm.sh"
generation="$repo/os/dev/next-generation.sh"

fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

if grep -Fq "printf -- '-machine q35,smm=on'" "$common" \
  && grep -Fq "printf -- '-machine q35'" "$common" \
  && ! grep -Eq -- '-machine q35[^[:space:]'\"']*accel=kvm' "$common" \
  && grep -Fq -- '-accel tcg,thread=multi -cpu max' "$common"; then
  ok 'machine type does not override the selected KVM/TCG accelerator'
else
  bad 'VM can request conflicting KVM and TCG accelerators'
fi

if grep -Fq 'QOS_VM_CYCLE=' "$common" \
  && grep -Fq '$QOS_CYCLE_DIR/installer-work.qcow2' "$common" \
  && grep -Fq '$QOS_CYCLE_DIR/target.qcow2' "$common" \
  && grep -Fq '/qos-vm-$QOS_VM_CYCLE' "$common" \
  && grep -Fq 'QOS_TPM_DIR="${QOS_TPM_DIR:-$QOS_RUN/tpm}"' "$common" \
  && grep -Fq 'QOS_SECBOOT_KEYS" = "none" ] && [ -z "$QOS_VM_CYCLE"' "$common" \
  && grep -Fq 'cmd_init_cycle()' "$vm" \
  && grep -Fq 'refusing to clear non-cycle runtime' "$vm" \
  && grep -Fq '|| rm -r "$QOS_CYCLE_DIR"' "$vm"; then
  ok 'named cycles isolate overlay, target, firmware runtime and TPM state'
else
  bad 'mutable release-VM state is not scoped to one named cycle'
fi

[ "$(grep -Fc 'serial=qos-install-target' "$vm")" -ge 3 ] \
  && ok 'the target keeps one stable serial through attach, install and reboot' \
  || bad 'the target disk serial changes across the install cycle'

if ! grep -Eq 'list-tags .*--creds|--creds .*list-tags' "$generation" \
  && grep -Fq 'REGISTRY_AUTH_FILE="$auth" skopeo list-tags' "$generation" \
  && grep -Fq "printf '{\"auths\":{\"%s\":{\"auth\":\"%s\"}}}" "$generation" \
  && grep -Fq 'chmod 0600 "$auth"' "$generation"; then
  ok 'registry tokens use a temporary protected auth file, never process argv'
else
  bad 'generation lookup can expose registry credentials in process argv'
fi

bash -n "$common" "$vm" "$generation" "$repo/os/dev/assert-native-arch.sh" \
  || bad 'release VM helper shell syntax is invalid'

tmp="$(mktemp -d)"
cleanup() { rm -r "$tmp"; }
trap cleanup EXIT
mkdir "$tmp/bin"
cat > "$tmp/bin/skopeo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" > "$QOS_TEST_ARGS"
printf '%s\n' "$REGISTRY_AUTH_FILE" > "$QOS_TEST_AUTH_PATH"
cp "$REGISTRY_AUTH_FILE" "$QOS_TEST_AUTH_COPY"
[ "${QOS_TEST_FAIL:-0}" != 1 ] || exit 19
printf '{"Tags":[]}\n'
EOF
chmod 0755 "$tmp/bin/skopeo"
if result="$(
  PATH="$tmp/bin:$PATH" \
  QOS_DISTROBOX='' \
  QOS_GENERATION_VERIFY_KEY="$tmp/unused.pub" \
  QOS_PUBLISH_REPO='registry.invalid/qos/qos' \
  QOS_REGISTRY_CREDS='ci-user:do-not-leak-token' \
  QOS_TEST_ARGS="$tmp/args" \
  QOS_TEST_AUTH_PATH="$tmp/auth-path" \
  QOS_TEST_AUTH_COPY="$tmp/auth-copy" \
    "$generation"
)" \
  && [ "$result" = 1 ] \
  && ! grep -Fq 'do-not-leak-token' "$tmp/args" \
  && grep -Fq 'registry.invalid/qos/qos' "$tmp/args" \
  && grep -Fq 'Y2ktdXNlcjpkby1ub3QtbGVhay10b2tlbg==' "$tmp/auth-copy" \
  && [ ! -e "$(cat "$tmp/auth-path")" ]; then
  ok 'generation lookup securely materializes and removes explicit credentials'
else
  bad 'generation lookup did not protect or clean its temporary registry auth'
fi

if ! PATH="$tmp/bin:$PATH" \
    QOS_DISTROBOX='' \
    QOS_PUBLISH_REPO='registry.invalid/qos/qos' \
    QOS_REGISTRY_CREDS='ci-user:do-not-leak-token' \
    QOS_TEST_ARGS="$tmp/fail-args" \
    QOS_TEST_AUTH_PATH="$tmp/fail-auth-path" \
    QOS_TEST_AUTH_COPY="$tmp/fail-auth-copy" \
    QOS_TEST_FAIL=1 \
    "$generation" >/dev/null 2>&1 \
  && [ -s "$tmp/fail-auth-path" ] \
  && [ ! -e "$(cat "$tmp/fail-auth-path")" ]; then
  ok 'registry auth cleanup also runs when the lookup command fails'
else
  bad 'failed generation lookup retained its temporary registry auth'
fi

cycle_output="$tmp/cycle-output"
cycle_runtime="$tmp/runtime"
mkdir -p "$cycle_runtime"
if QOS_DISTROBOX='' QOS_INSTALLER_OUTPUT="$cycle_output" \
    XDG_RUNTIME_DIR="$cycle_runtime" QOS_VM_CYCLE='release-42-x86_64' \
    "$vm" init-cycle >/dev/null \
  && [ -d "$cycle_output/vm-cycles/release-42-x86_64" ] \
  && [ -d "$cycle_runtime/qos-vm-release-42-x86_64" ]; then
  touch "$cycle_output/vm-cycles/release-42-x86_64/stale-target"
  touch "$cycle_runtime/qos-vm-release-42-x86_64/stale-tpm"
  if QOS_DISTROBOX='' QOS_INSTALLER_OUTPUT="$cycle_output" \
      XDG_RUNTIME_DIR="$cycle_runtime" QOS_VM_CYCLE='release-42-x86_64' \
      "$vm" init-cycle >/dev/null \
    && [ ! -e "$cycle_output/vm-cycles/release-42-x86_64/stale-target" ] \
    && [ ! -e "$cycle_runtime/qos-vm-release-42-x86_64/stale-tpm" ]; then
    ok 'cycle initialization removes stale disk, TPM and firmware state'
  else
    bad 'cycle initialization retained stale mutable state'
  fi
else
  bad 'cycle initialization did not create isolated storage and runtime roots'
fi
if ! QOS_DISTROBOX='' QOS_INSTALLER_OUTPUT="$cycle_output" \
    QOS_RUN='/' XDG_RUNTIME_DIR="$cycle_runtime" \
    QOS_VM_CYCLE='release-unsafe-runtime' \
    "$vm" init-cycle >/dev/null 2>&1; then
  ok 'cycle initialization refuses a non-cycle runtime path'
else
  bad 'cycle initialization accepted a broad runtime deletion target'
fi

case "$(uname -m)" in
  x86_64|amd64) wrong_arch=aarch64 ;;
  *) wrong_arch=x86_64 ;;
esac
if "$repo/os/dev/assert-native-arch.sh" "$(uname -m)" >/dev/null \
  && ! "$repo/os/dev/assert-native-arch.sh" "$wrong_arch" >/dev/null 2>&1; then
  ok 'native architecture assertion accepts the host and rejects a mismatched lane'
else
  bad 'native architecture assertion does not fail closed'
fi

exit "$fail"
