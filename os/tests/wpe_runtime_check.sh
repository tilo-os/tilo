#!/usr/bin/env bash
# os/scripts/check-wpe-runtime.sh names the packages a WPE engine is missing.
#
#   ./os/tests/wpe_runtime_check.sh
#
# Runs the check against a fake engine tree with stub ldd and dnf: an engine
# whose libraries all resolve passes, a missing soname fails naming the package
# the frozen repository says provides it, and no engine passes.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$HERE/../scripts/check-wpe-runtime.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fails=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*"; fails=$((fails + 1)); }

mkdir -p "$tmp/bin" "$tmp/root/usr/lib64" "$tmp/root/usr/libexec/wpe-webkit-2.0"
printf '\177ELF fake' > "$tmp/root/usr/lib64/libWPEWebKit-2.0.so.1"
printf '\177ELF fake' > "$tmp/root/usr/libexec/wpe-webkit-2.0/WPEWebProcess"
printf 'not an object' > "$tmp/root/usr/libexec/wpe-webkit-2.0/README"
cat > "$tmp/bin/ldd" <<'EOF'
#!/bin/sh
case "$1" in
  *libWPEWebKit*) printf '\tlibglib-2.0.so.0 => /usr/lib64/libglib-2.0.so.0 (0x1)\n'
                  [ -z "${MISSING:-}" ] || printf '\t%s => not found\n' $MISSING ;;
  *WPEWebProcess) printf '\tlibWPEWebKit-2.0.so.1 => /usr/lib64/libWPEWebKit-2.0.so.1 (0x1)\n' ;;
  *) exit 1 ;;
esac
EOF
cat > "$tmp/bin/dnf" <<'EOF'
#!/bin/sh
for a in "$@"; do
  case "$a" in
    libjxl.so.0.11*) echo libjxl ;;
    libflite.so.1*) echo flite ;;
  esac
done
EOF
chmod +x "$tmp/bin/ldd" "$tmp/bin/dnf"
run() { env PATH="$tmp/bin:$PATH" TILO_WPE_ROOT="$tmp/root" "$@" bash "$CHECK" "the builder list" "$tmp/repo" 2>&1; }

out="$(run env)" && grep -q 'all 2 WPE objects resolve' <<<"$out" \
  && ok 'an engine whose libraries resolve passes' || bad "resolving engine: $out"

out="$(run env MISSING='libjxl.so.0.11 libflite.so.1 libunknown.so.3')"; status=$?
if [ "$status" -ne 0 ] && grep -q 'Add to the builder list: flite libjxl' <<<"$out" \
   && grep -q 'libunknown.so.3  <- no package in the frozen RPM snapshot provides it' <<<"$out"; then
  ok 'missing libraries fail, naming the packages to add'
else
  bad "missing libraries: status $status: $out"
fi

rm -rf "$tmp/root/usr/lib64/libWPEWebKit-2.0.so.1" "$tmp/root/usr/libexec/wpe-webkit-2.0"
out="$(run env)" && grep -q 'no WPE engine' <<<"$out" \
  && ok 'a stage without the engine has nothing to check' || bad "no engine: $out"

[ "$fails" -eq 0 ] || exit 1
