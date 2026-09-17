#!/usr/bin/env bash
# Build the tilo-session RPM: the Tilo compositor as a Wayland session on a host
# that runs something else the rest of the time.
#
#   dist/rpm/build-rpm.sh              build binaries, then the RPM
#   dist/rpm/build-rpm.sh --no-cargo   package binaries that are already built
#
# Output lands in dist/rpm/build/RPMS/<native-rpm-architecture>/.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
out=${TILO_PACKAGE_OUT:-$repo_root/dist/rpm/build}
name=tilo-session
version=$(<"$repo_root/dist/VERSION")

run_cargo=1
[[ ${1:-} == --no-cargo ]] && run_cargo=0

say() { printf '\n== %s\n' "$*"; }

# ---------------------------------------------------------------------------
# 1. The binaries.
# ---------------------------------------------------------------------------
# `shell` is built with --no-default-features on purpose. Its default set
# includes `hdr-decode`, which links libavcodec; the build container and the
# host do not necessarily carry the same libavcodec soname (Fedora 43 ships
# .so.61, Fedora 44 ships .so.62), and a compositor that cannot load is a
# session that fails at the login screen. Dropping the default set also drops
# `hosted-test-control`, which a real login session never reaches anyway.
#
# What is lost: receive-side HEVC Main10 decode, i.e. viewing a peer's HDR
# stream. Everything else, including sending, is unaffected.
if (( run_cargo )); then
    say "cargo build --release (launcher, unlock helper)"
    ( cd "$repo_root" && cargo build --release --locked -p launcher -p tilo-unlock-helper )
    say "cargo build --release (compositor, no default features)"
    # `shell` also owns developer tools such as `shots` and `tilo-control`.
    # Building the package without an explicit target compiles and LTO-links
    # those large binaries even though stage-payload never ships them.
    ( cd "$repo_root" && cargo build --release --locked -p shell --bin shell --no-default-features )
fi

rel=${TILO_BIN_DIR:-$repo_root/target/release}
for bin in shell tilo-launcher tilo-unlock-helper; do
    [[ -x $rel/$bin ]] || { echo "missing $rel/$bin — run without --no-cargo" >&2; exit 1; }
done

# ---------------------------------------------------------------------------
# 2. Stage the canonical payload.
# ---------------------------------------------------------------------------
say "staging $name-$version"
stage=$out/stage/$name-$version
# Each invocation produces one self-contained artifact set. Leaving an older
# version under RPMS makes the documented tilo-session-*.rpm globs select both
# versions, which package managers reject (and the transient unpacker would
# misleadingly overlay in lexical order).
rm -rf "$out/stage" "$out/SOURCES" "$out/BUILD" "$out/BUILDROOT" \
       "$out/RPMS" "$out/SRPMS"
mkdir -p "$out"/{SOURCES,SPECS,BUILD,RPMS,SRPMS}
"$repo_root/dist/stage-payload.sh" "$rel" "$stage"

tar -C "$out/stage" -czf "$out/SOURCES/$name-$version.tar.gz" "$name-$version"
cp "$repo_root/dist/rpm/$name.spec" "$out/SPECS/"

# ---------------------------------------------------------------------------
# 3. rpmbuild.
# ---------------------------------------------------------------------------
# The host is image-based and has no rpm-build; the same container that builds
# the workspace has it. `.build-container` is the one the cargo shim uses.
say "rpmbuild"
rpmbuild_cmd=(
    rpmbuild -bb
    --define "_topdir $out"
    --define "tilo_version $version"
    "$out/SPECS/$name.spec"
)
if command -v rpmbuild >/dev/null 2>&1; then
    "${rpmbuild_cmd[@]}"
else
    if [ ! -s "$repo_root/.build-container" ]; then
        echo "rpmbuild is unavailable: install rpm-build locally or create .build-container with your distrobox name (see DEVELOPMENT.md)." >&2
        exit 1
    fi
    container=$(head -n1 "$repo_root/.build-container" | tr -d '[:space:]')
    if [ -z "$container" ]; then
        echo ".build-container must contain a distrobox name (see DEVELOPMENT.md)." >&2
        exit 1
    fi
    if ! command -v distrobox >/dev/null 2>&1; then
        echo "distrobox is unavailable: install it or install rpm-build locally." >&2
        exit 1
    fi
    printf -v quoted '%q ' "${rpmbuild_cmd[@]}"
    distrobox enter "$container" -- bash -lc "$quoted"
fi

say "built"
find "$out/RPMS" -name '*.rpm' -printf '%p\n'
