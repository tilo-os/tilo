# ---- The runtime bundle, the browser engine runtime and the app store -------
# Packed from the compiled tree (the `compiled` build context: os/image/compile.sh's
# OUT) in the OS builder, whose rootfs is the base the runtime bundle collects its
# native closure from. Nothing here compiles.
FROM builder-base AS apps-payload
COPY --from=builder-sources /build/ /build/
COPY --from=compiled / /out/
ARG TILO_PREBUILT_APPS=0
ARG TILO_WITH_BROWSER
ARG TILO_APP_COLLECTION_PUBLISHER=""
# Development images sign their runtime bundle, browser runtime and apps with
# identities derived from what they sign (os/scripts/derived-identity.py), so
# the same payload gives the same signatures and the same layers in every build;
# a key minted per build made every build's runtime and app layers new. Their
# keys are no secret, and no trust list but the image's own names them, as
# before. Releases pass 0 and their own identities.
ARG TILO_DERIVED_IDENTITIES=0
COPY --chmod=0755 os/scripts/derived-identity.py /usr/local/libexec/tilo-derived-identity
# A prebuilt runtime (TILO_PREBUILT_APPS=1) is where the bake below looks for it.
RUN if [ "${TILO_PREBUILT_APPS}" = 1 ]; then \
      mkdir -p /usr/lib/tilo-runtime && cp -a /out/prebuilt-runtime/tilo-runtime/. /usr/lib/tilo-runtime/; \
    fi

# ---- Bake a signed runtime bundle from THIS image's rootfs -----------------
# Runs at image-build time so the produced OS image ships a pre-mounted
# /usr/lib/tilo-runtime/<release>+native.<digest>/ that the launcher can find
# without a post-boot `tilo-install runtime install`.
#
# The signing key is generated fresh per build. Its pubkey is baked into
# /usr/share/tilo/runtime-bundle-trusted-publishers.txt so the launcher's
# signature-verify step accepts this bake. The private key is discarded
# once the bundle is signed — this is fine for a build-time attestation
# (nothing else ever needs to sign as this identity again).
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
ARG SOURCE_DATE_EPOCH=0
ENV SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}
# libstd is not on the system loader path, so it is included by name. The
# bundle's other libraries are the native closure of the runtime and the thin
# apps (os/scripts/collect-runtime-dependencies.py, with /out on the loader's
# path), including GLib and GStreamer, which the runtime loads on first use
# rather than at exec. The closure check then loads every bundled
# object and thin app the way the launcher does, with only the bundle's lib/
# on LD_LIBRARY_PATH.
RUN --mount=type=bind,source=os/scripts/verify-runtime-bundle-closure.sh,target=/usr/local/libexec/tilo-verify-runtime-bundle-closure.sh,ro \
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
      && python3 /build/tools/font-catalog.py verify \
        "/usr/lib/tilo-runtime/$(cat /out/runtime-version)/lib/fonts" --scope product \
      && bash /usr/local/libexec/tilo-verify-runtime-bundle-closure.sh \
        "/usr/lib/tilo-runtime/$(cat /out/runtime-version)"; \
    else \
      python3 /build/tools/font-catalog.py prepare \
        /build/crates/sdk/ui-core/assets/fonts /out/fonts --scope product \
      && if [ "${TILO_DERIVED_IDENTITIES}" = 1 ]; then \
           /usr/local/libexec/tilo-derived-identity runtime-bundle /out/libruntime_dylib.so \
             /out/tilo-runtime-account-capability /out/runtime-engines $(sed 's|^|/out/|' /out/runtime-engines) \
             $(find /out -maxdepth 1 -name 'libstd-*.so') /out/fonts > /out/runtime-bundle-identity.json; \
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
        engines=(); \
        while IFS= read -r engine; do engines+=("/out/$engine"); args+=(--include "/out/$engine"); done </out/runtime-engines; \
        dependencies="$(python3 /usr/local/libexec/tilo-collect-runtime-dependencies.py \
          --library-dir /out /out/libruntime_dylib.so /out/tilo-runtime-account-capability "${engines[@]}" /out/thin-apps/*)"; \
        while IFS= read -r dependency; do args+=(--include "$dependency"); done <<<"$dependencies"; \
        "${args[@]}"' bundle "${RUST_LIBSTD}" \
      && /out/tilo-runtime-bundle key pubkey \
        --identity /out/runtime-bundle-identity.json \
        > /out/runtime-bundle-publisher.hex \
      && /out/tilo-runtime-bundle verify \
        "/usr/lib/tilo-runtime/$(cat /out/runtime-version)" \
        --require-publisher "$(cat /out/runtime-bundle-publisher.hex)" \
      && bash /usr/local/libexec/tilo-verify-runtime-bundle-closure.sh \
        "/usr/lib/tilo-runtime/$(cat /out/runtime-version)" /out/tilo-runtime-account-capability /out/thin-apps/* \
      && rm -f /out/runtime-bundle-identity.json; \
    fi

