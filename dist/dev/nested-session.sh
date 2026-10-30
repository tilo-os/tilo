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
#                      builds (the default) or a subset of it, possibly none.
#   --install-tapp FILE  also install this signed package (repeatable): a
#                      tilolang app `tilolang run` built for this session.
#   --state DIR        state directory (default: $XDG_STATE_HOME/tilo-nested)
#   --no-build         use the last build in target/nested-session (or in
#                      $TILO_NESTED_SESSION_BUILD, where a check hands its
#                      build over in a read-only tree)
#   --prepare-only     build and install, then exit without starting the shell
#   --reset-identity   discard the throwaway identity and mint a new one
#   --fresh-desktop    forget workspaces and open windows; keep identity and apps
#   --headless         run without a window on the offscreen backend, for
#                      scripted runs driven only through tilo-control
#   --software         render with Mesa's software Vulkan driver (lavapipe)
#                      only, and never open this machine's GPU: the shell
#                      and every app it launches load lavapipe alone, and
#                      the shell advertises no render node, so it neither
#                      probes the GPU's drivers nor exchanges a buffer with
#                      it. For agents and tests beside a live desktop.
#   --community SITE   the community site this session is in (a site's
#                      address, as /etc/tilo/community.toml names one): it
#                      joins the site's home workspace through the site's door
#                      as a computer joins tilo.coffee's Lounge. Without it
#                      the session follows this machine's configuration.
#                      nested-pair.sh points two sessions at a local site.
#   --model-provider SPEC  give the session's person a model provider, as
#                      Settings' provider pane does (provider settings, and
#                      the key in the model gateway's own custody), before
#                      the shell starts. SPEC is PRESET:MODEL (a Settings
#                      preset slug: anthropic, openai, gemini, openrouter,
#                      ollama, ...) with the key from $TILO_DEV_MODEL_KEY or
#                      the provider's usual variable (ANTHROPIC_API_KEY,
#                      OPENAI_API_KEY, ...), or `host`: the default provider
#                      and key of this machine's own Tilo configuration. The
#                      key travels on a pipe only, never an argument.
#   --ready FILE       once the shell answers on its control socket, write the
#                      socket, the runtime directory and the state directory to
#                      FILE as JSON (also kept as session.json in the runtime
#                      directory)
#
# The window opens at 1280x800 logical pixels; set TILO_WINIT_SIZE=WxH
# (physical pixels) to choose another size. A headless session renders a
# 1280x800 output (TILO_HEADLESS_SIZE overrides it) only when tilo-control
# asks for a snapshot.
#
# dist/dev/nested-session-build.sh builds everything with cargo, into
# target/nested-session. Re-running repackages only apps
# whose binary, manifest or surface changed. A changed shared runtime
# repackages every app, since apps link its exact build.
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
software=0
community=
model_provider=
install_tapps=()
ready_out=
shell_args=()

die() { printf 'nested-session: %s\n' "$*" >&2; exit 1; }
say() { printf '==> %s\n' "$*" >&2; }

while (($#)); do
    case $1 in
        --apps) apps=${2?--apps needs a list}; shift 2 ;;
        --install-tapp) install_tapps+=("$(realpath -m "${2:?--install-tapp needs a file}")"); shift 2 ;;
        --ready) ready_out=$(realpath -m "${2:?--ready needs a file}"); shift 2 ;;
        --state) state=${2:?--state needs a directory}; shift 2 ;;
        --no-build) build=0; shift ;;
        --prepare-only) prepare_only=1; shift ;;
        --reset-identity) reset_identity=1; shift ;;
        --fresh-desktop) fresh_desktop=1; shift ;;
        --headless) headless=1; shift ;;
        --software) software=1; shift ;;
        --community) community=${2:?--community needs a site}; shift 2 ;;
        --model-provider) model_provider=${2:?--model-provider needs PRESET:MODEL or host}; shift 2 ;;
        -h|--help) sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p}' "$0"; exit 0 ;;
        --) shift; shell_args=("$@"); break ;;
        *) die "unknown option: $1 (see --help)" ;;
    esac
