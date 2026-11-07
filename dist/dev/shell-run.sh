#!/usr/bin/env bash
# Run the tilolang shell as the whole screen of a headless tilo-session, the way a
# person uses it, and see it: rendered by Mesa's software Vulkan driver (lavapipe),
# never this machine's GPU; with the tilolang apps installed as packages and their
# scenario data in their records (`--seed-apps`); with the agent door admitting
# `tilolang repl` to act as the person (`--admit-repl`); and with the development
# test-control socket `tilo-control snapshot PATH` writes a PNG of the screen over
# (`--test-control`).
#
# The Tilo runtime the apps run on, and the tilolang runtime whose binary runs them, are this
# tree's: the thin app build in `target/thin-app` (made by `dist/dev/nested-session-build.sh`)
# is baked into a signed runtime bundle of its own, and the tilolang binary beside it into the
# tilolang runtime bundle. So a session started here runs what this tree built.
#
# The apps are installed the way a person's device installs an app: each is
# `tilolang build` from `apps/tl/<name>` into a signed `.tapp`, and `tilo-install`
# puts it in the session's own app store. The session runs them through the launcher
# like any installed app, and plans each origin connector its manifest declares, so
# Mail's sync and the YouTube client's search reach their services.
#
# Nothing touches the person's own desktop: the session gets a scratch HOME and XDG
# tree, a private XDG_RUNTIME_DIR, no D-Bus and no X11. It holds secrets in the
# sealed file store a headless machine uses, in its own scratch directory, so
# the workspaces it founds have channel keys and their shared desks are written
# as on any device. It prints where its agent door and control socket are and
# how to drive it, then runs until it ends or this script is stopped (Ctrl+C,
# SIGTERM, SIGHUP). Either way the session, everything it started, its FUSE
# mount, its secret store and the scratch directory go with it.
#
# Usage:
#   dist/dev/shell-run.sh [--no-build] [--size WxH] [--no-apps] [--dev-rust-views]
#
# Options:
#   --no-build    use the binaries already in target/debug
#   --no-apps     start without the seeded apps, as a bare session does
#   --size WxH    the headless output's size (default 1280x800)
#   --dev-rust-views  seed a Rust app's sample resources and views too (Tube's channels and
#                     videos), so a shell draws their views live: a shot of a Rust view on the
#                     canvas. Development builds only.
#
# Drive it from another terminal with the commands it prints:
#   target/debug/tilolang repl --door "$TILO_AGENT_DOOR"
#   TILO_TEST_CONTROL=... target/debug/tilo-control snapshot /tmp/shot.png
#
# The tilolang runtime the apps run on: this tree's thin-app build
# (`dist/dev/tilolang-runtime-build.sh`, or `dist/dev/nested-session-build.sh`), baked into a
# signed runtime bundle of its own. The Tilo runtime it runs on is this tree's too, baked of
# `target/thin-app` beside it.
set -euo pipefail

repo_root=$(cd "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
say() { printf '==> %s\n' "$*" >&2; }
die() { printf 'shell-run: %s\n' "$*" >&2; exit 1; }

# The tilolang apps a development session installs, by the directory under `apps/tl`:
# the package id of each is its `app` declaration's.
tl_apps=(discussion notes tasks chat mail tube)
# The Rust apps it installs beside them, by the directory under `apps/`: Chat's record
# (`so.tilo.chat`) is the one the session's agents answer in and the one the canvas's ask
# conversation is a channel of (`docs/designs/app-language/shell.md` §8.1), so a session
# that asks must have it, as a person's device has the app installed.
rust_apps=(chat)
build=1
size=1280x800
apps=(--seed-apps)
rust_views=0
while (($#)); do
    case $1 in
        --no-build) build=0; shift ;;
        --no-apps) apps=(); rust_views=0; shift ;;
        --size) size=${2:?--size needs WxH}; shift 2 ;;
        --dev-rust-views) rust_views=1; shift ;;
        -h|--help) sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p' "$0"; exit 0 ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
done

