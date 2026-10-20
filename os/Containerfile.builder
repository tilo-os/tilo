# CI build environment: everything `cargo check`/`test`/`nextest` needs for the
# workspace, so jobs do not `dnf install` ~30 packages on every run.
#
# Rebuilt weekly by .forgejo/workflows/build-builder-image.yml. Fedora rather
# than the runner's default Ubuntu image: the OS is Fedora-based, so the build
# deps and their versions match what the image build uses.
FROM scratch AS external-assets
COPY --from=github-assets / /

FROM registry.fedoraproject.org/fedora:44@sha256:55159f5c92b7baf5f1c1ca1d727c95e77a4cb77c0829b2c11740a700ff4ea02b

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

# The workspace's build dependencies, the test suites' runtime (GStreamer
# plugins, fonts, lavapipe, PulseAudio, the sandbox/LUKS/TPM test subjects) and
# the pinned cargo-nextest come from the same script and profile the developer
# distrobox (dist/dev/Containerfile.fedora44) installs, so CI builds and tests
# against exactly what developers do. The script also bootstraps the pinned Rust
# toolchain through its checksum-pinned rustup-init. rustup, not Fedora's rust:
# the workspace pins its compiler in rust-toolchain.toml, and a job that
# silently used a different one would report failures the pinned toolchain does
# not have. rust-analyzer is a test subject too (the code editor's
# language-server suite); it comes from rustup because the shims in
# /usr/local/cargo/bin precede /usr/bin, so a Fedora package would be shadowed
# by a shim reporting the component missing.
ARG RUST_CHANNEL
RUN --mount=type=bind,source=dist/ci/install-build-deps.sh,target=/tmp/install-build-deps.sh,ro \
    test -n "${RUST_CHANNEL}" \
    && TILO_RUST_TOOLCHAIN="${RUST_CHANNEL}" \
       TILO_RUST_COMPONENTS="rustfmt clippy rust-src rust-analyzer" \
       bash /tmp/install-build-deps.sh fedora test \
    && rustc --version && rust-analyzer --version && cargo nextest --version

# The X11 bridge the shell's X11 clipboard tests drive: xwayland-satellite
# built from pinned source with Tilo's patch
# (os/external-assets/xwayland-satellite/README.md).
RUN --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    --mount=type=bind,source=tools/ci/install-xwayland-satellite.sh,target=/tmp/install-xwayland-satellite.sh,ro \
    --mount=type=bind,source=os/external-assets/xwayland-satellite,target=/tmp/xwayland-satellite,ro \
    --mount=type=bind,source=os/external-assets/github-assets.tsv,target=/tmp/tilo-assets.tsv,ro \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    bash /tmp/install-xwayland-satellite.sh /tilo-assets /usr /tmp/tilo-assets.tsv /tmp/xwayland-satellite

# CI-only workflow tooling. nodejs: actions/checkout. ripgrep, time, yt-dlp:
# workflow and measurement tools (tools/ci/measure.sh needs GNU time). Qt
# examples and LibreOffice are installed by the path-gated legacy-app E2E.
RUN dnf install -y \
      nodejs \
      ripgrep time yt-dlp \
    && dnf clean all

# The launcher's signed OSTree/OpenPGP capture fixture (launcher-root) commits
# a GPG-signed OSTree repository and compiles the OpenPGP engine probe against
# gpgme, as the image build of that engine does.
RUN dnf install -y --setopt=install_weak_deps=False ostree gpgme-devel \
    && dnf clean all && rm -rf /var/cache/dnf

# Required by production PDF capture tests. Use the same verified external
# asset context as sccache; runtime/library upgrades do not touch Fedora packages.
RUN --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    --mount=type=bind,source=tools/ci/install-pdfium.sh,target=/tmp/install-pdfium.sh,ro \
    --mount=type=bind,source=os/external-assets/github-assets.tsv,target=/tmp/tilo-assets.tsv,ro \
    bash /tmp/install-pdfium.sh /tilo-assets /usr /tmp/tilo-assets.tsv
ENV TILO_PDFIUM_LIB=/usr/lib64/libpdfium.so

