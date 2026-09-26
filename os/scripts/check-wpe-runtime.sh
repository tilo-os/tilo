#!/bin/bash
# Fail, naming packages, when the WPE engine in this stage links a library the
# stage does not have.
#
#   check-wpe-runtime.sh LIST-NAME [REPO-DIR]
#
# os/Containerfile copies the engine the pinned WPE buildroot built (the
# wpe-builder stage) into two stages that must then carry every library it
# records in DT_NEEDED: the builder, which links the browser against it, and
# the rootfs, which runs it. Both install those libraries from explicit
# package lists, and a buildroot refresh can build the engine with a feature
# neither list knows about (JPEG XL and flite arrived that way). Without this
# check that surfaces as hundreds of undefined references while linking, or as
# a browser that dies at exec on a booted machine.
#
# Every ELF object of the engine (libWPE*, the WebKit helper processes and
# injected bundles) must resolve all of its libraries here. For each soname
# that does not, the frozen RPM repository at REPO-DIR (default /repo) is
# asked which package provides it, and the failure names those packages and
# the list (LIST-NAME) to add them to. With TILO_WITH_BROWSER=0 there is no
# engine and nothing to check.
set -euo pipefail

list=${1:?name the package list to extend, e.g. "the builder stage's dnf list"}
repo=${2:-/repo}
# The filesystem the engine is installed in (a test points it elsewhere).
root=${TILO_WPE_ROOT:-}

objects=()
while IFS= read -r -d '' path; do
  # ELF only: the engine directories also hold data files and scripts.
  if [ "$(head -c 4 "$path" 2>/dev/null | od -An -c | tr -d ' ')" = '177ELF' ]; then
    objects+=("$path")
  fi
done < <(find "$root/usr/lib64" "$root/usr/libexec" -maxdepth 3 \
           \( -path "$root/usr/lib64/libWPE*" -o -path "$root/usr/lib64/wpe-webkit-*" \
              -o -path "$root/usr/libexec/wpe-webkit-*" \) \
           -type f -print0 2>/dev/null)
if [ "${#objects[@]}" -eq 0 ]; then
  echo "check-wpe-runtime: no WPE engine in this stage; nothing to check"
  exit 0
fi

missing=$(for object in "${objects[@]}"; do
            ldd "$object" 2>/dev/null | awk '/=> not found/ { print $1 }'
          done | sort -u)
if [ -z "$missing" ]; then
  echo "check-wpe-runtime: all ${#objects[@]} WPE objects resolve their libraries"
  exit 0
fi

printf 'check-wpe-runtime: the WPE engine links libraries this stage does not have:\n' >&2
packages=()
while IFS= read -r soname; do
  provider=$(dnf repoquery -q --setopt=reposdir="$repo" --whatprovides "${soname}()(64bit)" \
               --qf '%{name}\n' 2>/dev/null | sort -u | head -1 || true)
  printf '  %s  <- %s\n' "$soname" "${provider:-no package in the frozen RPM snapshot provides it}" >&2
  [ -z "$provider" ] || packages+=("$provider")
done <<<"$missing"
if [ "${#packages[@]}" -gt 0 ]; then
  printf 'Add to %s: %s\n' "$list" "$(printf '%s\n' "${packages[@]}" | sort -u | tr '\n' ' ')" >&2
fi
printf 'The pinned WPE buildroot (os/wpe-buildroot-snapshot.env) built the engine with these features.\n' >&2
exit 1
