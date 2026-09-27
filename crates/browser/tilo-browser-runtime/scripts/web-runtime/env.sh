# Sourced by every web runtime build step. The core and each app side module
# must be compiled with exactly these settings: they are part of the runtime
# ABI id (abi.mjs), and a unit built differently has other symbol names.

web_scripts=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$web_scripts/../../../../.." && pwd)
web_crate="$repo/crates/browser/tilo-browser-runtime"
web_target=wasm32-unknown-unknown
# The release profile, in its own artifact directory, with what dynamic
# linking cannot use overridden (no separate profile duplicating the graph):
#   LTO internalises the toolkit symbols side modules import; the symbol-size
#   profile needs the name section, which wasm-opt drops afterwards; debug info
#   only costs disk. panic=abort stays, and is part of the runtime ABI id.
web_profile=release
export CARGO_PROFILE_RELEASE_LTO=false
export CARGO_PROFILE_RELEASE_STRIP=none
export CARGO_PROFILE_RELEASE_DEBUG=0
# The native runtime bundle this web runtime corresponds to; tilo-app.toml's
# `runtime` names the same string.
web_runtime_name=tilo-runtime/2026.07
# Read Cargo's ordinary artifact location without changing its cache key.
web_artifacts="${CARGO_TARGET_DIR:-$repo/target}/$web_target/$web_profile"

# Every crate is position independent so side modules can reference the
# core's data through their GOT; the core itself is linked non-PIE.
unset CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS
export RUSTFLAGS="-C relocation-model=pic"
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="$RUSTFLAGS"
# The core's link keeps these toolkit crates whole: apps may call any of
# their functions, not only those the core happens to use.
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER="$web_scripts/tilo-wasm-ld"
export TILO_WEB_WHOLE_CRATES="tilo_app tilo_ui tilo_resource tilo_actions"
web_jobs=${TILO_WEB_JOBS:-4}

# The web app collection. As runtime-dylib must be the superset of what every
# thin app needs, the core is built in one Cargo invocation with these apps, so
# every crate they share with it has the features all of them need; an app
# built later in the same invocation then shares the core's units exactly.
# An app outside it that needs more is refused until it joins, which changes
# the runtime ABI id.
read -r -a web_collection <<<"${TILO_WEB_COLLECTION:-ebook word-processor}"
# Cargo arguments selecting the core, the collection and any extra packages.
web_roots() {
  local roots=(-p tilo-browser-runtime) package
  for package in "${web_collection[@]}" "$@"; do roots+=(-p "$package"); done
  printf '%s\n' "${roots[@]}"
}

web_cargo() {
  (cd "$repo" && cargo "$1" --locked --profile "$web_profile" --target "$web_target" -j "$web_jobs" "${@:2}")
}

# Package ids (name@version) in each root's graph: {"<root>": [...]}.
web_graph() {
  (cd "$repo" && cargo tree --locked --target "$web_target" -e normal --prefix none --no-dedupe -f '{p}' "$@") |
    node "$web_scripts/side-module.mjs" graph
}

need_tool() {
  command -v "$1" >/dev/null || { echo "missing tool: $1 ($2)" >&2; exit 2; }
}