# Remote compiler caching is part of the build environment rather than a
# per-job download. Pin and verify the upstream static binary so every shard
# gets the same client without spending runner CPU compiling sccache itself.
ARG SCCACHE_VERSION=0.17.0
ARG SCCACHE_SHA256_X86_64=67c4a96dd237c1f518f6b36083f270f9976d516f1e57fce891755ea782e50006
ARG SCCACHE_SHA256_AARCH64=821a86343191aa1cbab74bd42f9e93c9a63bf85e4742945f40d3ae84193c1c77
RUN --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    case "$(uname -m)" in \
      x86_64) sccache_arch=x86_64; sccache_sha="$SCCACHE_SHA256_X86_64" ;; \
      aarch64) sccache_arch=aarch64; sccache_sha="$SCCACHE_SHA256_AARCH64" ;; \
      *) echo "unsupported sccache architecture: $(uname -m)" >&2; exit 1 ;; \
    esac \
    && archive="sccache-v${SCCACHE_VERSION}-${sccache_arch}-unknown-linux-musl.tar.gz" \
    && echo "${sccache_sha}  /tilo-assets/${archive}" | sha256sum -c - \
    && tar -xzf "/tilo-assets/${archive}" -C /tmp \
    && install -m 0755 \
      "/tmp/sccache-v${SCCACHE_VERSION}-${sccache_arch}-unknown-linux-musl/sccache" \
      /usr/local/bin/sccache \
    && rm -rf "/tmp/sccache-v${SCCACHE_VERSION}-${sccache_arch}-unknown-linux-musl" \
    && sccache --version

# Keep tool-install downloads and extracted crate sources out of the image.
# CI restores its own Cargo downloads under /cargo-home; only these binaries ship.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo install --locked cargo-deny cargo-machete \
    && cargo deny --version && cargo machete --version

# Android NDK and Rust targets for check.yml's Android lane (docs/android.md).
# The NDK ships Linux host tools for x86_64 only, so the ARM builder, whose
# jobs never run that lane, does not carry it. API 29 is the host's minimum.
RUN --mount=type=bind,source=tools/ci/install-android-ndk.sh,target=/tmp/install-android-ndk.sh,ro \
    if [ "$(uname -m)" = x86_64 ]; then \
      TILO_RUST_TOOLCHAIN="${RUST_CHANNEL}" bash /tmp/install-android-ndk.sh \
      && dnf clean all && rm -rf /var/cache/dnf; \
    fi
# The browser target for check.yml's wasm lane.
RUN rustup target add --toolchain "${RUST_CHANNEL}" wasm32-unknown-unknown
# The web build's toolchain (`tilo-app build --platform web`, the web core's
# scripts): binaryen's wasm-opt, lld's wasm-ld, Node.js, brotli, binutils' ar,
# and wasm-bindgen's CLI at exactly the version Cargo.lock pins the crate at,
# from its checksum-pinned upstream release.
ARG WASM_BINDGEN_VERSION=0.2.108
RUN --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    --mount=type=bind,source=os/external-assets/github-assets.tsv,target=/tmp/tilo-assets.tsv,ro \
    dnf install -y --setopt=install_weak_deps=False binaryen lld nodejs brotli binutils \
    && dnf clean all && rm -rf /var/cache/dnf \
    && archive="wasm-bindgen-${WASM_BINDGEN_VERSION}-$(uname -m)-unknown-linux-musl.tar.gz" \
    && sha=$(awk -F '\t' -v name="$archive" '$1 !~ /^#/ && $2 == name {print $1}' /tmp/tilo-assets.tsv) \
    && test -n "$sha" \
    && echo "${sha}  /tilo-assets/${archive}" | sha256sum --check --strict \
    && tar -xzf "/tilo-assets/${archive}" -C /tmp \
    && install -m 0755 "/tmp/${archive%.tar.gz}/wasm-bindgen" /usr/local/bin/wasm-bindgen \
    && rm -rf "/tmp/${archive%.tar.gz}" \
    && wasm-bindgen --version && wasm-opt --version && wasm-ld --version && node --version