# ---- The browser engine runtime ---------------------------------------------
# The Chromium engine is its own signed runtime bundle, apart from the Browser
# tapp: the app updates through the registry like every app, and the launcher
# mounts the engine into its sandbox read-only, verified against the trusted
# publishers, because the Browser's manifest lists it in
# `[package].optional-runtimes`. Its version is the browser's
# ui::webview::chromium::runtime::RUNTIME_VERSION (scripts/browser/
# test_engine_runtime.py keeps the two equal). Release builds sign it with the
# runtime publisher key; other builds with a key made for this build alone.
ARG TILO_BROWSER_RUNTIME_PUBLISHER=""
ARG CHROMIUM_RUNTIME_VERSION
RUN --mount=type=secret,id=browser_runtime_identity \
    : > /out/browser-runtime-publisher.hex \
    && if [ "${TILO_WITH_BROWSER}" != 0 ]; then \
         pinned="$(sed -n 's/^pub const RUNTIME_VERSION: &str = "\([^"]*\)";$/\1/p' \
              /build/crates/sdk/ui-core/src/webview/chromium/runtime.rs)" \
         && test -n "$pinned" \
         && test "$pinned" = "${CHROMIUM_RUNTIME_VERSION}" \
         && test -x /out/chromium-engine/chromium/tilo-chromium-host \
         && if [ -n "$TILO_BROWSER_RUNTIME_PUBLISHER" ]; then \
              test -s /run/secrets/browser_runtime_identity \
              && actual="$(/out/tilo-runtime-bundle key pubkey \
                   --identity /run/secrets/browser_runtime_identity)" \
              && test "$actual" = "$TILO_BROWSER_RUNTIME_PUBLISHER" \
              && identity=/run/secrets/browser_runtime_identity; \
            elif [ "${TILO_DERIVED_IDENTITIES}" = 1 ]; then \
              /usr/local/libexec/tilo-derived-identity browser-runtime /out/chromium-engine/chromium \
                > /out/browser-runtime-identity.json \
              && identity=/out/browser-runtime-identity.json; \
            else \
              /out/tilo-runtime-bundle key generate --out /out/browser-runtime-identity.json \
              && identity=/out/browser-runtime-identity.json; \
            fi \
         && /out/tilo-runtime-bundle build \
              --version "${CHROMIUM_RUNTIME_VERSION}" --arch "$(uname -m)" --from / \
              --include /out/chromium-engine/chromium \
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
# Apps compiled here are signed by the identity in the `app_publisher_identity`
# secret when the build is given one (Build / OS dev: DEV_APP_PUBLISHER_IDENTITY),
# else by one minted for this build. Ed25519 signs deterministically, so with
# the one identity the same binaries give the same package ids, payload paths
# and cohort archives in every build, and an update re-sends only the apps
# that changed. The key is in no trust list: see the store's COPY
# below. Secrets are not part of BuildKit's cache key and never enter a layer.
RUN --mount=type=secret,id=app_publisher_identity \
    export TILO_IMAGE_BUILD=1 \
    && if [ -s /run/secrets/app_publisher_identity ]; then \
         export IDENTITY_JSON=/run/secrets/app_publisher_identity; \
       elif [ "${TILO_DERIVED_IDENTITIES}" = 1 ]; then \
         /usr/local/libexec/tilo-derived-identity app-publisher /out/thin-apps /out/prebuilt-apps \
           > /tmp/tilo-app-publisher.json \
         && export IDENTITY_JSON=/tmp/tilo-app-publisher.json; \
       fi \
    && BIN_DIR=/out/thin-apps \
    TAPP_DIR=/out/prebuilt-apps \
    COHORT_TAPP_DIR=/usr/share/tilo-apps/cohort \
    EXPECTED_PUBKEY="${TILO_APP_COLLECTION_PUBLISHER}" \
    PKG_BIN=/out/tilo-app-package \
    INSTALL_BIN=/out/tilo-install \
    BUNDLE_BIN=/out/tilo-runtime-bundle \
    SHIM_BIN=/out/tilo-cli-shim \
    PUBKEY_OUT=/out/app-publisher.hex \
    RUNTIME_ID="tilo-runtime/$(cat /out/runtime-version)" \
    /build/os/scripts/pack-apps.sh

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
    && python3 /run/tilo-hardlink-identical.py /usr/share/tilo-apps/pkgs
