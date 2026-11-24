#!/usr/bin/env bash
# The OS's compiled tree: every binary, thin app, runtime library and native
# helper the image ships, compiled in the OS builder image (os/image/builder:
# the pinned base and the frozen RPM snapshot with the toolchains, so what it
# links runs on the image) into OUT, as os/components.toml reads it. No image
# build compiles: this runs as an ordinary build action, with the compiler
# cache, warm build directories and keys every other build of the workspace has.
#
# usage: compile.sh OUT | --dev OUT | --chromium OUT | --boot OUT | --apps FROM OUT | --image FROM OUT
#   CARGO_TARGET_DIR   the build directory (default target/)
#   TILO_RUST_LTO      thin (published images) or off (integration and development)
#   TILO_WITH_BROWSER  1 to build the browser's engine host, 0 not to
#   TILO_BOOT_GREETER  `apart` leaves the initramfs's greeter (`--boot`) out of OUT, for
#                      an image build given it beside the tree (os/image/build-layer.sh)
#   TILO_CHROMIUM_ENGINE  what `--chromium` left: the engine taken from there
#                      instead of built again; `apart` leaves it out of OUT, for an
#                      image build given it beside the tree (os/dev/build-image.sh)
#   TILO_APP_MODELS    the modules `tools/ci/app-models.sh` built for every app
#                      that keeps a model (the app-models check's output), taken
#                      instead of building them here; the packing below ships
#                      them inside each app's package
#   TILO_GITHUB_ASSET_CACHE  the checksum-pinned external assets (default
#                      os/external-assets/cache), for the CEF distribution
#   TILO_APP_COLLECTION  the collection in dist/apps/app-collections.toml whose
#                      apps this image ships (default `image`, or `default` for a
#                      build given a prebuilt app collection); its plan is
#                      written to OUT/app-plan.json for the packing stage
#
# The compiler cache is the one the caller gives: t's, through RUSTC_WRAPPER and
# its endpoint, as every build t runs gets it; else, with TILO_COMPILER_CACHE=local,
# sccache over SCCACHE_DIR, which os/dev/build-image.sh keeps between local builds;
# TILO_COMPILER_CACHE=off compiles with none and refuses any it is given.
set -euo pipefail

# The browser's engine host and its runtime tree (scripts/browser/build-host.py) into
# DEST/chromium-engine, built in WORK from the pinned CEF SDK in ASSETS.
chromium_engine() {
  local dest=$1 work=$2 assets=$3 cef_platform cef_version
  case "$(uname -m)" in
    x86_64) cef_platform=linux64 ;;
    aarch64) cef_platform=linuxarm64 ;;
    *) echo "no Chromium engine for $(uname -m)" >&2; return 1 ;;
  esac
  cef_version="$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["chromium_version"])' scripts/browser/cef-lock.json)" \
    && python3 -I scripts/browser/build-host.py --platform "$cef_platform" \
         --sdk-archive "$assets/cef-${cef_version}-${cef_platform}-minimal.tar.bz2" \
         --runtime "$dest"/chromium-engine --work "$work" --jobs "${TILO_CARGO_BUILD_JOBS:-$(nproc)}" \
    && test -x "$dest"/chromium-engine/chromium/tilo-chromium-host
}

# --chromium: the browser's engine alone, which changes with its own sources, not with the
# workspace's; a compile given it (TILO_CHROMIUM_ENGINE) takes it as it is.
if [[ ${1:-} == --chromium ]]; then
  out=${2:?usage: compile.sh --chromium OUT}
  mkdir -p "$out"
  out=$(cd "$out" && pwd)
  export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$(pwd)/target} LC_ALL=C.UTF-8 TZ=UTC CARGO_INCREMENTAL=0 SOURCE_DATE_EPOCH=0
  chromium_engine "$out" "$CARGO_TARGET_DIR" "${TILO_GITHUB_ASSET_CACHE:-$(pwd)/os/external-assets/cache}"
  exit 0
fi
# The initramfs's greeter (OUT/tilo-greeter-boot). The release build resolves features for the
# whole workspace through workspace-hack, which hands the greeter the media stack other members
# enable. This copy (profile `initrd`) links the empty os/initrd/workspace-hack instead, so it
# takes only what its own manifests name and every boot unpacks no GStreamer or PipeWire. The
# resolve that build writes to Cargo.lock is put back after it. It links as its profile says (fat
# LTO, one codegen unit), which keeps it within the initramfs's budget, not with the release
# build's thin LTO; an image linked without LTO (TILO_RUST_LTO=off) links it without LTO too.
boot_greeter() {
  local dest=$1 lock lto=fat units=1 status=0
  [ "${TILO_RUST_LTO:-thin}" = off ] && lto=off units=16
  lock=$(mktemp "${TMPDIR:-/tmp}/tilo-boot-lock.XXXXXX")
  cp Cargo.lock "$lock"
  CARGO_PROFILE_INITRD_LTO=$lto CARGO_PROFILE_INITRD_CODEGEN_UNITS=$units \
    cargo build --profile initrd -p greeter --bin tilo-greeter \
      --config 'patch.crates-io.workspace-hack.path="os/initrd/workspace-hack"' || status=$?
  cp "$lock" Cargo.lock
  rm -f "$lock"
  [ "$status" = 0 ] || return "$status"
  cp "$CARGO_TARGET_DIR"/initrd/tilo-greeter "$dest"/tilo-greeter-boot
}

