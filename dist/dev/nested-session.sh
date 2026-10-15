#!/usr/bin/env bash
# Run a nested Tilo desktop in a window on the current (non-Tilo) desktop, with
# Tilo's own apps installed as signed packages, so the launcher lists them and
# runs them the way it does on Tilo OS: sandboxed, with resources, actions,
# collaboration and the action broker. The model gateway runs beside it, so the
# workspace agent can plan once a provider is configured in Settings.
#
# Nothing is installed system-wide and nothing needs root. Everything lives in
# one state directory: a scratch HOME and XDG tree, the app store, the runtime
# bundle, a development publisher key and a throwaway identity. The compositor
# gets a private XDG_RUNTIME_DIR and D-Bus session, so its sockets, services
# and portals cannot collide with the host desktop or another Tilo session.
#
# Usage:
#   dist/dev/nested-session.sh [options] [-- extra shell arguments]
#
# Options:
#   --apps "NAME ..."  apps to build and install (directory names under apps/;
#                      "files" means file-explorer). Default: the core set below.
#   --state DIR        state directory (default: $XDG_STATE_HOME/tilo-nested)
#   --no-build         use binaries already in target/ instead of building
#   --prepare-only     build and install, then exit without starting the shell
#   --reset-identity   discard the throwaway identity and mint a new one
#   --fresh-desktop    forget workspaces and open windows; keep identity and apps
#   --headless         run without a window on the offscreen backend, for
#                      scripted runs driven only through tilo-control
#
# The window opens at 1280x800 logical pixels; set TILO_WINIT_SIZE=WxH
# (physical pixels) to choose another size. A headless session renders a
# 1280x800 output (TILO_HEADLESS_SIZE overrides it) only when tilo-control
# asks for a snapshot.
#
# Re-running rebuilds through $CARGO (incremental on the builder) and repackages
# only apps whose binary, manifest or surface changed. A changed shared runtime
# repackages every app, since apps link its exact build.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
default_apps="agent notes code-editor word-processor chat settings sheets file-explorer browser terminal web-search"
apps=$default_apps
state=${XDG_STATE_HOME:-$HOME/.local/state}/tilo-nested
build=1
prepare_only=0
reset_identity=0
fresh_desktop=0
headless=0
shell_args=()

die() { printf 'nested-session: %s\n' "$*" >&2; exit 1; }
say() { printf '==> %s\n' "$*" >&2; }

while (($#)); do
    case $1 in
        --apps) apps=${2:?--apps needs a list}; shift 2 ;;
        --state) state=${2:?--state needs a directory}; shift 2 ;;
        --no-build) build=0; shift ;;
        --prepare-only) prepare_only=1; shift ;;
        --reset-identity) reset_identity=1; shift ;;
        --fresh-desktop) fresh_desktop=1; shift ;;
        --headless) headless=1; shift ;;
        -h|--help) sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p}' "$0"; exit 0 ;;
        --) shift; shell_args=("$@"); break ;;
        *) die "unknown option: $1 (see --help)" ;;
    esac
done

if ((build)); then
    # Builds go through a remote cargo drop-in that mirrors the named artifacts
    # back with --fetch PATH; name it in $CARGO.
    remote_cargo=${CARGO:?set CARGO to a remote cargo drop-in that accepts --fetch PATH, or pass --no-build}
    command -v "$remote_cargo" >/dev/null || die "$remote_cargo (from CARGO) is required on PATH"
fi
for tool in jq bwrap dbus-run-session readelf python3 sha256sum; do
    command -v "$tool" >/dev/null || die "$tool is required on PATH"
