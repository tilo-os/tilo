# CI build environment: everything `cargo check`/`test`/`nextest` needs for the
# workspace, so jobs do not `dnf install` ~30 packages on every run.
#
# Rebuilt weekly by .forgejo/workflows/build-builder-image.yml. Fedora rather
# than the runner's default Ubuntu image: the OS is Fedora-based, so the build
# deps and their versions match what the image build uses.
FROM github-assets AS github-assets

FROM registry.fedoraproject.org/fedora:42

# nodejs: actions/checkout. capnproto: runtime-collab build.rs.
# vulkan-loader + mesa-vulkan-drivers: llvmpipe, so GPU tests run headless.
# GNOME Text Editor, qt6-qtbase-examples and LibreOffice Writer are real-process
# subjects for the legacy-app caret E2E. Keeping them in this weekly-built
# image makes the path-gated PR job deterministic and avoids live dnf traffic.
# bubblewrap is a test *subject*, not a tool — the launcher's confinement
# suites drive the real sandbox and refuse to skip when it is absent. So is
# rust-analyzer, for the code editor's language-server suite, but it comes from
# rustup below: the shims in /usr/local/cargo/bin precede /usr/bin, so a Fedora
# package here would be shadowed by a shim reporting the component missing.
RUN dnf install -y \
      nodejs \
      capnproto \
      gcc gcc-c++ make cmake perl perl-FindBin python3 git jq time yt-dlp \
      clang clang-devel pkgconf-pkg-config \
      libxkbcommon-devel wayland-devel \
      libseat-devel libinput-devel systemd-devel libdrm-devel \
      mesa-libgbm-devel mesa-libGL-devel mesa-libEGL-devel \
      pipewire-devel \
      ffmpeg-free-devel libva-devel \
      gstreamer1 gstreamer1-devel gstreamer1-plugins-base-devel \
      gstreamer1-plugins-good gstreamer1-plugins-bad-free \
      gstreamer1-plugin-openh264 \
      openssl-devel sqlite-devel zlib-ng-compat-devel \
      fontconfig-devel freetype-devel harfbuzz-devel expat-devel \
      google-noto-sans-fonts google-noto-sans-mono-fonts \
      google-noto-serif-fonts google-noto-color-emoji-fonts \
      google-noto-sans-symbols-fonts google-noto-sans-symbols-2-fonts \
      google-noto-sans-math-fonts google-noto-sans-cjk-fonts \
      google-noto-sans-arabic-fonts google-noto-sans-hebrew-fonts \
      google-noto-sans-devanagari-fonts google-noto-sans-bengali-fonts \
      google-noto-sans-thai-fonts \
      fuse3-devel dbus-devel \
      nasm \
      vulkan-loader mesa-vulkan-drivers \
      bubblewrap \
      gnome-text-editor qt6-qtbase-examples libreoffice-writer \
    && dnf clean all

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
ARG SCCACHE_SHA256=67c4a96dd237c1f518f6b36083f270f9976d516f1e57fce891755ea782e50006
RUN --mount=type=bind,from=github-assets,source=/,target=/qos-assets,ro \
    archive="sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
    && echo "${SCCACHE_SHA256}  /qos-assets/${archive}" | sha256sum -c - \
    && tar -xzf "/qos-assets/${archive}" -C /tmp \
    && install -m 0755 \
      "/tmp/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl/sccache" \
      /usr/local/bin/sccache \
    && rm -rf "/tmp/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl" \
    && sccache --version

RUN cargo install --locked cargo-nextest cargo-deny cargo-machete \
    && cargo nextest --version && cargo deny --version && cargo machete --version

# Match the workspace's own check so a stale image fails loudly rather than
# compiling against the wrong libraries.
COPY tools/check-build-env.sh /usr/local/bin/qos-check-build-env
RUN chmod +x /usr/local/bin/qos-check-build-env && qos-check-build-env