# --boot: the initramfs's greeter alone, a build of its own resolve that shares nothing with the
# release build's but the compiler cache, so it runs beside that build rather than after it.
if [[ ${1:-} == --boot ]]; then
  out=${2:?usage: compile.sh --boot OUT}
  mkdir -p "$out"
  out=$(cd "$out" && pwd)
  export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$(pwd)/target} LC_ALL=C.UTF-8 TZ=UTC CARGO_INCREMENTAL=0 SOURCE_DATE_EPOCH=0
  boot_greeter "$out"
  exit 0
fi
# --dev: the development components instead (os/scripts/install-components.py --dev names them,
# with their dev features), for the dev image (os/Containerfile.dev).
if [[ ${1:-} == --dev ]]; then
  out=${2:?usage: compile.sh --dev OUT}
  mkdir -p "$out"
  out=$(cd "$out" && pwd)
  export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$(pwd)/target} LC_ALL=C.UTF-8 TZ=UTC CARGO_INCREMENTAL=0 SOURCE_DATE_EPOCH=0
  : "${TILO_DEV_RUST_LTO:=off}"
  case "$TILO_DEV_RUST_LTO" in thin|off) ;; *) echo 'TILO_DEV_RUST_LTO must be thin or off' >&2; exit 1 ;; esac
  dev_cargo_args=$(python3 -I os/scripts/install-components.py --dev --dry-run --format cargo)
  dev_bins=$(python3 -I os/scripts/install-components.py --dev --dry-run --format bins)
  [[ -n $dev_cargo_args && -n $dev_bins ]] || { echo 'install-components.py names no dev components' >&2; exit 1; }
  printf 'dev image build: cargo build --release %s\n' "$dev_cargo_args"
  # The build directory is trusted as for the image's own compile (TILO_CARGO_FRESHNESS,
  # below). Under t (`cargo`, the default) a job's sources are stamped newer wherever they
  # changed, and hashing every file would make the whole checkout this step's reads, so any
  # commit would run it again. A kept target directory fed a tree whose times were rewritten or
  # kept from the checkout asks for `content`.
  : "${TILO_CARGO_FRESHNESS:=cargo}"
  dev_built_sources="$CARGO_TARGET_DIR/.tilo-built-sources-dev"
  case "$TILO_CARGO_FRESHNESS" in
    cargo) ;;
    clean) cargo clean --release ;;
    content) sh os/scripts/cargo-freshness.sh "$(pwd)" "$dev_built_sources" || cargo clean --release ;;
    *) echo 'TILO_CARGO_FRESHNESS must be cargo, clean or content' >&2; exit 1 ;;
  esac
  # The image compile's settings without LTO (16 codegen units, no line tables, which the
  # release profile strips from what it links anyway), so the crates both build come from
  # one compiler cache entry, and the shell, rebuilt by most changes, compiles in parallel.
  # shellcheck disable=SC2086
  CARGO_PROFILE_RELEASE_LTO=$TILO_DEV_RUST_LTO \
  CARGO_PROFILE_RELEASE_CODEGEN_UNITS=$([ "$TILO_DEV_RUST_LTO" = off ] && echo 16 || echo 8) \
  CARGO_PROFILE_RELEASE_DEBUG=0 \
    bash tools/ci/measure.sh "dev cargo" -- cargo build --release $dev_cargo_args
  if [ "$TILO_CARGO_FRESHNESS" = content ]; then sh os/scripts/cargo-freshness.sh --built "$dev_built_sources"; fi
  for b in $dev_bins; do cp "$CARGO_TARGET_DIR/release/$b" "$out/"; done
  os/scripts/verify-elf-hardening.sh "$out"
  exit 0
