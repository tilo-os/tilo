# Screenshot + Interaction Workflow for Visual Parity

This document explains how to capture screenshots of the running nirios shell
(`shell`) and drive it interactively for visual-parity work against
the `nirios-concept/` React mockup.

## Why a custom workflow

On GNOME Wayland:

- `wmctrl` / `xdotool` only see XWayland windows — our native wayland-client
  binary is invisible to them.
- `org.gnome.Shell.Introspect.GetWindows` is **access-denied** to user
  clients.
- `xdg-desktop-portal.Screenshot` captures the **full screen**, not a
  specific window. GNOME's `ScreenshotWindow` is access-denied for D-Bus
  callers.

The one API that works for per-window geometry is a GNOME Shell extension
that exposes `org.gnome.Shell.Extensions.Windows`:

- **Window Calls Extended** (`window-calls-extended@hseliger.eu`), or
- **Window Calls** (`window-calls@domandoman.xyz`) — preinstalled on
  Fedora Workstation 42; exposes the same interface.

Either works. The script below detects by interface, not by extension ID.

## Prerequisites

- `python3-gobject` (Fedora system python only — `gi` is not in linuxbrew
  python). The script's shebang is pinned to `/usr/bin/python3` for this
  reason.
- `ImageMagick` (`magick` or `convert` CLI) for cropping.
- A GNOME Shell extension that provides `org.gnome.Shell.Extensions.Windows`.
- `ydotool` + `ydotoold` running for keyboard/mouse synthesis (optional,
  only needed for interactive flows).

Verify:

```
gdbus introspect --session --dest org.gnome.Shell \
    --object-path /org/gnome/Shell/Extensions/Windows | head -20
```

If you see `method: List`, `GetFrameRect`, `Activate`, etc., you're set.

## One-shot screenshot

`scripts/screenshot-app.py` handles the whole loop:

1. Launches the binary (optional — omit `--launch` to screenshot an already-running window).
2. Polls `Windows.List` until a title substring matches.
3. Fetches the window's frame rect via `GetFrameRect`.
4. Captures the full screen via `xdg-desktop-portal.Screenshot` (non-interactive).
5. Crops the PNG to the frame rect via ImageMagick.
6. Writes to `--out` and kills the launched process unless `--keep` is passed.

**Standard invocation:**

```bash
scripts/screenshot-app.py \
    --launch './target/debug/shell' \
    --title 'QuickUI App' \
    --out target/nirios-screenshots/01-desktop-baseline.png \
    --settle 1.5
```

- `--title` is a **substring** match. The window's actual title is set in
  `ui/src/app.rs` via `.with_title("QuickUI App")`.
- `--settle` waits that many seconds after the window is found before
  capturing, so the first frame has been painted. Bump to 2.0–3.0 if
  you're running a debug build that's slow to initialize.
- If the host lacks `libxkbcommon-devel`, build/run the binary inside
  distrobox: `distrobox enter my-distrobox -- bash -lc 'cargo build -p shell'`
  and invoke the script from the host as normal (the D-Bus session is shared).

**Outputs** go under `target/nirios-screenshots/` (gitignored via the
existing `target/` rule).

**Naming convention:**

```
NN-<view>-<state>.png
```

e.g. `01-desktop-baseline.png`, `02-desktop-gradient.png`,
`03-overview-open.png`, `04-search-palette.png`.

The `NN` prefix keeps `ls` chronological.

## Keeping the window alive for interaction

Pass `--keep` to leave the launched process running after the screenshot:

```bash
scripts/screenshot-app.py \
    --launch './target/debug/shell' \
    --out target/nirios-screenshots/01-desktop-baseline.png \
    --keep
```

Then you can drive it with `ydotool` and re-screenshot an already-running
instance (omit `--launch`, use the same `--title`).

## Input synthesis with ydotool

`ydotool` works on Wayland via `/dev/uinput`. Make sure `ydotoold` is
running (usually a user systemd unit). Then:

