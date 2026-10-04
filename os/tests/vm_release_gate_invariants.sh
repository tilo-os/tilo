#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
common="$repo/os/dev/_common.sh"
vm="$repo/os/dev/vm.sh"
generation="$repo/os/dev/next-generation.sh"
installer_cycle="$repo/os/dev/test-installer-cycle.sh"

fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

if grep -Fq "printf -- '-machine q35,smm=on'" "$common" \
  && grep -Fq "printf -- '-machine q35'" "$common" \
  && grep -Fq "printf -- '-machine virt,gic-version=max'" "$common" \
  && ! grep -Eq -- '-machine q35[^[:space:]'\"']*accel=kvm' "$common" \
  && grep -Fq -- '-accel tcg,thread=multi -cpu max' "$common"; then
  ok 'x86 q35 and ARM virt machines do not override the selected accelerator'
else
  bad 'VM can request conflicting KVM and TCG accelerators'
fi

platform_probe() {
  local arch="$1"
  TILO_DISTROBOX='' TILO_VM_ARCH="$arch" bash -c '
    source "$1"
    printf "arch=%s\nqemu=%s\nmachine=%s\ndisplay=%s\ninput=%s\n" \
      "$TILO_VM_ARCH" "$TILO_QEMU_SYSTEM" "$(machine_args)" \
      "$(display_qemu_args 2d)" "$(input_qemu_args)"
  ' _ "$common"
}
if x86_platform="$(platform_probe amd64)" \
  && arm_platform="$(platform_probe arm64)" \
  && grep -Fq 'arch=x86_64' <<<"$x86_platform" \
  && grep -Fq 'qemu=qemu-system-x86_64' <<<"$x86_platform" \
  && grep -Fq 'machine=-machine q35' <<<"$x86_platform" \
  && grep -Fq 'display=-device virtio-vga,' <<<"$x86_platform" \
  && grep -Fq 'input=-usb -device usb-tablet' <<<"$x86_platform" \
  && grep -Fq 'arch=aarch64' <<<"$arm_platform" \
  && grep -Fq 'qemu=qemu-system-aarch64' <<<"$arm_platform" \
  && grep -Fq 'machine=-machine virt,gic-version=max' <<<"$arm_platform" \
  && grep -Fq 'display=-device virtio-gpu-pci,' <<<"$arm_platform" \
  && grep -Fxq 'input=-device qemu-xhci -device usb-tablet -device usb-kbd' <<<"$arm_platform"; then
  ok 'VM platform selection maps x86 and ARM to native QEMU machines and devices'
else
  bad 'VM platform selection can mix a release medium with another architecture'
fi

if grep -Fq 'QEMU_EFI-pflash.raw' "$common" \
  && grep -Fq 'vars-template-pflash.raw' "$common" \
  && grep -Fq 'AAVMF_CODE.fd' "$common" \
  && grep -Fq 'AAVMF_VARS.fd' "$common" \
  && grep -Fq 'aarch64 VM cycle has no verified measured-AAVMF profile yet' "$common"; then
  ok 'ARM firmware supports Fedora and Debian AAVMF layouts and rejects unproved TPM claims'
else
  bad 'ARM firmware selection is incomplete or can claim unverified measurement'
fi

if grep -Fq 'TILO_VM_CYCLE=' "$common" \
  && grep -Fq '$TILO_CYCLE_DIR/installer-work.qcow2' "$common" \
  && grep -Fq '$TILO_CYCLE_DIR/target.qcow2' "$common" \
  && grep -Fq '/tilo-vm-$TILO_VM_CYCLE' "$common" \
  && grep -Fq 'TILO_TPM_DIR="${TILO_TPM_DIR:-$TILO_RUN/tpm}"' "$common" \
  && grep -Fq 'TILO_SECBOOT_KEYS" = "none" ] && [ -z "$TILO_VM_CYCLE"' "$common" \
  && grep -Fq 'cmd_init_cycle()' "$vm" \
  && grep -Fq 'refusing to clear non-cycle runtime' "$vm" \
  && grep -Fq '|| rm -r "$TILO_CYCLE_DIR"' "$vm"; then
  ok 'named cycles isolate overlay, target, firmware runtime and TPM state'
else
  bad 'mutable release-VM state is not scoped to one named cycle'
fi

if grep -Fq 'cmd_pointer()' "$vm" \
  && grep -Fq 'mouse_move $qx $qy 1' "$vm" \
  && grep -Fq 'mouse_button $mask' "$vm" \
  && grep -Fq 'mouse_button 0' "$vm" \
  && grep -Fq 'pointer coordinate ($x,$y) is outside' "$vm"; then
  ok 'VM interaction helper maps guest pixels to the absolute tablet and releases clicks'
