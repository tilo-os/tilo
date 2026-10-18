# ---- The sources the Rust compile reads -------------------------------------
# Only what the builder stage reads, never the whole context: with
# `COPY . /build`, a unit, an /etc file or a test script under os/ re-ran the
# compile and relinked every binary (about twenty minutes in CI), although none
# of them depends on it. That is the workspace, the few repository files a
# workspace crate include!s from outside it, the scripts the builder's RUNs
# call, and the prebuilt payloads CI may stage (empty in a checkout).
# tools/policy/check_image_builder_inputs.py keeps this list complete. A
# scratch stage, so these commits are cheap and the builder takes one layer.
FROM scratch AS builder-sources
COPY Cargo.toml Cargo.lock rust-toolchain.toml /build/
COPY .cargo /build/.cargo
COPY crates /build/crates
COPY apps /build/apps
COPY examples /build/examples
COPY vendor /build/vendor
COPY dist /build/dist
COPY docs/developers/tasks /build/docs/developers/tasks
COPY tools/ci/rustc-thin-app-wrapper.sh tools/ci/runtime-dylib-lazy-link.py /build/tools/ci/
# build-thin-apps.sh routes a cached build through this dispatcher
# (RUSTC_WRAPPER), which finds the size wrapper beside it in tools/ci/.
COPY tools/ci/thin-app/sccache /build/tools/ci/thin-app/sccache
COPY tools/policy/app_portability_rules.toml /build/tools/policy/app_portability_rules.toml
COPY tools/policy/app_capability_usage.toml /build/tools/policy/app_capability_usage.toml
COPY tools/protocols/airdrop_sender.py /build/tools/protocols/airdrop_sender.py
COPY tools/test-assets-build.rs /build/tools/test-assets-build.rs
COPY tools/test-assets.py /build/tools/test-assets.py
COPY tools/font-catalog.py /build/tools/font-catalog.py
COPY os/legacy-guest-snapshot.env /build/os/legacy-guest-snapshot.env
COPY os/hardware/profiles/lenovo-83ed/input/keyboard.json os/hardware/profiles/lenovo-83ed/input/mouse.json os/hardware/profiles/lenovo-83ed/input/touchpad.json /build/os/hardware/profiles/lenovo-83ed/input/
COPY scripts/browser/build-host.py scripts/browser/cef-lock.json /build/scripts/browser/
COPY \
    os/scripts/cargo-freshness.sh \
    os/scripts/build-thin-apps.sh \
    os/scripts/cargo-timings-summary.py \
    os/scripts/verify-elf-hardening.sh \
    os/scripts/pack-apps.sh \
    os/scripts/check-shipped-runtimes.sh \
    os/scripts/stamp-runtime.sh \
    os/scripts/install-env-runtime.sh \
    /build/os/scripts/
COPY os/desktop-artifacts /build/os/desktop-artifacts
COPY os/app-artifacts /build/os/app-artifacts
COPY os/app-runtime /build/os/app-runtime
