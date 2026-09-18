#!/usr/bin/env bash
# Test driver for the Flatpak share pipeline.
#
# Runs every layer of the pipeline that the current host can exercise —
# openh264 software encode inside distrobox for the standard case, real
# NVENC on the host via an LD_LIBRARY_PATH shim, VA-API when the render
# node advertises H.264, the frame-link shim, and the side-by-side demo.
#
# See docs/flatpak-share-testing.md for what each subcommand covers.
#
# Usage:
#   scripts/test-flatpak-share.sh <subcommand>
#
# Subcommands:
#   sw          openh264 software backend + probe + fallback
#   vaapi       VA-API H.264 (self-skips on hosts without an encode profile)
#   nvenc       NVENC H.264 — builds in distrobox, runs on host
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

# Matching the host's ffmpeg ABI avoids the library staging shim. A custom
# container is conservatively treated as unmatched unless the caller opts out
# with an empty TILO_DISTROBOX.
HOST_ABI_MATCHES=$([ "$DISTROBOX" = "my-distrobox-44" ] && echo 1 || echo 0)
cargo_in_distrobox() {
    if [ -n "$DISTROBOX" ]; then
        distrobox enter "$DISTROBOX" -- bash -c "cd $REPO && $*"
    else
        bash -c "cd $REPO && $*"
    fi
}

# Root the on-host scratch dir here so subsequent runs are cheap.
SCRATCH="${TILO_TEST_SCRATCH:-$REPO/.claude/test-scratch/flatpak-share}"
FF_LIBS="$SCRATCH/ff-libs"
NV_LIBS="$SCRATCH/nvidia-libs"

run_sw() {
    echo "=== software (openh264) + probe ==="
    cargo_in_distrobox \
        "cargo test -p shell \
            --test flatpak_share_e2e \
            --test flatpak_share_encoder_probe \
            --release -- --include-ignored"
}

run_vaapi() {
    echo "=== VA-API ==="
    cargo_in_distrobox \
        "cargo test -p shell --features vaapi \
            --test vaapi_hardware_encode \
            --test flatpak_share_encoder_probe \
            --release -- --include-ignored --nocapture"
    echo "Note: probe self-skips when the render node has no H.264 encode profile."
}