bin=$repo_root/target/debug
if ((build)); then
    say "building tilo-session, tilolang, tilo-control, tilo-launcher, tilo-install, tilo-app-package and tilo-runtime-bundle"
    # A shot of a Rust app's view on the canvas builds the session with Tube's sample
    # resources and views linked (`--dev-rust-views`, development builds only).
    session_features=()
    ((rust_views)) && session_features=(--features dev-rust-views)
    (cd "$repo_root" && cargo build --locked -p tilo-session --bin tilo-session \
        "${session_features[@]}" \
        -p tilolang-cli --bin tilolang -p tilo-control --bin tilo-control \
        -p launcher --bin tilo-launcher -p install --bin tilo-install \
        -p tilo-app-package-cli --bin tilo-app-package \
        -p runtime-bundle --bin tilo-runtime-bundle)
fi
for tool in tilo-session tilolang tilo-control tilo-launcher tilo-install tilo-app-package tilo-runtime-bundle; do
    [[ -x $bin/$tool ]] || die "no $bin/$tool (run without --no-build)"
done
# The thin-app build every Tilo runtime here is made of: `dist/dev/nested-session-build.sh`
# makes one in `target/thin-app` (the shared runtime library, its engines and the tilolang
# binary), and `dist/dev/tilolang-runtime-build.sh` one in `target/tilolang-runtime` of the
# tilolang binary alone. A tree with neither runs no app at all.
thin=$repo_root/target/thin-app
[[ -f $thin/libruntime_dylib.so && -f $thin/runtime-engines ]] \
    || die "no thin-app build in $thin: run dist/dev/nested-session-build.sh"
# The tilolang binary a runtime bundle is baked of: the one `dist/dev/tilolang-runtime-build.sh`
# built last, which is newer than the thin-app one when a source changed since.
tilolang_bin=$thin/tilolang-app
if [[ -x $repo_root/target/tilolang-runtime/tilolang-app ]]; then
    tilolang_bin=$repo_root/target/tilolang-runtime/tilolang-app
fi
[[ -x $tilolang_bin ]] || die "no tilolang binary: run dist/dev/tilolang-runtime-build.sh"
# The runtime bundle carries the exact libstd the runtime links, so the session needs no local
# Rust toolchain.
needed_std=$(readelf -dW "$thin/libruntime_dylib.so" | sed -n 's/.*Shared library: \[\(libstd-[^]]*\)\].*/\1/p')
[[ -n $needed_std ]] || die "$thin/libruntime_dylib.so links no libstd"
[[ -f $thin/$needed_std ]] || cp -p "$(rustc --print target-libdir)/$needed_std" "$thin/$needed_std"

# Lavapipe's loader manifest: the session, and everything it starts, loads this
# driver alone, and an empty TILO_DMABUF_DEVICE advertises no render node, so
# nothing opens /dev/dri.
lavapipe=
for dir in /etc/vulkan/icd.d /usr/share/vulkan/icd.d /usr/local/share/vulkan/icd.d; do
    for icd in "$dir/lvp_icd.$(uname -m).json" "$dir/lvp_icd.json"; do
        [[ -z $lavapipe && -f $icd ]] && lavapipe=$icd
    done
done
[[ -n $lavapipe ]] || die "Mesa's lavapipe is not installed (no lvp_icd.json under */vulkan/icd.d)"
export VK_DRIVER_FILES=$lavapipe VK_ICD_FILENAMES=$lavapipe WGPU_ADAPTER_NAME=llvmpipe
export TILO_DMABUF_DEVICE=

# One scratch directory holds everything; its runtime directory is short, as a
# socket path must be.
scratch=$(mktemp -d /tmp/tilo-shell-run.XXXXXX)
chmod 700 "$scratch"
home=$scratch/home
runtime=$scratch/run
mkdir -m 700 "$home" "$runtime"
log=$scratch/session.log
store=$scratch/apps
runtimes=$scratch/runtimes
keys=$scratch/keys
# This session's own secret store: a file-backed one, as a headless machine uses
# (`TILO_SECRETS_DIR`, `crates/platform/tilo-secrets/src/files.rs`). The
# session's private D-Bus answers no Secret Service, so without this it holds no
# secret at all — and so no channel keys of any workspace, which leaves every
# workspace's shared desk unwritten and unread here. A development session
# exercises the real path: the store it makes is its own, mode 0700 inside the
# scratch directory, and goes with the rest of it.
secrets=$scratch/secrets
mkdir -m 700 "$secrets"
mkdir -m 700 "$store" "$runtimes" "$keys"