done
host_runtime=${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR is unset}
host_display=
if ((!headless)); then
    [[ -n ${WAYLAND_DISPLAY:-} ]] || die "run this from a Wayland desktop session (WAYLAND_DISPLAY is unset), or pass --headless"
    case $WAYLAND_DISPLAY in
        /*) host_display=$WAYLAND_DISPLAY ;;
        *) host_display=$host_runtime/$WAYLAND_DISPLAY ;;
    esac
    [[ -S $host_display ]] || die "host Wayland socket $host_display does not exist"
fi

selected=()
for app in $apps; do
    [[ $app == files ]] && app=file-explorer
    if [[ ! -f $repo_root/apps/$app/tilo-app.toml ]]; then
        # The default list names apps some branches do not carry yet (web
        # search, for one); an app asked for by name must exist.
        [[ $apps == "$default_apps" ]] && continue
        die "no app named $app under apps/"
    fi
    selected+=("$app")
done
((${#selected[@]})) || die "no apps selected"

state=$(realpath -m "$state")
[[ $state != "$HOME" && $state != / ]] || die "refusing state directory $state"
mkdir -p "$state"
chmod 700 "$state"
# The compositor's own runtime directory: private sockets, services and the
# control socket, under the host's runtime directory so it stays on tmpfs and
# per-user.
runtime=$host_runtime/tilo-nested-$(printf '%s' "$state" | sha256sum | cut -c1-12)
pidfile=$state/session.pid
if [[ -f $pidfile ]] && kill -0 "$(<"$pidfile")" 2>/dev/null; then
    die "a nested session for $state is already running (pid $(<"$pidfile"))"
fi

target=$repo_root/target
# Host tools build in their own profile: release optimisation without the
# shipped profile's fat LTO and single codegen unit, whose link alone takes
# tens of minutes on every shell change.
profile=nested
bin=$target/$profile
profile_config=(--config "profile.$profile.inherits=\"release\"" --config "profile.$profile.lto=false"
                --config "profile.$profile.codegen-units=16" --config "profile.$profile.incremental=true")
thin=$target/thin-app
host_bins=(shell tilo-launcher tilo-install tilo-app-package tilo-runtime-bundle
           tilo-model-gateway tilo-control)

# --- 1. Host binaries -------------------------------------------------------
# Optimised builds, so the nested desktop is not sluggish. `dev-overrides` lets the shell mint a throwaway
# identity and serve the control socket, and lets the launcher and installer
# take the runtime and trust inputs from this state directory. Never ship it.
if ((build)); then
    say "building host binaries ($remote_cargo, profile $profile, dev-overrides)"
    fetch=()
    for name in "${host_bins[@]}"; do fetch+=(--fetch "$profile/$name"); done
    fetch+=(--fetch "$profile/examples/packaged_native_governor")
    (cd "$repo_root" && "$remote_cargo" "${fetch[@]}" build --locked --profile "$profile" "${profile_config[@]}" \
        -p shell --bin shell --example packaged_native_governor --features shell/dev-overrides \
        -p launcher --bin tilo-launcher --features launcher/dev-overrides \
        -p install --bin tilo-install \
        -p tilo-app-package-cli --bin tilo-app-package \
        -p runtime-bundle --bin tilo-runtime-bundle \
        -p tilo-model-gateway --bin tilo-model-gateway \
        -p tilo-control --bin tilo-control)
fi
for name in "${host_bins[@]}" examples/packaged_native_governor; do
    [[ -x $bin/$name ]] || die "missing $bin/$name (run without --no-build)"
done

# --- 2. Thin apps and the shared runtime library ----------------------------
plan=$state/plan.json
python3 "$repo_root/tools/apps/app_release.py" --apps "${selected[*]}" > "$plan"
mapfile -t packages < <(jq -r '.packages[]' "$plan")
runtime_plan=$state/runtime-plan.json
python3 "$repo_root/tools/apps/app_release.py" --collection default > "$runtime_plan"
mapfile -t runtime_packages < <(jq -r '.packages[]' "$runtime_plan")
mapfile -t engines < <(for manifest in "$repo_root"/crates/runtime/engines/*/Cargo.toml; do
    sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$manifest" | head -1; done)
