#!/usr/bin/env bash
# Regenerate src/generated/collab_capnp.rs from schema/collab.capnp and
# update the schema fingerprint build.rs checks.
#
# The checked-in bindings depend on the exact schema compiler: different
# capnp releases encode the same schema differently. Generation therefore
# runs the capnp that the CI builder image (os/Containerfile.builder) ships,
# inside that image's base, and the capnpc-rust plugin at the workspace's
# pinned capnpc version. Change these together with that image.
set -euo pipefail

CAPNP_IMAGE="registry.fedoraproject.org/fedora:44" # capnproto 1.3.0
CAPNPC_VERSION="0.26.0"                              # workspace `capnpc`

here="$(cd "$(dirname "$0")" && pwd)"
engine="$(command -v podman || command -v docker)"
plugin_root="${XDG_CACHE_HOME:-$HOME/.cache}/tilo-capnpc-$CAPNPC_VERSION"
if [ ! -x "$plugin_root/bin/capnpc-rust" ]; then
    cargo install --locked --quiet "capnpc@$CAPNPC_VERSION" --root "$plugin_root"
fi

# The compiler emits a CodeGeneratorRequest; the host-built plugin turns it
# into Rust. Only the request crosses the container boundary.
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
"$engine" run --rm --security-opt label=disable -v "$here/schema:/schema:ro" "$CAPNP_IMAGE" \
    sh -c 'dnf -q -y install capnproto >/dev/null && capnp compile -o- --src-prefix=/schema /schema/collab.capnp' \
    >"$scratch/request.bin"
(cd "$scratch" && "$plugin_root/bin/capnpc-rust" <request.bin)
cp "$scratch/collab_capnp.rs" "$here/src/generated/collab_capnp.rs"

python3 - "$here" <<'PY'
import pathlib, re, sys
root = pathlib.Path(sys.argv[1])
digest = 0xcbf29ce484222325
for byte in (root / "schema/collab.capnp").read_bytes():
    digest = ((digest ^ byte) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
text = f"{digest:016x}"
literal = "0x" + "_".join(text[i:i + 4] for i in range(0, 16, 4))
build = root / "build.rs"
build.write_text(re.sub(r"const EXPECTED_SCHEMA_FNV1A: u64 = 0x[0-9a-f_]+;",
                        f"const EXPECTED_SCHEMA_FNV1A: u64 = {literal};", build.read_text()))
PY
