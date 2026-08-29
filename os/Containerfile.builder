# CI build environment: everything `cargo check`/`test`/`nextest` needs for the
# workspace, so jobs do not `dnf install` ~30 packages on every run.
#
# Rebuilt weekly by .forgejo/workflows/build-builder-image.yml. Fedora rather
# than the runner's default Ubuntu image: the OS is Fedora-based, so the build
# deps and their versions match what the image build uses.
FROM registry.fedoraproject.org/fedora:42

# nodejs: actions/checkout. capnproto: runtime-collab build.rs.
# vulkan-loader + mesa-vulkan-drivers: llvmpipe, so GPU tests run headless.
# bubblewrap + rust-analyzer are test *subjects*, not tools — the launcher's
# confinement suites and the code editor's language-server suite drive the real
# thing and refuse to skip when it is absent.
RUN dnf install -y \
      nodejs \
      capnproto \
      gcc gcc-c++ make cmake perl perl-FindBin python3 git \
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
      bubblewrap rust-analyzer \
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
    && cargo --version && rustc --version

RUN cargo install --locked cargo-nextest cargo-deny cargo-machete \
    && cargo nextest --version && cargo deny --version && cargo machete --version

# Match the workspace's own check so a stale image fails loudly rather than
# compiling against the wrong libraries.
COPY tools/check-build-env.sh /usr/local/bin/qos-check-build-env
RUN chmod +x /usr/local/bin/qos-check-build-env && qos-check-build-env