if ((build)); then
    say "building ${#packages[@]} thin app(s) against the default collection's runtime ($remote_cargo)"
    # build-thin-apps.sh owns the Cargo graph; route its one Cargo call through
    # $CARGO and mirror back only what packaging reads, not the whole target.
    wrapper=$state/cargo-via-remote
    {
        printf '#!/usr/bin/env bash\nexec %q' "$remote_cargo"
        printf ' --fetch %q' thin-app/libruntime_dylib.so
        jq -r '.catalog[].binary' "$plan" | while read -r name; do printf ' --fetch %q' "thin-app/$name"; done
        for engine in "${engines[@]}"; do
            printf ' --fetch %q --fetch %q' "thin-app/lib${engine//-/_}.so" "thin-app/$engine-check"
        done
        printf ' "$@"\n'
    } > "$wrapper"
    chmod +x "$wrapper"
    # The build checks each thin binary links by running it once; keep it
    # away from the host display so no app window opens there.
    env -u WAYLAND_DISPLAY -u DISPLAY -u CARGO_ENCODED_RUSTFLAGS -u RUSTFLAGS \
        CARGO="$wrapper" INCLUDE_PACKAGES="${runtime_packages[*]}" \
        BUILD_PACKAGES="${packages[*]}" \
        APP_FEATURES="$(jq -r '.features | join(",")' "$runtime_plan")" \
        "$repo_root/os/scripts/build-thin-apps.sh"
fi
[[ -f $thin/libruntime_dylib.so && -f $thin/runtime-engines ]] \
    || die "no thin-app build in $thin (run without --no-build)"

# --- 3. Keys -----------------------------------------------------------------
keys=$state/keys
mkdir -p "$keys"
chmod 700 "$keys"
identity=$keys/publisher.json
[[ -f $identity ]] || "$bin/tilo-runtime-bundle" key generate --out "$identity" >/dev/null
pubkey=$("$bin/tilo-runtime-bundle" key pubkey --identity "$identity")
[[ $pubkey =~ ^[0-9a-fA-F]{64}$ ]] || die "cannot read the development publisher key"
trust=$keys/runtime-publishers.txt
printf '%s\n' "$pubkey" > "$trust"

# --- 4. Runtime bundle --------------------------------------------------------
runtimes=$state/runtimes
mkdir -p "$runtimes"
rust_libstd=$(find "$(rustc --print target-libdir)" -maxdepth 1 -name 'libstd-*.so' -print -quit)
[[ -f $rust_libstd ]] || die "no Rust libstd dylib in the local toolchain"
needed_std=$(readelf -dW "$thin/libruntime_dylib.so" | sed -n 's/.*Shared library: \[\(libstd-[^]]*\)\].*/\1/p')
[[ $needed_std == "$(basename "$rust_libstd")" ]] \
    || die "the runtime links $needed_std but the local toolchain has $(basename "$rust_libstd"); install the toolchain from rust-toolchain.toml"
# Read the whole cache listing: an early exit would fail the pipeline on SIGPIPE.
keyboard_library=$(ldconfig -p | awk '/libxkbcommon\.so\.0 .*(x86-64|AArch64)/ && !found {print $NF; found=1}')
[[ -f $keyboard_library ]] || die "libxkbcommon.so.0 is not installed"
runtime_libs=("$thin/libruntime_dylib.so" "$rust_libstd" "$keyboard_library")
while IFS= read -r engine; do runtime_libs+=("$thin/$engine"); done < "$thin/runtime-engines"
release=$(jq -er '[.catalog[].runtime] | unique | if length == 1 then .[0] else error("apps require different runtimes") end | sub("^tilo-runtime/"; "")' "$plan")
runtime_key=$( { printf '%s\n' "$release" "$pubkey"; sha256sum "${runtime_libs[@]}" "$repo_root/os/scripts/bake-runtime-bundle.sh"; } | sha256sum | cut -c1-64)
runtime_version=
if [[ -f $runtimes/.key && $(<"$runtimes/.key") == "$runtime_key" && -f $runtimes/.version ]]; then
    runtime_version=$(<"$runtimes/.version")
    [[ -f $runtimes/$runtime_version/bundle.toml ]] || runtime_version=
fi
if [[ -z $runtime_version ]]; then
    say "baking the runtime bundle"
    fonts=$state/work/fonts
    rm -rf "$fonts" "$state/work/runtime"
    mkdir -p "$state/work"
    python3 "$repo_root/tools/font-catalog.py" prepare \
        "$repo_root/crates/sdk/ui-core/assets/fonts" "$fonts" --scope product
    OUT_DIR="$state/work/runtime" RUNTIME_RELEASE="$release" FROM=/ \
        VERSION_OUT="$state/work/runtime-version" ARCH="$(uname -m)" \
        IDENTITY_JSON="$identity" BUNDLE_BIN="$bin/tilo-runtime-bundle" \
        FONT_CATALOG_ROOT="$fonts" EXTRA_LIBS="${runtime_libs[*]}" \
        "$repo_root/os/scripts/bake-runtime-bundle.sh"
    runtime_version=$(<"$state/work/runtime-version")
    find "$runtimes" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
    mv "$state/work/runtime/tilo-runtime/$runtime_version" "$runtimes/$runtime_version"
    rm -rf "$state/work/runtime" "$fonts"
    printf '%s\n' "$runtime_version" > "$runtimes/.version"
    printf '%s\n' "$runtime_key" > "$runtimes/.key"
