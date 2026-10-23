#!/usr/bin/env bash
# The CI fast image before it is published: the toolchain, the audit tools and every tool the
# security-boundary invariants need (so none of them skips).
# usage: ci_fast_images.sh CI_FAST_ARCHIVE (an OCI archive, as tools/ci/publish/oci-build.sh
# writes it). Runs in the buildah image.
set -euo pipefail
fast=${1:?usage: ci_fast_images.sh CI_FAST_ARCHIVE}
run=$(dirname -- "${BASH_SOURCE[0]}")/../../tools/ci/publish/oci-run.sh
bash "$run" "$fast" bash -euo pipefail -c '
  . /etc/os-release
  printf "%s %s\n" "$NAME" "$VERSION_ID"
  cargo --version
  rustc --version
  cargo fmt --version
  cargo deny --version
  cargo machete --version
  cargo hakari --version
  sccache --version
  for tool in git python3 jq rg node gpg curl diff which; do
    command -v "$tool"
  done
  for tool in cc modinfo clang ld.lld rpmbuild rpmkeys systemd-escape fc-match bwrap mkfs.vfat udevadm sfdisk partx blkid blockdev findmnt flock lsblk losetup; do
    command -v "$tool"
  done
  bwrap --version
  python3 -c "import PIL"
  node -e "process.exit([...new Intl.Segmenter(\"en\", {granularity: \"grapheme\"}).segment(\"e\u0301x\")].length === 2 ? 0 : 1)"
  fc-match -f "%{family}" "Noto Sans" | grep -Fq "Noto Sans"
  test -n "$(find /etc/pki/rpm-gpg -maxdepth 1 -name "RPM-GPG-KEY-fedora-*-primary" -print -quit)"'
