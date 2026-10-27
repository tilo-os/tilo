#!/usr/bin/env bash
# Run a nested Tilo desktop in a window on the current (non-Tilo) desktop, with
# Tilo's own apps installed as signed packages, so the launcher lists them and
# runs them the way it does on Tilo OS: sandboxed, with resources, actions,
# collaboration and the action broker. The model gateway runs beside it, so the
# workspace agent can plan once a provider is configured in Settings, and so
# does the ush supervisor (as its user unit starts it), with an env-runtime in
# the session's system store, so Terminal opens a shell.
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
#   --apps "NAME ..."  apps to install (directory names under apps/; "files"
#                      means file-explorer): the set nested-session-build.sh
#                      builds (the default) or a subset of it.
#   --state DIR        state directory (default: $XDG_STATE_HOME/tilo-nested)
#   --no-build         use the last build in target/nested-session (or in
#                      $TILO_NESTED_SESSION_BUILD, where a check hands its
#                      build over in a read-only tree)
#   --prepare-only     build and install, then exit without starting the shell
#   --reset-identity   discard the throwaway identity and mint a new one
#   --fresh-desktop    forget workspaces and open windows; keep identity and apps
#   --headless         run without a window on the offscreen backend, for
#                      scripted runs driven only through tilo-control
#   --community SITE   the community site this session is in (a site's
#                      address, as /etc/tilo/community.toml names one): it
#                      joins the site's home workspace through the site's door
#                      as a computer joins tilo.coffee's Lounge. Without it
#                      the session follows this machine's configuration.
#                      nested-pair.sh points two sessions at a local site.
#
# The window opens at 1280x800 logical pixels; set TILO_WINIT_SIZE=WxH
# (physical pixels) to choose another size. A headless session renders a
# 1280x800 output (TILO_HEADLESS_SIZE overrides it) only when tilo-control
# asks for a snapshot.
#
# Everything is built on a t server by t.toml's `nested-session` check
# (dist/dev/nested-session-build.sh) and downloaded with `t outputs`, so the
# machine running the session needs no Rust toolchain. An unchanged tree
# returns the stored build at once. Re-running repackages only apps whose
# binary, manifest or surface changed. A changed shared runtime repackages
# every app, since apps link its exact build.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
built_apps=$("$repo_root/dist/dev/nested-session-build.sh" --apps)
apps=$built_apps
state=${XDG_STATE_HOME:-$HOME/.local/state}/tilo-nested
build=1
prepare_only=0
reset_identity=0
fresh_desktop=0
headless=0
community=
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
        --community) community=${2:?--community needs a site}; shift 2 ;;
        -h|--help) sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p}' "$0"; exit 0 ;;
        --) shift; shell_args=("$@"); break ;;
        *) die "unknown option: $1 (see --help)" ;;
    esac
done

if ((build)); then
    command -v t >/dev/null || die "t is required on PATH to build (docs/t.md), or pass --no-build"
fi
for tool in jq bwrap readelf python3 sha256sum; do
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
    # A Tilo desktop admits a Wayland client that it did not launch only
    # when the client shares its user and mount namespaces: anything else
    # could be a sandboxed app reaching past its launch. A toolbox or other
    # container is such a namespace, and the host compositor drops the
    # connection ("Connection reset by peer"). The session needs no
    # toolchain, so it runs on the host itself.
    if [[ -e /run/.containerenv || -e /.dockerenv ]]; then
        die "run the windowed session on the host, not inside a container (toolbox): the host desktop refuses Wayland clients from other namespaces; --headless works anywhere"
    fi
fi

selected=()
for app in $apps; do
    [[ $app == files ]] && app=file-explorer
    if [[ ! -f $repo_root/apps/$app/tilo-app.toml ]]; then
        die "no app named $app under apps/"
    fi
    [[ " $built_apps " == *" $app "* ]] \
        || die "$app is not built for the nested session; add it to dist/dev/nested-session-build.sh"
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

# --- 1. Build -------------------------------------------------------------------
# One check builds the host programs, the thin apps and the runtime library
# they link, on a t node; its outputs land in target/nested-session.
build_out=${TILO_NESTED_SESSION_BUILD:-$repo_root/target/nested-session}
bin=$build_out/bin
thin=$build_out/thin-app
if ((build)); then
    # The build runs on a node of this machine's architecture.
    check=nested-session
    [[ $(uname -m) == aarch64 ]] && check=nested-session-arm64
    say "building the nested session on t (t outputs $check)"
    (cd "$repo_root" && t outputs "$check" --human >/dev/null) \
        || die "the nested-session build failed (t log $check shows why)"