# Extract the ffmpeg .so files from distrobox and stage them on the
# host, plus symlink the host's NVIDIA libs into the same dir. Idempotent.
stage_host_libs() {
    mkdir -p "$FF_LIBS" "$NV_LIBS"

    # NVIDIA userspace libs live on the host, not in distrobox.
    for lib in libcuda.so.1 libnvidia-encode.so.1 libnvidia-ml.so.1 \
               libnvcuvid.so.1 libnvidia-cfg.so.1; do
        src=$(readlink -f "/lib64/$lib" 2>/dev/null || true)
        if [ -n "$src" ] && [ -f "$src" ]; then
            ln -sf "$src" "$NV_LIBS/$lib"
            ln -sf "$src" "$NV_LIBS/$(basename "$src")"
        fi
    done

    # ffmpeg libs from distrobox — the host's `ffmpeg-free` (Fedora
    # default) usually lacks NVENC linkage, so we prefer the container's.
    if [ -z "$DISTROBOX" ]; then
        echo "TILO_DISTROBOX is required to stage codec libraries" >&2
        return 1
    fi
    distrobox enter "$DISTROBOX" -- bash -c "
        for so in libavcodec.so.61 libavutil.so.59 libswscale.so.8 \
                  libavformat.so.61 libswresample.so.5; do
            path=\$(ldconfig -p | grep -E \"^\\s*\$so \" | head -1 | awk '{print \$NF}')
            if [ -n \"\$path\" ] && [ -f \"\$path\" ]; then
                real=\$(readlink -f \"\$path\")
                cp -f \"\$real\" '$FF_LIBS/' 2>/dev/null || true
                ln -sf \"\$(basename \"\$real\")\" '$FF_LIBS/'\$so 2>/dev/null || true
            fi
        done
    "

    # First-run misses transitive codec deps; grab whatever ldd on the
    # host still can't resolve after the initial batch. Iterate until
    # the resolution converges (each pass may unlock more deps whose
    # SONAMEs only become visible once their loader can find the caller).
    local TEST_BIN missing_file
    TEST_BIN=$(ls -t "$REPO"/target/release/deps/nvenc_hardware_encode-* 2>/dev/null \
        | grep -v '\.d$' | head -1 || true)
    missing_file="$SCRATCH/missing.txt"
    for _ in 1 2 3; do
        [ -n "$TEST_BIN" ] || break
        LD_LIBRARY_PATH="$FF_LIBS:$NV_LIBS:/lib64" \
            ldd "$TEST_BIN" 2>&1 | awk '/not found/ {print $1}' | sort -u > "$missing_file"
        [ -s "$missing_file" ] || break
        distrobox enter "$DISTROBOX" -- bash -c "
            while read so; do
                path=\$(ldconfig -p | grep -E \"^\\s*\$so \" | head -1 | awk '{print \$NF}')
                if [ -n \"\$path\" ] && [ -f \"\$path\" ]; then
                    real=\$(readlink -f \"\$path\")
                    cp -nf \"\$real\" '$FF_LIBS/' 2>/dev/null || true
                    ln -sf \"\$(basename \"\$real\")\" '$FF_LIBS/'\$so 2>/dev/null || true
                fi
            done < '$missing_file'
        "
    done
}

run_nvenc() {
    echo "=== NVENC (build in $DISTROBOX, run on host) ==="
    if [ ! -e /dev/nvidia0 ] && [ ! -e /dev/nvidiactl ]; then
        echo "no /dev/nvidia* on this host — NVENC probe self-skips, running anyway to prove it"
    fi
    cargo_in_distrobox \
        "cargo test -p shell --features nvenc \
            --test nvenc_hardware_encode --release --no-run"
    local TEST_BIN
    TEST_BIN=$(ls -t "$REPO"/target/release/deps/nvenc_hardware_encode-* \
        | grep -v '\.d$' | head -1)
    if [ -z "$TEST_BIN" ] || [ ! -x "$TEST_BIN" ]; then
        echo "no nvenc_hardware_encode binary produced — build failed" >&2
        exit 1
    fi
    echo "running: $TEST_BIN"
    if [ "$HOST_ABI_MATCHES" = 1 ]; then
        # Distrobox and host share ffmpeg SONAMEs — the binary can
        # dlopen host libs directly. This is the happy path.
        "$TEST_BIN" --include-ignored --nocapture --test-threads 1
    else
        # Fallback path when the distrobox's ffmpeg SONAMEs don't
        # match the host's (e.g. F42 distrobox on an F44 host).
        # Stage distrobox's libavcodec/libavutil/libswscale + the
        # host's libnvidia-encode/libcuda into a scratch dir and
        # LD_LIBRARY_PATH the binary at them.
        stage_host_libs
        LD_LIBRARY_PATH="$FF_LIBS:$NV_LIBS:/lib64" "$TEST_BIN" \
            --include-ignored --nocapture --test-threads 1
    fi
}

run_dmabuf() {
    echo "=== DMABUF capture-path routing ==="
    cargo_in_distrobox \
        "cargo test -p shell \
            --test dmabuf_direct_capture_path \
            --release -- --include-ignored"
}

run_substream() {
    echo "=== QUIC substream substrate ==="
    cargo_in_distrobox \
        "cargo test -p shell \
            --test surface_substream_e2e \
            --release -- --include-ignored"
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
    if command -v ffprobe >/dev/null 2>&1; then
        ffprobe -hide_banner "$out" 2>&1 | grep -E "Stream|Duration" | head -3
    fi
}

run_all() {
    run_sw
    run_shim
    run_substream
    run_dmabuf
    run_e2e
    run_vaapi || echo "vaapi step skipped/failed — expected on non-H.264 iGPUs"
    if [ -e /dev/nvidia0 ] || [ -e /dev/nvidiactl ]; then
        run_nvenc
    else
        echo "=== NVENC skipped: no /dev/nvidia* ==="
    fi
    run_demo
    echo
    echo "netns pair test needs sudo — run manually:"
    echo "    sudo scripts/two-netns-share.sh"
}

case "${1:-}" in
    sw|software|openh264)  run_sw ;;
    vaapi)                 run_vaapi ;;
    nvenc|nvidia)          run_nvenc ;;
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