fi
runtime_id=tilo-runtime/$runtime_version

# --- 5. Session environment ----------------------------------------------------
for dir in home config data state cache identity; do mkdir -p "$state/$dir"; done
export HOME=$state/home
export XDG_CONFIG_HOME=$state/config XDG_DATA_HOME=$state/data
export XDG_STATE_HOME=$state/state XDG_CACHE_HOME=$state/cache
export TILO_IDENTITY_ROOT=$state/identity
export TILO_APPS_DATA_DIR=$state/apps
export TILO_RUNTIME_BUNDLE_DIR=$runtimes
export TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE=$trust
export TILO_LAUNCHER_BIN=$bin/tilo-launcher
unset TILO_APPS_SYSTEM_DIR TILO_TEST_CONTROL TILO_STATE_PATH

# --- 6. Apps -------------------------------------------------------------------
stamps=$state/stamps
mkdir -p "$stamps" "$TILO_APPS_DATA_DIR"
installed=$("$bin/tilo-install" list 2>/dev/null || true)
while IFS=$'\t' read -r name binary id version manifest; do
    built=$thin/$binary
    [[ -x $built ]] || die "missing app binary $built"
    work=$state/work/app-$name
    rm -rf "$work"
    mkdir -p "$work/payload/bin"
    install -pm 0755 "$built" "$work/payload/bin/$binary"
    surface=$repo_root/$(dirname "$manifest")/surface.toml
    [[ -f $surface ]] && install -pm 0644 "$surface" "$work/payload/surface.toml"
    staged=$work/manifest.toml
    sed "s|^\(identity  *= *\)\"ed25519:[0-9a-fA-F]*\"|\1\"ed25519:$pubkey\"|" \
        "$repo_root/$manifest" > "$staged"
    "$repo_root/os/scripts/stamp-runtime.sh" "$staged" "$runtime_id"
    python3 "$repo_root/tools/apps/app_package_isa.py" stage --arch "$(uname -m)" \
        --manifest "$staged" --binary "$work/payload/bin/$binary" --output "$staged"
    digest=$( (cd "$work" && find . -type f -print0 | sort -z | xargs -0 sha256sum) | sha256sum | cut -c1-64)
    if [[ -f $stamps/$name && $(<"$stamps/$name") == "$digest" ]] && grep -q "$id" <<<"$installed"; then
        rm -rf "$work"
        continue
    fi
    say "packaging and installing $id"
    tapp=$work/$name.tapp
    "$bin/tilo-app-package" pack --manifest "$staged" --payload "$work/payload" \
        --identity "$identity" --output "$tapp" >/dev/null
    # --force replaces the package of the same version; app data stays. The
    # main runtime is installed above; an optional one (Browser's Chromium
    # engine) is offered for download by the app itself.
    "$bin/tilo-install" install "$tapp" --allow-self-signed --force \
        --ignore-missing-runtime >/dev/null
    printf '%s\n' "$digest" > "$stamps/$name"
    rm -rf "$work"
done < <(jq -r '.catalog[] | [.name,.binary,.id,.version,.manifest] | @tsv' "$plan")

((prepare_only)) && { say "prepared $state"; exit 0; }

