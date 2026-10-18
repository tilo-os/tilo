#!/usr/bin/env bash
# The OS's compiled tree: every binary, thin app, runtime library and native
# helper the image ships, compiled in the OS builder image (os/image/builder:
# the pinned base and the frozen RPM snapshot with the toolchains, so what it
# links runs on the image) into OUT, as os/components.toml reads it. No image
# build compiles: this runs as an ordinary build action, with the compiler
# cache, warm build directories and keys every other build of the workspace has.
#
# usage: compile.sh OUT | --dev OUT
#   CARGO_TARGET_DIR   the build directory (default target/)
#   TILO_RUST_LTO      thin (published images) or off (integration and development)
#   TILO_WITH_BROWSER  1 to build the browser's engine host, 0 not to
#   TILO_GITHUB_ASSET_CACHE  the checksum-pinned external assets (default
#                      os/external-assets/cache), for the CEF distribution
#
# The compiler cache is the one the caller gives: t's, through RUSTC_WRAPPER and
# its endpoint, as every build t runs gets it; else, with TILO_COMPILER_CACHE=local,
# sccache over SCCACHE_DIR, which os/dev/build-image.sh keeps between local builds;
# else none.
set -euo pipefail
# --dev: the development components instead (os/scripts/install-components.py --dev names them,
# with their dev features), for the dev image (os/Containerfile.dev).
if [[ ${1:-} == --dev ]]; then
  out=${2:?usage: compile.sh --dev OUT}
  mkdir -p "$out"
  out=$(cd "$out" && pwd)
  export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$(pwd)/target} LC_ALL=C.UTF-8 TZ=UTC CARGO_INCREMENTAL=0 SOURCE_DATE_EPOCH=0
  : "${TILO_DEV_RUST_LTO:=off}"
  case "$TILO_DEV_RUST_LTO" in thin|off) ;; *) echo 'TILO_DEV_RUST_LTO must be thin or off' >&2; exit 1 ;; esac
  dev_cargo_args=$(python3 os/scripts/install-components.py --dev --dry-run --format cargo)
  dev_bins=$(python3 os/scripts/install-components.py --dev --dry-run --format bins)
  [[ -n $dev_cargo_args && -n $dev_bins ]] || { echo 'install-components.py names no dev components' >&2; exit 1; }
  printf 'dev image build: cargo build --release %s\n' "$dev_cargo_args"
  # shellcheck disable=SC2086
  CARGO_PROFILE_RELEASE_LTO=$TILO_DEV_RUST_LTO CARGO_PROFILE_RELEASE_CODEGEN_UNITS=8 \
    bash tools/ci/measure.sh "dev cargo" -- cargo build --release $dev_cargo_args
  for b in $dev_bins; do cp "$CARGO_TARGET_DIR/release/$b" "$out/"; done
  os/scripts/verify-elf-hardening.sh "$out"
  exit 0
