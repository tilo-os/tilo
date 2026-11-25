#!/usr/bin/env bash
# A person's use of the Windows daemon, walked under Wine: a tilolang app installed, opened,
# looked at, acted on and read back from History, and a clip played.
#
#   walk.sh build <sdk> <runtime> <out>   the walk's programs and the app it installs
#   walk.sh run <walk> <out>              the walk itself; its pictures land in <out>
#
# <runtime> is what build/windows-runtime-x86_64 made: its runtimes/ folder is the tilolang
# runtime the daemon carries. The daemon is the test build that may start a tilolang process
# unconfined, as Wine has no AppContainer: it never ships, so it is built here and not by
# build.sh (tools/policy/check_test_support_shipping.py).
set -euo pipefail

# Wine describes a file's access from its Unix mode bits: group and other bits become entries for
# groups and Everyone, and a directory made under one inherits them, whatever protected list the
# program sets. A Windows profile is private to its account, so everything Wine makes here is too.
umask 077

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
mode="${1:?build or run}"

case "$mode" in
  build)
    sdk="${2:?SDK archive}" runtime="${3:?the directory the runtime build made}" out="${4:?output directory}"
    . "$here/xwin-env.sh"
    target="$(windows_target x86_64)"
    use_windows_sdk x86_64 "$sdk"
    cargo xwin build --locked -p tilo-daemon --features test-support \
      --bin tilo-runtime-unconfined-test --target "$target"
    cargo xwin build --locked -p install --bin tilo-install --target "$target"
    rm -rf -- "$out" && mkdir -p "$out/apps"
    for program in tilo-runtime-unconfined-test tilo-install; do
      install -m 0644 "${CARGO_TARGET_DIR:-target}/$target/debug/$program.exe" "$out/$program.exe"
    done
    # The tilolang runtime, carried beside the daemon as a shipping one carries it.
    cp -a -- "$runtime/runtimes" "$out/runtimes"
    # The app, built and signed as a person's own app is: by a publisher no one vouches for.
    cargo build --locked --release -p tilolang-cli --bin tilolang \
      -p tilo-runtime-bundle --bin tilo-runtime-bundle
    tools="${CARGO_TARGET_DIR:-target}/release"
    "$tools/tilo-runtime-bundle" key generate --out "$out/apps/.publisher.json"
    "$tools/tilolang" build --locked apps/tl/video --search apps/tl --out "$out/apps" \
      --identity "$out/apps/.publisher.json"
    rm -f -- "$out/apps/.publisher.json"
    [ -f "$out/apps/video.tapp" ] || { printf 'apps/tl/video built no video.tapp\n' >&2; exit 1; }
    ;;
  run)
    walk="${2:?the directory the walk build made}" out="${3:?output directory}"
    # On disk, as wine-test.sh keeps its prefix: Wine reports a tmpfs drive as a virtual disk.
    root=/target/wine
    mkdir -p "$root" 2>/dev/null || root="${TMPDIR:-/tmp}"
    export WINEPREFIX="$root/tilo-walk-wine"
    rm -rf -- "$WINEPREFIX"
    bash "$here/wine-prefix.sh"
    mkdir -p "$out"
    # The clip a person drops on Video: VP8 in WebM, which the carried runtime decodes.
    clip="$WINEPREFIX/clip.webm"
    gst-launch-1.0 -q videotestsrc num-buffers=90 pattern=smpte \
      ! video/x-raw,width=320,height=240,framerate=30/1 ! vp8enc ! webmmux \
      ! filesink location="$clip"
    python3 -I "$here/walk.py" --walk "$walk" --clip "$clip" --out "$out"
    ;;
  *) printf 'unknown mode: %s\n' "$mode" >&2; exit 2 ;;
esac
