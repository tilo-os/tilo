# CI build environment: everything `cargo check`/`test`/`nextest` needs for the
# workspace, so jobs do not `dnf install` ~30 packages on every run.
#
# Rebuilt weekly by .forgejo/workflows/build-builder-image.yml. Fedora rather
# than the runner's default Ubuntu image: the OS is Fedora-based, so the build
# deps and their versions match what the image build uses.
FROM scratch AS external-assets
COPY --from=github-assets / /

FROM registry.fedoraproject.org/fedora:42

# nodejs: actions/checkout. capnproto: runtime-collab build.rs.
# vulkan-loader + mesa-vulkan-drivers: llvmpipe, so GPU tests run headless.
# Qt examples and LibreOffice are installed by the path-gated legacy-app E2E.
# GNOME Text Editor stays here: shell integration tests also launch it directly.
# bubblewrap is a test *subject*, not a tool — the launcher's confinement
# suites drive the sandbox and refuse to skip when it is absent. So is
# rust-analyzer, for the code editor's language-server suite, but it comes from
# rustup below: the shims in /usr/local/cargo/bin precede /usr/bin, so a Fedora
# package here would be shadowed by a shim reporting the component missing.
# pulseaudio supplies only the private software endpoint started by
# tools/with-headless-audio.sh; CI does not start or replace a host audio service.
# cryptsetup, swtpm and tpm2-tools are test subjects too: tilo-rollback's
# mechanism suite drives a real LUKS2 header and a private software TPM and must
# fail rather than report success when that security boundary is unavailable.
# The pinned NTFS reader archive bootstraps with autotools; libgcrypt-devel
# supplies its configure macro even though production crypto is disabled.
RUN dnf install -y \
      nodejs \
      capnproto \
      gcc gcc-c++ make cmake perl perl-FindBin python3 git jq ripgrep time yt-dlp \
      autoconf automake libtool libgcrypt-devel \
      clang clang-devel pkgconf-pkg-config \
      libxkbcommon-devel wayland-devel \
      libseat-devel libinput-devel systemd-devel libdrm-devel \
      mesa-libgbm-devel mesa-libGL-devel mesa-libEGL-devel \
      pipewire-devel pulseaudio \
      ffmpeg-free-devel libva-devel \
      gstreamer1 gstreamer1-devel gstreamer1-plugins-base-devel \
      gstreamer1-plugins-good gstreamer1-plugins-bad-free \
      gstreamer1-plugin-openh264 \
      sqlite-devel zlib-ng-compat-devel \
      fontconfig-devel freetype-devel harfbuzz-devel expat-devel \
      google-noto-sans-fonts google-noto-sans-mono-fonts \
      google-noto-serif-fonts google-noto-color-emoji-fonts \
      google-noto-sans-symbols-fonts google-noto-sans-symbols-2-fonts \
      google-noto-sans-math-fonts google-noto-sans-cjk-fonts \
      google-noto-sans-arabic-fonts google-noto-sans-hebrew-fonts \
      google-noto-sans-devanagari-fonts google-noto-sans-bengali-fonts \
      google-noto-sans-thai-fonts \
      fuse3-devel dbus-devel dbus-daemon xdg-dbus-proxy \
      nasm \
      vulkan-loader mesa-vulkan-drivers \
      bubblewrap gnome-text-editor \
      cryptsetup swtpm swtpm-tools tpm2-tools \
    && dnf clean all

# Required by production PDF capture tests. Use the same verified external
# asset context as sccache; runtime/library upgrades do not touch Fedora packages.
RUN --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    --mount=type=bind,source=tools/ci/install-pdfium.sh,target=/tmp/install-pdfium.sh,ro \
    --mount=type=bind,source=os/external-assets/github-assets.tsv,target=/tmp/tilo-assets.tsv,ro \
    bash /tmp/install-pdfium.sh /tilo-assets /usr /tmp/tilo-assets.tsv
ENV TILO_PDFIUM_LIB=/usr/lib64/libpdfium.so

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

# rustup, not Fedora's rust: the workspace pins its compiler in
# rust-toolchain.toml, and a job that silently used a different one would
# report failures the pinned toolchain does not have.
ARG RUST_CHANNEL
RUN curl -fsSL https://sh.rustup.rs \
      | sh -s -- -y --no-modify-path --profile minimal \
          --default-toolchain "${RUST_CHANNEL}" \
          --component rustfmt --component clippy \
          --component rust-src --component rust-analyzer \
    && cargo --version && rustc --version && rust-analyzer --version

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
    cargo install --locked cargo-nextest cargo-deny cargo-machete \
    && cargo nextest --version && cargo deny --version && cargo machete --version

# Match the workspace's own check so a stale image fails loudly rather than
# compiling against the wrong libraries.
COPY tools/ci/check-build-env.sh /usr/local/bin/tilo-check-build-env
RUN chmod +x /usr/local/bin/tilo-check-build-env && tilo-check-build-env

# Keep the optional CI linker in the final layer so it does not invalidate the
# expensive native dependencies or Rust tools above. Fedora 42's aarch64 mold
# pulls in a mimalloc build that uses ARM LSE atomics, while the native CI pool
# includes ARMv8.0 Graviton 1 hosts. Use mold's checksum-pinned baseline binary
# on both architectures so the qualification covers one consistent toolchain.
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
COPY tools/ci/native-mold-linker.sh /usr/local/bin/tilo-ci-mold
RUN --mount=type=bind,source=tools/ci/verify-native-mold.sh,target=/tmp/verify-native-mold.sh,ro \
    chmod 0755 /usr/local/bin/tilo-ci-mold \
    && bash /tmp/verify-native-mold.sh