else
  bad 'VM interaction helper can leave buttons held or inject coordinates outside the guest mode'
fi

if grep -Fq 'cmd_wait_greeter_return()' "$vm" \
  && grep -Fq '! busctl --user status org.freedesktop.impl.portal.desktop.tilo' "$vm" \
  && grep -Fq 'systemctl is-active --quiet greetd.service' "$vm" \
  && grep -Fq 'cmd_shutdown()' "$vm" \
  && grep -Fq 'sudo systemctl poweroff' "$vm" \
  && grep -Fq 'guest did not power off' "$vm"; then
  ok 'VM helpers prove UI logout returned to greetd and bound clean guest shutdown'
else
  bad 'VM lifecycle helpers cannot distinguish logout, crash, and forced VM termination'
fi

if grep -Fq 'virtserialport,chardev=tiloconsole,name=org.tilo.console.0' "$common" \
  && [ "$(grep -Fc '$(console_qemu_args)' "$vm")" -eq 3 ] \
  && grep -Fq 'check greeter /accepts_input' "$vm" \
  && grep -Fq 'rm -f "$TILO_SERIAL" "$TILO_MON" "$TILO_PIDFILE" "$TILO_CONSOLE"' "$vm" \
  && grep -Fq 'if [ "$s" = "-" ]; then IFS= read -r s || true; fi' "$vm" \
  && ! grep -Fq 'org.tilo.console.0' "$repo/os/Containerfile.installer"; then
  ok 'the installer, target and disk boots attach the console port, login waits for the greeter to accept input, and secrets can be typed from stdin'
else
  bad 'a boot lacks the console channel, login can type before the greeter accepts input, or unlock secrets must travel on argv'
fi

[ "$(grep -Fc 'tilo-install-target tilo-install-target)' "$vm")" -ge 3 ] \
  && grep -Fq -- '-device virtio-blk-pci,drive=%s%s' "$vm" \
  && ! grep -Eq -- '-drive [^ ]*serial=' "$vm" \
  && ok 'the target keeps one stable serial through attach, install and reboot' \
  || bad 'the target disk serial changes across the install cycle'

grep -Fq 'python3 -m http.server "$browser_fixture_port"' "$installer_cycle" \
  && grep -Fq '"$vm" keys meta_l-spc' "$installer_cycle" \
  && grep -Fq "wait_screen 18-browser-offline 'Tilo[[:space:]]+browser[[:space:]]+offline[[:space:]]+ready'" "$installer_cycle" \
  && grep -Fq "'browser_offline_page_verified':True" "$repo/os/dev/run-supervised-install-lane.py" \
  && ok 'the installed-target cold boot proves Browser renders a local offline page' \
  || bad 'the installed-target cold boot lacks durable Browser render evidence'

if grep -Fq "wait_screen 09-oobe-backup 'Back[[:space:]]+up[[:space:]]+your[[:space:]]+Tilo[[:space:]]+identity'" "$installer_cycle" \
  && grep -Fq "wait_screen 10-oobe-backup-resumed 'Back[[:space:]]+up[[:space:]]+your[[:space:]]+Tilo[[:space:]]+identity'" "$installer_cycle" \
  && grep -Fq 'OOBE did not resume after an interrupted first boot' "$installer_cycle" \
  && grep -Fq '"$vm" keys tab tab tab spc tab ret' "$installer_cycle" \
  && grep -Fq "wait_screen 11-oobe-ready 'Your[[:space:]]+identity[[:space:]]+is[[:space:]]+ready'" "$installer_cycle" \
  && grep -Fq "wait_screen 15-tour-start 'Your[[:space:]]+first[[:space:]]+five[[:space:]]+minutes'" "$installer_cycle" \
  && grep -Fq '"$vm" keys tab tab ret' "$installer_cycle"; then
  ok 'graphical cycle acknowledges backup loss and completes all four tutorial pages'
else
  bad 'graphical cycle can bypass required OOBE recovery or strand the tutorial'
fi

if grep -Fq -- '--software-tpm) software_tpm=1' "$installer_cycle" \
  && grep -Fq 'TILO_SECBOOT_KEYS=/path/to/pre-enrolled-vars' "$installer_cycle" \
  && grep -Fq 'Microsoft-only store do not trust the owner-signed UKI' "$installer_cycle" \
  && grep -Fq 'TILO_INSTALLER_CYCLE_PIN:-804927' "$installer_cycle" \
  && grep -Fq '"$vm" boot-installer "$display_arg" --tpm' "$installer_cycle" \
  && grep -Fq '"$vm" boot-target "$display_arg" --tpm' "$installer_cycle" \
  && grep -Fq 'did not pass verified-boot preflight' "$installer_cycle"; then
  ok 'software-TPM graphical cycle is opt-in and fails closed without verified boot'
