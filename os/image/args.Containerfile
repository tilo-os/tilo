# syntax=docker/dockerfile:1.7
# ============================================================================
# Tilo bootc image: the build arguments every piece of it reads
# ----------------------------------------------------------------------------
# The image is built as layers, each its own action (os/image/layers.toml):
# the packages over the pinned base, the kernel, the system configuration, the
# public trust, the Tilo binaries (compiled outside any image build, by
# os/image/compile.sh in the OS builder image), the derived state that reads
# them all (initramfs, font and linker caches, the final checks), and the seal.
# Each piece under os/image/ is one of them, and builds over the layers below
# it given as an image (`--build-context <stage>=container-image://...`), so a
# changed binary reruns the pieces above the binaries and never the package
# transaction. No piece compiles the workspace: the compiled tree comes in as
# the `compiled` build context.
#
# os/Containerfile is these pieces in order, written by
# `python3 tools/ci/os/pieces.py write`, for a build of the whole image in one
# `podman build` (os/dev/build-image.sh) and for the checks that read the
# image's recipe as one file.
# ============================================================================


# ---- Frozen RPM repo (cross-time reproducibility) --------------------------
# Every `dnf install` below installs ONLY from this digest-pinned snapshot of
# the exact RPM set (built by os/Containerfile.rpms / the snapshot-rpms CI
# job), never live Fedora mirrors — so the same source rebuilds the same image
# regardless of when. RPMS_REF (registry.tilo.so/tilo/rpms@sha256:...) comes from
# resolve-rpm-snapshot.sh via the build scripts.
# ADD/REMOVE A PACKAGE below -> re-run the `snapshot-rpms` workflow (it prints a
# new set-<fingerprint> tag), else the build fails "No
# match for argument". The snapshot's package list is derived from this file.
# All three ARGs are declared here (before the first FROM) so they're in
# scope for FROM directives. An `ARG NAME` placed between two FROMs is
# scoped to the previous stage, not global — buildkit then reports
# `base name (${…}) should not be blank` when the next FROM tries to
# use it.
#
# BASE_REF (registry.tilo.so/tilo/fedora-bootc@sha256:...) is the Fedora bootc
# base, mirrored into our registry and pinned by digest — see
# os/base-snapshot.env for why the mirror exists and how to refresh it. Every
# fedora-bootc stage below uses it, so builder and final share one base and
# their glibc cannot drift.
ARG BASE_REF
ARG RPMS_REF
ARG KERNEL_RPMS_REF
ARG TILO_WINDOWS_ARCH=x86_64
# Scratch is only a parseable placeholder for an unselected provider branch.
# ARM validation rejects it; no diagnostic payload or mutable tag is a fallback.
ARG FEX_PAYLOAD_REF=scratch
ARG WINDOWS_GUEST_REF=scratch
# TILO_WITH_WINDOWS=0 builds an ARM image without the Windows provider (FEX and
# the x86 guest): /usr/share/tilo/windows/provider.json records provider
# "none", the image carries org.tilo.with-windows=0, and the compatibility
# service refuses capsule launches with that explanation. Never choose it by
# hand: `os/dev/windows_provider.py mode` derives it from the two pins (0 only
# while neither is set) for os/dev/build-image.sh and CI alike, and x86_64,
# whose Fedora Wine is always present, refuses it.
ARG TILO_WITH_WINDOWS=1
# The published env-runtime environment image (os/dev/resolve-env-runtime.sh),
# shipped in the system app store so the first environment opens offline.
# Scratch while no runtime is published for this checkout's definition: the
# image then ships none, and environments fetch it on first use.
ARG ENV_RUNTIME_REF=scratch

# TILO_WITH_BROWSER=0 builds the image without the browser: no Browser app and
# no Chromium engine runtime. Default 1.
#
# The engine is the native CEF host (apps/browser/chromium) built against the
# pinned CEF distribution (scripts/browser/cef-lock.json, fetched as an external
# asset) and signed as its own runtime bundle; the Browser is an ordinary tapp.
# Nothing about the boot chain (kernel, initramfs, LUKS/TPM unlock, greeter,
# installer, rollback) depends on it, so opting out is how you get an image to
# boot-test while the engine's own build is being repaired.
ARG TILO_WITH_BROWSER=1
# The engine runtime's version: ui::webview::chromium::runtime::RUNTIME_VERSION.
ARG CHROMIUM_RUNTIME_VERSION=2026.10.17-chromium
# The pinned builder bootc-builder compiles the image's bootc in, the one
# os/Containerfile.bootc-helper uses.
ARG BOOTC_BUILD_IMAGE=registry.tilo.so/tilo/tilo-builder@sha256:568fca1f8b4a4b1700c871cca033d2d291a5eb02c4b3be9681410f456b5a7bc9
