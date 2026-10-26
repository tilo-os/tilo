#!/usr/bin/env bash
# One-password sign-in, end to end (docs/security/sign-in.md §3, §4), as t's signin-e2e-x86_64
# runs it: the development installer medium dev-installer-medium-x86_64 built from this commit
# is installed onto a blank disk in a KVM guest with a software TPM, and the installed system is
# booted again and again through first boot, migration at the greeter, the boot sign-in screen,
# wrong entries, the PAM surfaces and the recovery key (tools/ci/signin_e2e.py). A second install
# uses the installer's separate disk secret.
#
#   signin-e2e.sh MEDIUM OUT
#
# OUT receives report.md / report.json, the boot sign-in and greeter screenshots, and per-boot
# serial logs and journals, each searched for every secret the run used.
set -euo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."

medium=${1:?usage: signin-e2e.sh MEDIUM OUT}
out=${2:?usage: signin-e2e.sh MEDIUM OUT}
[[ $(uname -m) == x86_64 ]] || { echo "signin-e2e runs on x86_64, not $(uname -m)" >&2; exit 2; }
[[ -f $medium ]] || { echo "no installer medium at $medium: the check needs dev-installer-medium-x86_64" >&2; exit 2; }
[[ -c /dev/kvm ]] || { echo 'no /dev/kvm: the check needs requires = ["/dev/kvm"]' >&2; exit 2; }

# QEMU, firmware and swtpm for vm.sh; tesseract and ImageMagick read the boot screens. The
# check's setup layer has them already (t.toml); without it, they come from dnf.
bash tools/ci/install-packages.sh tools/ci/packages/signin-e2e.txt
python3 os/dev/probe-kvm.py x86_64 || { echo 'this node cannot create a KVM VM' >&2; exit 1; }

rm -rf "$out"
mkdir -p "$out"
storage=/var/tmp/signin-e2e
rm -rf "$storage"
mkdir -p "$storage"
status=0
TMPDIR=/var/tmp python3 -I tools/ci/signin_e2e.py --medium "$medium" --out "$out" --storage "$storage" || status=$?
rm -rf "$storage"
exit "$status"