else
  bad 'software-TPM graphical cycle can claim a PIN path from an unverified boot'
fi

if ! grep -Eq 'list-tags .*--creds|--creds .*list-tags' "$generation" \
  && grep -Fq 'REGISTRY_AUTH_FILE="$auth" skopeo list-tags' "$generation" \
  && grep -Fq 'run --rm --interactive' "$generation" \
  && grep -Fq 'cat > "$REGISTRY_AUTH_FILE"' "$generation" \
  && ! grep -Eq 'skopeo list-tags .*2>&1' "$generation" \
  && grep -Fq "printf '{\"auths\":{\"%s\":{\"auth\":\"%s\"}}}" "$generation" \
  && grep -Fq 'chmod 0600 "$auth"' "$generation"; then
  ok 'registry tokens use a temporary protected auth file, never process argv'
else
  bad 'generation lookup can expose registry credentials in process argv'
fi

bash -n "$common" "$vm" "$generation" "$installer_cycle" "$repo/os/dev/assert-native-arch.sh" \
  || bad 'release VM helper shell syntax is invalid'

tmp="$(mktemp -d)"
cleanup() { rm -r "$tmp"; }
trap cleanup EXIT
mkdir "$tmp/bin"

mkdir -p "$tmp/aavmf-fedora" "$tmp/aavmf-debian"
: > "$tmp/aavmf-fedora/QEMU_EFI-pflash.raw"
: > "$tmp/aavmf-fedora/vars-template-pflash.raw"
: > "$tmp/aavmf-debian/AAVMF_CODE.fd"
: > "$tmp/aavmf-debian/AAVMF_VARS.fd"
if fedora_fw="$(TILO_DISTROBOX='' TILO_VM_ARCH=aarch64 TILO_RUN="$tmp/aavmf-run" \
    TILO_AAVMF_DIRS="$tmp/aavmf-fedora" "$vm" firmware --no-tpm 2>/dev/null)" \
  && debian_fw="$(TILO_DISTROBOX='' TILO_VM_ARCH=aarch64 TILO_RUN="$tmp/aavmf-run" \
    TILO_AAVMF_DIRS="$tmp/aavmf-debian" "$vm" firmware --no-tpm 2>/dev/null)" \
  && grep -Fq "code       $tmp/aavmf-fedora/QEMU_EFI-pflash.raw" <<<"$fedora_fw" \
  && grep -Fq "vars       $tmp/aavmf-fedora/vars-template-pflash.raw" <<<"$fedora_fw" \
  && grep -Fq "code       $tmp/aavmf-debian/AAVMF_CODE.fd" <<<"$debian_fw" \
  && grep -Fq "vars       $tmp/aavmf-debian/AAVMF_VARS.fd" <<<"$debian_fw" \
  && ! TILO_DISTROBOX='' TILO_VM_ARCH=aarch64 TILO_RUN="$tmp/aavmf-run" TILO_AAVMF_DIRS="$tmp/aavmf-fedora" \
    "$vm" firmware --tpm >/dev/null 2>&1; then
  ok 'ARM firmware resolution works for both package families and TPM fails closed'
else
  bad 'ARM firmware resolution does not match installed AAVMF layouts'
fi

cat > "$tmp/bin/skopeo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" > "$TILO_TEST_ARGS"
printf '%s\n' "$REGISTRY_AUTH_FILE" > "$TILO_TEST_AUTH_PATH"
cp "$REGISTRY_AUTH_FILE" "$TILO_TEST_AUTH_COPY"
[ "${TILO_TEST_NAME_UNKNOWN:-0}" != 1 ] || {
  echo 'Error listing repository tags: fetching tags list: name unknown' >&2
  exit 1
}
[ "${TILO_TEST_FAIL:-0}" != 1 ] || exit 19
printf '{"Tags":[]}\n'
EOF
chmod 0755 "$tmp/bin/skopeo"
if result="$(
  PATH="$tmp/bin:$PATH" \
  TILO_DISTROBOX='' \
  TILO_GENERATION_VERIFY_KEY="$tmp/unused.pub" \
  TILO_PUBLISH_REPO='registry.invalid/tilo/tilo' \
  TILO_REGISTRY_CREDS='ci-user:do-not-leak-token' \
  TILO_TEST_ARGS="$tmp/args" \
  TILO_TEST_AUTH_PATH="$tmp/auth-path" \
  TILO_TEST_AUTH_COPY="$tmp/auth-copy" \
    "$generation"
)" \
  && [ "$result" = 1 ] \
  && ! grep -Fq 'do-not-leak-token' "$tmp/args" \
  && grep -Fq 'registry.invalid/tilo/tilo' "$tmp/args" \
  && grep -Fq 'Y2ktdXNlcjpkby1ub3QtbGVhay10b2tlbg==' "$tmp/auth-copy" \
  && [ ! -e "$(cat "$tmp/auth-path")" ]; then
  ok 'generation lookup securely materializes and removes explicit credentials'
