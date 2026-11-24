# ---- The runtime bundle, the browser engine runtime and the app store -------
# Packed from the compiled tree (the `compiled` build context: os/image/compile.sh's
# OUT) in the OS builder, whose rootfs is the base the runtime bundle collects its
# native closure from. Nothing here compiles, and nothing reads the sources: the
# compile left what the packing takes of them in the tree (each app's manifest,
# public surface and model crate under apps/, the language packs, the product
# fonts), so a change to a crate that ships no app leaves this stage's inputs as
# they were. The scripts it runs are copied, each to its repository path.
FROM builder-base AS apps-payload
COPY --from=compiled / /out/
COPY os/scripts/pack-apps.sh os/scripts/stamp-runtime.sh os/scripts/check-shipped-runtimes.sh \
     os/scripts/install-env-runtime.sh /build/os/scripts/
COPY tools/apps/stage_sdk_services.py /build/tools/apps/
# The tilolang packages' packing steps and the language runtimes' bundle
# builder, which the release's packing shares.
COPY tools/apps/tilolang_packages.sh tools/apps/runtime_packages.sh /build/tools/apps/
COPY os/usr/share/tilo/system-interfaces.toml /build/os/usr/share/tilo/system-interfaces.toml
COPY crates/core/tilo-app-package/max-app-bytes /build/crates/core/tilo-app-package/max-app-bytes
ARG TILO_PREBUILT_APPS=0
ARG TILO_WITH_BROWSER
# thin or off, as the compile was (os/image/compile.sh): a build without LTO only reports
# each app's size against its budget, a release enforces it.
ARG TILO_RUST_LTO=thin
ARG TILO_APP_COLLECTION_PUBLISHER=""
# A prebuilt runtime (TILO_PREBUILT_APPS=1) is where the bake below looks for it.
RUN if [ "${TILO_PREBUILT_APPS}" = 1 ]; then \
      mkdir -p /usr/lib/tilo-runtime && cp -a /out/prebuilt-runtime/tilo-runtime/. /usr/lib/tilo-runtime/ \
      && if [ -d /out/prebuilt-runtime/tilolang ]; then \
           cp -a /out/prebuilt-runtime/tilolang/. /usr/lib/tilo-runtime/; fi; \
    fi