fi
# --apps: of a compiled tree FROM, what the apps and their runtimes are packed from (the payload
# action of os/image/layers.toml): the thin apps, the runtime and its engines, the app models,
# the prebuilt apps, the packing tools and what the packing reads of the sources (the apps'
# manifests, the language packs, the product fonts), copied into OUT. A step of its own that t never
# caches, so the payload is keyed by these files' bytes rather than by the compile's key: a
# change that relinks only system programs leaves them as they were, and the payload is served.
# --image: of a compiled tree FROM, what the image layer installs (the image action of
# os/image/layers.toml), as os/image/install-components.sh selects it: every component's files
# but the desktop's programs (os/components.toml's desktop-binaries layer), which os-image-ARCH
# lays over the composed image. Like --apps, a step t never caches, so the image layer is keyed
# by these files' bytes: a change that relinks only desktop programs, or only apps, leaves them
# as they were, and the image layer is served.
if [[ ${1:-} == --image ]]; then
  from=${2:?usage: compile.sh --image FROM OUT} out=${3:?usage: compile.sh --image FROM OUT}
  rm -rf "$out"
  mkdir -p "$out"
  mapfile -t names < <(python3 -I - os/scripts/install-components.py os/components.toml <<'PY'
import importlib.util, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location("install_components", sys.argv[1])
installer = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = installer
spec.loader.exec_module(installer)
manifest = installer.load_manifest(Path(sys.argv[2]))
entries = installer.select(manifest, [], True, [], True, skip_layers=["desktop-binaries"])
print("\n".join(sorted({e.source.split("/", 1)[0] for e in entries
                         if e.source and not e.source.startswith(("@", "/"))})))
PY
  )
  [[ ${#names[@]} -gt 0 ]] || { echo "compile.sh --image: os/components.toml installs nothing from a compiled tree" >&2; exit 1; }
  # A name the tree lacks is one install-components.py looks for beside it (the initramfs's
  # greeter, the browser's engine) or an optional file.
  for name in "${names[@]}"; do
    [[ ! -e $from/$name && ! -L $from/$name ]] || cp -a --reflink=auto "$from/$name" "$out/"
  done
  exit 0
fi

if [[ ${1:-} == --apps ]]; then
  from=${2:?usage: compile.sh --apps FROM OUT} out=${3:?usage: compile.sh --apps FROM OUT}
  rm -rf "$out"
  mkdir -p "$out"
  files=(libruntime_dylib.so tilo-runtime-account-capability runtime-engines
         tilo-runtime-bundle tilo-app-package tilo-install tilo-cli-shim)
  [[ ! -f $from/runtime-engines ]] || mapfile -t -O "${#files[@]}" files < "$from/runtime-engines"
  files+=(apps language-packs)
  # The tilolang packages the app plan names are built with `tilolang` and run by the
  # thin `tilolang-process` the packing bakes into their runtime bundle, whose manifest
  # (`crates/lang/tilolang-process/tilo-runtime.toml`) names its version.
  for f in "${files[@]}" runtime-bundle-publisher.hex thin-apps app-models prebuilt-apps prebuilt-runtime fonts app-plan.json \
      tilolang crates; do
    [[ -e $from/$f ]] || continue
    cp -a "$from/$f" "$out/"
  done
  for f in "$from"/libstd-*.so; do [[ ! -e $f ]] || cp -a "$f" "$out/"; done
  for f in "${files[@]}"; do [[ -e $out/$f ]] || { echo "compile.sh --apps: $from has no $f" >&2; exit 1; }; done
  # The plan says whether the payload packs any tilolang package, so a tree that
  # names one without the compiler that builds it is refused here rather than at
  # the packing, a whole payload build later.
  if [[ -f $out/app-plan.json ]] \
      && python3 -I -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1])).get("tilolang") else 1)' \
           "$out/app-plan.json"; then
    for f in tilolang crates/lang/tilolang-process/tilo-runtime.toml; do
      [[ -e $out/$f ]] \
        || { echo "compile.sh --apps: $from has no $f, which the app plan's tilolang packages are built and run with" >&2; exit 1; }
    done
    [[ -e $out/thin-apps/tilolang-process || -d $out/prebuilt-runtime/tilolang ]] \
      || { echo "compile.sh --apps: $from has no thin-apps/tilolang-process, which the app plan's tilolang packages run on" >&2; exit 1; }
  fi
  exit 0