# --- 7. Run ---------------------------------------------------------------------
rm -rf "$runtime"
mkdir -m 700 "$runtime"
export XDG_RUNTIME_DIR=$runtime
control=$runtime/control.sock
children=()
cleanup() {
    local pid
    # Each child leads its own process group, so apps the shell started go too.
    for pid in "${children[@]}"; do kill -TERM -- "-$pid" 2>/dev/null || true; done
    # A compositor whose main loop is stuck never acts on SIGTERM.
    local tries=50
    while ((tries--)); do
        local alive=0
        for pid in "${children[@]}"; do kill -0 -- "-$pid" 2>/dev/null && alive=1; done
        ((alive)) || break
        sleep 0.1
    done
    for pid in "${children[@]}"; do kill -KILL -- "-$pid" 2>/dev/null || true; done
    for pid in "${children[@]}"; do wait "$pid" 2>/dev/null || true; done
    rm -f "$pidfile"
    # The document portal's FUSE mount under the runtime directory goes away
    # with its process, a moment after the kill.
    rm -rf "$runtime" 2>/dev/null || { sleep 1; rm -rf "$runtime" 2>/dev/null; } || true
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP
printf '%s\n' "$$" > "$pidfile"
logs=$state/logs
mkdir -p "$logs"

wait_for_socket() {
    local path=$1 pid=$2 tries=200
    while ((tries--)); do
        [[ -S $path ]] && return 0
        kill -0 "$pid" 2>/dev/null || return 1
        sleep 0.1
    done
    return 1
}

if ((fresh_desktop)); then
    say "forgetting the saved desktop"
    rm -rf "$XDG_CONFIG_HOME"/tilo/state.json "$XDG_CONFIG_HOME"/tilo/state.json.* \
        "$XDG_CONFIG_HOME"/tilo/state.automerge*
fi

# A throwaway identity's certificates last 24 hours; mint a fresh one well
# before that, rather than have the shell refuse an expired chain.
identity_dir=$XDG_CONFIG_HOME/tilo/identity
marker=$identity_dir/throwaway-test-identity
if ((reset_identity)) || { [[ -f $marker ]] && [[ -n $(find "$marker" -mmin +1200) ]]; }; then
    # A new identity is a new person: the native workspace the old one
    # governs belongs to someone else now.
    say "discarding the throwaway identity and its desktop"
    rm -rf "$identity_dir" "$TILO_IDENTITY_ROOT" \
        "$XDG_CONFIG_HOME"/tilo/state.json "$XDG_CONFIG_HOME"/tilo/state.json.* \
        "$XDG_CONFIG_HOME"/tilo/state.automerge*
    mkdir -p "$TILO_IDENTITY_ROOT"
fi
if [[ ! -f $marker ]]; then
    # Nobody completes first-login setup here; a headless pass mints the
    # development identity a dev image would, then exits.
    say "minting a throwaway identity"
    env -u WAYLAND_DISPLAY -u DISPLAY TILO_TEST_CONTROL="$control" setsid dbus-run-session -- \
        "$bin/shell" --backend headless > "$logs/identity.log" 2>&1 &
    pid=$!
    children+=("$pid")
    wait_for_socket "$control" "$pid" || die "headless shell did not start (see $logs/identity.log)"
    TILO_TEST_CONTROL=$control "$bin/tilo-control" raw '{"op":"provision_test_identity"}' \
        | grep -q '"throwaway": *true' || die "identity provisioning failed (see $logs/identity.log)"
    kill -TERM -- "-$pid"; wait "$pid" 2>/dev/null || true
    children=()
    rm -rf "$runtime"
    mkdir -m 700 "$runtime"
fi

# --- Native workspace ---------------------------------------------------------
# Agent tasks act with a workspace's native authority: a task may open a
# closed app only in a room this device's native owner holds, and a room is
# native once a governor admitted the device through a signed invitation.
# The throwaway person has no other device to govern a room, so the first
# start runs the repository's packaged-desktop governor fixture: it signs an
# invitation with this session's own device custody, and the shell redeems it
# at startup into the Home workspace (TILO_SHELL_JOIN_INVITATION_FILE, the
# ordinary boot join). The device then governs that room itself, and every
# later start restores it from the saved desktop.
native_marker=$state/native-workspace
[[ -f $XDG_CONFIG_HOME/tilo/state.json ]] || rm -f "$native_marker"
join_file=
if [[ ! -f $native_marker ]]; then
    say "enrolling the Home workspace into a native room"
    invitation=$state/native-invitation.json
    rm -f "$invitation"
    setsid "$bin/examples/packaged_native_governor" "$state" "$invitation" \
        > "$logs/governor.log" 2>&1 &
    governor_pid=$!
    children+=("$governor_pid")
    tries=300
    until [[ -s $invitation ]]; do
        ((tries--)) && kill -0 "$governor_pid" 2>/dev/null \
            || die "the governor fixture issued no invitation (see $logs/governor.log)"
        sleep 0.1
    done
    # The invitation is a capability: it stays in owner-only files and is
    # removed once redeemed.
    join_file=$runtime/join-invitation
    (umask 077 && jq -er .invitation "$invitation" > "$join_file")
    rm -f "$invitation"
fi

mkdir -p "$runtime/tilo/services"
chmod 700 "$runtime/tilo" "$runtime/tilo/services"
setsid "$bin/tilo-model-gateway" serve --socket "$runtime/tilo/services/model-gateway.sock" \
    > "$logs/model-gateway.log" 2>&1 &
children+=("$!")
wait_for_socket "$runtime/tilo/services/model-gateway.sock" "$!" \
    || die "model gateway did not start (see $logs/model-gateway.log)"

# Audio reaches apps through the host's sound server.
for name in pipewire-0 pulse; do
    [[ -e $host_runtime/$name ]] && ln -s "$host_runtime/$name" "$runtime/$name"
done

# The default window is 1600x900 physical pixels, which a 2x host shows as
# 800x450: too small for the task centre, which drops a card that does not
# fit. Ask for 1280x800 logical at the host's scale (GNOME reports it; others
# get scale 1) unless the caller chose a size.
if ((headless)); then
    export TILO_HEADLESS_SIZE=${TILO_HEADLESS_SIZE:-1280x800}
elif [[ -z ${TILO_WINIT_SIZE:-} ]]; then
    host_scale=$(busctl --user --json=short call org.gnome.Mutter.DisplayConfig \
        /org/gnome/Mutter/DisplayConfig org.gnome.Mutter.DisplayConfig GetCurrentState \
        2>/dev/null | jq -r '[.data[2][] | select(.[4]) | .[2]][0] // empty' 2>/dev/null) || true
    export TILO_WINIT_SIZE=$(awk -v s="${host_scale:-1}" \
        'BEGIN { if (s + 0 <= 0) s = 1; printf "%dx%d", 1280 * s, 800 * s }')
fi

if ((headless)); then
    backend=(--backend headless)
    display_env=(-u WAYLAND_DISPLAY -u DISPLAY)
    say "starting the shell headless at ${TILO_HEADLESS_SIZE} (log: $logs/shell.log)"
else
    backend=(--backend winit)
    display_env=(-u DISPLAY "WAYLAND_DISPLAY=$host_display")
    say "starting the nested shell at ${TILO_WINIT_SIZE} (log: $logs/shell.log)"
fi
cat >&2 <<EOF
    drive it:  XDG_RUNTIME_DIR=$runtime TILO_TEST_CONTROL=$control $bin/tilo-control actions list
EOF
join_env=()
[[ -n $join_file ]] && join_env=("TILO_SHELL_JOIN_INVITATION_FILE=$join_file")
env "${display_env[@]}" TILO_TEST_CONTROL="$control" "${join_env[@]}" setsid dbus-run-session -- \
    "$bin/shell" "${backend[@]}" "${shell_args[@]}" > "$logs/shell.log" 2>&1 &
shell_pid=$!
children+=("$shell_pid")

# Note once the boot join committed the Home workspace's native room.
confirm_native() {
    local dump tries=600
    while ((tries--)); do
        kill -0 "$shell_pid" 2>/dev/null || return 0
        dump=$("$bin/tilo-control" state-dump 2>/dev/null) || dump='{}'
        if jq -e '.workspace_tree as $t | (($t.workspaces // [])[] | select(.id == $t.active) | .room | tostring)
                as $room | ($t.native_authority.rooms // {}) | has($room)' <<<"$dump" >/dev/null; then
            rm -f "$join_file"
            touch "$native_marker"
            echo "the Home workspace is native"
            return 0
        fi
        sleep 0.2
    done
    echo "the Home workspace did not become native; see governor.log and shell.log"
    return 1
}
if [[ -n $join_file ]]; then
    TILO_TEST_CONTROL=$control confirm_native > "$logs/native.log" 2>&1 &
fi
status=0
wait "$shell_pid" || status=$?
exit "$status"