fi
for name in shell tilo-launcher tilo-install tilo-app-package tilo-runtime-bundle \
            tilo-model-gateway tilo-ush tilo-control; do
    [[ -f $bin/$name ]] || die "missing $bin/$name (run without --no-build)"
    chmod +x "$bin/$name"
done
[[ -f $thin/libruntime_dylib.so && -f $thin/runtime-engines ]] \
    || die "no thin-app build in $thin (run without --no-build)"
[[ -f $build_out/env-runtime/publisher.hex ]] \
    || die "no env-runtime in $build_out/env-runtime (run without --no-build)"
# Each shell gets a private session bus (dist/dev/session-bus.sh). Tilo OS
# hosts have no dbus-daemon of their own; the build carries one.
session_bus=$repo_root/dist/dev/session-bus.sh
if ! dbus_daemon=$(command -v dbus-daemon); then
    dbus_daemon=$bin/dbus-daemon
    [[ -f $dbus_daemon ]] || die "no dbus-daemon on PATH or in $bin (run without --no-build)"
    chmod +x "$dbus_daemon"
fi

# The product fonts: the shell sets text in them from a `fonts/` directory
# beside its executable, and the runtime bundle carries the same catalog. They
# are pinned artifacts the catalog tool materializes, not build outputs.
fonts=$state/fonts
rm -rf "$fonts"
python3 "$repo_root/tools/font-catalog.py" prepare \
    "$repo_root/crates/sdk/ui-core/assets/fonts" "$fonts" --scope product
ln -sfn "$fonts" "$bin/fonts"

# --- 2. Plans -------------------------------------------------------------------
plan=$state/plan.json
python3 "$repo_root/tools/apps/app_release.py" --apps "${selected[*]}" > "$plan"

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
needed_std=$(readelf -dW "$thin/libruntime_dylib.so" | sed -n 's/.*Shared library: \[\(libstd-[^]]*\)\].*/\1/p')
rust_libstd=$thin/$needed_std
[[ -n $needed_std && -f $rust_libstd ]] || die "the build carries no $needed_std beside the runtime"
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
    rm -rf "$state/work/runtime"
    mkdir -p "$state/work"
    OUT_DIR="$state/work/runtime" RUNTIME_RELEASE="$release" FROM=/ \
        VERSION_OUT="$state/work/runtime-version" ARCH="$(uname -m)" \
        IDENTITY_JSON="$identity" BUNDLE_BIN="$bin/tilo-runtime-bundle" \
        FONT_CATALOG_ROOT="$fonts" EXTRA_LIBS="${runtime_libs[*]}" \
        "$repo_root/os/scripts/bake-runtime-bundle.sh"
    runtime_version=$(<"$state/work/runtime-version")
    find "$runtimes" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
    mv "$state/work/runtime/tilo-runtime/$runtime_version" "$runtimes/$runtime_version"
    rm -rf "$state/work/runtime"
    printf '%s\n' "$runtime_version" > "$runtimes/.version"
    printf '%s\n' "$runtime_key" > "$runtimes/.key"
fi
runtime_id=tilo-runtime/$runtime_version

# --- 5. Session environment ----------------------------------------------------
for dir in home config data state cache identity; do mkdir -p "$state/$dir"; done
mkdir -p -m 700 "$state/secrets"
export HOME=$state/home
export XDG_CONFIG_HOME=$state/config XDG_DATA_HOME=$state/data
export XDG_STATE_HOME=$state/state XDG_CACHE_HOME=$state/cache
export TILO_IDENTITY_ROOT=$state/identity
export TILO_APPS_DATA_DIR=$state/apps
# The session's system store (below), as /usr/share/tilo-apps is a device's.
system_store=$state/system
export TILO_APPS_SYSTEM_DIR=$system_store
export TILO_RUNTIME_BUNDLE_DIR=$runtimes
export TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE=$trust
export TILO_LAUNCHER_BIN=$bin/tilo-launcher
# The interfaces this build of the system provides, not the host's: on a Tilo
# host /usr/share/tilo/system-interfaces.toml is that image's, which may be
# newer or older than the apps built here.
export TILO_SYSTEM_INTERFACES=$repo_root/os/usr/share/tilo/system-interfaces.toml
# The private D-Bus answers no Secret Service, so secrets (the collaboration
# channels' keys among them) go to the sealed file store a headless machine
# uses, in the state directory.
export TILO_SECRETS_DIR=$state/secrets
unset TILO_APPS_SYSTEM_PAYLOAD_DIR TILO_TEST_CONTROL TILO_STATE_PATH
if [[ -n $community ]]; then
    # The shell reads its community site from this file instead of
    # /etc/tilo/community.toml and /usr/share/tilo/community.toml.
    printf 'site = "%s"\n' "$community" > "$state/community.toml"
    export TILO_COMMUNITY_CONFIG=$state/community.toml
