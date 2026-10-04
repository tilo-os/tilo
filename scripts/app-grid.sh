#!/usr/bin/env bash
# Render every app's offscreen `--shots` frame into one labelled contact grid.
# Drives app binaries directly; unlike `app-contact-sheet.py`, it needs no
# compositor or display.
#
# Requires ImageMagick (`magick`, `montage`).
#
# Usage:
#   scripts/app-grid.sh [--out PATH] [--theme dark|light] [--no-build]
#                       [--shot-timeout SECONDS]
#
#   --no-build   Reuse the binaries already in target/debug and any shots
#                already captured, recomposing the grid only. Use it to iterate
#                on the montage itself without paying for a rebuild.
#   --shot-timeout
#                Seconds to allow each app's first frame (default 60). Raise it
#                on a loaded machine: a starved app looks exactly like a broken
#                one here, and the grid cannot tell them apart.
set -u

cd "$(git rev-parse --show-toplevel)"

OUT=target/app-grid.png
THEME=dark
BUILD=1
SHOT_TIMEOUT=60
# `set -u` turns a value-less flag into a raw bash error about $2, which tells
# the reader nothing about which flag they got wrong.
need_value() {
  [ $# -ge 2 ] || { echo "$1 needs a value" >&2; exit 2; }
}
while [ $# -gt 0 ]; do
  case "$1" in
    --out) need_value "$@"; OUT="$2"; shift 2 ;;
    --theme) need_value "$@"; THEME="$2"; shift 2 ;;
    --no-build) BUILD=0; shift ;;
    --shot-timeout) need_value "$@"; SHOT_TIMEOUT="$2"; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

case "$THEME" in
  dark|light) ;;
  # Silently shooting an unknown theme yields a grid that looks fine and is not
  # what was asked for.
  *) echo "unknown theme '$THEME' (expected dark or light)" >&2; exit 2 ;;
esac
case "$SHOT_TIMEOUT" in
  ''|*[!0-9]*) echo "--shot-timeout wants whole seconds, got '$SHOT_TIMEOUT'" >&2; exit 2 ;;
esac

for tool in magick montage; do
  command -v "$tool" >/dev/null || { echo "need ImageMagick ($tool not on PATH)" >&2; exit 1; }
done

SHOTS=$(mktemp -d "${TMPDIR:-/tmp}/app-grid.XXXXXX")
trap 'rm -rf "$SHOTS"' EXIT

# Package name -> the name a user would see in the launcher. The grid is read
# by people, so it is labelled the way the OS labels these apps, not with cargo
# package names.
declare -A NAME=(
[api-tester]="API Tester" [backups]="Backups" [calculator]="Calculator" [calendar]="Calendar"
[camera]="Camera" [chat]="Chat" [clocks]="Clocks" [code-editor]="Code Editor" [contacts]="Contacts"
[containers]="Containers" [db-client]="Database Client" [diffmerge]="Diff & Merge" [disks]="Disks"
[disk-usage]="Disk Usage" [ebook]="E-Book Reader" [feeds]="Feeds" [file-explorer]="Files" [firewall]="Firewall"
[git-client]="Git" [hex-editor]="Hex Editor" [imager]="Disk Imager" [logs]="Logs"
[mail]="Mail" [maps]="Maps" [monitor]="System Monitor" [music]="Music" [notes]="Notes"
[osk]="On-Screen Keyboard" [passwords]="Passwords" [pdf-viewer]="PDF Viewer" [photos]="Photos"
[print-centre]="Print Centre" [remote]="Remote Desktop" [scratchpad]="Scratchpad" [settings]="Settings"
[sheets]="Sheets" [slides]="Slides" [shortcuts]="Shortcuts" [snippets]="Snippets" [speed-test]="Speed Test" [store]="App Store"
[terminal]="Terminal" [themes]="Themes" [totp]="TOTP" [tube]="Tube"
[typst-editor]="Typst Editor" [video-editor]="Video Editor" [video-player]="Video Player"
[vpn]="VPN" [weather]="Weather" [windows]="Windows Apps" [word-processor]="Word Processor")

# Apps with no window to shoot, and why. Listing them explicitly is what lets
# the drift check below treat anything *else* that is missing as a mistake.
declare -A NO_WINDOW=(
[agent]="a participant without a window"
[browser]="iced 0.13 has no headless surface, so no --shots mode yet")

APPS=$(printf '%s\n' "${!NAME[@]}" | sort)

