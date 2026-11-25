# Sourced by the Windows cross-build scripts: the MSVC CRT and Windows SDK
# versions cargo-xwin uses, pinned so a build means the same thing whenever it
# runs, and where it finds them.
#
# cargo-xwin downloads them from Microsoft (accepting Microsoft's license for
# the build, as Visual Studio's installer would). They are not redistributable,
# so they never go into an image or a release: the windows-sdk checks keep them
# as an output in t's store, and builds unpack them into their own directory.
# Changing a version here changes those checks' inputs, so the next build
# downloads the new ones.

# Visual Studio's release channel manifest, and the versions in it.
export XWIN_VERSION=17
export XWIN_SDK_VERSION=10.0.26100
export XWIN_CRT_VERSION=14.44.17.14
export XWIN_ACCEPT_LICENSE=1

# The channel manifest this repository holds is what the download starts from, and the
# package manifest it names is fetched once and checked against this hash. cargo-xwin
# would otherwise follow Microsoft's moving release channel and take whatever package
# manifest it serves that day; both pins make a download the bytes this checkout names,
# so a repeat run of a windows-sdk check is served from t's cache, and a new SDK is a
# commit that replaces the manifest and this hash.
xwin_here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
XWIN_CHANNEL_MANIFEST="$xwin_here/vs-$XWIN_VERSION.chman"
XWIN_PACKAGE_MANIFEST_SHA256=d1d8109899558618035724949673f69f8c1b5704f542a2e76f31d87e425d5710

windows_target() {
  case "$1" in
    x86_64|aarch64) printf '%s-pc-windows-msvc' "$1" ;;
    *) printf 'unsupported Windows architecture: %s\n' "$1" >&2; return 2 ;;
  esac
}

# Put the pinned channel and package manifests where cargo-xwin reads them from,
# so the only thing it downloads is the SDK's own packages, each checked against
# the package manifest's hashes.
seed_windows_sdk_manifests() {
  local cache="$1" dl declared url named channel
  # cargo-xwin keeps its work in the cache's own `xwin` directory.
  dl="$cache/xwin/dl"
  mkdir -p "$dl"
  install -m 0644 "$XWIN_CHANNEL_MANIFEST" "$dl/manifest_$XWIN_VERSION.json"
  # The name xwin reads the package manifest by is the hash the channel names, which
  # Microsoft never corrects for the file it serves; the bytes are held to the hash above.
  channel="$(python3 -I -c '
import json, sys
items = json.load(open(sys.argv[1], encoding="utf-8"))["channelItems"]
payload = next(payload for item in items if item.get("type") == "Manifest"
               for payload in item["payloads"])
print(payload["sha256"], payload["url"])' "$XWIN_CHANNEL_MANIFEST")"
  declared="${channel%% *}"
  url="${channel#* }"
  named="$dl/pkg_manifest_$declared.vsman"
  [ -f "$named" ] && return 0
  curl -sSfL -o "$dl/package.vsman.part" "$url"
  printf '%s  %s\n' "$XWIN_PACKAGE_MANIFEST_SHA256" "$dl/package.vsman.part" |
    sha256sum --check --strict --quiet -
  mv -- "$dl/package.vsman.part" "$named"
}

# Unpack the SDK archive a windows-sdk check produced and point cargo-xwin at
# it, so it downloads nothing.
use_windows_sdk() {
  local arch="$1" archive="$2" dir
  [ -s "$archive" ] || { printf 'no Windows SDK archive at %s\n' "$archive" >&2; return 1; }
  (cd "$(dirname -- "$archive")" && sha256sum --check --strict --quiet "$(basename -- "$archive").sha256")
  dir="${TMPDIR:-/tmp}/xwin-$arch"
  rm -rf -- "$dir"
  mkdir -p "$dir"
  zstd -dc --quiet "$archive" | tar -xf - -C "$dir"
  export XWIN_CACHE_DIR="$dir" XWIN_ARCH="$arch"
}
