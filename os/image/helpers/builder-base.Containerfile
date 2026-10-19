# ---- The OS builder ---------------------------------------------------------
# The image the OS's binaries are compiled in (os/image/compile.sh), as its own
# image, pinned by what it is built from: the same base and frozen RPM snapshot
# as the OS, so what it links resolves on the OS.
# Base pinned by digest against our own mirror (BASE_REF, os/base-snapshot.env):
# a tag can be repointed, a digest can't — but upstream deletes the manifest a
# digest points at within about a day, so the pin remains usable only if we hold a
# copy. Builder and final share BASE_REF, so their glibc matches by
# construction. Bump deliberately: re-run `mirror-base`, commit the new
# BASE_REF, re-run `snapshot-rpms`.
FROM ${BASE_REF} AS builder-base

# Buildah leaves the rpms mount out of the layer cache key; this ARG keys it on RPMS_REF.
ARG RPMS_REF
# Rust toolchain + the C/-devel deps the workspace's *-sys crates link or build
# against. Superset of the my-distrobox dev deps (repo CLAUDE.md) plus the base
# toolchain a minimal bootc image lacks: cmake/perl/clang etc. that
# vendored crates (aws-lc-sys, the freetype/harfbuzz/fontconfig font stack,
# jemalloc, sqlite) need to build. `sccache` honours the project's
# .cargo/config.toml `rustc-wrapper`. Throwaway stage — nothing here ships.
#
# The browser engine's native host (apps/browser/chromium) builds with the same
# C++ toolchain and cmake against the pinned CEF distribution; it links nothing
# from this list beyond the C++ runtime.
RUN --mount=type=bind,from=rpms,source=/repo,target=/repo dnf install -y --setopt=reposdir=/repo \
        rust cargo sccache capnproto \
        gcc gcc-c++ make cmake perl perl-FindBin python3 git \
        clang clang-devel pkgconf-pkg-config \
        bash-devel readline-devel \
        libxkbcommon-devel wayland-devel \
        libseat-devel libinput-devel systemd-devel libdrm-devel \
        mesa-libgbm-devel mesa-libGL-devel mesa-libEGL-devel \
        pipewire-devel \
        sqlite-devel zlib-ng-compat-devel \
        fontconfig-devel freetype-devel harfbuzz-devel expat-devel \
        glib2-devel \
        nasm \
        gstreamer1-devel gstreamer1-plugins-base-devel \
    && dnf clean all

# Fedora's sccache RPM is built without the S3 backend. Use the same pinned,
# full-feature static client as the canonical CI builder so the nested image
# build can reach the shared compiler cache without leaking credentials into a
# layer. The archive checksum is also pinned in os/Containerfile.builder.
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
    && /usr/local/bin/sccache --version

# mold links the thirty-odd binaries below, where GNU ld spent minutes; with
# LTO off (TILO_RUST_LTO below) the link is most of what a one-crate rebuild
# does. The checksum-pinned upstream binary, the same one os/Containerfile.builder
# installs, not Fedora's package: its aarch64 build pulls in a mimalloc that
# needs ARM LSE atomics. It is the stage's default linker through a cargo config
# above the workspace, /.cargo/config.toml, whose rustflags Cargo joins with the
# workspace's own target flags, so every cargo invocation here links with it and
# none has to ask. mold's output is deterministic, and the hardening the
# workspace flags request is read back off the binaries below
# (verify-elf-hardening.sh), so a linker that dropped one fails the build.
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
    && rm -rf "/tmp/${source}" \
    && mold --version \
    && mkdir -p /.cargo \
    && printf '%s\n' \
         '# The OS image builder stage links with mold.' \
         "[target.'cfg(target_os = \"linux\")']" \
         'rustflags = ["-C", "link-arg=-fuse-ld=mold"]' \
         > /.cargo/config.toml \
    && printf 'int main(void) { return 0; }\n' > /tmp/mold-probe.c \
    && cc -fuse-ld=mold -Wl,-z,relro,-z,now -o /tmp/mold-probe /tmp/mold-probe.c \
    && readelf -p .comment /tmp/mold-probe | grep -q 'mold' \
    && rm -f /tmp/mold-probe /tmp/mold-probe.c

WORKDIR /build

# Keep cargo's home and sccache's cache under /build (which exists via WORKDIR/
# itself). The fedora-bootc base has no /root, so nesting cache mounts there fails
# ("stat /root: no such file or directory"); single subdirs of /build are safe.
# Deterministic compile (reproducible builds): no incremental state in output,
# remap the build path so binaries don't embed /build, fixed locale/timezone so
# any sorted/formatted output is stable.
#
# The compile sees a fixed SOURCE_DATE_EPOCH, not the commit time the build
# scripts pass. Nothing Cargo builds reads it except a C compiler's __DATE__,
# where any fixed value is as reproducible as another, but as an environment
# variable it is part of this stage's cache key: carrying the commit time here
# made every commit, even one that touched only os/ configuration, recompile
# and relink every binary. The packaging steps after the compile, which stamp
# bundles and registry rows with it, take the real value (ARG below).
# No compiler cache settings here: the compile (os/image/compile.sh) takes the
# one its caller gives, and t gives every build its own cache only when the
# image names none.
ENV CARGO_INCREMENTAL=0 \
    SOURCE_DATE_EPOCH=0 \
    LC_ALL=C.UTF-8 \
    TZ=UTC