done

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
# One build (nested-session-build.sh) makes the host programs, the thin apps
# and the runtime library they link; its output lands in target/nested-session.
build_out=${TILO_NESTED_SESSION_BUILD:-$repo_root/target/nested-session}
bin=$build_out/bin
# A build shared read-only between sessions (TILO_NESTED_SESSION_BUILD) runs from this
# session's own copy of its programs: hard links, so nothing is duplicated, with room for the
# session's fonts beside them. A fresh tree has no build yet: the build below makes one.
if [[ -d $bin && ! -w $bin ]]; then
    rm -rf "$state/bin"
    mkdir -p "$state"
    cp -al "$bin" "$state/bin" 2>/dev/null || { chmod -R u+w "$state/bin" 2>/dev/null; rm -rf "$state/bin"; cp -a "$bin" "$state/bin"; }
    find "$state/bin" -type d -exec chmod u+w {} +
    bin=$state/bin
fi
thin=$build_out/thin-app
if ((build)); then
    say "building the nested session (dist/dev/nested-session-build.sh)"
    "$repo_root/dist/dev/nested-session-build.sh" "$build_out" \
        || die "the nested-session build failed (see above)"
fi
for name in shell tilo-launcher tilo-install tilo-app-package tilo-runtime-bundle \
            tilo-model-gateway tilo-ush tilo-control; do
    [[ -f $bin/$name ]] || die "missing $bin/$name (run without --no-build)"
    chmod +x "$bin/$name"
done
# A builder of another architecture builds programs this machine cannot run.
case $(uname -m) in
    x86_64) machine='X86-64' ;;
    aarch64) machine='AArch64' ;;
    *) machine= ;;
esac
if [[ -n $machine ]] && ! readelf -hW "$bin/shell" | grep -q "Machine:.*$machine"; then
    die "$bin/shell is not built for $(uname -m): build on a builder of this architecture"
fi
[[ -f $thin/libruntime_dylib.so && -f $thin/runtime-engines ]] \
    || die "no thin-app build in $thin (run without --no-build)"
# A build without the env-runtime trees ships none (nested-session-build.sh).
[[ -f $build_out/env-runtime/publisher.hex ]] \
    || say "no env-runtime in $build_out/env-runtime: Terminal opens no shell in this session"
# Each shell gets a private session bus (dist/dev/session-bus.sh). Tilo OS
# hosts have no dbus-daemon of their own; the build carries the build
# environment's (nested-session-bus, or a dbus-daemon from older builds).
session_bus=$repo_root/dist/dev/session-bus.sh
if ! dbus_daemon=$(command -v dbus-daemon); then
    dbus_daemon=$bin/nested-session-bus
    [[ -f $dbus_daemon ]] || dbus_daemon=$bin/dbus-daemon
    [[ -f $dbus_daemon ]] || die "no dbus-daemon on PATH and no nested-session-bus in $bin (run without --no-build)"
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

# The developer's provider, read now, in this machine's own environment, before
# the session's is put in place: the record (profile and key) stays in this
# shell's memory until it is piped into the session's configuration below.
provider_record=
provider_key=
if [[ -n $model_provider ]]; then
    "$bin/tilo-model-gateway" help provision-provider >/dev/null 2>&1 \
        || die "$bin/tilo-model-gateway cannot provision a provider (run without --no-build, or use a build that has it)"
    if [[ $model_provider == host ]]; then
        provider_record=$("$bin/tilo-model-gateway" export-provider) \
            || die "this machine's Tilo configuration has no default provider to give the session"
    else
        [[ $model_provider == *:?* ]] || die "--model-provider takes PRESET:MODEL or host"
        provider_preset=${model_provider%%:*}
        provider_model=${model_provider#*:}
        case $provider_preset in
            anthropic) provider_var=ANTHROPIC_API_KEY ;;
            openai) provider_var=OPENAI_API_KEY ;;
            gemini) provider_var=GEMINI_API_KEY ;;
            mistral) provider_var=MISTRAL_API_KEY ;;
            openrouter) provider_var=OPENROUTER_API_KEY ;;
            groq) provider_var=GROQ_API_KEY ;;
            xai) provider_var=XAI_API_KEY ;;
            deepseek) provider_var=DEEPSEEK_API_KEY ;;
            together) provider_var=TOGETHER_API_KEY ;;
            fireworks) provider_var=FIREWORKS_API_KEY ;;
            huggingface) provider_var=HF_TOKEN ;;
            *) provider_var= ;;
        esac
        provider_key=${TILO_DEV_MODEL_KEY:-${provider_var:+${!provider_var:-}}}
    fi
fi

# --- 2. Plans -------------------------------------------------------------------
# The plan of every built app names the runtime they link, whichever of them
# (possibly none) this session installs.
plan=$state/plan.json
python3 "$repo_root/tools/apps/app_release.py" --apps "$built_apps" > "$plan"

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