# ---- Bake a signed runtime bundle from THIS image's rootfs -----------------
# Runs at image-build time so the produced OS image ships a pre-mounted
# /usr/lib/tilo-runtime/<release>+native.<digest>/ that the launcher can find
# without a post-boot `tilo-install runtime install`.
#
# The runtimes and the apps are signed by the identity in the
# `publisher_identity` secret when the build is given one, else by one
# generated for this build and discarded once they are signed. A development
# image is given the development identity (os/image/build-layer.sh), the RFC
# 8032 test-vector key it already trusts as its owner, so packing the same
# files signs the same bytes and the payload is the same in every build; a
# release generates its own. The publisher's pubkey is baked into
# /usr/share/tilo/runtime-bundle-trusted-publishers.txt so the launcher's
# signature-verify step accepts this bake. Secrets are not part of the layer
# cache key and never enter a layer.
# The release every app manifest under apps/ declares
# (`runtime = "tilo-runtime/<release>"`). The bundle baked here is one build of
# it, named by its ABI: `/usr/lib/tilo-runtime/<release>+native.<digest>/`,
# where the digest covers the bundle's payload (libruntime_dylib.so, the Rust
# libstd, the loader set). Thin apps link those Rust dylibs, which have no
# stable ABI, so pack-apps.sh stamps that exact id into every packaged app
# and the launcher, `tilo-install` and the Store match apps to runtimes by it:
# an app built against another build is refused, or its runtime fetched,
# instead of dying at exec with an unresolved `libstd-<hash>.so`.
# /out/runtime-version records the id for the steps below.
#
# Baked at /usr/lib/tilo-runtime/ — the path it will occupy on the booted
# system — and not into a staging directory, for the same reason the app
# store below is written at its final path: `tilo-install` refuses to install
# a package whose declared runtime is absent, and it looks only in the
# per-user bundle dir and this system one.
ARG RUNTIME_RELEASE=2026.07
ENV RUNTIME_RELEASE=${RUNTIME_RELEASE}
# The bundle and the app store below record when they were produced: the build
# scripts' SOURCE_DATE_EPOCH, not the fixed one the compile above ran with.
#
# The tilolang runtime bundle is baked here too, when the image's app set names a
# tilolang package: beside the Tilo runtime, at `/usr/lib/tilo-runtime/<version>`,
# signed by the same runtime publisher, so one trust root covers both. It is the
# binaries alone (tools/apps/tilolang_packages.sh), run on the Tilo runtime.
ARG SOURCE_DATE_EPOCH=0
ENV SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}
# libstd is not on the system loader path, so it is included by name. The
# bundle's other libraries are the native closure of the runtime, the thin apps
# and the tilolang host (os/scripts/collect-runtime-dependencies.py, with /out
# on the loader's path), including GLib and GStreamer, which the runtime loads
# on first use rather than at exec. The closure check then loads every bundled
# object and thin app the way the launcher does, with only the bundle's lib/
# on LD_LIBRARY_PATH.
RUN --mount=type=secret,id=publisher_identity \
    --mount=type=bind,source=os/scripts/verify-runtime-bundle-closure.sh,target=/usr/local/libexec/tilo-verify-runtime-bundle-closure.sh,ro \
    --mount=type=bind,source=tools/font-catalog.py,target=/usr/local/libexec/tilo-font-catalog.py,ro \
    --mount=type=bind,source=os/scripts/collect-runtime-dependencies.py,target=/usr/local/libexec/tilo-collect-runtime-dependencies.py,ro \
    --mount=type=bind,source=os/registry-runtime-publisher.hex,target=/usr/local/share/tilo-registry-runtime-publisher.hex,ro \
    grep -Eqx '[0-9a-f]{64}' /usr/local/share/tilo-registry-runtime-publisher.hex \
    && cp /usr/local/share/tilo-registry-runtime-publisher.hex /out/registry-runtime-publisher.hex \
    && if [ "${TILO_PREBUILT_APPS}" = 1 ]; then \
      test -s /out/runtime-bundle-publisher.hex \
      && runtime_dirs="$(find /usr/lib/tilo-runtime -mindepth 1 -maxdepth 1 -type d \
           -name "${RUNTIME_RELEASE}+native.*" -printf '%f\n')" \
      && [ "$(printf '%s\n' "$runtime_dirs" | grep -c .)" = 1 ] \
      && printf '%s\n' "$runtime_dirs" > /out/runtime-version \
      && /out/tilo-runtime-bundle verify \
        "/usr/lib/tilo-runtime/$(cat /out/runtime-version)" \
        --require-publisher "$(cat /out/runtime-bundle-publisher.hex)" \
      && python3 /usr/local/libexec/tilo-font-catalog.py verify \
        "/usr/lib/tilo-runtime/$(cat /out/runtime-version)/lib/fonts" --scope product \
      && bash /usr/local/libexec/tilo-verify-runtime-bundle-closure.sh \
        "/usr/lib/tilo-runtime/$(cat /out/runtime-version)"; \
    else \
      test -d /out/fonts \
      && if [ -s /run/secrets/publisher_identity ]; then \
           cp /run/secrets/publisher_identity /out/runtime-bundle-identity.json; \
         else \
           /out/tilo-runtime-bundle key generate --out /out/runtime-bundle-identity.json; \
         fi \
      && RUST_LIBSTD="$(find /out -maxdepth 1 -name 'libstd-*.so' -print -quit)" \
      && [ -n "${RUST_LIBSTD}" ] \
      && /bin/bash -eu -o pipefail -c ' \
        args=(/out/tilo-runtime-bundle build \
          --release "${RUNTIME_RELEASE}" --arch "$(uname -m)" --from / \
          --include /out/libruntime_dylib.so \
          --include-bin /out/tilo-runtime-account-capability \
          --include /out/fonts \
          --include "$1" \
          --identity /out/runtime-bundle-identity.json \
          --out-root /usr/lib/tilo-runtime \
          --version-out /out/runtime-version); \
        objects=(); [ ! -f /out/tilolang-process ] || objects=(/out/tilolang-process); \
        while IFS= read -r engine; do objects+=("/out/$engine"); args+=(--include "/out/$engine"); done </out/runtime-engines; \
        dependencies="$(python3 /usr/local/libexec/tilo-collect-runtime-dependencies.py \
          --library-dir /out /out/libruntime_dylib.so /out/tilo-runtime-account-capability "${objects[@]}" /out/thin-apps/*)"; \
        while IFS= read -r dependency; do args+=(--include "$dependency"); done <<<"$dependencies"; \
        "${args[@]}"' bundle "${RUST_LIBSTD}" \
      && /out/tilo-runtime-bundle key pubkey \
        --identity /out/runtime-bundle-identity.json \
        > /out/runtime-bundle-publisher.hex \
      && /out/tilo-runtime-bundle verify \
        "/usr/lib/tilo-runtime/$(cat /out/runtime-version)" \
        --require-publisher "$(cat /out/runtime-bundle-publisher.hex)" \
      && bash /usr/local/libexec/tilo-verify-runtime-bundle-closure.sh \
        "/usr/lib/tilo-runtime/$(cat /out/runtime-version)" /out/tilo-runtime-account-capability /out/thin-apps/* $(find /out -maxdepth 1 -name tilolang-process) \
      && tilolang_packages="$(python3 -I -c 'import json,sys; print(len(json.load(open(sys.argv[1])).get("tilolang", [])))' /out/app-plan.json 2>/dev/null || echo 0)" \
      && if [ "$tilolang_packages" != 0 ]; then \
           . /build/tools/apps/tilolang_packages.sh \
           && tilolang_version="$(tilolang_runtime_version /out)" \
           && tilolang_bake_runtime /out/tilo-runtime-bundle /out/thin-apps/tilolang-process \
                /out/runtime-bundle-identity.json \
                "/usr/lib/tilo-runtime/$tilolang_version" "$tilolang_version"; \
         fi \
      && language_runtimes="$(python3 -I -c 'import json,sys; plan=json.load(open(sys.argv[1])); print(" ".join(sorted({r.split("/", 1)[0] for r in plan.get("runtimes", [])} - {"tilo-runtime", "tilolang"})))' /out/app-plan.json 2>/dev/null || true)" \
      && for runtime in $language_runtimes; do \
           . /build/tools/apps/runtime_packages.sh \
           && PACK_RUNTIME_KEEP_FULL=1 runtime_package_build "$runtime" \
                /out/tilo-runtime-bundle /out /out/runtime-bundle-identity.json \
                /out/language-runtimes 0.1.0 >/dev/null \
           && mkdir -p "/out/language-runtime-packages/$runtime" \
           && cp "/out/language-runtimes/runtime-package/language/$runtime"/full-*.tapp \
                 "/out/language-runtime-packages/$runtime/$runtime.tapp"; \
         done \
      && rm -rf /out/language-runtimes \
      && rm -f /out/runtime-bundle-identity.json; \
    fi

# ---- The browser engine runtime ---------------------------------------------
# The Chromium engine is its own signed runtime bundle, apart from the Browser
# tapp: the app updates through the registry like every app, and the launcher
# mounts the engine into its sandbox read-only, verified against the trusted
# publishers, because the Browser's manifest lists it in
# `[package].optional-runtimes`. Its version is the browser's
# ui::webview::chromium::runtime::RUNTIME_VERSION (scripts/browser/
# test_engine_runtime.py keeps the two equal). It is signed by the build's
# publisher identity, as the runtime bundle above; TILO_BROWSER_RUNTIME_PUBLISHER,
# where a build names it, is the key that identity must have.
# The engine is the `chromium-engine` build context's chromium-engine/: the compiled
# tree where the compile built it, else the engine's own build (os-chromium-<arch>),
# which a compile keeping its build directory leaves out (os/dev/build-image.sh).
ARG TILO_BROWSER_RUNTIME_PUBLISHER=""
ARG CHROMIUM_RUNTIME_VERSION
RUN --mount=type=secret,id=publisher_identity \
    --mount=type=bind,from=chromium-engine,source=/,target=/run/tilo-chromium,ro \
    --mount=type=bind,source=crates/engines/tilo-webview/src/chromium/runtime.rs,target=/run/tilo-chromium-runtime.rs,ro \
    : > /out/browser-runtime-publisher.hex \
    && if [ "${TILO_WITH_BROWSER}" != 0 ]; then \
         pinned="$(sed -n 's/^pub const RUNTIME_VERSION: &str = "\([^"]*\)";$/\1/p' \
              /run/tilo-chromium-runtime.rs)" \
         && test -n "$pinned" \
         && test "$pinned" = "${CHROMIUM_RUNTIME_VERSION}" \
         && test -x /run/tilo-chromium/chromium-engine/chromium/tilo-chromium-host \
         && if [ -s /run/secrets/publisher_identity ]; then \
              identity=/run/secrets/publisher_identity; \
            else \
              test -z "$TILO_BROWSER_RUNTIME_PUBLISHER" \
              && /out/tilo-runtime-bundle key generate --out /out/browser-runtime-identity.json \
              && identity=/out/browser-runtime-identity.json; \
            fi \
         && { [ -z "$TILO_BROWSER_RUNTIME_PUBLISHER" ] \
              || [ "$(/out/tilo-runtime-bundle key pubkey --identity "$identity")" = "$TILO_BROWSER_RUNTIME_PUBLISHER" ]; } \
         && /out/tilo-runtime-bundle build \
              --version "${CHROMIUM_RUNTIME_VERSION}" --arch "$(uname -m)" --from / \
              --include /run/tilo-chromium/chromium-engine/chromium \
              --identity "$identity" \
              --out "/usr/lib/tilo-runtime/${CHROMIUM_RUNTIME_VERSION}" \
         && /out/tilo-runtime-bundle key pubkey \
              --identity "$identity" \
              > /out/browser-runtime-publisher.hex \
         && /out/tilo-runtime-bundle verify \
              "/usr/lib/tilo-runtime/${CHROMIUM_RUNTIME_VERSION}" \
              --require-publisher "$(cat /out/browser-runtime-publisher.hex)" \
         && test -x "/usr/lib/tilo-runtime/${CHROMIUM_RUNTIME_VERSION}/lib/chromium/tilo-chromium-host" \
         && rm -rf /out/chromium-engine /out/browser-runtime-identity.json; \
       fi

# ---- Pack + install every app into the image-wide app store -----------------
# Each app's size budget, which pack-apps.sh holds every package to. Copied
# here, after the builds, so a budget change does not rebuild anything.
COPY tools/perf/tapp-size-budget /build/tools/perf/tapp-size-budget
# Without this the launcher's registry is empty on a fresh install and the
# command palette's Applications provider has nothing to list, however many
# app binaries the image contains.
#
# The store is written at the path it will occupy on the booted system:
# `tilo-install` records absolute payload paths in registry.toml, so staging it
# elsewhere and relocating it afterwards would leave every row dangling.
#
# Packages take the ordinary route — `tilo-app-package pack` then `tilo-install install`
# — so a preinstalled app is described exactly as a user-installed one is, and
# the launcher grants it exactly the same thing. See os/scripts/pack-apps.sh.
#
# The app set is the image's collection (dist/apps/app-collections.toml), whose
# plan the compile wrote: it names every native app and the tilolang packages that
# ship beside them. A tilolang package is signed here by `tilolang build`, and the
# tilolang runtime it executes is baked into /usr/lib/tilo-runtime beside the Tilo
# runtime, where the launcher and `tilo-install` find both.
#
# Apps compiled here are signed by the build's publisher identity, as the
# runtimes above. The key is in no trust list: see the store's COPY below. The
# publisher key is in each package's signed root, whose hash is its id, so a minted key
# gives the same app a new id in every build; its installed tree is named by
# its payload instead (os/scripts/signed-records.py), the same in every build
# that packs the same files, and every row is verified again after the move.
#
# Each app is in the image once, as its installed tree and signed record. The
# dev VM daemon, the one reader of a complete archive, rebuilds it from those
# (crates/os/tilo-device-steps/src/cohort.rs); a second copy of every app as a `.tapp`
# was half of the app bytes in every update.
RUN --mount=type=secret,id=publisher_identity \
    --mount=type=bind,source=os/scripts/signed-records.py,target=/usr/local/libexec/tilo-signed-records.py,ro \
    export TILO_IMAGE_BUILD=1 \
    && if [ -s /run/secrets/publisher_identity ]; then \
         export IDENTITY_JSON=/run/secrets/publisher_identity; \
       fi \
    && REPO=/out \
    BIN_DIR=/out/thin-apps \
    APP_MODELS=/out/app-models \
    APP_PLAN="$([ -f /out/app-plan.json ] && printf %s /out/app-plan.json || true)" \
    TAPP_DIR=/out/prebuilt-apps \
    TILOLANG_BIN=/out/tilolang \
    RUNTIME_ROOT=/usr/lib/tilo-runtime \
    TAPP_BUDGET="$([ "$TILO_RUST_LTO" = off ] && echo report || echo enforce)" \
    EXPECTED_PUBKEY="${TILO_APP_COLLECTION_PUBLISHER}" \
    PKG_BIN=/out/tilo-app-package \
    INSTALL_BIN=/out/tilo-install \
    BUNDLE_BIN=/out/tilo-runtime-bundle \
    SHIM_BIN=/out/tilo-cli-shim \
    PUBKEY_OUT=/out/app-publisher.hex \
    RUNTIME_ID="tilo-runtime/$(cat /out/runtime-version)" \
    /build/os/scripts/pack-apps.sh \
    && python3 /usr/local/libexec/tilo-signed-records.py name-trees /usr/share/tilo-apps \
    && TILO_APPS_DATA_DIR=/usr/share/tilo-apps /out/tilo-install verify \
    && rm -rf /out/language-runtime-packages

# Buildah leaves the env-runtime mount out of the layer cache key; this ARG keys it on ENV_RUNTIME_REF.
ARG ENV_RUNTIME_REF
# The env-runtime environment image joins the apps in the read-only store, so
# the first environment opens offline. It takes the path every image takes on
# a device (`tilo-install image add`); see os/scripts/install-env-runtime.sh.
# Its glibc locales repeat one 2.6 MB LC_COLLATE under 19 names: the store's
# identical files are hard-linked, so the COPY into the rootfs carries 53 MB
# once. Readers verify each file against its signed hash, which a
# hard link does not change; the signed tree records no inode identity.
RUN --mount=type=bind,from=env-runtime,source=/,target=/tilo-env-runtime,ro \
    --mount=type=bind,source=os/scripts/hardlink-identical.py,target=/run/tilo-hardlink-identical.py,ro \
    /build/os/scripts/install-env-runtime.sh /tilo-env-runtime/env-runtime \
      /usr/share/tilo-apps /usr/share/tilo-apps /out/tilo-install \
    && python3 /run/tilo-hardlink-identical.py /usr/share/tilo-apps/pkgs /usr/share/tilo-apps/trees \
    && (cd / && find usr/lib/tilo-runtime usr/share/tilo-apps out/language-packs -type f -print0 \
        | LC_ALL=C sort -z | xargs -0 sha256sum) | sha256sum | cut -c1-64 > /out/payload-digest
