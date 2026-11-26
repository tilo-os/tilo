#!/usr/bin/env bash
# The Lounge image's programs: tilo-server with the feature `agents` (the host's
# agent session crate links every web app, since the host mounts the apps' own
# components) and the Discord connector's outside worker, which the bridge
# starts beside the server. usage: lounge-binaries.sh OUT [PROFILE]
#
# `release` is what the image carries. A check that only runs these two
# programs (e2e, the tilo.sh stack) builds `lounge-check`
# instead: the same code and the same opt-level without the whole-program link,
# which is most of a run and buys nothing a check can see.
#
# The apps these programs run (the launcher, the runtimes and the packages) are
# lounge-apps.sh's. Only its `lounge-check` launcher is built with
# `dev-overrides`; no program of the image's `release` build ever is.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: lounge-binaries.sh OUT [PROFILE]'
out=${1:?$usage}
profile=${2:-release}
cd "$repo_root"
target=${CARGO_TARGET_DIR:-$repo_root/target}
rm -rf "$out"
mkdir -p "$out"
case $profile in
  release)
    # Fat LTO over one codegen unit got the server's link killed for memory;
    # thin LTO keeps these server binaries fast at a fraction of the memory and
    # time.
    export CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 ;;
  lounge-check) ;;
  *) die "unknown profile $profile: release or lounge-check" ;;
esac
bins=(tilo-server discord-worker)
cargo build --locked --profile "$profile" -p tilo-server -p discord-worker \
  --features tilo-server/agents "${bins[@]/#/--bin=}"
for bin in "${bins[@]}"; do cp "$target/$profile/$bin" "$out/"; done
# The image is a minimal system with no display: a program that needs a display
# or compositor library would not start there. Each one's needed libraries are
# read from the program itself, so a dependency that pulls one in fails here.
for bin in "${bins[@]}"; do
  needed=$(readelf -d "$out/$bin" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
  display=$(grep -E '^lib(xkbcommon|wayland|X11|xcb|EGL|GL|gbm|drm|input|seat|pixman)' <<<"$needed" || true)
  [[ -z $display ]] || die "$bin needs display libraries the image does not carry: $(tr '\n' ' ' <<<"$display")"
done
