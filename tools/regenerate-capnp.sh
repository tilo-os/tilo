#!/usr/bin/env bash
# Regenerate a crate's checked-in Cap'n Proto bindings and record their
# digests for tools/policy/check_capnp_bindings.py.
#
#   tools/regenerate-capnp.sh CRATE_DIR
#
# CRATE_DIR's schemas and source prefix are listed in
# tools/policy/capnp_bindings.toml. The bindings go to
# CRATE_DIR/src/generated/<schema>_capnp.rs. Checking them in means building
# the crate never needs the `capnp` compiler, so an app outside this
# repository builds without it.
#
# Different capnp releases encode the same schema differently, so generation
# runs the capnp the CI builder image (os/Containerfile.builder) ships, inside
# that image's base, and the capnpc-rust plugin at the workspace's pinned
# capnpc version. Change these together with that image.
set -euo pipefail

CAPNP_IMAGE="registry.fedoraproject.org/fedora:44" # capnproto 1.3.0
CAPNPC_VERSION="0.26.0"                              # workspace `capnpc`

if [ "$#" -ne 1 ]; then
    echo "usage: $0 CRATE_DIR" >&2
    exit 2
fi
repo="$(cd "$(dirname "$0")/.." && pwd)"
crate="$(cd "$1" && pwd)"
relative="${crate#"$repo"/}"
mapfile -t layout < <(python3 - "$repo/tools/policy/capnp_bindings.toml" "$relative" <<'PY'
import sys, tomllib
manifest, path = sys.argv[1:]
for entry in tomllib.load(open(manifest, "rb"))["crate"]:
    if entry["path"] == path:
        print(entry["src_prefix"])
        print("\n".join(entry["schemas"]))
        break
else:
    sys.exit(f"{manifest} lists no crate at {path}")
PY
)
prefix="${layout[0]}"
schemas=("${layout[@]:1}")

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
for schema in "${schemas[@]}"; do
    "$engine" run --rm --security-opt label=disable -v "$crate:/crate:ro" "$CAPNP_IMAGE" \
        sh -c "dnf -q -y install capnproto >/dev/null && cd /crate && capnp compile -o- --src-prefix='$prefix' '$schema'" \
        >"$scratch/request.bin"
    (cd "$scratch" && "$plugin_root/bin/capnpc-rust" <request.bin)
    generated="$(basename "$schema" .capnp | tr '-' '_')_capnp.rs"
    # capnpc-rust suppresses unused RPC dispatch parameters at interface scope.
    # Keep its output intact except for explaining that generated allowance,
    # just as handwritten Rust must explain every lint suppression.
    python3 - "$scratch/$generated" "$crate/src/generated/$generated" <<'PY'
from pathlib import Path
import sys

source, destination = map(Path, sys.argv[1:])
destination.write_text(source.read_text().replace(
    '#![allow(unused_variables)]',
    '#![allow(unused_variables, reason = "capnpc-rust RPC dispatch templates may leave interface or method parameters unused")]',
))
PY
    echo "$crate/src/generated/$generated"
done

python3 "$repo/tools/policy/check_capnp_bindings.py" --record "$relative"