fi
out=${1:?usage: compile.sh OUT | --dev OUT}
src=$(pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
target=${CARGO_TARGET_DIR:-$src/target}
export CARGO_TARGET_DIR=$target
assets=${TILO_GITHUB_ASSET_CACHE:-$src/os/external-assets/cache}
scratch=${TMPDIR:-/tmp}
[[ -f Cargo.toml && -d os/image ]] || { echo 'compile.sh runs at the repository root' >&2; exit 1; }
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
# defaults is the one case where the trade-off is wrong — it ships a music
# player that cannot produce sound, a video player that synthesises frames
# instead of decoding them, a camera that cannot capture and clients whose
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
# what the build is for (os/dev/build-image.sh, .forgejo/workflows/build-image.yml),
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
: "${TILO_CARGO_BUILD_JOBS:=4}"
# Debug info the release profile compiles (it strips debug info from the
# output either way, keeping the symbol table a crash log names functions
# from): the profile's line tables by default; 0 skips them, which only makes
# a dev build compile faster.
: "${TILO_RUST_DEBUGINFO:=line-tables-only}"
# 1 prints where each cargo build spent its time (cargo --timings).
: "${TILO_CARGO_TIMINGS:=}"
: "${TILO_REQUIRE_COMPILER_CACHE:=0}"
# `shared` compiles through sccache: CI's runner cache when the build passes
# the actions_cache_url and actions_runtime_token secrets (the Actions cache
# protocol the runners serve), an S3 bucket when SCCACHE_BUCKET is set, else
# only the local cache mount. `off` is
# the independent reproduction lane of a publication: with no cache at all, a
# planted object in the shared cache makes the two builds differ. It runs no
# wrapper, and refuses compiler-cache credentials or settings.
# `publication` is a publication's own cache: short-lived role credentials
# (a session token is required, so the shared static key cannot stand in)
# for a bucket only publication runs can write, under tilo/publication/.
: "${TILO_COMPILER_CACHE:=local}"
export CARGO_BUILD_JOBS=$TILO_CARGO_BUILD_JOBS
: "${TILO_APP_COLLECTION_PUBLISHER:=}"
# Names the target cache, so builds that compile differently (os/dev/build-image.sh
# hashes the settings into it) keep separate target directories instead of
# rebuilding each other's artifacts. The cargo home and sccache are shared.
: "${TILO_BUILD_CACHE_KEY:=default}"

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
if [[ -n ${RUSTC_WRAPPER:-} ]]; then
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

python3 crates/runtime/ush/native/build.py --out "$out/ush-editor/bash-editor.so"
python3 crates/system/ntfs-mount/native/build.py --out "$out/ntfs-mount/fuse-handoff.so"

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
    && APP_FEATURES="music/audio,music/live,music/runtime-engine,video-player/decode,camera/live-capture,tube/live,tube/media,tube/grayjay-sync,tube/runtime-engine,mail/live,disks/live,video-editor/decode,video-editor/gst,typst-editor/runtime-engine,canvas/runtime-engine,mail/runtime-engine,settings/runtime-engine,calendar/runtime-engine,clocks/runtime-engine,contacts/runtime-engine,messages/runtime-engine" \
    && if [ "${TILO_PREBUILT_DESKTOP}" = 1 ]; then \
         DESKTOP_RELEASE_ARGS=""; \
       else \
         DESKTOP_RELEASE_ARGS="-p shell --bin shell --bin tilo-runtime-mcp -p launcher --bin tilo-launcher -p tilo-unlock-helper --bin tilo-unlock-helper"; \
       fi \
    && RELEASE_ARGS="${DESKTOP_RELEASE_ARGS} -p tilo-fs -p greeter -p installer -p imager -p runtime-bundle -p tilo-rollback -p tilo-sibling-unlock -p tilo-esp-write -p tilo-extension -p tilo-staging-reclaim -p tilo-install-helpers -p tilo-boot-order -p tilo-image-policy-check -p tilo-board-successor -p tilo-firstboot-files -p tilo-crypto -p tilo-user-identity -p tilo-cli -p app-cli -p ush -p tilo-projector -p tilo-session-record -p tilo-pdf -p tilo-app-package-cli -p tilo-control -p install -p cli-shim -p app-host -p local-model-service -p tilo-model-gateway -p tilo-secret-service -p system-extension -p compat-service -p account-custody -p legacy-vm -p tilo-ntfs-mount" \
    && RELEASE_BINS="--bin fs --bin tilo-model-manager --bin tilo-speech-worker --bin tilo-model-gateway --bin tilo-model-http --bin tilo-secret-service --bin tilo-greeter --bin tilo-installer --bin tilo-firstboot-home-seeds --bin tilo-installer-prepared-setup-extract --bin tilo-prepared-setup --bin tilo-runtime-bundle --bin tilo-app-host --bin tilo-rollback --bin tilo-sibling-unlock --bin tilo-esp-write --bin tilo-extension --bin tilo-staging-reclaim --bin tilo-install-handoff-copy --bin tilo-install-dev-ssh --bin tilo-vm-bootc-copy-cache --bin tilo-installer-offline-target --bin tilo-staging-handoff --bin tilo-vm-install-proof --bin tilo-boot-order --bin tilo-image-policy-check --bin tilo-board-successor --bin tilo-firstboot-files --bin tilo-crypto --bin tilo-image-trust --bin tilo-device-key-init --bin tilo --bin app --bin tilo-app-package --bin tilo-control --bin tilo-install --bin tilo-cli-shim --bin tilo-ush --bin tilo-projector --bin tilo-flight-recorder --bin tilo-session-report --bin tilo-pdf-helper --bin tilo-system-extension-manager --bin tilo-windows-runtime --bin tilo-relay --bin tilo-relay-admin --bin tilo-enroll --bin tilo-home-peer-control --bin tilo-relay-environment --bin tilo-relay-offers --bin tilo-legacy --bin tilo-account-credential --bin tilo-ntfs-mount" \
    && build_thin_apps() { \
         EXCLUDE_PACKAGES="${THIN_EXCLUDES}" \
         APP_FEATURES="${APP_FEATURES}" \
         CARGO_PROFILE_THIN_APP_CODEGEN_UNITS="$([ "$TILO_RUST_LTO" = off ] && echo 16 || echo 1)" \
         CARGO_PROFILE_THIN_APP_DEBUG="$TILO_RUST_DEBUGINFO" \
         EXTRA_CARGO_ARGS="$1" \
           os/scripts/build-thin-apps.sh; \
       } \
    && thin_pid="" \
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
       cargo build --release ${TILO_CARGO_TIMINGS:+--timings} ${RELEASE_ARGS} -p tilo-relay ${RELEASE_BINS} --features tilo-relay/web,tilo-model-gateway/local-models,tilo-model-gateway/host-backend \
    && { [ -z "$TILO_CARGO_TIMINGS" ] || python3 os/scripts/cargo-timings-summary.py $target/cargo-timings/cargo-timing.html; } \
    && build_phase rust-windows-vm \
    && CARGO_PROFILE_RELEASE_LTO="$TILO_RUST_LTO" \
       CARGO_PROFILE_RELEASE_CODEGEN_UNITS="$release_units" \
       CARGO_PROFILE_RELEASE_DEBUG="$TILO_RUST_DEBUGINFO" \
       cargo build --release -p windows-vm \
         --bin tilo-windows-vm-control --bin tilo-windows-rail \
    && build_phase rust-output-validation \
    && missing=0 \
    && for b in shell tilo-runtime-mcp tilo-projector tilo-flight-recorder tilo-session-report tilo-pdf-helper fs tilo-relay tilo-relay-admin tilo-enroll tilo-home-peer-control tilo-relay-environment tilo-relay-offers tilo-legacy tilo-model-manager tilo-speech-worker tilo-model-gateway tilo-model-http tilo-secret-service tilo-greeter tilo-installer tilo-firstboot-home-seeds tilo-installer-prepared-setup-extract tilo-prepared-setup tilo-runtime-bundle tilo-launcher tilo-app-host tilo-unlock-helper tilo-rollback tilo-sibling-unlock tilo-esp-write tilo-extension tilo-staging-reclaim tilo-install-handoff-copy tilo-install-dev-ssh tilo-vm-bootc-copy-cache tilo-installer-offline-target tilo-staging-handoff tilo-vm-install-proof tilo-boot-order tilo-image-policy-check tilo-board-successor tilo-firstboot-files tilo-crypto tilo-image-trust tilo-device-key-init tilo app tilo-app-package tilo-control tilo-install tilo-cli-shim tilo-ush tilo-system-extension-manager tilo-windows-vm-control tilo-windows-rail tilo-windows-runtime tilo-account-credential tilo-ntfs-mount; do \
         case "${TILO_PREBUILT_DESKTOP}:$b" in 1:shell|1:tilo-launcher|1:tilo-unlock-helper|1:tilo-runtime-mcp) continue ;; esac; \
         if [ ! -x "$target/release/$b" ]; then \
           printf 'Required build output is missing: %s\n' "$b" >&2; missing=1; \
         fi; \
       done \
    && [ "$missing" = 0 ] \
    && build_phase rust-stage-outputs \
    && mkdir -p "$out" "$out"/thin-apps \
    && if [ "${TILO_PREBUILT_DESKTOP}" = 1 ]; then \
         cp os/desktop-artifacts/shell \
            os/desktop-artifacts/tilo-runtime-mcp \
            os/desktop-artifacts/tilo-launcher \
            os/desktop-artifacts/tilo-unlock-helper \
            "$out"/; \
       else \
         cp $target/release/shell \
            $target/release/tilo-runtime-mcp \
            $target/release/tilo-launcher \
            $target/release/tilo-unlock-helper \
            "$out"/; \
       fi \
    && cp $target/release/tilo-projector \
          $target/release/tilo-flight-recorder \
          $target/release/tilo-session-report \
          $target/release/tilo-pdf-helper \
          $target/release/fs \
          $target/release/tilo-relay \
          $target/release/tilo-relay-admin \
          $target/release/tilo-enroll \
          $target/release/tilo-home-peer-control \
          $target/release/tilo-relay-environment \
          $target/release/tilo-relay-offers \
          $target/release/tilo-legacy \
          $target/release/tilo-model-manager \
          $target/release/tilo-speech-worker \
          $target/release/tilo-model-gateway \
          $target/release/tilo-model-http \
          $target/release/tilo-secret-service \
          $target/release/tilo-system-extension-manager \
          $target/release/tilo-greeter \
          $target/release/tilo-installer \
          $target/release/tilo-firstboot-home-seeds \
          $target/release/tilo-installer-prepared-setup-extract \
          $target/release/tilo-prepared-setup \
          $target/release/tilo-runtime-bundle \
          $target/release/tilo-app-host \
          $target/release/tilo-rollback \
          $target/release/tilo-sibling-unlock \
          $target/release/tilo-esp-write \
          $target/release/tilo-extension \
          $target/release/tilo-staging-reclaim \
          $target/release/tilo-install-handoff-copy \
          $target/release/tilo-install-dev-ssh \
          $target/release/tilo-vm-bootc-copy-cache \
          $target/release/tilo-installer-offline-target \
          $target/release/tilo-staging-handoff \
          $target/release/tilo-vm-install-proof \
          $target/release/tilo-boot-order \
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
          "$out"/ \
    && ln -s app "$out"/store \
    && ln -s tilo "$out"/system \
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
           && { [ -z "$TILO_CARGO_TIMINGS" ] || python3 os/scripts/cargo-timings-summary.py $target/cargo-timings/cargo-timing.html; }; \
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
         && LIBSTD="$(find "$(rustc --print target-libdir)" -maxdepth 1 -name 'libstd-*.so' -print -quit)" \
         && [ -n "${LIBSTD}" ] \
         && cp "${LIBSTD}" "$out"/; \
       fi \
    && build_phase chromium-engine \
    && if [ "${TILO_WITH_BROWSER}" != 0 ]; then \
         case "$(uname -m)" in \
           x86_64) cef_platform=linux64 ;; \
           aarch64) cef_platform=linuxarm64 ;; \
           *) echo "no Chromium engine for $(uname -m)" >&2; exit 1 ;; \
         esac \
         && cef_version="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["chromium_version"])' scripts/browser/cef-lock.json)" \
         && python3 scripts/browser/build-host.py --platform "$cef_platform" \
              --sdk-archive "$assets/cef-${cef_version}-${cef_platform}-minimal.tar.bz2" \
              --runtime "$out"/chromium-engine --jobs "${TILO_CARGO_BUILD_JOBS}" \
         && test -x "$out"/chromium-engine/chromium/tilo-chromium-host; \
       fi \
    && if [ "$TILO_CARGO_FRESHNESS" = content ]; then \
         sh os/scripts/cargo-freshness.sh --built "$built_sources"; \
       fi \
    && build_phase rust-cleanup \
    && if [ "$TILO_CARGO_FRESHNESS" = clean ]; then \
         find "$target" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +; \
       fi

os/scripts/verify-elf-hardening.sh "$out" \
    && { [ "${TILO_PREBUILT_APPS}" = 1 ] \
         || os/scripts/verify-elf-hardening.sh "$out"/thin-apps; } \
    && os/scripts/verify-elf-hardening.sh "$out"/libruntime_dylib.so "$out"/tilo-runtime-account-capability \
         $(sed "s|^|$out/|" "$out/runtime-engines")
