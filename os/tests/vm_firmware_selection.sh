#!/usr/bin/env bash
# os/dev/_common.sh firmware_setup finds OVMF under Fedora's and the
# Debian-family names, and never claims measurement it has not verified.
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
fail=0
bad() { printf 'FAIL  %s\n' "$1" >&2; fail=1; }
ok() { printf 'ok    %s\n' "$1"; }

# Run firmware_setup in a subshell against DIR; print CODE VARS FMT MEASURES SMM.
select_in() {
  TILO_DISTROBOX="" TILO_RUN="$work/run" TILO_VM_ARCH=x86_64 TILO_OVMF_DIRS="$1" \
    TILO_TPM="${2:-0}" TILO_VM_S3="${3:-0}" bash -c '
      source "$0/os/dev/_common.sh"
      firmware_setup >/dev/null 2>&1
      printf "%s %s %s %s %s\n" "$(basename "$TILO_FW_CODE")" "$(basename "$TILO_FW_VARS")" \
        "$TILO_FW_FMT" "$TILO_FW_MEASURES" "$TILO_FW_SMM"' "$repo"
}

debian="$work/debian"; mkdir -p "$debian"
touch "$debian/OVMF_CODE_4M.fd" "$debian/OVMF_CODE_4M.secboot.fd" "$debian/OVMF_VARS_4M.fd" "$debian/OVMF_VARS_4M.ms.fd"
[[ $(select_in "$debian") == "OVMF_CODE_4M.secboot.fd OVMF_VARS_4M.fd raw 0 1" ]] \
  && ok 'Debian/Ubuntu 4 MB raw OVMF is found, with its own variable store and SMM' \
  || bad "Debian/Ubuntu OVMF selection: $(select_in "$debian")"
[[ -z $(select_in "$debian" 1 | awk '{print $1}') ]] \
  && ok 'a TPM is refused on the unverified Debian-family build' \
  || bad 'a TPM was attached to the unverified Debian-family build'
[[ $(select_in "$debian" 0 1) == "OVMF_CODE_4M.fd OVMF_VARS_4M.fd raw 0 0" ]] \
  && ok 'suspend-to-RAM takes the Debian/Ubuntu build without SMM' \
  || bad "Debian/Ubuntu OVMF selection for S3: $(select_in "$debian" 0 1)"

fedora="$work/fedora"; mkdir -p "$fedora"
touch "$fedora/OVMF_CODE_4M.secboot.qcow2" "$fedora/OVMF_VARS_4M.qcow2" "$fedora/OVMF_CODE_4M.fd"
[[ $(select_in "$fedora") == "OVMF_CODE_4M.secboot.qcow2 OVMF_VARS_4M.qcow2 qcow2 1 1" ]] \
  && ok "Fedora's measuring qcow2 build still wins" \
  || bad "Fedora OVMF selection: $(select_in "$fedora")"
touch "$fedora/OVMF_CODE_4M.qcow2"
[[ $(select_in "$fedora" 0 1) == "OVMF_CODE_4M.qcow2 OVMF_VARS_4M.qcow2 qcow2 1 0" ]] \
  && ok "suspend-to-RAM takes Fedora's measuring build without SMM" \
  || bad "Fedora OVMF selection for S3: $(select_in "$fedora" 0 1)"

smm_only="$work/smm-only"; mkdir -p "$smm_only"
touch "$smm_only/OVMF_CODE_4M.secboot.fd" "$smm_only/OVMF_VARS_4M.fd"
[[ -z $(select_in "$smm_only" 0 1 | awk '{print $1}') ]] \
  && ok 'suspend-to-RAM refuses a firmware that has only SMM builds' \
  || bad "S3 booted an SMM build: $(select_in "$smm_only" 0 1)"

empty="$work/empty"; mkdir -p "$empty"
[[ -z $(select_in "$empty" | awk '{print $1}') ]] \
  && ok 'no firmware stops the boot' || bad 'a boot went ahead without firmware'
exit "$fail"
