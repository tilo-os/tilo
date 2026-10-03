#!/usr/bin/env bash
# Test driver for the Flatpak share pipeline.
#
# Runs every layer of the pipeline that the current host can exercise —
# royalty-free software encode (VP9/AV1) everywhere, hardware encode and
# 10-bit HDR decode where the GStreamer registry lists a hardware encoder,
# the frame-link shim, and the side-by-side demo.
#
# See docs/flatpak-share-testing.md for what each subcommand covers.
#
# Usage:
#   scripts/test-flatpak-share.sh <subcommand>
#
# Subcommands:
#   sw          software encoders + codec negotiation + fallback
#   hw          hardware encoders, zero-copy dmabuf, 10-bit HDR decode
#               (each test self-skips when no hardware encoder is listed)
#   dmabuf      DMABUF direct-import capture-path routing
#   substream   QUIC substream framing + backpressure
#   e2e         SharedFlatpakSession end-to-end pipeline
#   shim        frame-link latency + throughput + drop tests
#   demo        side-by-side comparison video (target/comparison.mp4)
#   all         everything runnable on this host, in order

set -euo pipefail

# Move to the workspace root so cargo picks up the right target dir.
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"

# apps/browser pulls libservo from git and often fails to resolve on
# fresh checkouts — that's independent of the share pipeline, so we
# temporarily drop it from the workspace to keep this script robust.
# Always restore on exit — the canonical state is "browser included",
# regardless of what the working tree looked like when the script
# started. Committing a `# tmp:` line is a footgun.
tmp_exclude_browser() {
    if grep -q '^    "apps/browser",' Cargo.toml; then
        sed -i 's|^    "apps/browser",|    # tmp: "apps/browser",|' Cargo.toml
    fi
}
restore_browser() {
    if grep -q '^    # tmp: "apps/browser",' Cargo.toml; then
        sed -i 's|^    # tmp: "apps/browser",|    "apps/browser",|' Cargo.toml
    fi
}
trap restore_browser EXIT
tmp_exclude_browser

# Use the host by default, or set TILO_DISTROBOX when the build dependencies
# live in a development container. Auto-detect the conventional containers
# only when neither choice was explicit.
if [ -n "${TILO_DISTROBOX+x}" ]; then
    DISTROBOX="$TILO_DISTROBOX"
elif podman container exists my-distrobox-44 2>/dev/null; then
    DISTROBOX="my-distrobox-44"
elif podman container exists my-distrobox 2>/dev/null; then
    DISTROBOX="my-distrobox"
else
    DISTROBOX=""
fi

cargo_in_distrobox() {
    if [ -n "$DISTROBOX" ]; then
        distrobox enter "$DISTROBOX" -- bash -c "cd $REPO && $*"
    else
        bash -c "cd $REPO && $*"
    fi
}

run_sw() {
    echo "=== software encoders + negotiation ==="
    cargo_in_distrobox \
        "cargo test -p shell --test it --release --locked \
            flatpak_share_encoder_probe:: flatpak_share_e2e:: \
            -- --include-ignored"
}

run_hw() {
    echo "=== hardware encoders + HDR decode ==="
    cargo_in_distrobox \
        "cargo test -p shell --test it --release --locked \
            hardware_encode:: hdr_decode_pipeline:: \
            -- --include-ignored --nocapture --test-threads 1"
    echo "Note: each test reports and returns when no hardware encoder is listed."
}

run_dmabuf() {
    echo "=== DMABUF capture-path routing ==="
    cargo_in_distrobox \
        "cargo test -p shell \
            --test it --release --locked dmabuf_direct_capture_path:: \
            -- --include-ignored"
}

run_substream() {
    echo "=== QUIC substream substrate ==="
    cargo_in_distrobox \
        "cargo test -p shell \
            --test it --release --locked surface_substream_e2e:: \
            -- --include-ignored"
}

run_e2e() {
    echo "=== end-to-end pipeline ==="
    cargo_in_distrobox \
        "cargo test -p shell \
            --test it --release --locked flatpak_share_e2e:: \
            -- --include-ignored && \
         cargo test -p shell \
            --test it --release --locked \
            two_netns_share::in_process_udp_pipeline_sustains_10_fps_and_bounded_latency \
            -- --exact --nocapture"
}

run_shim() {
    echo "=== frame-link shim ==="
    cargo_in_distrobox \
        "cargo test -p frame-link --release -- --include-ignored"
}

run_demo() {
    echo "=== side-by-side comparison video ==="
    local out="${OUT:-target/comparison.mp4}"
    cargo_in_distrobox \
        "cargo run --release -p frame-link --bin flatpak-share-demo -- \
            --duration ${DURATION:-10} \
            --width ${WIDTH:-1280} --height ${HEIGHT:-720} --fps ${FPS:-30} \
            --latency-ms ${LATENCY_MS:-80} \
            --throughput-kbps ${THROUGHPUT_KBPS:-2000} \
            --out $out"
    echo "video: $REPO/$out"
    if command -v gst-discoverer-1.0 >/dev/null 2>&1; then
        gst-discoverer-1.0 "$out" 2>&1 | grep -E "video|Duration" | head -3
    fi
}

run_all() {
    run_sw
    run_shim
    run_substream
    run_dmabuf
    run_e2e
    run_hw
    run_demo
    echo
    echo "netns pair test needs sudo — run manually:"
    echo "    sudo scripts/two-netns-share.sh"
}

case "${1:-}" in
    sw|software)           run_sw ;;
    hw|hardware|vaapi|nvenc) run_hw ;;
    dmabuf)                run_dmabuf ;;
    substream|quic)        run_substream ;;
    e2e|pipeline)          run_e2e ;;
    shim|frame-link)       run_shim ;;
    demo|video)            run_demo ;;
    all)                   run_all ;;
    -h|--help|"")
        grep -E '^# ' "$0" | sed 's/^# \{0,1\}//'
        exit 0
        ;;
    *)
        echo "unknown subcommand: $1" >&2
        echo "run '$0 --help' for the list." >&2
        exit 2
        ;;
esac
