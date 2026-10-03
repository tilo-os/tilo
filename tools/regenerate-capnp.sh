#!/usr/bin/env bash
# Regenerate a crate's checked-in Cap'n Proto bindings and the schema
# fingerprint its build.rs checks.
#
#   tools/regenerate-capnp.sh CRATE_DIR SRC_PREFIX SCHEMA...
#
# SRC_PREFIX and each SCHEMA are relative to CRATE_DIR. The bindings go to
# CRATE_DIR/src/generated/<schema>_capnp.rs. Checking them in means building
# the crate never needs the `capnp` compiler, so an app outside this
# repository builds without it.
#
# Different capnp releases encode the same schema differently, so generation
# runs the capnp the CI builder image (os/Containerfile.builder) ships, inside
# that image's base, and the capnpc-rust plugin at the workspace's pinned
# capnpc version. Change these together with that image, and with
# crates/runtime/runtime-collab/regenerate.sh.
set -euo pipefail

CAPNP_IMAGE="registry.fedoraproject.org/fedora:42" # capnproto 1.0.1
CAPNPC_VERSION="0.26.0"                              # workspace `capnpc`

if [ "$#" -lt 3 ]; then
    echo "usage: $0 CRATE_DIR SRC_PREFIX SCHEMA..." >&2
    exit 2
fi
crate="$(cd "$1" && pwd)"
prefix="$2"
shift 2

engine="$(command -v podman || command -v docker)"
plugin_root="${XDG_CACHE_HOME:-$HOME/.cache}/tilo-capnpc-$CAPNPC_VERSION"
if [ ! -x "$plugin_root/bin/capnpc-rust" ]; then
    "${CARGO:-cargo}" install --locked --quiet "capnpc@$CAPNPC_VERSION" --root "$plugin_root"
fi

# The compiler emits a CodeGeneratorRequest; the host-built plugin turns it
# into Rust. Only the request crosses the container boundary.
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$crate/src/generated"
for schema in "$@"; do
    "$engine" run --rm --security-opt label=disable -v "$crate:/crate:ro" "$CAPNP_IMAGE" \
        sh -c "dnf -q -y install capnproto >/dev/null && cd /crate && capnp compile -o- --src-prefix='$prefix' '$schema'" \
        >"$scratch/request.bin"
    (cd "$scratch" && "$plugin_root/bin/capnpc-rust" <request.bin)
    generated="$(basename "$schema" .capnp | tr '-' '_')_capnp.rs"
    cp "$scratch/$generated" "$crate/src/generated/$generated"
    echo "$crate/src/generated/$generated"
done

python3 - "$crate" "$@" <<'PY'
import pathlib, re, sys
crate = pathlib.Path(sys.argv[1])
schemas = sys.argv[2:]
generated = [f"src/generated/{pathlib.Path(s).stem.replace('-', '_')}_capnp.rs" for s in schemas]

def fingerprint(paths):
    digest = 0xcbf29ce484222325
    for path in paths:
        for byte in (crate / path).read_bytes():
            digest = ((digest ^ byte) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    text = f"{digest:016x}"
    return "0x" + "_".join(text[i:i + 4] for i in range(0, 16, 4))

build = crate / "build.rs"
source = build.read_text()
for name, paths in (("SCHEMA", schemas), ("BINDINGS", generated)):
    pattern = rf"const EXPECTED_{name}_FNV1A: u64 = 0x[0-9a-f_]+;"
    if not re.search(pattern, source):
        sys.exit(f"{build} has no EXPECTED_{name}_FNV1A constant to update")
    source = re.sub(pattern, f"const EXPECTED_{name}_FNV1A: u64 = {fingerprint(paths)};", source)
build.write_text(source)
PY
