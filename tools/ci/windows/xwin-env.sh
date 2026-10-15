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

windows_target() {
  case "$1" in
    x86_64|aarch64) printf '%s-pc-windows-msvc' "$1" ;;
    *) printf 'unsupported Windows architecture: %s\n' "$1" >&2; return 2 ;;
  esac
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
