#!/usr/bin/env bash
# Build the tilo-session RPM: the Tilo compositor as a Wayland session on a host
# that runs something else the rest of the time.
#
#   dist/rpm/build-rpm.sh              build binaries, then the RPM
#   dist/rpm/build-rpm.sh --no-cargo   package session binaries that are already built
#                                      (the app collection is still built when stale)
#
# Each step can fail and be rerun on its own: Cargo rebuilds only what changed,
# the app collection is kept while it matches the checkout
# (dist/rpm/app-collection.sh), and staging starts afresh. Binaries compile
# with this host's `cargo`; rpmbuild runs here, or in the build container on a
# host without rpm-build (dist/rpm/rpmbuild.sh).
#
# Output lands in dist/rpm/build/RPMS/<native-rpm-architecture>/.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
out=${TILO_PACKAGE_OUT:-$repo_root/dist/rpm/build}
name=tilo-session
version=$(<"$repo_root/dist/VERSION")
# Published packages are release 1; dist/rpm/host-update.sh names the commit.
release=${TILO_RPM_RELEASE:-1}
[[ $release =~ ^[0-9][0-9A-Za-z._+~]*$ ]] || { echo "invalid TILO_RPM_RELEASE: $release" >&2; exit 1; }

run_cargo=1
[[ ${1:-} == --no-cargo ]] && run_cargo=0

say() { printf '\n== %s\n' "$*"; }
# Asked before each long step, so a host that cannot package learns it first.
check_rpmbuild() { "$repo_root/dist/rpm/rpmbuild.sh" --check; }

# ---------------------------------------------------------------------------
# 1. The binaries.
# ---------------------------------------------------------------------------
# `shell` is built with --no-default-features on purpose: that drops
# `hosted-test-control`, which a normal login session never reaches. Video
# goes through GStreamer, whose library ABI is stable across Fedora releases.
if (( run_cargo )); then
    check_rpmbuild
    say "cargo build --release (session and app support)"
    ( cd "$repo_root" && cargo build --release --locked \
        -p launcher -p tilo-unlock-helper -p tilo-app-package-cli -p install -p runtime-bundle \
        -p cli-shim -p app-host -p ush -p local-model-service -p tilo-model-gateway )
    ( cd "$repo_root" && cargo build --release --locked -p imager --bin imager )
    say "cargo build --release (compositor, no default features)"
    # `shell` also owns developer tools such as `shots` and `tilo-control`.
    # Building the package without an explicit target compiles and LTO-links
    # those large binaries even though stage-payload never ships them.
    ( cd "$repo_root" && cargo build --release --locked -p shell --bin shell --bin tilo-runtime-mcp --no-default-features )
fi

# Cargo resolves a relative CARGO_TARGET_DIR from the repository root (the
# directory used by both builds above). Canonicalise that path here as well so
# packaging follows the artifacts produced by Cargo.
target_dir=${CARGO_TARGET_DIR:-$repo_root/target}
[[ $target_dir = /* ]] || target_dir=$repo_root/$target_dir
target_dir=$(realpath -m "$target_dir")
rel=${TILO_BIN_DIR:-$target_dir/release}
for bin in shell imager tilo-launcher tilo-unlock-helper tilo-install tilo-runtime-bundle \
           tilo-cli-shim tilo-app-host tilo-ush tilo-model-manager tilo-model-gateway; do
    [[ -x $rel/$bin ]] || { echo "missing $rel/$bin — run without --no-cargo" >&2; exit 1; }
done
(( run_cargo )) || check_rpmbuild

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

# A desktop session that advertises Tilo Settings must ship that application.
# Build the same default tapp collection as the OS image and materialise its
# immutable system store.  The companion RPM is version-locked to the session
# below, so `rpm-ostree update` advances compositor, launcher, runtime and
# apps in one deployment while user-installed packages retain precedence.
say "the default Tilo app collection"
app_release=$out/app-release
"$repo_root/dist/rpm/app-collection.sh" "$app_release"

say "staging immutable Tilo app store"
app_stage=$stage/apps
"$repo_root/dist/rpm/stage-app-store.sh" "$app_release" "$rel" "$app_stage"
mkdir -p "$app_stage"/{bin,libexec,systemd-user,user-tmpfiles,license}
install -pm 0755 "$rel/tilo-install" "$app_stage/bin/tilo-install"
install -pm 0755 "$rel/tilo-app-package" "$app_stage/bin/tilo-app-package"
install -pm 0755 "$rel/tilo-runtime-bundle" "$app_stage/bin/tilo-runtime-bundle"
install -pm 0755 "$rel/tilo-model-manager" "$app_stage/bin/tilo-model-manager"
install -pm 0755 "$rel/tilo-model-gateway" "$app_stage/bin/tilo-model-gateway"
install -pm 0755 "$rel/tilo-cli-shim" "$app_stage/libexec/tilo-cli-shim"
install -pm 0755 "$rel/tilo-app-host" "$app_stage/libexec/tilo-app-host"
install -pm 0755 "$rel/tilo-ush" "$app_stage/libexec/tilo-ush"
install -pm 0644 "$repo_root/os/usr/lib/systemd/user/tilo-app-host.service" \
    "$app_stage/systemd-user/tilo-app-host.service"
install -pm 0644 "$repo_root/os/usr/lib/systemd/user/tilo-models.service" \
    "$app_stage/systemd-user/tilo-models.service"
install -pm 0644 "$repo_root/os/usr/lib/systemd/user/tilo-model-gateway.service" \
    "$app_stage/systemd-user/tilo-model-gateway.service"
install -pm 0644 "$repo_root/os/usr/lib/systemd/user/tilo-ush.service" \
    "$app_stage/systemd-user/tilo-ush.service"
for socket in tilo-app-host tilo-models tilo-model-gateway tilo-ush; do
    install -pm 0644 "$repo_root/os/usr/lib/systemd/user/$socket.socket" \
        "$app_stage/systemd-user/$socket.socket"
done
install -pm 0644 "$repo_root/os/usr/share/user-tmpfiles.d/tilo-model-manager.conf" \
    "$app_stage/user-tmpfiles/tilo-model-manager.conf"
mkdir -p "$app_stage/profile.d"
install -pm 0644 "$repo_root/os/etc/profile.d/tilo-apps.sh" \
    "$app_stage/profile.d/tilo-apps.sh"
install -pm 0644 "$repo_root/LICENSE" "$app_stage/license/LICENSE"

tar -C "$out/stage" -czf "$out/SOURCES/$name-$version.tar.gz" "$name-$version"
cp "$repo_root/dist/rpm/$name.spec" "$out/SPECS/"

# ---------------------------------------------------------------------------
# 3. rpmbuild.
# ---------------------------------------------------------------------------
say "rpmbuild"
"$repo_root/dist/rpm/rpmbuild.sh" -bb \
    --define "_topdir $out" \
    --define "tilo_version $version" \
    --define "tilo_release $release" \
    "$out/SPECS/$name.spec"

say "built"
find "$out/RPMS" -name '*.rpm' -printf '%p\n'