ENV ANDROID_NDK_HOME=/opt/android-ndk \
    ANDROID_NDK_ROOT=/opt/android-ndk \
    CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android29-clang \
    CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android29-clang \
    CC_aarch64_linux_android=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android29-clang \
    CXX_aarch64_linux_android=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android29-clang++ \
    AR_aarch64_linux_android=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar \
    CC_x86_64_linux_android=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android29-clang \
    CXX_x86_64_linux_android=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android29-clang++ \
    AR_x86_64_linux_android=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar

# Windows, cross-compiled from Linux (docs/publishing-on-t.md, Windows): Rust's MSVC
# targets and cargo-xwin, which drives clang-cl and lld-link (already here
# from Fedora's clang and lld). The MSVC CRT and Windows SDK that cargo-xwin
# downloads are deliberately not in the image: Microsoft's license does not
# allow redistributing them, and this image is pullable by anyone. The
# windows-sdk-<arch> checks download them into t's private store instead.
ARG CARGO_XWIN_VERSION=0.23.1
RUN --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    --mount=type=bind,source=os/external-assets/github-assets.tsv,target=/tmp/tilo-assets.tsv,ro \
    rustup target add --toolchain "${RUST_CHANNEL}" x86_64-pc-windows-msvc aarch64-pc-windows-msvc \
    && archive="cargo-xwin-v${CARGO_XWIN_VERSION}.$(uname -m)-unknown-linux-musl.tar.gz" \
    && sha=$(awk -F '\t' -v name="$archive" '$1 !~ /^#/ && $2 == name {print $1}' /tmp/tilo-assets.tsv) \
    && test -n "$sha" \
    && echo "${sha}  /tilo-assets/${archive}" | sha256sum --check --strict \
    && tar -xzf "/tilo-assets/${archive}" -C /usr/local/bin cargo-xwin \
    && chmod 0755 /usr/local/bin/cargo-xwin \
    && cargo-xwin --version && clang-cl --version && lld-link --version
# x86_64 only, because Fedora builds Wine for x86 only: Wine runs Inno Setup's
# compiler (the imager's Windows installer, apps/imager/packaging/
# package-windows.sh) and the x86_64 Windows test executables. Inno Setup is
# installed into a throwaway Wine prefix and only its program directory kept,
# at /opt/inno-setup. jsign signs Authenticode with a key that stays in AWS
# KMS (no OpenSSL here: os/tests/no_openssl_runtime.sh keeps it out of every
# image; the JDK checks the signing certificate). PyYAML and
# jsonschema check the WinGet manifests against WinGet's published schemas.
# wine-core alone loops forever creating a prefix: wine.inf copies files that
# wine-common (winmd metadata) and wine-cms (the sRGB profile) ship.
ARG INNO_SETUP_VERSION=6.7.3
ARG JSIGN_VERSION=7.6
RUN --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    --mount=type=bind,source=os/external-assets/github-assets.tsv,target=/tmp/tilo-assets.tsv,ro \
    --mount=type=bind,source=tools/ci/windows/wine-prefix.sh,target=/tmp/wine-prefix.sh,ro \
    if [ "$(uname -m)" = x86_64 ]; then \
      dnf install -y --setopt=install_weak_deps=False \
        wine-core wine-common wine-cms java-25-openjdk-headless \
        python3-pyyaml python3-jsonschema \
      && dnf clean all && rm -rf /var/cache/dnf \
      && for archive in "innosetup-${INNO_SETUP_VERSION}.exe" "jsign-${JSIGN_VERSION}.jar"; do \
           sha=$(awk -F '\t' -v name="$archive" '$1 !~ /^#/ && $2 == name {print $1}' /tmp/tilo-assets.tsv) \
           && test -n "$sha" \
           && echo "${sha}  /tilo-assets/${archive}" | sha256sum --check --strict || exit 1; \
         done \
      && export WINEPREFIX=/tmp/inno-prefix \
      && bash /tmp/wine-prefix.sh \
      && wine "/tilo-assets/innosetup-${INNO_SETUP_VERSION}.exe" /VERYSILENT /SUPPRESSMSGBOXES \
           /NORESTART /CURRENTUSER /PORTABLE=1 '/DIR=C:\inno-setup' \
      && wineserver -w \
      && cp -r /tmp/inno-prefix/drive_c/inno-setup /opt/inno-setup \
      && rm -rf /tmp/inno-prefix /tmp/.wine-* \
      && test -f /opt/inno-setup/ISCC.exe \
      && install -D -m 0644 "/tilo-assets/jsign-${JSIGN_VERSION}.jar" /usr/local/share/jsign/jsign.jar \
      && printf '#!/bin/sh\nexec java -jar /usr/local/share/jsign/jsign.jar "$@"\n' > /usr/local/bin/jsign \
      && chmod 0755 /usr/local/bin/jsign \
      && jsign --help >/dev/null; \
    fi