session_pid=
group=
documents=$runtime/tilo/documents
mounted() { grep -qF " $1 " /proc/self/mountinfo; }
# Stop the session started last: it ends on SIGTERM as it does on its own,
# unmounting what it mounted; one that does not is ended outright, and so is
# whatever else of its process group still runs.
stop_session() {
    if [[ -n $session_pid ]]; then
        kill -TERM "$session_pid" 2>/dev/null || true
        local tries=100
        while ((tries--)) && kill -0 "$session_pid" 2>/dev/null; do sleep 0.1; done
        kill -KILL -- "-$group" 2>/dev/null || true
        wait "$session_pid" 2>/dev/null || true
        session_pid=
    fi
    if [[ -n $group ]]; then
        kill -KILL -- "-$group" 2>/dev/null || true
        group=
    fi
    # A session that could not end on its own leaves its mount behind.
    if mounted "$documents"; then
        say "removing the mount the session left at $documents"
        fusermount3 -u -z "$documents" 2>/dev/null || true
    fi
}
cleanup() {
    # A second signal must not cut the cleanup short and leave the session up.
    trap '' INT TERM HUP
    stop_session
    rm -rf "$scratch" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

# The publisher key every package and runtime here is signed with, and the file the
# launcher trusts it through: the session's own, as a development image's is.
identity=$keys/publisher.json
[[ -f $identity ]] || "$bin/tilo-runtime-bundle" key generate --out "$identity" >/dev/null
pubkey=$("$bin/tilo-runtime-bundle" key pubkey --identity "$identity")
[[ $pubkey =~ ^[0-9a-fA-F]{64}$ ]] || die "cannot read the development publisher key"
trust=$keys/runtime-publishers.txt
printf '%s\n' "$pubkey" > "$trust"

# The Tilo runtime the apps run on: this tree's own, baked and signed as
# `dist/dev/nested-session.sh` bakes one — the thin app build's shared runtime library, its
# engines, the exact libstd it links and the keyboard library, all folded into one signed
# bundle. The session and every app it starts run on it, so what they link is what this tree
# built.
say "baking the Tilo runtime bundle"
fonts=$scratch/fonts
python3 "$repo_root/tools/font-catalog.py" prepare \
    "$repo_root/crates/sdk/ui-core/assets/fonts" "$fonts" --scope product
keyboard_library=$(ldconfig -p | awk '/libxkbcommon\.so\.0 .*(x86-64|AArch64)/ && !found {print $NF; found=1}')
[[ -f $keyboard_library ]] || die "libxkbcommon.so.0 is not installed"
runtime_libs=("$thin/libruntime_dylib.so" "$thin/$needed_std" "$keyboard_library")
while IFS= read -r engine; do runtime_libs+=("$thin/$engine"); done < "$thin/runtime-engines"
mkdir -p "$scratch/work"
OUT_DIR="$scratch/work/runtime" RUNTIME_RELEASE=2026.07 FROM=/ \
    VERSION_OUT="$scratch/work/runtime-version" ARCH="$(uname -m)" \
    IDENTITY_JSON="$identity" BUNDLE_BIN="$bin/tilo-runtime-bundle" \
    FONT_CATALOG_ROOT="$fonts" EXTRA_LIBS="${runtime_libs[*]}" \
    "$repo_root/os/scripts/bake-runtime-bundle.sh" >/dev/null
tilo_runtime_version=$(<"$scratch/work/runtime-version")
mv "$scratch/work/runtime/tilo-runtime/$tilo_runtime_version" "$runtimes/$tilo_runtime_version"
rm -rf "$scratch/work"
app_runtime=tilo-runtime/$tilo_runtime_version
say "the apps run on $app_runtime"

# The tilolang runtime bundle: the binary every tilolang app's entrypoint executes, built
# from this tree and signed with the session's key. It holds the binaries alone
# (`--binaries-only`): the launcher runs it on the app's Tilo runtime, whose loader and
# libraries it links, so a loader set of its own is never read.
tilolang_version=$(sed -n 's/^provides *= *"tilolang\/\([^"]*\)".*/\1/p' \
    "$repo_root/apps/tilolang/tilo-runtime.toml")
[[ -n $tilolang_version ]] || die "apps/tilolang/tilo-runtime.toml provides no tilolang version"
say "baking the tilolang runtime bundle $tilolang_version"
"$bin/tilo-runtime-bundle" build --binaries-only --version "$tilolang_version" \
    --out "$runtimes/$tilolang_version" --include-bin "$tilolang_bin" \
    --identity "$identity" >/dev/null
tilolang_runtime=tilolang/$tilolang_version

# Build each tilolang app into a signed `.tapp` and install it into the session's own app
# store, as `tilo-install` installs a person's app: the store the session reads, with its
# runtime taken from this scratch tree.
install_apps() {
    local out=$scratch/tapps
    rm -rf "$out"
    mkdir -p "$out"
    for app in "${tl_apps[@]}"; do
        # A package is a directory of `.tl` files; the packages it uses are found beside it
        # (`apps/tl`), and each is built and installed before the one that uses it.
        "$bin/tilolang" build "$repo_root/apps/tl/$app" \
            --search "$repo_root/apps/tl" --out "$out" --identity "$identity" \
            --tilo-runtime "$app_runtime" --version 0.1.0 >/dev/null
    done
    local tapps=("$out"/*.tapp)
    ((${#tapps[@]})) || die "the tilolang apps built no package"
    for tapp in "${tapps[@]}"; do
        say "installing $(basename "$tapp")"
        install_tapp "$tapp"
    done
    # The Rust apps, packed the way `dist/dev/nested-session.sh` packs one: its binary from the
    # thin app build, its surface and its manifest, the runtime stamped to the one this session
    # runs and the manifest signed with this session's publisher key.
    local package
    for app in "${rust_apps[@]}"; do
        package=$(python3 "$repo_root/tools/apps/app_release.py" --apps "$app" \
            | jq -r '.catalog[0] | [.binary, .id, .manifest] | @tsv') \
            || die "app_release.py cannot describe the $app app"
        local binary id manifest
        IFS=$'\t' read -r binary id manifest <<<"$package"
        local built=$thin/$binary
        [[ -x $built ]] || die "no $built: run dist/dev/nested-session-build.sh with $app in TILO_NESTED_APPS"
        local work=$scratch/rust-app-$app
        rm -rf "$work"
        mkdir -p "$work/payload/bin"
        install -pm 0755 "$built" "$work/payload/bin/$binary"
        local surface
        surface=$repo_root/$(dirname "$manifest")/surface.toml
        [[ -f $surface ]] && install -pm 0644 "$surface" "$work/payload/surface.toml"
        local staged=$work/manifest.toml
        sed "s|^\(identity  *= *\)\"ed25519:[0-9a-fA-F]*\"|\1\"ed25519:$pubkey\"|" \
            "$repo_root/$manifest" > "$staged"
        "$repo_root/os/scripts/stamp-runtime.sh" "$staged" "$app_runtime"
        python3 "$repo_root/tools/apps/app_package_isa.py" stage --arch "$(uname -m)" \
            --manifest "$staged" --binary "$work/payload/bin/$binary" --output "$staged"
        local tapp=$work/$app.tapp
        "$bin/tilo-app-package" pack --manifest "$staged" --payload "$work/payload" \
            --identity "$identity" --output "$tapp" --local-build >/dev/null
        say "installing $id"
        install_tapp "$tapp" --force
        rm -rf "$work"
    done
    rm -rf "$out"
}

# Install the package at `tapp` into the session's own store, as `tilo-install` installs a
# person's app: signed with this session's key, which the launcher's trust file names.
install_tapp() {
    local tapp=$1
    shift
    env HOME="$home" XDG_DATA_HOME="$home/.local/share" \
        TILO_APPS_DATA_DIR="$store" TILO_RUNTIME_BUNDLE_DIR="$runtimes" \
        TILO_SYSTEM_INTERFACES="$repo_root/os/usr/share/tilo/system-interfaces.toml" \
        "$bin/tilo-install" install "$tapp" --allow-self-signed "$@" \
        >/dev/null
}
if ((${#apps[@]})); then
    install_apps
fi

door=$runtime/agent.sock
control=$runtime/test-control.sock
# The session's own private session bus (dist/dev/session-bus.sh), as a dev
# image has one and `dist/dev/nested-session.sh` gives its sessions: a Rust app
# reaches its host through the bus (`so.tilo.chat` and every other native app
# does), and a session with none cannot start one. Tilo OS hosts have no
# dbus-daemon of their own, so the build carries the build environment's.
session_bus=$repo_root/dist/dev/session-bus.sh
dbus_daemon=$(command -v dbus-daemon || true)
if [[ -z $dbus_daemon ]]; then
    for candidate in "$bin/nested-session-bus" "$bin/dbus-daemon"; do
        [[ -x $candidate ]] && { dbus_daemon=$candidate; break; }
    done
fi
[[ -n $dbus_daemon ]] || die "no dbus-daemon on PATH and none in $bin (run without --no-build)"
# Start tilo-session headless with the arguments given, its output in $log, and
# wait until it printed its control socket, which it does last. It runs on the
# private bus, which goes when the session does.
start_session() {
    env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS \
        HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
        XDG_STATE_HOME="$home/.local/state" XDG_CACHE_HOME="$home/.cache" \
        XDG_RUNTIME_DIR="$runtime" TILO_IDENTITY_ROOT="$scratch/identity" \
        TILO_SECRETS_DIR="$secrets" \
        TILO_APPS_DATA_DIR="$store" TILO_RUNTIME_BUNDLE_DIR="$runtimes" \
        TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE="$trust" \
        TILO_LAUNCHER_BIN="$bin/tilo-launcher" \
        TILO_SYSTEM_INTERFACES="$repo_root/os/usr/share/tilo/system-interfaces.toml" \
        TILO_AGENT_DOOR="$door" \
        setsid "$session_bus" "$dbus_daemon" \
        "$bin/tilo-session" --backend headless --size "$size" --no-x11 \
        --system-bus none --test-control "$control" "$@" \
        > "$log" 2>&1 &
    session_pid=$!
    group=$session_pid
    local tries=600
    until grep -q '^TILO_TEST_CONTROL=' "$log" 2>/dev/null; do
        kill -0 "$session_pid" 2>/dev/null || { cat "$log" >&2; die "the session ended as it started"; }
        ((tries--)) || die "the session did not come up (see $log)"
        sleep 0.1
    done
}

# Nobody completes first-use setup here: a first pass mints the throwaway
# identity a development session opens its desk with, as a dev image's does.
say "minting a throwaway identity"
mkdir -m 700 "$scratch/identity"
start_session
minted=$(TILO_TEST_CONTROL=$control "$bin/tilo-control" raw '{"op":"provision_test_identity"}') \
    || die "no throwaway identity: $minted (see $log)"
[[ $minted == *'"throwaway":true'* ]] || die "no throwaway identity: $minted"
stop_session

say "starting the session headless at $size (log: $log)"
start_session --shell screen --admit-repl "$bin/tilolang" "${apps[@]}"
# The door is announced once the session's own doors are up, which can be after its control
# socket: wait for it as for the socket, while the session lives.
tries=600
until grep -q "^TILO_AGENT_DOOR=$door\$" "$log"; do
    kill -0 "$session_pid" 2>/dev/null || { tail -40 "$log" >&2; die "the session ended before serving its agent door"; }
    ((tries--)) || { tail -40 "$log" >&2; die "the session serves no agent door"; }
    sleep 0.1
done

cat <<EOF
TILO_AGENT_DOOR=$door
TILO_TEST_CONTROL=$control
XDG_RUNTIME_DIR=$runtime
The session runs (pid $session_pid). From another terminal:
  the REPL:      $bin/tilolang repl --door $door
  a screenshot:  TILO_TEST_CONTROL=$control $bin/tilo-control snapshot /tmp/shot.png
Stop it with Ctrl+C (or SIGTERM to pid $$); everything it started goes with it.
EOF

status=0
wait "$session_pid" || status=$?
session_pid=
exit "$status"