# The tilolang runtime bundle: the binary every tilolang app's entrypoint
# executes (apps/tilolang/tilo-runtime.toml), built against the runtime above
# and signed with the same key. Its version is the one the bundle provides,
# and `tilolang run` reads it from .tilolang-version. It holds the binaries
# alone (--binaries-only): the launcher runs it on the app's runtime, whose
# loader and libraries it links, so a loader set of its own is never read.
tilolang_bin=$thin/tilolang-app
# Beside it, the tilolang host (crates/platform/tilolang-host), which runs a workspace's tilolang
# apps by one author as instances: built without the toolkit, it runs on the app runtime's
# loader and libraries alone.
tilolang_host_bin=$bin/tilolang-host
if [[ -f $tilolang_bin ]]; then
    [[ -f $tilolang_host_bin ]] || die "no tilolang host at $tilolang_host_bin"
    tilolang_version=$(sed -n 's/^provides *= *"tilolang\/\([^"]*\)".*/\1/p' \
        "$repo_root/apps/tilolang/tilo-runtime.toml")
    [[ -n $tilolang_version ]] || die "apps/tilolang/tilo-runtime.toml provides no tilolang version"
    tilolang_key=$( { printf '%s\n' binaries-only "$tilolang_version" "$runtime_version" "$pubkey"; sha256sum "$tilolang_bin" "$tilolang_host_bin"; } | sha256sum | cut -c1-64)
    if [[ ! -f $runtimes/.tilolang-key || $(<"$runtimes/.tilolang-key") != "$tilolang_key" \
          || ! -f $runtimes/$tilolang_version/bundle.toml ]]; then
        say "baking the tilolang runtime bundle"
        chmod +x "$tilolang_bin"
        rm -rf "$state/work/tilolang" "$runtimes/$tilolang_version"
        mkdir -p "$state/work"
        "$bin/tilo-runtime-bundle" build --binaries-only --version "$tilolang_version" \
            --out "$state/work/tilolang" --include-bin "$tilolang_bin" \
            --include-bin "$tilolang_host_bin" \
            --identity "$identity" >/dev/null
        mv "$state/work/tilolang" "$runtimes/$tilolang_version"
        printf '%s\n' "$tilolang_key" > "$runtimes/.tilolang-key"
    fi
    printf '%s\n' "$tilolang_version" > "$runtimes/.tilolang-version"
    # Pinned (runtime_bundle::gc's .pins/<version>): no installed package names
    # it until the first `tilolang dev`, and the collection after each app
    # install below would otherwise take it away.
    mkdir -p "$runtimes/.pins"
    : > "$runtimes/.pins/$tilolang_version"
fi

# --- 5. Session environment ----------------------------------------------------
if ((software)); then
    # Lavapipe's loader manifest, where the Vulkan loader looks for drivers.
    lavapipe=
    for dir in /etc/vulkan/icd.d /usr/share/vulkan/icd.d /usr/local/share/vulkan/icd.d; do
        for icd in "$dir/lvp_icd.$(uname -m).json" "$dir/lvp_icd.json"; do
            [[ -z $lavapipe && -f $icd ]] && lavapipe=$icd
        done
    done
    [[ -n $lavapipe ]] || die "--software needs Mesa's lavapipe (no lvp_icd.json under */vulkan/icd.d)"
    # The loader of the shell, and of every app it launches (they follow a
    # compositor pinned to lavapipe), loads this driver alone; wgpu takes its
    # adapter. An empty TILO_DMABUF_DEVICE advertises no render node, so the
    # shell never opens /dev/dri to probe one.
    export VK_DRIVER_FILES=$lavapipe VK_ICD_FILENAMES=$lavapipe WGPU_ADAPTER_NAME=llvmpipe
    export TILO_DMABUF_DEVICE=
fi

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
# A development session: a running app whose payload is its module (a
# tilolang app) follows reinstalls of its package, verified by the launcher
# as a launch is, so `tilolang dev` swaps a new build in without a restart
# (crates/packaging/launcher/src/live_module.rs). A shipped launcher ignores it.
export TILO_LAUNCHER_LIVE_MODULES=1
# A development session logs warnings and errors unless RUST_LOG says
# otherwise: a shipped shell logs errors only, and a watch that could not
# start or a catalogue that did not settle would leave no trace here.
export RUST_LOG=${RUST_LOG:-warn}
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
env_runtime_id=
[[ ! -f $build_out/env-runtime/publisher.hex ]] \
    || env_runtime_id=$(<"$build_out/env-runtime/env-runtime-$(uname -m).id")