# Native browser tests use Fedora's signed Chromium RPM, not a per-job download.
# Bypass its desktop launcher: tests supply their own flags and fresh profiles.
# Keep this late so browser updates do not rebuild the cross toolchains.
ENV CHROME=/usr/lib64/chromium-browser/chromium-browser
RUN dnf install -y --setopt=install_weak_deps=False chromium \
    && dnf clean all && rm -rf /var/cache/dnf \
    && test -x "$CHROME" && "$CHROME" --version

# Match the workspace's own check so a stale image fails loudly rather than
# compiling against the wrong libraries.
COPY tools/ci/check-build-env.sh /usr/local/bin/tilo-check-build-env
RUN chmod +x /usr/local/bin/tilo-check-build-env && tilo-check-build-env

# Keep the CI linker in the final layer so it does not invalidate the
# expensive native dependencies or Rust tools above. Fedora's aarch64 mold
# package pulls in a mimalloc build that uses ARM LSE atomics, while the native
# CI pool includes ARMv8.0 Graviton 1 hosts. Use mold's checksum-pinned baseline
# binary on both architectures so the qualification covers one consistent
# toolchain.
ARG MOLD_VERSION=2.40.4
ARG MOLD_SHA256_X86_64=4c999e19ffa31afa5aa429c679b665d5e2ca5a6b6832ad4b79668e8dcf3d8ec1
ARG MOLD_SHA256_AARCH64=c799b9ccae8728793da2186718fbe53b76400a9da396184fac0c64aa3298ec37
RUN --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    case "$(uname -m)" in \
      x86_64) mold_arch=x86_64; mold_sha="$MOLD_SHA256_X86_64" ;; \
      aarch64) mold_arch=aarch64; mold_sha="$MOLD_SHA256_AARCH64" ;; \
      *) echo "unsupported mold architecture: $(uname -m)" >&2; exit 1 ;; \
    esac \
    && archive="mold-${MOLD_VERSION}-${mold_arch}-linux.tar.gz" \
    && source="mold-${MOLD_VERSION}-${mold_arch}-linux" \
    && echo "${mold_sha}  /tilo-assets/${archive}" | sha256sum -c - \
    && tar -xzf "/tilo-assets/${archive}" -C /tmp \
    && install -m 0755 "/tmp/${source}/bin/mold" /usr/local/bin/mold \
    && install -m 0755 "/tmp/${source}/bin/ld.mold" /usr/local/bin/ld.mold \
    && mkdir -p /usr/local/share/tilo-builder \
    && printf '%s  %s\n' "$mold_sha" "$archive" \
        > /usr/local/share/tilo-builder/mold-source.txt \
    && rm -rf "/tmp/${source}" \
    && mold --version
#
# mold is every job's default linker for the native GNU target. CI jobs point
# CARGO_HOME at their restored download cache, so the image's own CARGO_HOME
# config would not be read. The root /.cargo/config.toml is: Cargo searches it
# from every working directory. It sets only the linker and the wrapper's
# explicit target, so the workspace's hardening rustflags still apply, and a
# cross target keeps its ordinary linker.
COPY tools/ci/native-mold-linker.sh /usr/local/bin/tilo-ci-mold
RUN --mount=type=bind,source=tools/ci/verify-native-mold.sh,target=/tmp/verify-native-mold.sh,ro \
    chmod 0755 /usr/local/bin/tilo-ci-mold \
    && target="$(uname -m)-unknown-linux-gnu" \
    && mkdir -p /.cargo \
    && printf '[target.%s]\nlinker = "/usr/local/bin/tilo-ci-mold"\n\n[env]\nTILO_CI_MOLD_TARGET = "%s"\n' \
        "$target" "$target" > /.cargo/config.toml \
    && bash /tmp/verify-native-mold.sh
