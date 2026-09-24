#!/usr/bin/env bash
# Record a three-pane 1920x1080 demo video of the tilo shell.
#
# Two sources provide different capabilities:
#
#   --source composed  (default)  Three `shots --mode recording` streams at
#       exactly 640x1080 each, hstacked to 1920x1080. Geometry is exact and
#       reproducible, no screen recorder and no privileges are involved, and
#       the frames are the compositor's own output. Use this for scripted
#       flows. Requires: ffmpeg + a built `shots` binary.
#
#   --source live                 Three nested compositor windows, captured
#       off the screen. Use this when the point is to film something
#       unscripted -- an agent driving the desktop over the MCP shim.
#       Requires a working screen recorder AND a host compositor that will
#       place the windows; see the geometry warning below.
#
# GEOMETRY LIMITATION (live source): the winit backend's window size is a
# compile-time constant (INITIAL_WIDTH/INITIAL_HEIGHT = 1600x900 in
# crates/session/shell/src/compositor/backend/winit.rs) with no flag or env override, and
# Wayland gives no way for a script to position another client's window. So
# on a stock Wayland session this script cannot itself lay the three panes
# out side by side -- it spawns them and tells you what it could not do.
# Under a scriptable tiling host (sway, labwc) you can place them; the
# composed source avoids the problem entirely.
set -euo pipefail

OUT="target/demo/tilo-demo.mp4"
DURATION=30
DRY_RUN=0
SOURCE="composed"
SCENARIOS="palette_typing,workspace_switch_animated,strip_scroll_across_apps"
PANE_W=640
PANE_H=1080
FPS=30

usage() {
    # Print the header comment block, stopping at the first non-comment line.
    awk 'NR>1 { if (/^#/) { sub(/^# ?/, ""); print } else { exit } }' "$0"
    cat <<EOF

Usage: $(basename "$0") [options]
  --out PATH          output video (default: $OUT)
  --duration SECONDS  cap the output length (default: $DURATION)
  --dry-run           print the plan and the detected recorder, record nothing
  --source SRC        composed | live   (default: $SOURCE)
  --scenarios A,B,C   three scenario names for the composed source
                      (default: $SCENARIOS)
                      list them with: cargo run --bin shots -- --list-scenarios
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --out)       OUT="$2"; shift 2 ;;
        --duration)  DURATION="$2"; shift 2 ;;
        --dry-run)   DRY_RUN=1; shift ;;
        --source)    SOURCE="$2"; shift 2 ;;
        --scenarios) SCENARIOS="$2"; shift 2 ;;
        -h|--help)   usage; exit 0 ;;
        *) echo "unknown flag: $1" >&2; usage >&2; exit 2 ;;
    esac
done

case "$SOURCE" in composed|live) ;; *)
    echo "--source must be 'composed' or 'live', got '$SOURCE'" >&2; exit 2 ;;
esac

# --- Report the host compositor rather than assuming one ------------------

SESSION="${XDG_SESSION_TYPE:-unknown}"
DESKTOP="${XDG_CURRENT_DESKTOP:-unknown}"
echo "host session : ${SESSION} / ${DESKTOP} (WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-unset}, DISPLAY=${DISPLAY:-unset})"

# --- Detect a screen recorder --------------------------------------------
# Only consulted by the live source; reported either way so --dry-run tells
# you what a live run would do.

RECORDER=""
RECORDER_NOTE=""
if command -v wf-recorder >/dev/null 2>&1; then
    RECORDER="wf-recorder"
    RECORDER_NOTE="needs the wlr-screencopy protocol; GNOME and KDE do not implement it"
elif command -v gpu-screen-recorder >/dev/null 2>&1; then
    RECORDER="gpu-screen-recorder"
    RECORDER_NOTE="uses the xdg-desktop-portal ScreenCast route; works on GNOME"
elif command -v wl-screenrec >/dev/null 2>&1; then
    RECORDER="wl-screenrec"
    RECORDER_NOTE="needs the wlr-screencopy protocol"
elif command -v ffmpeg >/dev/null 2>&1; then
    if [[ "$SESSION" == "wayland" ]]; then
        RECORDER="ffmpeg-kmsgrab"
        RECORDER_NOTE="kmsgrab needs CAP_SYS_ADMIN (run as root or setcap); ffmpeg here has no pipewiregrab, and x11grab under Wayland captures only XWayland surfaces, not native ones"
    else
        RECORDER="ffmpeg-x11grab"
        RECORDER_NOTE="X11 session, x11grab captures the root window"
    fi
fi

if [[ -n "$RECORDER" ]]; then
    echo "recorder     : $RECORDER  ($RECORDER_NOTE)"
else
    echo "recorder     : none found"
fi

if [[ "$SOURCE" == "live" && -z "$RECORDER" ]]; then
    cat >&2 <<'EOF'

No screen recorder found. Install one of:

  sudo dnf install gpu-screen-recorder   # portal-based; the one that works on GNOME
  sudo dnf install wf-recorder           # wlroots compositors only (sway, labwc, ...)
  sudo dnf install ffmpeg                # kmsgrab needs root; x11grab needs an X11 session

Or use the composed source, which needs no screen recorder at all:
  scripts/demo/record.sh --source composed
EOF
    exit 1
fi

if ! command -v ffmpeg >/dev/null 2>&1; then
    echo "" >&2
    echo "ffmpeg is required to mux/compose the output. Install it:" >&2
    echo "  sudo dnf install ffmpeg" >&2
    exit 1
