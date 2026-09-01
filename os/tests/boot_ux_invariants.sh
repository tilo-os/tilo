#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
uki="$root/os/dev/uki.sh"
kargs="$root/os/usr/lib/bootc/kargs.d/03-boot.toml"
containerfile="$root/os/Containerfile"

grep -Fq -- '--splash /out/qos-splash.bmp' "$uki"
grep -Fq 'make-boot-splash.py /out/qos-splash.bmp' "$uki"
grep -Fq 'COPY os/dev/make-boot-splash.py' "$containerfile"

for arg in quiet loglevel=3 systemd.show_status=auto rd.udev.log_level=3 vt.global_cursor_default=0; do
  grep -Fq "\"$arg\"" "$kargs" || {
    printf 'missing boot UX argument: %s\n' "$arg" >&2
    exit 1
  }
done

mkdir -p "$root/target"
tmp="$(mktemp -d "$root/target/boot-ux.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
python3 "$root/os/dev/make-boot-splash.py" "$tmp/splash.bmp"
python3 - "$tmp/splash.bmp" <<'PY'
import struct, sys
data = open(sys.argv[1], "rb").read()
assert data[:2] == b"BM"
assert struct.unpack_from("<ii", data, 18) == (1024, 768)
assert len(set(data[54:])) > 24, "splash is visually empty"
PY

printf 'boot UX invariants hold\n'