fi

# --- 6. System store -----------------------------------------------------------
# What a Tilo device's /usr/share/tilo-apps holds that the session's own store
# does not: the env-runtime every environment starts from, and its publisher
# as the store's trusted image publisher. Installed by the script the OS image
# and the session package install theirs with, once per build.
env_runtime_id=$(<"$build_out/env-runtime/env-runtime-$(uname -m).id")
if [[ ! -d $system_store/pkgs/${env_runtime_id#tapp:} ]]; then
    say "installing the env-runtime $env_runtime_id"
    rm -rf "$system_store"
    env -u TILO_APPS_SYSTEM_DIR "$repo_root/os/scripts/install-env-runtime.sh" "$build_out/env-runtime" \
        "$system_store" "$system_store" "$bin/tilo-install"
fi

# --- 7. Apps -------------------------------------------------------------------
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

# --- 8. Run ---------------------------------------------------------------------
rm -rf "$runtime"
mkdir -m 700 "$runtime"
export XDG_RUNTIME_DIR=$runtime
control=$runtime/control.sock
children=()
# Stop the process groups the given children lead: each child leads its own
# group, so apps a shell started go too, and so does the shell itself when
# the child that started it (its session bus) has already gone.
stop_groups() {
    local pid
    for pid in "$@"; do kill -TERM -- "-$pid" 2>/dev/null || true; done
    # A compositor whose main loop is stuck never acts on SIGTERM.
    local tries=50
    while ((tries--)); do
        local alive=0
        for pid in "$@"; do kill -0 -- "-$pid" 2>/dev/null && alive=1; done
        ((alive)) || break
        sleep 0.1
    done
    for pid in "$@"; do kill -KILL -- "-$pid" 2>/dev/null || true; done
    for pid in "$@"; do wait "$pid" 2>/dev/null || true; done
}
cleanup() {
    # A second signal (the pair stops its whole group, then stops this
    # script again) must not cut the cleanup short and leave the shell up.
    trap '' INT TERM HUP
    stop_groups "${children[@]}"
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
    rm -rf "$identity_dir" "$TILO_IDENTITY_ROOT" "$state/secrets" \
        "$XDG_CONFIG_HOME"/tilo/state.json "$XDG_CONFIG_HOME"/tilo/state.json.* \
        "$XDG_CONFIG_HOME"/tilo/state.automerge*
    mkdir -p "$TILO_IDENTITY_ROOT"
    mkdir -m 700 "$state/secrets"
fi
if [[ ! -f $marker ]]; then
    # Nobody completes first-login setup here; a headless pass mints the
    # development identity a dev image would, then exits.
    say "minting a throwaway identity"
    env -u WAYLAND_DISPLAY -u DISPLAY TILO_TEST_CONTROL="$control" setsid "$session_bus" "$dbus_daemon" \
        "$bin/shell" --backend headless > "$logs/identity.log" 2>&1 &
    pid=$!
    children+=("$pid")
    wait_for_socket "$control" "$pid" || die "headless shell did not start (see $logs/identity.log)"
    TILO_TEST_CONTROL=$control "$bin/tilo-control" raw '{"op":"provision_test_identity"}' \
        | grep -q '"throwaway": *true' || die "identity provisioning failed (see $logs/identity.log)"
    # Gone before the session starts: it holds the state file's writer lease
    # while it lives.
    stop_groups "$pid"
    children=()
    rm -rf "$runtime"
    mkdir -m 700 "$runtime"
fi

# --- Native workspace ---------------------------------------------------------
# Agent tasks act with a workspace's native authority, and an app opens in a
# room only once the room lets it author there. The Home workspace becomes
# native as on any device: the shell places it on this device's own room
# host before it lets apps start, and the device's native owner is enrolled
# in it there. The session notes when it is (the marker below), so a check
# waits for it rather than for a guess.
native_marker=$state/native-workspace
rm -f "$native_marker"

mkdir -p "$runtime/tilo/services"
chmod 700 "$runtime/tilo" "$runtime/tilo/services"
setsid "$bin/tilo-model-gateway" serve --socket "$runtime/tilo/services/model-gateway.sock" \
    > "$logs/model-gateway.log" 2>&1 &
children+=("$!")
wait_for_socket "$runtime/tilo/services/model-gateway.sock" "$!" \
    || die "model gateway did not start (see $logs/model-gateway.log)"

# The ush supervisor Terminal asks for its shells, started as its user unit
# starts it: the unit's own command line and environment, with %t the
# session's runtime directory and the unit's binary this build's. systemd's
# socket activation and sandboxing directives are not reproduced; the
# supervisor binds the unit's socket itself.
ush_unit=$repo_root/os/usr/lib/systemd/user/tilo-ush.service
ush_unit_bin=/usr/libexec/tilo-ush
unit_word() {
    local word=${1//%t/$runtime}
    [[ $word == "$ush_unit_bin" ]] && word=$bin/tilo-ush
    [[ $word == *=$ush_unit_bin ]] && word=${word%"$ush_unit_bin"}$bin/tilo-ush
    printf '%s\n' "$word"
}
ush_command=() ush_env=()
for word in $(sed -n 's/^ExecStart=//p' "$ush_unit"); do ush_command+=("$(unit_word "$word")"); done
for word in $(sed -n 's/^Environment=//p' "$ush_unit"); do ush_env+=("$(unit_word "$word")"); done
[[ ${ush_command[0]:-} == "$bin/tilo-ush" ]] || die "$ush_unit does not start $ush_unit_bin"
ush_socket=$(unit_word "$(sed -n 's/^ListenStream=//p' "${ush_unit%.service}.socket")")
env "${ush_env[@]}" setsid "${ush_command[@]}" > "$logs/ush.log" 2>&1 &
children+=("$!")
wait_for_socket "$ush_socket" "$!" || die "the ush supervisor did not start (see $logs/ush.log)"

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

# The host desktop reads a Wayland client's namespaces to admit it, which
# the kernel hides once the nested shell makes itself non-dumpable (it does
# as it loads its device key, before its window connects): a dumpable helper
# connects for it and hands the connection over as WAYLAND_SOCKET.
host_client=()
if ((headless)); then
    backend=(--backend headless)
    display_env=(-u WAYLAND_DISPLAY -u DISPLAY)
    say "starting the shell headless at ${TILO_HEADLESS_SIZE} (log: $logs/shell.log)"
else
    backend=(--backend winit)
    display_env=(-u DISPLAY -u WAYLAND_DISPLAY)
    host_client=(python3 -I "$repo_root/dist/dev/host-wayland.py" "$host_display")
    say "starting the nested shell at ${TILO_WINIT_SIZE} (log: $logs/shell.log)"
fi
cat >&2 <<EOF
    drive it:  XDG_RUNTIME_DIR=$runtime TILO_TEST_CONTROL=$control $bin/tilo-control actions list
EOF
env "${display_env[@]}" TILO_TEST_CONTROL="$control" setsid "$session_bus" "$dbus_daemon" \
    "${host_client[@]}" "$bin/shell" "${backend[@]}" "${shell_args[@]}" > "$logs/shell.log" 2>&1 &
shell_pid=$!
children+=("$shell_pid")

# Note once the Home workspace is in a room this device's native owner holds.
confirm_native() {
    local dump tries=600
    while ((tries--)); do
        kill -0 "$shell_pid" 2>/dev/null || return 0
        dump=$("$bin/tilo-control" state-dump 2>/dev/null) || dump='{}'
        if jq -e '.workspace_tree as $t | (($t.workspaces // [])[] | select(.id == $t.active) | .room | tostring)
                as $room | ($t.native_authority.rooms // {}) | has($room)' <<<"$dump" >/dev/null; then
            touch "$native_marker"
            echo "the Home workspace is native"
            return 0
        fi
        sleep 0.2
    done
    echo "the Home workspace did not become native; see shell.log"
    return 1
}
TILO_TEST_CONTROL=$control confirm_native > "$logs/native.log" 2>&1 &
status=0
wait "$shell_pid" || status=$?
exit "$status"
