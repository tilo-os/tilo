# Real-input e2e design

> **Status:** spec, not yet implemented. The in-process e2e harness (`ShellDriver` + `--mode offscreen|recording`) covers >100 tests today; this spec is for the **out-of-process** path that drives a live `quick-desktop` binary with real Wayland-level input events.

## Goal

Let test code (and humans, and AI agents) drive a **running** `quick-desktop` instance — connected to a real Wayland display, painting real frames into a real window — using real input events that go through the full seat → focus → handler stack. Discover click targets via the same widget-tree resolver the in-process harness uses, so tests don't hard-code pixel coordinates.

## What's already in place

After the e2e-recording-resolver worker lands (PR in flight on `e2e-recording-resolver` branch):

- `ShellDriver::widget_registry() -> HashMap<String, Rect>` — recursive walk of the live widget tree, returns every addressable widget by `test_id` or by `path/name[index]` fallback.
- `Widget::test_id(&self) -> Option<&str>` trait method.
- `--mode recording --out path.h264` — frame-by-frame H.264 video output of any scenario.

The in-process harness covers everything *except* the smithay seat layer. The out-of-process path exists to cover the seat layer too, plus regressions that only show up under real timing (compositor frame pacing, input handler ordering, focus stealing).

## Architecture

### Component 1: control socket in the compositor

When the compositor boots with `NIRIOS_TEST_CONTROL=/run/user/N/nirios-test-N.sock` set, `compositor::native::run` binds an additional unix socket alongside the wayland one. The socket accepts newline-delimited JSON commands and emits newline-delimited JSON responses.

```
listen → control_sock
loop:
  accept() → conn
  spawn task: read_lines(conn) → dispatch(cmd) → write(resp)
```

Commands are dispatched to a new `compositor::test_control` module that holds a `Weak<Mutex<State>>` and a `Weak<Mutex<FrameBuilder>>`. Dispatch happens on the calloop loop's main thread (via a `calloop::ping::Ping`) so seat injection stays single-threaded.

### Component 2: commands

| Command | Behaviour | Response |
|---|---|---|
| `{"op":"list_targets"}` | Drain `widget_registry` from the current `FrameBuilder` | `{"targets": [{"name":"...", "rect":[x,y,w,h]}, ...]}` |
| `{"op":"click","name":"X"}` | Resolve `X` via registry → centre → inject `PointerMotion` + `PointerButton{Pressed,Released}` into smithay's `Seat` | `{"ok": true}` or error |
| `{"op":"click_at","x":N,"y":N}` | Same but with raw coords | `{"ok": true}` |
| `{"op":"drag","from":"A","to":"B","steps":N}` | Sequence: motion to A, press, N motion steps to B, release | `{"ok": true}` |
| `{"op":"key","key":"Enter"}` | Inject `KeyboardKey{Pressed,Released}` for named or character key | `{"ok": true}` |
| `{"op":"type","text":"hello"}` | Sequence of `key` events for each character | `{"ok": true}` |
| `{"op":"modifiers","ctrl":true,"shift":false,...}` | Update the modifier mask before next key event | `{"ok": true}` |
| `{"op":"snapshot","path":"/tmp/X.png"}` | Trigger off-screen render via the same `OffscreenRenderer` shots uses, write PNG | `{"ok": true, "path":"..."}` |
| `{"op":"state_dump"}` | JSON dump of `Chrome` state + active workspace tree (read-only view) | `{"chrome": {...}, "tree": {...}}` |

All commands are non-blocking from the client's perspective (the response includes the result), but the compositor processes them serially on its main thread.

### Component 3: `nirios-control` CLI

Thin wrapper for humans + scripts:

```bash
$ nirios-control list-targets
add_workspace                          (1234, 56, 32, 32)
ws_settings_tab[general]               ...
OverlayHost/TopBarChrome/Button[0]     ...

$ nirios-control click ws_settings_tab[general]
ok

$ nirios-control type "hello world"
ok
```

Lives at `quick-shell/src/bin/nirios-control.rs`. ~200 lines of CLI plumbing on top of the socket protocol.

### Component 4: test harness

New `quick-shell/tests/support/live_compositor.rs` helper:

```rust
pub struct LiveCompositor {
    child: Child,
    control: UnixStream,
    wayland_display: String,
}

impl LiveCompositor {
    pub fn spawn() -> Self;                          // spawns binary, waits for socket bind
    pub fn click(&mut self, name: &str) -> Result<()>;
    pub fn type_text(&mut self, text: &str) -> Result<()>;
    pub fn snapshot(&mut self, path: &Path) -> Result<()>;
    pub fn state(&mut self) -> Result<ChromeStateView>;
}

impl Drop for LiveCompositor { /* SIGTERM child */ }
```

Tests look like:

```rust
#[test]
fn live_palette_opens_on_ctrl_k() {
    let mut comp = LiveCompositor::spawn();
    comp.modifiers(Modifiers::CTRL).unwrap();
    comp.key("k").unwrap();
    let state = comp.state().unwrap();
    assert!(state.chrome.overlays.command_palette_open);
}
```

The test gates itself behind `#[ignore]` until the worker actually runs in distrobox with display access.

## Comparison with alternatives

| Approach | Pros | Cons |
|---|---|---|
| **Custom control socket** (chosen) | Deterministic; zero external deps; tests work in any distrobox; exposes widget_registry directly | Only works on our compositor; bespoke protocol |
| **libei** | Standard input-emulation API; works on KDE/GNOME/Sway too | Newer (since 2023); requires running libei daemon; doesn't know about widget rects so needs separate path for `list_targets` |
| **`zwlr_virtual_pointer_v1` + `zwp_virtual_keyboard_v1`** | Standard wlroots protocols; ecosystem tools (ydotool, wtype) work | Server-side: we'd need to advertise the protocols + parse the requests; client-side: need to bind+drive them — more code than option 1; still doesn't give widget discovery |

The control socket wins because the killer feature is **widget discovery integrated with input injection** in a single round-trip. The other approaches give you input emulation but not "what should I click on?"

Down the line we can ship `zwlr_virtual_pointer_v1` *in addition* — it brings ecosystem compatibility (e.g. external accessibility tools), it's parallel work. Item left as a Phase 2 stretch goal in the eventual plan.

## Phases (when the plan gets written)

1. **Phase 1** — `compositor::test_control` module + JSON command dispatch + `NIRIOS_TEST_CONTROL` env var. Tests: per-command unit tests against a `MockState`, one integration test that connects to a real spawned compositor and lists targets.
2. **Phase 2** — Real seat injection (motion / button / key / modifiers). Tests: spawn → click named widget → assert state mutated. Use `Chrome::overlays.command_palette_open` as the canary.
3. **Phase 3** — `nirios-control` CLI binary. Tests: shell-out from a bash script in `scripts/`.
4. **Phase 4** — Migrate ~5 existing in-process tests to also run as live-compositor tests (`#[ignore]`d by default; opt-in via `NIRIOS_LIVE_E2E=1`). Confirms parity between in-process and out-of-process paths.
5. **Phase 5 (stretch)** — `zwlr_virtual_pointer_v1` + `zwp_virtual_keyboard_v1` protocol support. Brings third-party tools (ydotool, wtype) into the picture.

## Risks

| Risk | Mitigation |
|---|---|
| Control socket exposes attack surface | Gate on `NIRIOS_TEST_CONTROL` env var (not advertised in `--help`); socket path is a `/run/user/$UID/...` so file-mode-based isolation; refuse if `XDG_RUNTIME_DIR` doesn't exist |
| Test flakiness from frame timing | Each command response includes the post-command frame counter; tests poll for state changes against frame number, not wall time |
| Seat injection bypasses focus rules | Use `smithay::input::Seat::input()` which respects focus; don't poke at the surface tree directly |
| Widget registry stale between paints | `widget_registry` only walks after the next `paint_settled()` to ensure layout has converged |

## Effort estimate

- Phases 1–3: 2–3 days of focused work
- Phase 4: 1 day (per-test migration is fast once helper exists)
- Phase 5 (stretch): 3–5 days (wlroots protocols are involved)

Total: ~1 week for the full Phase 1–4 stack. Stretch (Phase 5) is independent.

## Open questions

- Should `list_targets` include only visible widgets, or also offscreen/clipped ones? Inclination: visible-only (filter by intersection with viewport), with a flag for the full tree.
- Snapshot resolution: match the live window size, or always 1600×900 for diff stability? Inclination: live size for fidelity, with a separate `snapshot_at(w,h)` for golden assertions.
- Should the control socket survive across multi-output reconfiguration? Yes — it's bound to the binary, not the display.