else
  bad 'generation lookup did not protect or clean its temporary registry auth'
fi

if ! PATH="$tmp/bin:$PATH" \
    TILO_DISTROBOX='' \
    TILO_PUBLISH_REPO='registry.invalid/tilo/tilo' \
    TILO_REGISTRY_CREDS='ci-user:do-not-leak-token' \
    TILO_TEST_ARGS="$tmp/fail-args" \
    TILO_TEST_AUTH_PATH="$tmp/fail-auth-path" \
    TILO_TEST_AUTH_COPY="$tmp/fail-auth-copy" \
    TILO_TEST_FAIL=1 \
    "$generation" >/dev/null 2>&1 \
  && [ -s "$tmp/fail-auth-path" ] \
  && [ ! -e "$(cat "$tmp/fail-auth-path")" ]; then
  ok 'registry auth cleanup also runs when the lookup command fails'
else
  bad 'failed generation lookup retained its temporary registry auth'
fi

if result="$(
  PATH="$tmp/bin:$PATH" \
  TILO_DISTROBOX='' \
  TILO_GENERATION_VERIFY_KEY="$tmp/unused.pub" \
  TILO_PUBLISH_REPO='registry.invalid/tilo/tilo' \
  TILO_REGISTRY_CREDS='ci-user:do-not-leak-token' \
  TILO_ALLOW_EMPTY_SERIES=1 \
  TILO_TEST_ARGS="$tmp/bootstrap-args" \
  TILO_TEST_AUTH_PATH="$tmp/bootstrap-auth-path" \
  TILO_TEST_AUTH_COPY="$tmp/bootstrap-auth-copy" \
  TILO_TEST_NAME_UNKNOWN=1 \
    "$generation"
)" && [ "$result" = 1 ]; then
  ok 'validation can bootstrap an authenticated repository with no published series'
else
  bad 'validation could not bootstrap a genuinely absent published series'
fi

cycle_output="$tmp/cycle-output"
cycle_runtime="$tmp/runtime"
mkdir -p "$cycle_runtime"
if TILO_DISTROBOX='' TILO_INSTALLER_OUTPUT="$cycle_output" \
    XDG_RUNTIME_DIR="$cycle_runtime" TILO_VM_CYCLE='release-42-x86_64' \
    "$vm" init-cycle >/dev/null \
  && [ -d "$cycle_output/vm-cycles/release-42-x86_64" ] \
  && [ -d "$cycle_runtime/tilo-vm-release-42-x86_64" ]; then
  touch "$cycle_output/vm-cycles/release-42-x86_64/stale-target"
  touch "$cycle_runtime/tilo-vm-release-42-x86_64/stale-tpm"
  if TILO_DISTROBOX='' TILO_INSTALLER_OUTPUT="$cycle_output" \
      XDG_RUNTIME_DIR="$cycle_runtime" TILO_VM_CYCLE='release-42-x86_64' \
      "$vm" init-cycle >/dev/null \
    && [ ! -e "$cycle_output/vm-cycles/release-42-x86_64/stale-target" ] \
    && [ ! -e "$cycle_runtime/tilo-vm-release-42-x86_64/stale-tpm" ]; then
    ok 'cycle initialization removes stale disk, TPM and firmware state'
  else
    bad 'cycle initialization retained stale mutable state'
  fi
else
  bad 'cycle initialization did not create isolated storage and runtime roots'
fi
if ! TILO_DISTROBOX='' TILO_INSTALLER_OUTPUT="$cycle_output" \
    TILO_RUN='/' XDG_RUNTIME_DIR="$cycle_runtime" \
    TILO_VM_CYCLE='release-unsafe-runtime' \
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

# A freshly written disk is flushed before a VM boots it, or the guest's first
# flush waits on the host's writeback of the whole image and the boot budget
# measures the host's disk.
if grep -Fq "inenv \"sync -- '\$QCOW'\"" "$repo/os/dev/disk-image.sh" \
   && grep -Fq "inenv \"sync -- '\$RAW'\"" "$repo/os/dev/disk-image.sh"; then
  ok 'disk-image.sh writes its image out before anything boots it'
else
  bad 'a booted disk can still be dirty in the host page cache'
fi

exit "$fail"