fi
out=${1:?usage: compile.sh OUT | --dev OUT | --apps FROM OUT}
src=$(pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
target=${CARGO_TARGET_DIR:-$src/target}
export CARGO_TARGET_DIR=$target
assets=${TILO_GITHUB_ASSET_CACHE:-$src/os/external-assets/cache}
scratch=${TMPDIR:-/tmp}
[[ -f Cargo.toml && -d crates ]] || { echo 'compile.sh runs at the repository root' >&2; exit 1; }
# Fixed locale and time zone, no incremental state: the same sources give the same bytes.
export LC_ALL=C.UTF-8 TZ=UTC CARGO_INCREMENTAL=0 SOURCE_DATE_EPOCH=0

# Compile the binaries the OS images need. Cache mounts persist cargo's
# registry + git checkouts (CARGO_HOME), the target dir, and the sccache store
# across rebuilds; a cache-mounted target/ isn't in any layer, so the binaries
# are cp'd to /out (outside the cache mount) in the same RUN for the final stage
# to COPY.
#
# Two CI guards (the image builds under buildkit, not just podman/buildah):
#  - The resolve comes from the tracked `Cargo.lock`, which is why that file is
#    committed: a fresh resolve picks incompatible transitive versions (iroh
#    pulls a pre-release ed25519-dalek that does not compile against current
#    `signature`), and the failure surfaces inside a dependency rather than
#    anywhere this image touches. `cargo build` (not --locked) still lets the
#    resolve adjust conservatively where a manifest here differs, while keeping
#    the good pins.
#  - Brace the trailing sccache stat so a failing `cargo build` fails the RUN
#    instead of being swallowed by `|| true` (which masked a cargo failure as a
#    confusing downstream "COPY --from /out: not found").
#
# The application set is read out of the workspace members rather than listed
# here. A hand-maintained list is how the image came to ship two GUI apps while
# the tree held forty-six: adding an app to the workspace and forgetting this
# file was silent. Deriving it means a new `apps/<name>/` is shipped by the
# build that follows it, and an empty match is a hard failure rather than an
# image with no apps. The browser is filtered out only when TILO_WITH_BROWSER=0
# leaves its engine out.
#
# `tilo-app-package-cli`, `install` and `cli-shim` build the packaging tools: `tilo-app-package` and
# `tilo-install` pack and install those apps into the image-wide store below,
# and `tilo-install` ships so the running system can install and remove
# packages of its own.
# Every media engine in this tree sits behind a cargo feature that is OFF by
# default, and deliberately so: `--shots`, the offline tests and CI must build
# without a GStreamer stack, a camera or a network. An image that takes those
# defaults is the one case where the trade-off is wrong — it ships a video
# player that synthesises frames instead of decoding them, a camera that cannot
# capture and clients whose
# network backends are compiled out. The features are named here so the image,
# and only the image, turns them on.
#
# Package-qualified (`<pkg>/<feature>`). The tapp applications and their
# versioned shared runtime are built together below: Rust dylib symbol names
# contain crate hashes, so separate Cargo invocations are not ABI-compatible.
#
# No app takes a `portal` feature, because none has one: `ui` compiles the host
# file chooser into every app on every build, including --shots. Asking for
# `video-editor/portal` is not a no-op — cargo refuses the whole invocation with
# "none of the selected packages contains these features", which fails the image
# build after the package install and the cold Rust compile have already run.
# os/tests/app_store.sh checks each pair against cargo metadata for that reason.
#
# Fat LTO with one codegen unit serialized the final links for roughly fifty
# binaries and made a warm image rebuild spend over an hour in this step.
# The image-only Cargo overrides below use thin LTO, retaining cross-crate
# optimization while allowing LLVM to use the runner cores. Eight codegen
# units also bound concurrent thin-LTO scratch data on the image runner.
# Other release artifacts keep the workspace's stricter profile unchanged.
#
# TILO_RUST_LTO says which of two the image takes: `thin`, the default and what
# every published image is built with, or `off`, for integration, dev and local
# builds. Thin LTO re-optimizes each binary's whole dependency graph at link
# time, which sccache cannot cache: a warm CI rebuild that hit the compiler
# cache for every object still spent fifteen minutes relinking. Without it a
# one-crate change relinks in seconds. The build scripts choose the value from
# what the build is for (os/dev/build-image.sh),
# and the image says which it got in its org.tilo.rust-lto label.
# How the build directory is trusted. `cargo` (the default) trusts cargo's own
# fingerprints: t's build directories come from build snapshots whose sources
# are stamped newer than what they were built from wherever they changed, so a
# one-crate change relinks only what uses it. `content` has
# os/scripts/cargo-freshness.sh mark exactly the sources whose contents changed
# newer than the cached target, for a target directory restored by a tool that
# rewrites times (a layer cache). `clean` empties the release and thin-app
# profiles first and the whole directory after: the independent reproduction
# of a publication, which must not build on anything earlier.

: "${TILO_CARGO_FRESHNESS:=cargo}"
: "${TILO_RUST_LTO:=thin}"
: "${TILO_WITH_BROWSER:=}"
: "${TILO_PREBUILT_APPS:=0}"
: "${TILO_PREBUILT_DESKTOP:=0}"
# The app set the image ships, as a collection in dist/apps/app-collections.toml:
# the release plans a collection the same way, and an image's is every native app
# plus the tilolang packages it ships before the native app they replace retires.
# A build given a prebuilt app collection (TILO_PREBUILT_APPS) ships exactly what
# that release packed, which is the `default` collection: it plans that one, so it
# builds neither the tilolang packages nor the compiler that builds them.
if [ "${TILO_PREBUILT_APPS:-0}" = 1 ]; then
  : "${TILO_APP_COLLECTION:=default}"
else
  : "${TILO_APP_COLLECTION:=image}"
fi
# Parallel jobs, when the caller sets them (os/dev/build-image.sh, by the memory a
# link takes); else cargo's own count, within the jobserver t gives every build.
: "${TILO_CARGO_BUILD_JOBS:=}"
# Debug info the release profile compiles (it strips debug info from the
# output either way, keeping the symbol table a crash log names functions
# from): the profile's line tables by default; 0 skips them, which only makes
# a dev build compile faster.
: "${TILO_RUST_DEBUGINFO:=line-tables-only}"
# 1 prints where each cargo build spent its time (cargo --timings).
: "${TILO_CARGO_TIMINGS:=}"
: "${TILO_REQUIRE_COMPILER_CACHE:=0}"
# `local` compiles through sccache over SCCACHE_DIR unless the caller gives a
# wrapper of its own (t's). `off` is the independent reproduction job of a
# publication: with no cache at all, a planted object in a shared cache makes the
# two builds differ. It runs no wrapper, and refuses compiler-cache settings.
: "${TILO_COMPILER_CACHE:=local}"
[[ -z $TILO_CARGO_BUILD_JOBS ]] || export CARGO_BUILD_JOBS=$TILO_CARGO_BUILD_JOBS

phase=rust-setup
started=$(date +%s)
build_phase() {
  local now
  now=$(date +%s)
  printf 'TILO_BUILD_TIMING phase=%s elapsed_seconds=%s exit_code=0\n' "$phase" "$((now - started))"
  phase=$1
  started=$now
}
trap 'status=$?; printf "TILO_BUILD_TIMING phase=%s elapsed_seconds=%s exit_code=%s\n" "$phase" "$(( $(date +%s) - started ))" "$status"; command -v sccache >/dev/null && sccache --show-stats 2>/dev/null || true; exit "$status"' EXIT
# The greeter is built twice: by the release build, and for the initramfs (boot_greeter, phase
# rust-boot-greeter), unless TILO_BOOT_GREETER=apart leaves that to `--boot`.
if [[ $TILO_COMPILER_CACHE == off ]]; then
  if [[ -n ${RUSTC_WRAPPER:-}${SCCACHE_BUCKET:-}${SCCACHE_ENDPOINT:-}${SCCACHE_DIR:-}${ACTIONS_CACHE_URL:-}${ACTIONS_RUNTIME_TOKEN:-}${AWS_ACCESS_KEY_ID:-} ]]; then
    echo 'a build without the compiler cache received compiler-cache settings' >&2; exit 1
  fi
  export RUSTC_WRAPPER=
  echo 'compiling without a compiler cache' >&2
elif [[ -n ${RUSTC_WRAPPER:-} ]]; then
  printf 'compiler cache: %s, as the build was given it\n' "$RUSTC_WRAPPER"
elif [[ $TILO_COMPILER_CACHE == local ]]; then
  export RUSTC_WRAPPER=sccache SCCACHE_DIR=${SCCACHE_DIR:-$src/target/.sccache} SCCACHE_IGNORE_SERVER_IO_ERROR=1
  export SCCACHE_SERVER_UDS=$scratch/tilo-os-sccache.sock
  if ! sccache --start-server; then
    [[ $TILO_REQUIRE_COMPILER_CACHE != 1 ]] || { echo 'the required compiler cache did not start' >&2; exit 1; }
    export RUSTC_WRAPPER=
    echo 'sccache did not start; compiling without it' >&2
  fi
else
  echo 'compiling without a compiler cache' >&2
fi

python3 -I crates/os/ntfs-mount/native/build.py --out "$out/ntfs-mount/fuse-handoff.so"

# The app set this image ships, planned from the sources as a release plans one
# (tools/apps/app_release.py): the collection names every native app, and the
# tilolang packages that ship beside them until the native app they replace
# retires. The packing stage reads this plan rather than globbing a second list,
# so the image ships exactly the apps its collection names.
build_phase app-plan
python3 -I tools/apps/app_release.py --collection "$TILO_APP_COLLECTION" > "$out/app-plan.json" \
    || { echo "the $TILO_APP_COLLECTION app collection could not be planned" >&2; exit 1; }
tilolang_apps=$(python3 -I -c 'import json,sys; print(len(json.load(open(sys.argv[1])).get("tilolang", [])))' \
    "$out/app-plan.json")
printf 'app plan: %s, %s tilolang package(s)\n' "$TILO_APP_COLLECTION" "$tilolang_apps"

case "$TILO_RUST_LTO" in \
         thin|off) ;; \
         *) printf '%s\n' 'TILO_RUST_LTO must be thin or off' >&2; exit 1 ;; \
       esac \
    && built_sources="$target/.tilo-built-sources-apps${TILO_PREBUILT_APPS}-desktop${TILO_PREBUILT_DESKTOP}-browser${TILO_WITH_BROWSER}" \
    && case "$TILO_CARGO_FRESHNESS" in \
         cargo) ;; \
         clean) cargo clean --release && cargo clean --profile thin-app ;; \
         content) sh os/scripts/cargo-freshness.sh "$src" "$built_sources" \
                    || { cargo clean --release && cargo clean --profile thin-app; } ;; \
         *) printf '%s\n' 'TILO_CARGO_FRESHNESS must be cargo, clean or content' >&2; exit 1 ;; \
       esac \
    && if [ "${TILO_WITH_BROWSER}" = "0" ]; then THIN_EXCLUDES="browser"; else THIN_EXCLUDES=""; fi \
    && APP_FEATURES="camera/live-capture,settings/runtime-engine,messages/runtime-engine,osk/runtime-engine" \
    && if [ "${TILO_PREBUILT_DESKTOP}" = 1 ]; then \
         DESKTOP_RELEASE_ARGS=""; \
       else \
         DESKTOP_RELEASE_ARGS="-p tilo-session --bin tilo-session -p tilo-runtime --bin tilo-runtime -p launcher --bin tilo-launcher -p tilo-unlock-helper --bin tilo-unlock-helper"; \
       fi \
    && if [ "$tilolang_apps" != 0 ]; then \
         TILOLANG_RELEASE_ARGS="-p tilolang-cli"; \
         TILOLANG_RELEASE_BINS="--bin tilolang"; \
       else \
         TILOLANG_RELEASE_ARGS=""; \
         TILOLANG_RELEASE_BINS=""; \
       fi \
    && RELEASE_ARGS="${DESKTOP_RELEASE_ARGS} -p tilo-fs -p tilo-device-steps -p greeter -p imager -p tilo-runtime-bundle -p tilo-rollback -p tilo-auth -p tilo-boot-signin -p tilo-esp-write -p tilo-extension -p tilo-staging-reclaim -p tilo-install-helpers -p tilo-boot-order -p tilo-image-policy-check -p tilo-board-successor -p tilo-firstboot-files -p tilo-crypto -p tilo-user-identity -p tilo-device-identity -p tilo-cli -p app-cli -p ush -p tilo-projector-broker -p tilo-flight-log -p tilo-pdf -p tilo-app-package-cli -p tilo-control -p install -p cli-shim -p local-model-service -p tilo-model-gateway -p tilo-secret-service -p compat-service -p account-credentials -p linux-vm -p tilo-ntfs-mount -p tilo-device-vpn -p tilo-net-answers -p tilo-app-keeper -p tilo-audio-service -p tilo-messaging-service -p tilo-lock-screen -p osk -p tilolang-process ${TILOLANG_RELEASE_ARGS}" \
    && RELEASE_BINS="--bin fs --bin tilo-device-step --bin tilo-model-manager --bin tilo-speech-worker --bin tilo-model-gateway --bin tilo-secret-service --bin tilo-greeter --bin tilo-installer --bin tilo-firstboot-home-seeds --bin tilo-runtime-bundle --bin tilo-rollback --bin tilo-authd --bin tilo-auth --bin tilo-boot-signin --bin tilo-esp-write --bin tilo-extension --bin tilo-staging-reclaim --bin tilo-install-handoff-copy --bin tilo-install-dev-ssh --bin tilo-installer-offline-target --bin tilo-staging-handoff --bin tilo-vm-install-proof --bin tilo-boot-order --bin tilo-default-start --bin tilo-image-policy-check --bin tilo-board-successor --bin tilo-firstboot-files --bin tilo-crypto --bin tilo-image-trust --bin tilo-device-key-init --bin tilo --bin app --bin tilo-app-package --bin tilo-control --bin tilo-install --bin tilo-cli-shim --bin tilo-ush --bin tilo-projector --bin tilo-flight-recorder --bin tilo-session-report --bin tilo-pdf-helper --bin tilo-system-extension-manager --bin tilo-windows-runtime --bin tilo-linux-vm --bin tilo-account-credential --bin tilo-ntfs-mount --bin tilo-device-vpn --bin tilo-net-answers --bin tilo-app-keeper --bin tilo-audio --bin tilo-messaging --bin tilo-lock-screen --bin osk --bin tilolang-process ${TILOLANG_RELEASE_BINS}" \
    && if [ "$tilolang_apps" != 0 ]; then THIN_RUNTIMES=tilolang-process; else THIN_RUNTIMES=""; fi \
    && build_thin_apps() { \
         EXCLUDE_PACKAGES="${THIN_EXCLUDES}" \
         WITH_RUNTIMES="${THIN_RUNTIMES}" \
         APP_FEATURES="${APP_FEATURES}" \
         CARGO_PROFILE_THIN_APP_CODEGEN_UNITS="$([ "$TILO_RUST_LTO" = off ] && echo 16 || echo 1)" \
         EXTRA_CARGO_ARGS="$1" \
           os/scripts/build-thin-apps.sh; \
       } \
    && thin_pid="" models_pid="" \
    && if [ "${TILO_PREBUILT_APPS}" != 1 ] && [ -z "${TILO_APP_MODELS:-}" ]; then \
         { bash tools/ci/app-models.sh > "$scratch/tilo-app-models.log" 2>&1 & } \
         && models_pid=$!; \
       fi \
    && if [ "${TILO_PREBUILT_APPS}" != 1 ] && [ "$TILO_RUST_LTO" = off ]; then \
         { ( thin_started=$(date +%s); build_thin_apps "" \
             && printf 'TILO_BUILD_TIMING phase=rust-thin-apps-parallel elapsed_seconds=%s exit_code=0\n' "$(( $(date +%s) - thin_started ))" \
           ) > "$scratch/tilo-thin-apps.log" 2>&1 & } \
         && thin_pid=$!; \
       fi \
    && build_phase rust-release \
    && release_units="$([ "$TILO_RUST_LTO" = off ] && echo 16 || echo 8)" \
    && CARGO_PROFILE_RELEASE_LTO="$TILO_RUST_LTO" \
       CARGO_PROFILE_RELEASE_CODEGEN_UNITS="$release_units" \
       CARGO_PROFILE_RELEASE_DEBUG="$TILO_RUST_DEBUGINFO" \
       cargo build --release ${TILO_CARGO_TIMINGS:+--timings} ${RELEASE_ARGS} ${RELEASE_BINS} --features tilo-model-gateway/local-models,tilo-model-gateway/host-backend,imager/installation,imager/destination \
    && { [ -z "$TILO_CARGO_TIMINGS" ] || python3 -I os/scripts/cargo-timings-summary.py $target/cargo-timings/cargo-timing.html; } \
    && build_phase rust-windows-vm \
    && CARGO_PROFILE_RELEASE_LTO="$TILO_RUST_LTO" \
       CARGO_PROFILE_RELEASE_CODEGEN_UNITS="$release_units" \
       CARGO_PROFILE_RELEASE_DEBUG="$TILO_RUST_DEBUGINFO" \
       cargo build --release -p windows-vm \
         --bin tilo-windows-vm-control --bin tilo-windows-rail \
    && build_phase rust-pam \
    && CARGO_PROFILE_RELEASE_LTO="$TILO_RUST_LTO" \
       CARGO_PROFILE_RELEASE_CODEGEN_UNITS="$release_units" \
       CARGO_PROFILE_RELEASE_DEBUG="$TILO_RUST_DEBUGINFO" \
       cargo build --release -p pam-tilo --lib \
    && build_phase rust-output-validation \
    && missing=0 \
    && for b in tilo-session tilo-runtime tilo-device-step tilo-projector tilo-flight-recorder tilo-session-report tilo-pdf-helper fs tilo-linux-vm tilo-model-manager tilo-speech-worker tilo-model-gateway tilo-secret-service tilo-greeter tilo-installer tilo-firstboot-home-seeds tilo-runtime-bundle tilo-launcher tilo-unlock-helper tilo-rollback tilo-authd tilo-auth tilo-boot-signin tilo-esp-write tilo-extension tilo-staging-reclaim tilo-install-handoff-copy tilo-install-dev-ssh tilo-installer-offline-target tilo-staging-handoff tilo-vm-install-proof tilo-boot-order tilo-default-start tilo-image-policy-check tilo-board-successor tilo-firstboot-files tilo-crypto tilo-image-trust tilo-device-key-init tilo app tilo-app-package tilo-control tilo-install tilo-cli-shim tilo-ush tilo-system-extension-manager tilo-windows-vm-control tilo-windows-rail tilo-windows-runtime tilo-account-credential tilo-ntfs-mount tilo-device-vpn tilo-net-answers tilo-app-keeper tilo-audio tilo-messaging tilo-lock-screen osk tilolang-process; do \
         case "${TILO_PREBUILT_DESKTOP}:$b" in 1:tilo-session|1:tilo-launcher|1:tilo-unlock-helper|1:tilo-runtime) continue ;; esac; \
         if [ ! -x "$target/release/$b" ]; then \
           printf 'Required build output is missing: %s\n' "$b" >&2; missing=1; \
         fi; \
       done \
    && if [ ! -f "$target/release/libpam_tilo.so" ]; then \
         printf 'Required build output is missing: %s\n' libpam_tilo.so >&2; missing=1; \
       fi \
    && if [ -n "$TILOLANG_RELEASE_BINS" ] && [ ! -x "$target/release/tilolang" ]; then \
         printf 'Required build output is missing: %s\n' tilolang >&2; missing=1; \
       fi \
    && [ "$missing" = 0 ] \
    && build_phase rust-stage-outputs \
    && mkdir -p "$out" "$out"/thin-apps \
    && if [ "${TILO_PREBUILT_DESKTOP}" = 1 ]; then \
         cp os/desktop-artifacts/tilo-session \
            os/desktop-artifacts/tilo-runtime \
            os/desktop-artifacts/tilo-launcher \
            os/desktop-artifacts/tilo-unlock-helper \
            "$out"/; \
       else \
         cp $target/release/tilo-session \
            $target/release/tilo-runtime \
            $target/release/tilo-launcher \
            $target/release/tilo-unlock-helper \
            "$out"/; \
       fi \
    && cp $target/release/tilo-projector \
          $target/release/tilo-flight-recorder \
          $target/release/tilo-session-report \
          $target/release/tilo-pdf-helper \
          $target/release/fs \
          $target/release/tilo-device-step \
          $target/release/tilo-linux-vm \
          $target/release/tilo-model-manager \
          $target/release/tilo-speech-worker \
          $target/release/tilo-model-gateway \
          $target/release/tilo-secret-service \
          $target/release/tilo-system-extension-manager \
          $target/release/tilo-greeter \
          $target/release/tilo-installer \
          $target/release/tilo-firstboot-home-seeds \
          $target/release/tilo-runtime-bundle \
          $target/release/tilo-rollback \
          $target/release/tilo-authd \
          $target/release/tilo-auth \
          $target/release/libpam_tilo.so \
          $target/release/tilo-boot-signin \
          $target/release/tilo-esp-write \
          $target/release/tilo-extension \
          $target/release/tilo-staging-reclaim \
          $target/release/tilo-install-handoff-copy \
          $target/release/tilo-install-dev-ssh \
          $target/release/tilo-installer-offline-target \
          $target/release/tilo-staging-handoff \
          $target/release/tilo-vm-install-proof \
          $target/release/tilo-boot-order \
          $target/release/tilo-default-start \
          $target/release/tilo-image-policy-check \
          $target/release/tilo-board-successor \
          $target/release/tilo-firstboot-files \
          $target/release/tilo-crypto \
          $target/release/tilo-image-trust \
          $target/release/tilo-device-key-init \
          $target/release/tilo \
          $target/release/app \
          $target/release/tilo-app-package \
          $target/release/tilo-control \
          $target/release/tilo-install \
          $target/release/tilo-cli-shim \
          $target/release/tilo-ush \
          $target/release/tilo-windows-vm-control \
          $target/release/tilo-windows-rail \
          $target/release/tilo-windows-runtime \
          $target/release/tilo-account-credential \
          $target/release/tilo-ntfs-mount \
          $target/release/tilo-device-vpn \
          $target/release/tilo-net-answers \
          $target/release/tilo-app-keeper \
          $target/release/tilo-audio \
          $target/release/tilo-messaging \
          $target/release/tilo-lock-screen \
          $target/release/osk \
          $target/release/tilolang-process \
          "$out"/ \
    && if [ -n "$TILOLANG_RELEASE_BINS" ]; then cp $target/release/tilolang "$out"/; fi \
    && ln -sfn app "$out"/store \
    && ln -sfn tilo "$out"/system \
    && mkdir -p "$out"/completions/bash "$out"/completions/fish "$out"/completions/zsh \
    && $target/release/tilo completions bash > "$out"/completions/bash/tilo \
    && $target/release/app completions bash > "$out"/completions/bash/app \
    && $target/release/tilo completions fish > "$out"/completions/fish/tilo.fish \
    && $target/release/app completions fish > "$out"/completions/fish/app.fish \
    && $target/release/tilo completions zsh > "$out"/completions/zsh/_tilo \
    && $target/release/app completions zsh > "$out"/completions/zsh/_app \
    && build_phase rust-thin-apps \
    && if [ "${TILO_PREBUILT_APPS}" = 1 ]; then \
         mkdir -p "$out"/prebuilt-apps \
         && cp os/app-artifacts/*.tapp "$out"/prebuilt-apps/ \
         && mkdir -p "$out/prebuilt-runtime" && cp -a os/app-runtime/tilo-runtime "$out/prebuilt-runtime/" \
         && if [ -d os/app-runtime/tilolang ]; then cp -a os/app-runtime/tilolang "$out/prebuilt-runtime/"; fi \
         && cp os/app-runtime/runtime-bundle-publisher.hex "$out"/ \
         && RUNTIME_DYLIB="$(find "$out/prebuilt-runtime/tilo-runtime" -name libruntime_dylib.so -print -quit)" \
         && [ -n "$RUNTIME_DYLIB" ] \
         && cp "$RUNTIME_DYLIB" "$out"/libruntime_dylib.so \
         && test -x "$(dirname "$RUNTIME_DYLIB")/../bin/tilo-runtime-account-capability" \
         && cp "$(dirname "$RUNTIME_DYLIB")/../bin/tilo-runtime-account-capability" "$out"/ \
         && find "$(dirname "$RUNTIME_DYLIB")" -maxdepth 1 -name 'libtilo_*_engine.so' -printf '%f\n' \
              | sort > "$out"/runtime-engines \
         && while IFS= read -r engine; do cp "$(dirname "$RUNTIME_DYLIB")/$engine" "$out"/; done < "$out"/runtime-engines; \
       else \
         if [ -n "$thin_pid" ]; then \
           thin_status=0; wait "$thin_pid" || thin_status=$?; \
           cat "$scratch/tilo-thin-apps.log"; \
           [ "$thin_status" = 0 ]; \
         else \
           build_thin_apps "${TILO_CARGO_TIMINGS:+--timings}" \
           && { [ -z "$TILO_CARGO_TIMINGS" ] || python3 -I os/scripts/cargo-timings-summary.py $target/cargo-timings/cargo-timing.html; }; \
         fi \
         && for m in apps/*/tilo-app.toml; do \
              p="$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$(dirname "$m")/Cargo.toml" | head -1)"; \
              b="$(basename "$(sed -n 's/^bin  *= *"\([^"]*\)".*/\1/p' "$m" | head -1)")"; \
              case " ${THIN_EXCLUDES} " in *" $p "*) continue ;; esac; \
              [ -z "$b" ] || cp "$target/thin-app/$b" "$out"/thin-apps/; \
            done \
         && cp $target/thin-app/libruntime_dylib.so "$out"/ \
         && cp $target/thin-app/tilo-runtime-account-capability "$out"/ \
         && cp $target/thin-app/runtime-engines "$out"/runtime-engines \
         && while IFS= read -r engine; do cp "$target/thin-app/$engine" "$out"/; done < "$out"/runtime-engines \
         && if [ -n "$THIN_RUNTIMES" ]; then cp "$target/thin-app/tilolang-process" "$out"/thin-apps/; fi \
         && LIBSTD="$(find "$(rustc --print target-libdir)" -maxdepth 1 -name 'libstd-*.so' -print -quit)" \
         && [ -n "${LIBSTD}" ] \
         && cp "${LIBSTD}" "$out"/ \
         && build_phase rust-app-models \
         && rm -rf "$out/app-models" \
         && if [ -n "${TILO_APP_MODELS:-}" ]; then \
              cp -a "${TILO_APP_MODELS}" "$out"/app-models; \
            else \
              models_status=0; wait "$models_pid" || models_status=$?; \
              cat "$scratch/tilo-app-models.log"; \
              [ "$models_status" = 0 ] \
              && cp -a "$target/app-models" "$out"/app-models; \
            fi; \
       fi \
    && build_phase rust-boot-greeter \
    && if [ "${TILO_BOOT_GREETER:-}" != apart ]; then boot_greeter "$out"; fi \
    && build_phase chromium-engine \
    && if [ "${TILO_WITH_BROWSER}" = 0 ] || [ "${TILO_CHROMIUM_ENGINE:-}" = apart ]; then \
         :; \
       elif [ -n "${TILO_CHROMIUM_ENGINE:-}" ]; then \
         cp -a "$TILO_CHROMIUM_ENGINE"/chromium-engine "$out"/ \
         && test -x "$out"/chromium-engine/chromium/tilo-chromium-host; \
       else \
         chromium_engine "$out" "$target" "$assets"; \
       fi \
    && if [ "$TILO_CARGO_FRESHNESS" = content ]; then \
         sh os/scripts/cargo-freshness.sh --built "$built_sources"; \
       fi \
    && build_phase rust-cleanup \
    && if [ "$TILO_CARGO_FRESHNESS" = clean ]; then \
         find "$target" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +; \
       fi \
    || exit
# errexit ignores a failure anywhere in the list above but in its last command,
# so `|| exit` is what stops the build at a phase that fails.

# What the apps' packing (os/image/helpers/apps-payload.Containerfile) reads of the sources,
# so the packing reads no checkout and is keyed by the bytes it packs: each app's manifest,
# public surface and model crate (apps/), every language's pack (the packages carry English
# only) and, for the runtime bundle, the product font catalog's faces.
build_phase app-sources
rm -rf "$out/apps" "$out/language-packs" "$out/fonts"
for manifest in apps/*/tilo-app.toml; do
  app=$(dirname "$manifest")
  install -D -m 0644 "$manifest" "$out/$app/tilo-app.toml"
  [[ ! -f $app/surface.toml ]] || install -m 0644 "$app/surface.toml" "$out/$app/surface.toml"
  [[ ! -f $app-model-wasm/Cargo.toml ]] \
    || install -D -m 0644 "$app-model-wasm/Cargo.toml" "$out/$app-model-wasm/Cargo.toml"
done
# The tilolang packages, and the manifest of the runtime they run on: the packing
# compiles each of them with `tilolang build`, which searches `apps/tl` for the
# packages one uses, so every package's sources are staged, declared or not.
if [[ -d apps/tl ]]; then
  cp -a apps/tl "$out/apps/"
fi
install -D -m 0644 crates/lang/tilolang-process/tilo-runtime.toml "$out/crates/lang/tilolang-process/tilo-runtime.toml"
python3 -I tools/apps/language_packs.py build . "$out/language-packs"
if [[ $TILO_PREBUILT_APPS != 1 ]]; then
  python3 -I tools/font-catalog.py prepare crates/ui/ui-core/assets/fonts "$out/fonts" --scope product
fi

build_phase elf-hardening
os/scripts/verify-elf-hardening.sh "$out" \
    && { [ "${TILO_PREBUILT_APPS}" = 1 ] \
         || os/scripts/verify-elf-hardening.sh "$out"/thin-apps; } \
    && os/scripts/verify-elf-hardening.sh "$out"/libruntime_dylib.so "$out"/tilo-runtime-account-capability \
         $(sed "s|^|$out/|" "$out/runtime-engines") \
    || exit

# Every file's digest, which the stage that installs the tree's programs copies before it
# bind-mounts the tree (os/image/helpers/component-files.Containerfile): buildah keys that step by
# its instruction and the layer below, not by what it mounts, so a kept build would install an
# earlier tree without it.
build_phase digests
(cd "$out" && find . -type f ! -name sha256sums -print0 | LC_ALL=C sort -z | xargs -0 sha256sum) > "$scratch/sha256sums"
mv "$scratch/sha256sums" "$out/sha256sums"
