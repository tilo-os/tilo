"""Excerpts of the frozen sources the resume and reconstruction helpers rewrite.

The helpers in os/dev/native_resume.py and os/dev/kernel_checkpoint_restore.py
operate on recipes of two exact historical commits that are not part of this
repository's history. The tests exercise the rewriting on these excerpts: every
seam the helpers anchor on is copied verbatim from the frozen files, the rest of
each file is elided.
"""

# os/Containerfile at native_resume.SOURCE: the builder stage's compile RUN,
# the thin-app and packaging steps, and the native-builder payload stage.
NATIVE_RECIPE = r'''FROM ${BASE_REF} AS builder
ARG TILO_WITH_BROWSER=1
RUN --mount=type=cache,target=/build/.cargo-home \
    --mount=type=cache,target=/build/target \
    --mount=type=cache,target=/build/.sccache \
    phase=rust-setup && started=$(date +%s) \
    && build_phase() { \
         now=$(date +%s); \
         printf 'TILO_BUILD_TIMING phase=%s elapsed_seconds=%s exit_code=0\n' "$phase" "$((now - started))"; \
         phase=$1; started=$now; \
       } \
    && cargo clean --release \
    && cargo clean --profile thin-app \
    && if [ "${TILO_WITH_BROWSER}" = "0" ]; then \
         BROWSER_PKG=""; \
       else \
         BROWSER_PKG="-p browser --features browser/wpe"; \
       fi \
    && THIN_EXCLUDES="browser" \
    && RELEASE_ARGS="-p shell -p fs -p terminal -p tilo-crypto" \
    && build_phase rust-release \
    && CARGO_PROFILE_RELEASE_LTO=thin \
       CARGO_PROFILE_RELEASE_CODEGEN_UNITS=8 \
       cargo build --release ${RELEASE_ARGS} -p tilo-relay --bins ${BROWSER_PKG} \
    && build_phase rust-windows-vm \
    && CARGO_PROFILE_RELEASE_LTO=thin \
       CARGO_PROFILE_RELEASE_CODEGEN_UNITS=8 \
       cargo build --release -p windows-vm \
         --bin tilo-windows-vm-control --bin tilo-windows-rail \
    && build_phase rust-output-validation \
    && missing=0 \
    && for b in shell fs terminal tilo-crypto tilo-windows-vm-control tilo-windows-rail; do \
         if [ ! -x "target/release/$b" ]; then \
           printf 'Required build output is missing: %s\n' "$b" >&2; missing=1; \
         fi; \
       done \
    && [ "$missing" = 0 ] \
    && build_phase rust-stage-outputs \
    && mkdir -p /out /out/optional /out/thin-apps \
    && build_phase rust-thin-apps \
    && EXCLUDE_PACKAGES="${THIN_EXCLUDES}" \
       APP_FEATURES="" \
         /build/os/scripts/build-thin-apps.sh \
    && build_phase rust-cleanup \
    && find /build/target -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +

RUN BIN_DIR=/out/thin-apps \
    PKG_BIN=/out/tilo-pkg \
    INSTALL_BIN=/out/tilo-install \
    /build/os/scripts/pack-apps.sh

FROM scratch AS native-builder-payload
COPY --from=builder /out/ /out/
COPY --from=builder /usr/lib/tilo-runtime/ /usr/lib/tilo-runtime/
COPY --from=builder /usr/share/tilo-apps/ /usr/share/tilo-apps/

FROM scratch AS desktop-binaries
COPY --from=builder /out/shell /usr/bin/shell
'''

# os/dev/build-image.sh at native_resume.SOURCE: the helper source line, the
# kernel pin selection and the image build invocation.
NATIVE_DRIVER = r'''#!/bin/bash
# Build the Tilo bootc image and a bootable qcow2, end to end.
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

# An explicit digest is useful while validating a newly published kernel, but
# it is held to the same repository and signature policy as the tracked pin.
_tilo_kernel_ref_override="${KERNEL_RPMS_REF:-}"
# shellcheck disable=SC1091
[ -f "$TILO_OS_DIR/kernel-snapshot.env" ] && . "$TILO_OS_DIR/kernel-snapshot.env"
case "$(uname -m)" in
  x86_64) _tilo_native_kernel_ref="${KERNEL_RPMS_X86_64_REF:-}"; _tilo_registry_arch=amd64 ;;
  aarch64) _tilo_native_kernel_ref="${KERNEL_RPMS_AARCH64_REF:-}"; _tilo_registry_arch=arm64 ;;
  *) die "unsupported kernel architecture: $(uname -m)" ;;
esac
KERNEL_RPMS_REF="${_tilo_kernel_ref_override:-$_tilo_native_kernel_ref}"
[ -n "${KERNEL_RPMS_REF:-}" ] || die "no KERNEL_RPMS_REF — run the build-kernel CI job (or podman build -f os/kernel/Containerfile) first"
# Pre-flight: every host-side COPY source exists. Decidable in a second, and the
# alternative is finding out ~20 minutes in.

build_image() {
  local image_tag="${1-$TILO_IMAGE}"
  podman "${_tilo_podman_args[@]}" build -f "$TILO_OS_DIR/Containerfile" -t "$image_tag" \
    --build-arg "KERNEL_RPMS_REF=$KERNEL_RPMS_REF" \
    "$TILO_REPO"
}
'''

# os/kernel/Containerfile at kernel_checkpoint_restore.SOURCE: the compile RUN
# and the publish stage.
KERNEL_RECIPE = r'''FROM ${BASE_REF} AS build
RUN set -eux; \
    KSRC="$(find /work/rpmbuild/BUILD -maxdepth 4 -type d -name 'linux-*' | head -1)"; \
    test -n "$KSRC" || { echo "no prepared kernel source tree found"; exit 1; }; \
    cd "$KSRC"; \
    ./scripts/kconfig/merge_config.sh -m .config /work/tilo.config; \
    make olddefconfig

RUN set -eux; \
    KSRC="$(find /work/rpmbuild/BUILD -maxdepth 4 -type d -name 'linux-*' | head -1)"; \
    cd "$KSRC"; \
    JOBS="${KERNEL_BUILD_JOBS:-$(nproc)}"; \
    case "$JOBS" in ''|*[!0-9]*|0) echo "KERNEL_BUILD_JOBS must be a positive integer" >&2; exit 1;; esac; \
    make -j"$JOBS" all; \
    KVER="$(make -s kernelrelease)"; \
    printf 'KVER=%s\n' "$KVER" > /work/kver.env; \
    STAGE=/work/stage; \
    rm -rf "$STAGE"; \
    mkdir -p "$STAGE/lib/modules/$KVER"; \
    make INSTALL_MOD_PATH="$STAGE" INSTALL_MOD_STRIP=1 modules_install

FROM scratch AS secureboot-probe-inputs
COPY --from=build /work/secureboot-probe-inputs /

FROM scratch AS publish
COPY --from=build /repo /repo
'''
