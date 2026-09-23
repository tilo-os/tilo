#!/usr/bin/env bash
# Exercise the compressed-module path without requiring hardware acceleration.
set -euo pipefail
root=$(mktemp -d)
trap 'rm -rf -- "$root"' EXIT
release=7.2.5-test
stage="$root/stage"
mkdir -p "$stage/lib/modules/$release/kernel/drivers/net" "$root/bin"
printf 'synthetic kernel image\n' > "$stage/lib/modules/$release/vmlinuz"
python3 - "$root/dummy.ko" <<'PY'
import struct, sys
from pathlib import Path
unsigned = b"\x7fELF" + bytes(range(96))
signature = b"test-signature"
descriptor = b"\x00" * 8 + struct.pack(">I", len(signature))
Path(sys.argv[1]).write_bytes(
    unsigned + signature + descriptor + b"~Module signature appended~\n"
)
PY
zstd --quiet --compress --stdout "$root/dummy.ko" \
  > "$stage/lib/modules/$release/kernel/drivers/net/dummy.ko.zst"
tar -czf "$root/stage.tar.gz" -C "$stage" .
cat > "$root/bin/qemu-system-$(uname -m)" <<'SH'
#!/usr/bin/env bash
printf 'TILO_KERNEL_USERSPACE_OK release=7.2.5-test machine=%s\n' "$(uname -m)"
printf 'TILO_MODULE_SIGNATURE_ENFORCEMENT_OK\n'
SH
chmod +x "$root/bin/qemu-system-$(uname -m)"
PATH="$root/bin:$PATH" bash "$(dirname "$0")/boot-smoke.sh" \
  "$root/stage.tar.gz" "$release" "$root/evidence"
cmp "$root/dummy.ko" "$root/evidence/rootfs/signed.ko"
test -s "$root/evidence/rootfs/unsigned.ko"
test -s "$root/evidence/rootfs/tampered.ko"
python3 - "$root/evidence/inputs.json" <<'PY'
import json, sys
evidence = json.load(open(sys.argv[1], encoding="utf-8"))
assert evidence["release"] == "7.2.5-test"
assert len(evidence["signedModuleSha256"]) == 64
PY
