#!/usr/bin/env bash
# Build the qos-session RPM: the qOS compositor as a Wayland session on a host
# that runs something else the rest of the time.
#
#   dist/rpm/build-rpm.sh              build binaries, then the RPM
#   dist/rpm/build-rpm.sh --no-cargo   package binaries that are already built
#
# Output lands in dist/rpm/build/RPMS/x86_64/.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
out=$repo_root/dist/rpm/build
name=qos-session
version=0.1.0

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
    ( cd "$repo_root" && cargo build --release --locked -p shell --no-default-features )
fi

rel=$repo_root/target/release
for bin in shell qos-launcher qos-unlock-helper; do
    [[ -x $rel/$bin ]] || { echo "missing $rel/$bin — run without --no-cargo" >&2; exit 1; }
done

# ---------------------------------------------------------------------------
# 2. Refuse to package a developer build.
# ---------------------------------------------------------------------------
# A compositor built with `--features dev-session` in a debug profile carries
# a marker that the developer session entry looks for. A shipped binary must
# not have one: if it did, the entry that is supposed to start only builds you
# deliberately made would happily start the installed compositor instead, and
# the distinction the -devel subpackage rests on would be fiction.
marker='qos-dev-session-build-marker-v1'
if LC_ALL=C grep -q -a -F -e "$marker" "$rel/shell"; then
    echo "refusing to package $rel/shell: it carries the dev-session build marker" >&2
    echo "(a release build never should — rebuild without --features dev-session)" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 3. Stage the payload.
# ---------------------------------------------------------------------------
say "staging $name-$version"
stage=$out/stage/$name-$version
rm -rf "$out/stage" "$out/SOURCES"
mkdir -p "$stage"/{bin,libexec,pam,wayland-sessions} "$out"/{SOURCES,SPECS,BUILD,RPMS,SRPMS}

# /usr/bin/qos-shell, not /usr/bin/shell: the OS image owns its whole
# filesystem and can call it what it likes, a package layered onto someone
# else's distribution cannot squat a name that generic.
install -pm 0755 "$rel/shell"             "$stage/bin/qos-shell"
install -pm 0755 "$rel/qos-launcher"      "$stage/bin/qos-launcher"
install -pm 0755 "$rel/qos-unlock-helper" "$stage/libexec/qos-unlock-helper"

install -pm 0755 "$repo_root/dist/session/qos-session"     "$stage/libexec/qos-session"
install -pm 0755 "$repo_root/dist/session/qos-session-dev" "$stage/libexec/qos-session-dev"
install -pm 0644 "$repo_root/dist/session/qos.desktop"     "$stage/wayland-sessions/qos.desktop"
install -pm 0644 "$repo_root/dist/session/qos-dev.desktop" "$stage/wayland-sessions/qos-dev.desktop"
# The same PAM stack the OS image uses, so an unlock accepts exactly the
# factors a login on this host does. Sourced from os/ rather than copied, so
# the two cannot drift.
install -pm 0644 "$repo_root/os/etc/pam.d/qos-unlock"      "$stage/pam/qos-unlock"

tar -C "$out/stage" -czf "$out/SOURCES/$name-$version.tar.gz" "$name-$version"
cp "$repo_root/dist/rpm/$name.spec" "$out/SPECS/"

# ---------------------------------------------------------------------------
# 4. rpmbuild.
# ---------------------------------------------------------------------------
# The host is image-based and has no rpm-build; the same container that builds
# the workspace has it. `.build-container` is the one the cargo shim uses.
say "rpmbuild"
rpmbuild_cmd=(rpmbuild -bb --define "_topdir $out" "$out/SPECS/$name.spec")
if command -v rpmbuild >/dev/null 2>&1; then
    "${rpmbuild_cmd[@]}"
else
    container=$(cat "$repo_root/.build-container")
    printf -v quoted '%q ' "${rpmbuild_cmd[@]}"
    distrobox enter "$container" -- bash -lc "$quoted"
fi

say "built"
find "$out/RPMS" -name '*.rpm' -printf '%p\n'