fi

IFS=',' read -r -a SCN <<< "$SCENARIOS"
if [[ "$SOURCE" == "composed" && ${#SCN[@]} -ne 3 ]]; then
    echo "--scenarios needs exactly three comma-separated names, got ${#SCN[@]}" >&2
    exit 2
fi

SHOTS_BIN="target/debug/shots"
[[ -x "target/release/shots" ]] && SHOTS_BIN="target/release/shots"
SHELL_BIN="target/debug/shell"

WORK="$(dirname "$OUT")/panes"

# --- Print the plan -------------------------------------------------------

echo ""
echo "plan"
echo "  source     : $SOURCE"
echo "  output     : $OUT  (1920x1080, ${FPS}fps, capped at ${DURATION}s)"
if [[ "$SOURCE" == "composed" ]]; then
    echo "  panes      : ${PANE_W}x${PANE_H} each, hstacked left to right"
    for i in 0 1 2; do
        echo "    pane $((i+1))   : ${SCN[$i]} -> $WORK/pane$((i+1)).h264"
    done
    echo "  binary     : $SHOTS_BIN"
else
    echo "  panes      : 3 nested compositors ($SHELL_BIN --backend winit)"
    echo "  capture    : $RECORDER, full screen, cropped/scaled to 1920x1080"
    echo "  WARNING    : this script cannot place the three windows itself"
    echo "               (winit size is a compile-time constant, and Wayland has"
    echo "                no external window positioning). Arrange them by hand,"
    echo "                or run under a scriptable tiling host."
fi

if [[ $DRY_RUN -eq 1 ]]; then
    echo ""
    echo "dry run -- nothing recorded."
    exit 0
fi

mkdir -p "$(dirname "$OUT")"

# --- Composed source ------------------------------------------------------

if [[ "$SOURCE" == "composed" ]]; then
    if [[ ! -x "$SHOTS_BIN" ]]; then
        echo "" >&2
        echo "shots binary not found at $SHOTS_BIN. Build it first:" >&2
        echo "  cargo build -p shell --bin shots" >&2
        exit 1
    fi
    mkdir -p "$WORK"
    for i in 0 1 2; do
        echo "capturing pane $((i+1)): ${SCN[$i]}"
        "$SHOTS_BIN" --mode recording --scenario "${SCN[$i]}" \
            --width "$PANE_W" --height "$PANE_H" --fps "$FPS" \
            --out "$WORK/pane$((i+1)).h264"
    done
    echo "composing -> $OUT"
    ffmpeg -y -loglevel warning \
        -f h264 -r "$FPS" -i "$WORK/pane1.h264" \
        -f h264 -r "$FPS" -i "$WORK/pane2.h264" \
        -f h264 -r "$FPS" -i "$WORK/pane3.h264" \
        -filter_complex "[0:v][1:v][2:v]hstack=inputs=3,scale=1920:1080,format=yuv420p[v]" \
        -map "[v]" -t "$DURATION" -c:v libx264 -preset medium -crf 18 "$OUT"
    echo "wrote $OUT"
    exit 0
fi

# --- Live source ----------------------------------------------------------

if [[ ! -x "$SHELL_BIN" ]]; then
    echo "compositor binary not found at $SHELL_BIN. Build it first:" >&2
    echo "  cargo build -p shell --bin shell" >&2
    exit 1
fi

PIDS=()
cleanup() {
    for p in "${PIDS[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done
}
trap cleanup EXIT

CONTROL_DIR="${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR is unset; control sockets need a private directory}/tilo-session"
for i in 1 2 3; do
    TILO_TEST_CONTROL="$CONTROL_DIR/demo-pane$i.sock" "$SHELL_BIN" --backend winit &
    PIDS+=("$!")
done
echo "spawned 3 nested compositors; control sockets at $CONTROL_DIR/demo-pane{1,2,3}.sock"
echo "arrange the windows now -- recording starts in 8s"
sleep 8

echo "recording ${DURATION}s with $RECORDER -> $OUT"
case "$RECORDER" in
    wf-recorder)
        wf-recorder -f "$OUT" & REC=$!
        sleep "$DURATION"; kill -INT "$REC"; wait "$REC" 2>/dev/null || true ;;
    wl-screenrec)
        wl-screenrec -f "$OUT" & REC=$!
        sleep "$DURATION"; kill -INT "$REC"; wait "$REC" 2>/dev/null || true ;;
    gpu-screen-recorder)
        gpu-screen-recorder -w portal -f "$FPS" -o "$OUT" & REC=$!
        sleep "$DURATION"; kill -INT "$REC"; wait "$REC" 2>/dev/null || true ;;
    ffmpeg-x11grab)
        ffmpeg -y -loglevel warning -f x11grab -framerate "$FPS" -video_size 1920x1080 \
            -i "${DISPLAY:-:0}" -t "$DURATION" \
            -c:v libx264 -preset medium -crf 18 -pix_fmt yuv420p "$OUT" ;;
    ffmpeg-kmsgrab)
        echo "kmsgrab needs CAP_SYS_ADMIN; this will fail unless ffmpeg is privileged." >&2
        ffmpeg -y -loglevel warning -f kmsgrab -framerate "$FPS" -i - -t "$DURATION" \
            -vf 'hwdownload,format=bgr0,scale=1920:1080,format=yuv420p' \
            -c:v libx264 -preset medium -crf 18 "$OUT" ;;
esac
echo "wrote $OUT"
