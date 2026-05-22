# Browser: migrate from `iced` to `quick-ui` — RFC

> **Status:** spec, not yet implemented. Closes the last headless-screenshot gap (currently `quick-browser --shots` is the only system app without an offscreen mode).

## Problem

`apps/browser/` is the only system app still on `iced` 0.13:

```toml
# apps/browser/Cargo.toml
iced = { version = "0.13", features = ["tokio", "image"] }
iced_futures = "0.13"
servo = { git = "https://github.com/servo/servo.git", package = "libservo" }
```

`iced` 0.13's runtime owns the winit event loop and gives no public way to drive an offscreen wgpu surface. Result: `quick-browser` has no `--shots PATH` mode, no headless test path, and can't participate in the screenshot/recording infrastructure the other apps use. The per-app-shots worker (commit `f7226e5`) explicitly deferred this gap.

The shell's own UI toolkit (`quick-ui`) already supports headless rendering via `OffscreenRenderer`. Every other system app (terminal, file-explorer) uses `quick-ui` and gets `--shots` for free. The browser is the outlier.

## Goal

Replace `iced` with `quick-ui` for the browser's chrome (address bar, tab strip, navigation buttons, status indicator) **without touching Servo**. Servo is the rendering engine; it's framework-agnostic. The chrome around it is what needs porting.

After the migration:
- `quick-browser --shots /tmp/browser.png` works the same way it does for terminal + file-explorer
- The browser participates in the same e2e test harness as the rest of the shell (NIRIOS_TEST_CONTROL, snapshot, headless backend)
- Browser chrome theming flows through `quick_ui::theme::Theme` like everything else (currently the iced styling diverges from the rest of the OS)

## Inventory: what iced is doing in `apps/browser/`

Run `rg -l "iced::" apps/browser/src/` to enumerate. Expected hits (audit before estimating):
- `apps/browser/src/main.rs` — `iced::Application::run`
- `apps/browser/src/app.rs` — `Application` impl: `Message` enum, `update`, `view`, `subscription`
- `apps/browser/src/chrome/` — UI components (tab bar, address bar, nav buttons)
- `apps/browser/src/servo_glue.rs` (or similar) — wgpu surface handoff to Servo

The Servo glue is the only piece that depends on iced's renderer; everything else is iced's message/update/view pattern.

## Migration approach

### Phase A: Servo isolation (no UI changes yet)

The Servo↔chrome boundary today probably looks like: iced::Application owns the winit window + wgpu surface, hands a sub-region to Servo via `iced::widget::Shader` or a custom canvas widget.

Refactor so Servo gets its surface from a generic trait:

```rust
trait BrowserHost {
    fn servo_surface(&self) -> &wgpu::Surface<'static>;
    fn servo_size(&self) -> (u32, u32);
}
```

Both an iced and a quick-ui host can implement this trait. The Servo plumbing stays unchanged.

### Phase B: quick-ui chrome

Rewrite the chrome layer using `quick-ui` widgets:

| iced concept | quick-ui equivalent |
|---|---|
| `iced::Application` | `quick_ui::App` |
| `Column` / `Row` | `quick_ui::widget::Column` / `Row` |
| `text_input::TextInput` | `quick_ui::widget::TextInput` |
| `button::Button` | `quick_ui::widget::Button` |
| `container::Container` | `quick_ui::widget::Container` |
| `Message` enum + `update` | Direct CRDT mutation via the `Widget::handle_event` path (the quick-ui pattern — no message-passing layer) |
| `subscription` (for async events) | Tokio task that mutates the CRDT; `App::needs_repaint` picks up the change |
| Styling via `iced::theme::Theme` | `quick_ui::theme::Theme` (same `bg_app` / `accent` / etc. tokens) |

The biggest shape change: iced's `update(Message)` becomes direct CRDT writes from `handle_event`. The browser's tab list moves from `Vec<Tab>` to an `automerge::Object` so it composes with the rest of the shell's collaborative state.

### Phase C: `--shots PATH` mode

Once the chrome is `quick-ui`, copy the pattern from `apps/terminal/src/main.rs` (the `--shots` block): `OffscreenRenderer::new(w, h)` + one frame paint + PNG write. ~30 lines.

### Phase D: cleanup

- Remove `iced` from `apps/browser/Cargo.toml`
- Remove `iced_futures`
- Drop the `iced` workspace patch if there is one
- Update `docs/headless-testing.md`'s "What's still missing" section to delete the iced gap

## Estimated effort

Tier-1 plan (Phases A → B → C → D, no scope creep):

- Phase A (Servo isolation): 1 day
- Phase B (chrome rewrite): 2-3 days (depends on how many iced widgets are in use)
- Phase C (--shots mode): 1 hour
- Phase D (cleanup): 30 minutes

Total: ~3-4 days for a single focused engineer.

## Risks

| Risk | Mitigation |
|---|---|
| Servo's API expects iced-specific types | Phase A factors them out behind a trait. If Servo demands `iced::widget::Shader` semantics specifically, fall back to a `quick_ui::widget::CustomCanvas` widget that exposes a similar interface |
| iced's subscription system (e.g. for keyboard shortcuts) doesn't map cleanly | quick-ui's `handle_event` runs every winit event through every widget; the equivalent of "subscribe to keyboard" is "implement `handle_event` and pattern-match on `WindowEvent::KeyboardInput`" |
| Browser-specific features depend on iced's text rendering | `quick_ui` uses cosmic-text + vello text rendering — feature parity is high. If the browser exposes rich text styling iced doesn't, that's a follow-up |
| User-visible regression in the address bar UX | Use the e2e harness to snapshot every browser state before + after; pixel-diff tells the truth |

## What this RFC does NOT solve

- **Servo upgrade or replacement.** This is a UI-framework migration only; the rendering engine stays.
- **Browser-specific CRDT semantics.** Whether tabs sync across peers (so two collaborators see the same set of open URLs in a shared workspace) is a separate design question. v1 of this migration keeps tab state local; the CRDT migration is a follow-up if anyone wants it.
- **The iced ecosystem upstream change.** If `iced` 0.14 ever exposes headless rendering, this RFC becomes unnecessary. As of 2026-05-22 it doesn't, and there's no announced roadmap for it.

## Implementation hand-off

When a worker picks this up:
1. Open `apps/browser/src/` and inventory exactly which iced surfaces are in use (`rg "iced::" apps/browser/src/` and count distinct module imports).
2. Read `apps/terminal/src/main.rs` + `apps/file-explorer/src/main.rs` for the canonical `quick_ui::App::run` + `--shots PATH` pattern.
3. Phase the work as A → B → C → D above; commit each phase separately so a bisect is meaningful if a regression appears.
4. The acceptance test is `quick-browser --shots /tmp/browser.png` writing a valid PNG with chrome + a Servo-rendered page (`about:blank` is fine for the smoke).

## Open questions for the implementing session

1. **Does Servo's wgpu integration require the rendering thread to BE the winit thread?** If so, the swap from iced's event loop to quick_ui's App needs careful threading review. Audit during Phase A.
2. **Should the browser's tab state be a workspace-scoped CRDT (synced across peers) or a local-only doc?** v1: local-only (matches today's behaviour). v2: workspace-scoped if peer-shared browsing is wanted. Out of scope for this RFC.
3. **Do we keep `iced_futures` even after dropping `iced`?** No — its only purpose is iced subscription bridging. After Phase B, tokio tasks just call into `quick_ui::App` state directly.