# A hardcoded list stops covering the fleet when an app is added. Name anything
# on disk that this grid would neither shoot nor deliberately skip.
if [ -d apps ]; then
  unlabelled=""
  for d in apps/*/; do
    a=$(basename "$d")
    [ -f "$d/Cargo.toml" ] || continue
    [ -n "${NAME[$a]+x}" ] && continue
    [ -n "${NO_WINDOW[$a]+x}" ] && continue
    unlabelled="$unlabelled $a"
  done
  [ -n "$unlabelled" ] && {
    echo "note: not in the grid and not a known windowless app:$unlabelled" >&2
    echo "      add a display name to NAME (or a reason to NO_WINDOW)." >&2
  }
fi

if [ "$BUILD" -eq 1 ]; then
  echo "=== building $(printf '%s\n' "$APPS" | wc -l) apps at $(git rev-parse --short HEAD) ==="
  PKGS=""
  for a in $APPS; do PKGS="$PKGS -p $a"; done
  # shellcheck disable=SC2086  # $PKGS is a deliberately word-split package list
  # Job count is the cargo shim's to decide; do not override it here.
  cargo build --locked $PKGS || {
    echo "build failed — not composing a grid from stale binaries" >&2
    exit 1
  }
fi

echo "=== shooting each app (offscreen, $THEME) ==="
ok=0
missing=""
for a in $APPS; do
  bin="target/debug/$a"
  raw="$SHOTS/$a.raw"
  if [ ! -x "$bin" ]; then
    missing="$missing $a(nobin)"
    continue
  fi
  # Offscreen, in the test environment: no display, so the app takes its own
  # headless path, and no session bus, so it registers no media player or
  # portal client on the caller's desktop.
  # Keep the output: when an app fails to produce a frame its own complaint is
  # the only thing that explains why, and discarding it leaves a bare "noshot".
  TILO_SHOTS_THEME="$THEME" bash tools/with-test-environment.sh \
    timeout "$SHOT_TIMEOUT" "$bin" --shots "$raw" \
    > "$SHOTS/$a.log" 2>&1
  if [ ! -s "$raw" ]; then
    missing="$missing $a(noshot)"
    # Surface the tail of its output so the failure is diagnosable from this
    # run without rerunning the app by hand.
    if [ -s "$SHOTS/$a.log" ]; then
      echo "  $a produced no frame; its last output was:" >&2
      sed 's/\x1b\[[0-9;]*m//g' "$SHOTS/$a.log" | tail -3 | sed 's/^/    /' >&2
    fi
    continue
  fi
  # `png:` forces the decoder rather than letting ImageMagick guess from the
  # extensionless name. Each shot becomes a uniform tile with its display name
  # captioned underneath, so the montage stays readable at fleet scale.
  if magick "png:$raw" -resize 640x400 -background '#16181d' -gravity center -extent 640x400 \
      \( -size 640x34 -background '#0e0f13' -fill '#dfe3ec' -gravity center \
         -pointsize 22 label:"${NAME[$a]}" \) \
      -append "png:$SHOTS/$a.tile.png"; then
    ok=$((ok + 1))
  else
    missing="$missing $a(tile)"
  fi
done

echo "tiles: $ok ok${missing:+ — missing:$missing}"

# A starved app and a broken app both time out, and the tile grid cannot tell
# them apart. Say so when the machine is loaded enough for that to be the
# likely story, rather than reporting a confident "noshot".
case "$missing" in
  *noshot*)
    cpus=$(nproc 2>/dev/null || echo 1)
    load=$(awk '{print int($1)}' /proc/loadavg 2>/dev/null || echo 0)
    if [ "$load" -gt "$cpus" ]; then
      echo "  note: load is ${load} on ${cpus} cpus — apps that timed out may" >&2
      echo "        have been starved rather than broken. Re-run when the box" >&2
      echo "        is quiet, or raise --shot-timeout." >&2
    fi
    ;;
esac

[ "$ok" -gt 0 ] || { echo "no tiles rendered; nothing to compose" >&2; exit 1; }

# Order by display name so the grid reads alphabetically as labelled.
ORDERED=$(for a in $APPS; do
  [ -f "$SHOTS/$a.tile.png" ] && printf '%s\t%s\n' "${NAME[$a]}" "$SHOTS/$a.tile.png"
done | sort | cut -f2)

mkdir -p "$(dirname "$OUT")"
# shellcheck disable=SC2086  # $ORDERED is a deliberately word-split file list
montage $ORDERED -tile 6x -geometry +8+8 -background '#0b0c0f' "png:$OUT" || {
  echo "montage failed" >&2
  exit 1
}

echo "=== $OUT ==="
identify "$OUT" 2>/dev/null | head -1
