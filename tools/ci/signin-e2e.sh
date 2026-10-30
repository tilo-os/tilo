#!/usr/bin/env bash
# One-password sign-in, end to end (docs/security/sign-in.md §3, §4), as the signin-e2e-<arch>
# check runs it on x86_64 or aarch64: the development installer medium dev-installer-medium-<arch>
# built from this commit is installed onto a blank disk in a KVM guest with a software TPM, and
# the installed system is booted again and again through first boot, migration at the greeter,
# the boot sign-in screen, wrong entries, the PAM surfaces and the recovery key
# (tools/ci/signin_e2e.py). A second install uses the installer's separate disk secret.
#
#   signin-e2e.sh MEDIUM OUT [signin_e2e.py ARGS...]
#
# ARGS go to signin_e2e.py after --medium/--out/--storage, such as `--variant rollback
# --pre-pam-tilo-image ARCHIVE`. In a check they come from its dispatch inputs instead:
# T_INPUT_VARIANTS (variants separated by spaces; empty for the default two) and
# T_INPUT_PRE_PAM_TILO_IMAGE.
#
# OUT receives report.md / report.json, the boot sign-in and greeter screenshots, and per-boot
# serial logs and journals, each searched for every secret the run used.
#
# tools/ci/signin-e2e-builder.sh builds the image and the medium and runs this on a machine
# without t, such as a remote build host.
set -euo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."

medium=${1:?usage: signin-e2e.sh MEDIUM OUT [ARGS...]}
out=${2:?usage: signin-e2e.sh MEDIUM OUT [ARGS...]}
shift 2
arch=$(uname -m)
case $arch in
  x86_64|aarch64) ;;
  *) echo "signin-e2e runs on x86_64 or aarch64, not $arch" >&2; exit 2 ;;
esac
[[ -f $medium ]] || { echo "no installer medium at $medium: the check needs dev-installer-medium-$arch" >&2; exit 2; }
[[ -c /dev/kvm ]] || { echo 'no /dev/kvm: the check needs requires = ["/dev/kvm"]' >&2; exit 2; }

args=("$@")
if ((${#args[@]} == 0)); then
  for variant in ${T_INPUT_VARIANTS:-}; do
    args+=(--variant "$variant")
  done
  [[ -n ${T_INPUT_PRE_PAM_TILO_IMAGE:-} ]] && args+=(--pre-pam-tilo-image "$T_INPUT_PRE_PAM_TILO_IMAGE")
fi

# QEMU, firmware and swtpm for vm.sh; tesseract and ImageMagick read the boot screens. The
# check's setup has them already (t.toml); without it, they come from dnf.
bash tools/ci/install-packages.sh tools/ci/packages/signin-e2e.txt "tools/ci/packages/signin-e2e-$arch.txt"
python3 os/dev/probe-kvm.py "$arch" || { echo 'this node cannot create a KVM VM' >&2; exit 1; }

rm -rf "$out"
mkdir -p "$out"
storage=/var/tmp/signin-e2e
rm -rf "$storage"
mkdir -p "$storage"
status=0
TMPDIR=/var/tmp python3 -I tools/ci/signin_e2e.py --medium "$medium" --out "$out" --storage "$storage" \
  "${args[@]}" || status=$?
rm -rf "$storage"
exit "$status"