```bash
# Open the global search (Cmd+K / Ctrl+K)
ydotool key 29:1 37:1 37:0 29:0   # LeftCtrl+K (keycodes)

# Or by letter name via --key-delay if your ydotool version supports it:
ydotool key ctrl+k                # newer versions

# Type text
ydotool type 'notes'

# Escape
ydotool key 1:1 1:0

# Click at absolute screen coords (combine with GetFrameRect to find target)
ydotool mousemove --absolute -x 400 -y 300
ydotool click 0xC0                # left click (down + up)
```

To compute a click target **relative to the running window**: use the
window's frame rect from `GetFrameRect` and offset from `(rect.x, rect.y)`.
Snippet (bash):

```bash
RECT=$(gdbus call --session --dest org.gnome.Shell \
    --object-path /org/gnome/Shell/Extensions/Windows \
    --method org.gnome.Shell.Extensions.Windows.GetFrameRect "$WIN_ID" \
    | sed "s/^(//;s/,)$//")
X_ORIGIN=$(echo "$RECT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["x"])')
Y_ORIGIN=$(echo "$RECT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["y"])')
# Click the top-bar "workspace" button (~110 px from left, 24 px from top)
ydotool mousemove --absolute -x "$((X_ORIGIN + 110))" -y "$((Y_ORIGIN + 24))"
ydotool click 0xC0
```

For GNOME's **Activate** call to focus the window before typing into it:

```bash
gdbus call --session --dest org.gnome.Shell \
    --object-path /org/gnome/Shell/Extensions/Windows \
    --method org.gnome.Shell.Extensions.Windows.Activate "$WIN_ID"
```

## Interaction + screenshot loop

The pattern for "do X, then capture":

```bash
# 1. Launch + screenshot baseline (keep alive)
scripts/screenshot-app.py \
    --launch './target/debug/shell' \
    --out target/nirios-screenshots/01-baseline.png \
    --keep

# 2. Find the window ID for subsequent operations
WIN_ID=$(gdbus call --session --dest org.gnome.Shell \
    --object-path /org/gnome/Shell/Extensions/Windows \
    --method org.gnome.Shell.Extensions.Windows.List \
    | python3 -c "import sys,json,re; payload=re.match(r\"\\(('(.*)'|\\\"(.*)\\\"),\\)\", sys.stdin.read()); print([w['id'] for w in json.loads(payload.group(2) or payload.group(3)) if 'QuickUI App' in w.get('title','')][0])")

# 3. Activate + interact
gdbus call --session --dest org.gnome.Shell \
    --object-path /org/gnome/Shell/Extensions/Windows \
    --method org.gnome.Shell.Extensions.Windows.Activate "$WIN_ID"
sleep 0.2
ydotool key ctrl+k      # open search palette
sleep 0.3

# 4. Screenshot the new state (no --launch this time)
scripts/screenshot-app.py --out target/nirios-screenshots/02-search-open.png

# 5. Clean up
kill $(pgrep -f target/debug/shell)
```

## Decisions

- **Don't reinvent.** There has been more than one urge to build a parallel
  "ShellHarness" with offscreen rendering. Don't. Real-window capture via
  the portal exercises the actual wgpu + vello + winit paint path; a mock
  would diverge. Reserve offscreen rendering for unit tests of isolated
  widgets (see `ui/tests/support/offscreen.rs`).
- **Only inspect screenshots where you suspect visual regressions.**
  Treat screenshots like integration-test fixtures — compare only when
  you've just changed something that should affect the view, or when you
  suspect unrelated regressions.
- **Names are significant.** When you name a screenshot, include both
  the *view* (`desktop`, `overview`, `search`) and the *state* (`baseline`,
  `with-gradient`, `open`, `drag`) so a future ralph loop can diff
  timestamped captures against each other without re-running the whole
  flow.
