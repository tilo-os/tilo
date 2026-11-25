#!/usr/bin/env bash
# Package a tilolang app the way an image does: `tilolang build` compiles the
# package, stages its bundle (its module, the ids it was built with, its
# surface) and signs it as a `.tapp`. It is the same function an image's pack
# calls for every app of `apps/tl` (os/scripts/pack-apps.sh through
# tools/apps/tilolang_packages.sh), so what a test reads an action out of is the
# package an image installs rather than one the test compiled itself.
#
#   tools/ci/pack-model-tapp.sh OUT_DIR APP
#
# Writes OUT_DIR/<app id>.tapp. The publisher key is a throwaway one made here.
#
#   t test test-packaged-model
set -euo pipefail
cd "$(dirname "$0")/../.."

out=${1:?usage: pack-model-tapp.sh OUT_DIR APP}
app=${2:?usage: pack-model-tapp.sh OUT_DIR APP}
dir=apps/tl/$app
[ -f "$dir/app.tl" ] || { printf 'apps/tl/%s declares no app.tl\n' "$app" >&2; exit 1; }
id=$(sed -n 's/^[[:space:]]*app[[:space:]]\{1,\}\([^[:space:]]\{1,\}\).*/\1/p' "$dir/app.tl" | head -1)
[ -n "$id" ] || { printf 'apps/tl/%s/app.tl names no app id\n' "$app" >&2; exit 1; }
mkdir -p "$out"
out=$(realpath "$out")
target=${CARGO_TARGET_DIR:-$PWD/target}
bin=$target/debug
cargo=${CARGO:-cargo}

# The packing tools, as an image build has them: the compiler and signer of the
# package, and the reader that checks what came out.
"$cargo" build --locked -p tilolang-cli -p tilo-app-package-cli -p tilo-runtime-bundle

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
identity=$work/publisher.json
"$bin/tilo-runtime-bundle" key generate --out "$identity"

# The release pack's own function, which an image build sources too.
# shellcheck source=/dev/null
. tools/apps/tilolang_packages.sh
tilolang_build_package "$bin/tilolang" "$PWD" "$dir" "$id" "$out" "$identity" \
    0.1.0 tilo-runtime/2026.07 "$id"

tapp=$out/$id.tapp
"$bin/tilo-app-package" inspect "$tapp" > "$out/inspect.txt"
grep -qx 'signed: yes' "$out/inspect.txt" \
    || { printf 'the packed package is not signed:\n' >&2; cat "$out/inspect.txt" >&2; exit 1; }
for entry in app.wasm surface.json; do
    grep -q "^  $entry  (" "$out/inspect.txt" \
        || { printf 'the packed package carries no %s:\n' "$entry" >&2; cat "$out/inspect.txt" >&2; exit 1; }
done
printf 'packed %s\n' "$tapp"