if [[ -n $env_runtime_id && ! -d $system_store/pkgs/${env_runtime_id#tapp:} ]]; then
    say "installing the env-runtime $env_runtime_id"
    rm -rf "$system_store"
    env -u TILO_APPS_SYSTEM_DIR "$repo_root/os/scripts/install-env-runtime.sh" "$build_out/env-runtime" \
        "$system_store" "$system_store" "$bin/tilo-install"
fi

# --- 7. Apps -------------------------------------------------------------------
stamps=$state/stamps
mkdir -p "$stamps" "$TILO_APPS_DATA_DIR"
installed=$("$bin/tilo-install" list 2>/dev/null || true)
# The selected apps' catalog entries, read before the loop: a failing filter
# in a process substitution would install nothing and say nothing.
catalog=$(jq -r '.catalog[] | select(.name | IN($ARGS.positional[]))
    | [.name,.binary,.id,.version,.manifest] | @tsv' \
    --args ${selected[@]+"${selected[@]}"} < "$plan") \
    || die "cannot read the selected apps from $plan"
while IFS=$'\t' read -r name binary id version manifest; do
    [[ -n $name ]] || continue
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
    # A build for speed, larger than the distribution limit allows: the
    # session's store installs it, signed with the session's key, under the
    # local-build ceiling (package_store::size_limit).
    "$bin/tilo-app-package" pack --manifest "$staged" --payload "$work/payload" \
        --identity "$identity" --output "$tapp" --local-build >/dev/null
    # --force replaces the package of the same version; app data stays. The
    # main runtime is installed above; an optional one (Browser's Chromium
    # engine) is offered for download by the app itself.
    "$bin/tilo-install" install "$tapp" --allow-self-signed --force \
        --ignore-missing-runtime >/dev/null
    printf '%s\n' "$digest" > "$stamps/$name"
    rm -rf "$work"
done <<<"$catalog"
for tapp in "${install_tapps[@]}"; do
    [[ -f $tapp ]] || die "no package $tapp"
    say "installing $(basename "$tapp")"
    "$bin/tilo-install" install "$tapp" --allow-self-signed --force >/dev/null
done

# --- 7b. Model provider ----------------------------------------------------------
# Written as the session's person, in the session's configuration and secret
# store, by the gateway's own provider code (`provision-provider`): what
# Settings' "Add provider", "Use model" and "Save API key" write.
if [[ -n $provider_record ]]; then
    say "giving the session this machine's default model provider"
    printf '%s' "$provider_record" | "$bin/tilo-model-gateway" provision-provider --record >&2 \
        || die "the model provider could not be written"
elif [[ -n $model_provider ]]; then
    say "giving the session the $provider_preset provider with $provider_model"
    printf '%s' "$provider_key" | "$bin/tilo-model-gateway" provision-provider \
        --preset "$provider_preset" --model "$provider_model" >&2 \
        || die "the model provider could not be written"
fi
unset provider_record provider_key

((prepare_only)) && { say "prepared $state"; exit 0; }

# --- 8. Run ---------------------------------------------------------------------
rm -rf "$runtime"
mkdir -m 700 "$runtime"
export XDG_RUNTIME_DIR=$runtime
# The session's agent door, where an agent in a terminal of this login (and `tilolang repl`)
# reaches it as an admitted agent, at the path a real session's has.
export TILO_AGENT_DOOR=${TILO_AGENT_DOOR:-$runtime/tilo-session/agent.sock}
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
# Where this session is, for tools that attach to it by its runtime directory
# or control socket (`tilolang dev --session`): the same JSON as --ready.
jq -n --arg control "$control" --arg runtime "$runtime" --arg state "$state" \
    '{control: $control, runtime: $runtime, state: $state}' > "$runtime/session.json"
# Tell whoever drives the session that it answers: the shell serves its
# control socket once it is up.
if [[ -n $ready_out ]]; then
    rm -f "$ready_out"
    (
        wait_for_socket "$control" "$shell_pid" || exit 1
        tries=600
        until TILO_TEST_CONTROL=$control "$bin/tilo-control" state-dump >/dev/null 2>&1; do
            ((tries--)) && kill -0 "$shell_pid" 2>/dev/null || exit 1
            sleep 0.2
        done
        cp "$runtime/session.json" "$ready_out.partial"
        mv "$ready_out.partial" "$ready_out"
    ) &
fi
status=0
wait "$shell_pid" || status=$?
exit "$status"
