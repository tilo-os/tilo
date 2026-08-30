# CI build environment: everything `cargo check`/`test`/`nextest` needs for the
# workspace, so jobs do not `dnf install` ~30 packages on every run.
#
# Rebuilt weekly by .forgejo/workflows/build-builder-image.yml. Fedora rather
# than the runner's default Ubuntu image: the OS is Fedora-based, so the build
# deps and their versions match what the image build uses.
FROM registry.fedoraproject.org/fedora:42

# nodejs: actions/checkout. capnproto: runtime-collab build.rs.
# vulkan-loader + mesa-vulkan-drivers: llvmpipe, so GPU tests run headless.
# bubblewrap is a test *subject*, not a tool — the launcher's confinement
# suites drive the real sandbox and refuse to skip when it is absent. So is
# rust-analyzer, for the code editor's language-server suite, but it comes from
# rustup below: the shims in /usr/local/cargo/bin precede /usr/bin, so a Fedora
# package here would be shadowed by a shim reporting the component missing.
RUN dnf install -y \
      nodejs \
      capnproto \
      gcc gcc-c++ make cmake perl perl-FindBin python3 git time \
      clang clang-devel pkgconf-pkg-config \
      libxkbcommon-devel wayland-devel \
      libseat-devel libinput-devel systemd-devel libdrm-devel \
      mesa-libgbm-devel mesa-libGL-devel mesa-libEGL-devel \
      pipewire-devel \
      ffmpeg-free-devel libva-devel \
      gstreamer1-devel gstreamer1-plugins-base-devel \
      openssl-devel sqlite-devel zlib-ng-compat-devel \
      fontconfig-devel freetype-devel harfbuzz-devel expat-devel \
      fuse3-devel dbus-devel \
      nasm \
      vulkan-loader mesa-vulkan-drivers \
      bubblewrap \
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
RUN archive="sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
    && curl -fsSL \
      "https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/${archive}" \
      -o "/tmp/${archive}" \
    && echo "${SCCACHE_SHA256}  /tmp/${archive}" | sha256sum -c - \
    && tar -xzf "/tmp/${archive}" -C /tmp \
    && install -m 0755 \
      "/tmp/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl/sccache" \
      /usr/local/bin/sccache \
    && rm -rf "/tmp/${archive}" \
      "/tmp/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl" \
    && sccache --version

RUN cargo install --locked cargo-nextest cargo-deny cargo-machete \
    && cargo nextest --version && cargo deny --version && cargo machete --version

# Match the workspace's own check so a stale image fails loudly rather than
# compiling against the wrong libraries.
COPY tools/check-build-env.sh /usr/local/bin/qos-check-build-env
RUN chmod +x /usr/local/bin/qos-check-build-env && qos-check-build-env
