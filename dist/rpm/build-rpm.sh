#!/usr/bin/env bash
# Build the qos-session RPM: the qOS compositor as a Wayland session on a host
# that runs something else the rest of the time.
#
#   dist/rpm/build-rpm.sh              build binaries, then the RPM
#   dist/rpm/build-rpm.sh --no-cargo   package binaries that are already built
#
# Output lands in dist/rpm/build/RPMS/<native-rpm-architecture>/.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
out=${QOS_PACKAGE_OUT:-$repo_root/dist/rpm/build}
name=qos-session
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
    ( cd "$repo_root" && cargo build --release --locked -p launcher -p qos-unlock-helper )
    say "cargo build --release (compositor, no default features)"
    ( cd "$repo_root" && cargo build --release --locked -p shell --bin shell --no-default-features )
fi

rel=${QOS_BIN_DIR:-$repo_root/target/release}
for bin in shell qos-launcher qos-unlock-helper; do
    [[ -x $rel/$bin ]] || { echo "missing $rel/$bin — run without --no-cargo" >&2; exit 1; }
done

# ---------------------------------------------------------------------------
# 2. Stage the canonical payload.
# ---------------------------------------------------------------------------
say "staging $name-$version"
stage=$out/stage/$name-$version
rm -rf "$out/stage" "$out/SOURCES"
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
    --define "qos_version $version"
    "$out/SPECS/$name.spec"
)
if command -v rpmbuild >/dev/null 2>&1; then
    "${rpmbuild_cmd[@]}"
else
    container=$(cat "$repo_root/.build-container")
    printf -v quoted '%q ' "${rpmbuild_cmd[@]}"
    distrobox enter "$container" -- bash -lc "$quoted"
fi

say "built"
find "$out/RPMS" -name '*.rpm' -printf '%p\n'
