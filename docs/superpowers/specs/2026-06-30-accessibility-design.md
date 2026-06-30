# Accessibility Architecture for qOS (nirios)

> Status: **Design / not implemented.** This is a phased architecture plan, to be
> executed by multiple large subagents. It does not contain implementation code.
>
> Author intent (verbatim): *"a11y is very important to me… I want this to be the
> most accessible OS possible from our side."* The bar is **best-in-class**, not
> checkbox-compliance.

---

## Executive summary

qOS is a custom Wayland compositor (`shell`, smithay) hosting a custom retained-mode,
CRDT-native GPU UI toolkit (`ui`, wgpu/vello/taffy/winit/automerge) plus system apps
that each run as separate sandboxed Wayland clients. There is currently **no
accessibility layer at all** — `Widget` exposes geometry, focus, and a test-id, but
no semantics; the compositor exports no a11y tree; there is no TTS, magnifier, OSK, or
voice control. Because every system app is built on the same `ui` toolkit, the toolkit
is the highest-leverage place to add semantics: one `Widget`-trait extension makes
*every* app accessible at once. This plan adds (1) a semantic accessibility tree on the
`Widget` trait, exported via **AccessKit → AT-SPI2** so standard ATs (Orca) work, plus a
**built-in screen reader** for a first-party, deeply-integrated experience using
**speech-dispatcher + Piper (quality) + espeak-ng (low-latency fallback)**; (2) a
compositor-level **magnifier**, a **high-contrast** theme track, and **offline voice
control** (Vosk for command grammar); and (3) a first-party **on-screen keyboard** as an
`input-method-v2`/`virtual-keyboard` layer-shell client, with **FUTO swipe-to-type**
(C++ `swipe-library` via FFI, ExecuTorch models) and **on-device next-word prediction**.

The single hardest cross-cutting problem is **AT-SPI conformance across the per-workspace
Bubblewrap sandbox**: each workspace has a *private* D-Bus session bus, so the a11y bus
(`org.a11y.Bus`) must be proxied into every sandbox (the same problem Flatpak solves with
a filtered D-Bus proxy). Get that wrong and no AT can see any app. It is called out
explicitly and sequenced first.

### Phasing overview (foundational → later)

| Phase | Theme | Why here |
|------:|-------|----------|
| **P0 Foundation** | a11y bus plumbing across sandbox; `AccessNode`/`Role`/`Action` types in `ui`; high-contrast theme tokens; settings plumbing (`a11y` CRDT settings + env). | Everything else depends on the semantic model + the bus reaching apps. High-contrast is independently shippable and easy — ship it first as a morale/coverage win. |
| **P1 Core a11y tree** | `Widget` trait semantics on all ~55 widgets; AccessKit tree built + incrementally updated in `App`; AT-SPI export via `accesskit_unix`; Orca works end-to-end. | This is the "really, really good" pillar #1. Without it, nothing reads. |
| **P2 Built-in screen reader + speech** | speech-dispatcher integration, focus/caret/live-region narration, reading order, interrupts, keyboard nav completeness (caret browsing, landmark/heading nav). | Layered on the P1 tree. The first-party "from our side" experience. |
| **P3 OSK** | layer-shell OSK app, input-method-v2 text injection, multi-layout data format, responsive sizing, customizable toolbar. | The other "really good" pillar. Independent of screen reader but shares focus/IM plumbing. |
| **P4 OSK intelligence** | FUTO swipe FFI + ExecuTorch models; on-device word prediction (n-gram → small transformer). | Layered on P3. Highest external-integration risk; isolated so P3 ships without it. |
| **P5 Magnifier + Voice control** | compositor magnifier (focus/cursor tracking), offline STT command grammar (Vosk), action dispatch via control socket + AT-SPI actions. | Valuable but lower-priority than reader/OSK; magnifier touches the DRM/vello scanout path. |

---

## 0. Context: what exists today (grounding)

Read these before implementing anything in the relevant area.

### 0.1 The `ui` toolkit (`ui/src/`)
- **`Widget` trait** — `ui/src/widget/mod.rs:14`. Five-pass frame: `sync` (pull from
  the automerge doc) → `build_layout` (taffy) → `apply_layout` → `draw` (vello `Scene`)
  → `handle_event` (winit events mutate the CRDT directly). Focus model:
  `is_focusable()`/`set_focus()`/`is_focused()` (`mod.rs:39-41`), `children()`/
  `children_mut()` give the tree, `get_rect()` gives geometry, `name()` gives a debug
  label, `test_id()` (`mod.rs:37`) gives an opt-in stable id used by the e2e harness.
  **There is no semantic role, accessible name, value, or state.** This is where the
  a11y model attaches.
- **`App`** — `ui/src/app.rs`. Owns the winit event loop via `ApplicationHandler`
  (`app.rs:871`), the taffy tree, `node_map: HashMap<NodeId, *mut dyn Widget>`
  (`app.rs:103`), `focused_node`, and `focusable_order: Vec<NodeId>`
  (`app.rs:115`, built by `update_focusable_order` at `app.rs:513`). Keyboard handling
  in `window_event` (`app.rs:996`): F12 debug, Ctrl+Z/Y undo, **Tab/Shift+Tab linear
  focus** (`app.rs:1471`), **Alt+Arrows and bare Arrows → `navigate_focus(Direction)`**
  i.e. 2D *spatial/directional* focus (`app.rs:1507`, `1560`; impl at `app.rs:636-686`).
  Synthetic input injection (`UserEvent::{Hover,Click,Wheel,External}`, `app.rs:25`)
  re-enters the *same* `window_event` path — this is the model the screen reader and OSK
  reuse. Apps boot via `App::new(state, root_widget).run()` (see `apps/notes/src/main.rs`).
  There is **no IME/text-input handling** in `ui` yet (no `Ime` event branch); text
  arrives as `Key::Character`.
- **Theme + tokens** — `ui/src/theme.rs`, `ui/src/tokens/`. `Theme` is passed into
  every `draw`; widgets must read colors from it (OS-controlled theming). Tokens already
  model `Colors` (neutral/accent ramps, `SemanticColors`, `Surfaces`), `border_focus`,
  `Typography`, `Spacing`, `Radii`, `Motion`. Theme presets exist (`dark`, `light`,
  `dracula`, `nord`, …). High-contrast is "another preset + a few rules", i.e. cheap.
- **i18n** — `ui/src/i18n.rs` (`fluent`). `i18n!("key")` macro. Screen-reader
  announcements and OSK labels must use FTL keys, not literals.
- **Text** — `cosmic-text` via `ui/src/text_render.rs`; rich text / block editor in
  `ui/src/widget/block_editor.rs`, `rich_text_line.rs`, selection in `selection.rs`.
  Caret/selection already modelled (needed for caret browsing + OSK text ops).

### 0.2 The compositor (`shell`)
- smithay 0.6+ Wayland **server**; `--mode native` (`--backend drm` for bare metal /
  `winit` for dev). Per-frame `vello::Scene` paint (`shell/src/compositor/frame.rs`).
  Input via `smithay::input::Seat`. See `plan/compositor.md`.
- **`wlr-layer-shell` is already supported** — `shell/src/network/wayland.rs` does
  `delegate_layer_shell!` and holds `WlrLayerShellState`. The OSK can be a layer-shell
  client. **`input-method-v2`, `virtual-keyboard-v1`, and `text-input-v3` are NOT yet
  implemented** (grep finds none) — they must be added for the OSK to inject text.
- Touch: smithay seat declares `type TouchFocus = WlSurface` (`compositor/state.rs:505`)
  but the `ui` toolkit (winit) does **not** process `WindowEvent::Touch` yet — needed for
  OSK on touch panels.
- **Control socket** (`QOS_TEST_CONTROL`, `shell/src/compositor/test_control.rs`):
  newline-delimited JSON injected through `Seat::input`. The voice-control dispatcher and
  automated a11y tests reuse this.
- Shell chrome is one widget tree (`ShellOverlayHost`, `shell/src/shell/overlay_host.rs`)
  composited over app surfaces — the natural home for first-party a11y chrome (reader
  status, magnifier overlay, OSK when run as chrome rather than a separate client).

### 0.3 Sandbox & D-Bus (the conformance constraint) — `plan/security.md`
- The security boundary is the **workspace**: each runs apps in a Bubblewrap sandbox
  with its own user/mount/pid namespaces, seccomp-bpf, a **per-workspace private D-Bus
  session bus** (`SandboxProfile.dbus_socket`), and a per-workspace Wayland **bridge
  socket** (`shell/src/compositor/bridge.rs`). seccomp blocks `socket(AF_PACKET)`, `bpf`,
  `keyctl`.
- **Implication for AT-SPI:** AT-SPI2 works by apps registering on a *dedicated*
  accessibility bus discovered via `org.a11y.Bus.GetAddress()` on the session bus. With a
  private per-workspace session bus, sandboxed apps cannot reach the host's a11y bus
  unless we **proxy it in** (exactly Flatpak's `xdg-dbus-proxy --filter` approach). This is
  P0 foundational work and the single biggest conformance risk.

### 0.4 OS layer — `os/`
- Fedora Bootc 42, immutable root, state under `/var`. systemd **system** units in
  `os/usr/lib/systemd/system/` and **user** units in `…/user/` (e.g.
  `shell-portal.service`). D-Bus service files in `os/usr/share/dbus-1/services/`. New
  a11y daemons (a11y bus launcher, speech-dispatcher, OSK, magnifier helper) are added
  here as user services in the graphical session.

---

## Dependency / sequencing diagram

```
                         ┌─────────────────────────────────────────────┐
                         │ P0 FOUNDATION                               │
                         │                                             │
   ui semantic model ───►│  W0: AccessNode/Role/Action/State types     │
   (Widget trait)        │      in `ui` (no behaviour yet)             │
                         │                                             │
   sandbox/dbus     ────►│  W1: a11y D-Bus bus proxied into sandbox    │
   (security.md)         │      + at-spi2 registry as user service     │
                         │                                             │
   theme/tokens     ────►│  W2: high-contrast theme track + a11y       │
                         │      settings (CRDT + env + reduce-motion)  │
                         └───────┬───────────────┬──────────────┬──────┘
                                 │               │              │
        ┌────────────────────────┘               │              └───────────────┐
        ▼                                         ▼                              ▼
┌──────────────────┐                  ┌────────────────────────┐    ┌────────────────────┐
│ P1 A11Y TREE     │                  │ P3 OSK (base)         │    │ P5a MAGNIFIER      │
│  W3: impl        │                  │  W7: input-method-v2 + │    │  W10: compositor   │
│  accessibility() │                  │   virtual-kbd + text-  │    │   output magnify + │
│  on all widgets  │                  │   input-v3 in shell    │    │   focus/cursor     │
│  W4: AccessKit   │                  │  W8: OSK layer-shell   │    │   tracking         │
│  tree in App +   │                  │   app (layouts,        │    └────────────────────┘
│  accesskit_unix  │                  │   toolbar, responsive) │
│  AT-SPI export   │                  └───────────┬────────────┘
└───────┬──────────┘                              │
        │ (needs W3+W4 tree & actions)            ▼
        ▼                              ┌────────────────────────┐    ┌────────────────────┐
┌──────────────────┐                  │ P4 OSK INTELLIGENCE   │    │ P5b VOICE CONTROL  │
│ P2 SCREEN READER │                  │  W9a: FUTO swipe FFI   │    │  W11: Vosk STT +   │
│  W5: speechd +   │                  │   (cxx, ExecuTorch)    │    │   command grammar  │
│  Piper/espeak    │                  │  W9b: word prediction  │    │   → control socket │
│  W6: narration,  │                  │   (n-gram→transformer) │    │   + AT-SPI actions │
│  caret browse,   │                  └────────────────────────┘    └────────────────────┘
│  landmark nav    │
└──────────────────┘

Legend: W0..W11 are the execution workstreams (see "Execution: large subagents").
Arrows are hard dependencies. P3/P5a only need P0. P2 needs P1. P4 needs P3.
```

Critical path to a *usable blind-user experience*: **W0 → W1 → W3 → W4 → W5 → W6.**
Everything else parallelizes off P0.

---

## 1. Accessibility tree + screen reader (priority #1)

> Goal: a blind or low-vision user can boot qOS, log in, and operate every system app
> and the shell entirely by keyboard + speech, with **standard ATs (Orca) working** *and*
> a **first-party screen reader** that is tightly integrated ("the most accessible OS from
> our side"). Reading is accurate, fast, and never lies about state.

### 1.1 Recommendation: AccessKit **and** a built-in reader (justified)

We do **both**, because they serve different goals and share one tree:

1. **Integrate AccessKit + export over AT-SPI2.** AccessKit (`accesskit` **v0.24.1**,
   dual **MIT/Apache-2.0**) is the de-facto cross-platform a11y abstraction; its Linux
   adapter `accesskit_unix` (**v0.22.0**, MIT/Apache, **pure-Rust over zbus 5.5** — no C
   `libatspi`/`atk`) publishes the tree on AT-SPI2 over D-Bus, so **Orca and any other
   AT-SPI consumer work with zero qOS-specific knowledge.** This is non-negotiable:
   users have muscle memory and configuration in Orca; respecting the platform is the
   ethical default and the conformance baseline. It also gives us braille (BrlTTY via
   Orca) and existing AT tooling for free.
2. **Ship a built-in screen reader** that consumes the *same* AccessKit tree (in-process,
   no D-Bus round-trip) for the default "out-of-the-box, works on first boot, deeply
   integrated, spatially aware" experience. The shell knows things a generic AT cannot:
   the 2D cluster/strip/workspace topology, remote-collaborator presence ("Alice's caret
   entered this block"), permission prompts, and the wheel-disc spatial model. A
   first-party reader can narrate *those* natively. Users can disable it and use Orca
   instead; both are driven by the same semantic source of truth.

This "one tree, two consumers" design means **app authors implement semantics once**
(the `Widget` trait), and both Orca and the built-in reader benefit.

> Why not "only Orca"? Then first-boot has no voice until the user installs/configures an
> AT they can't see to configure — a chicken-and-egg failure. Why not "only our own"? Then
> we abandon braille, existing AT muscle memory, and years of Orca refinement, and we'd
> have to reinvent AT-SPI conformance anyway to be a good Linux citizen. Both.

### 1.2 `Widget` trait additions (the semantic model)

Add to `ui/src/widget/mod.rs`. All default to "no semantics" so the ~55 existing widgets
compile unchanged and are incrementally annotated.

> Note: AccessKit's own schema is far richer than we need to re-state — `accesskit::Role`
> has **182 variants** and `accesskit::Action` has **22** (incl. `Click`, `Focus`,
> `Increment`/`Decrement`, `Expand`/`Collapse`, `SetValue`, `SetTextSelection`,
> `ScrollIntoView`, `ShowContextMenu`, …), with built-in `Live` politeness. Our
> `AccessNode`/`Role`/`Action`/`A11yState` below are a **thin, toolkit-ergonomic
> convenience layer** that the `a11y_tree` builder (W4) maps onto AccessKit's types. Keep
> our `Role` set small and map liberally to AccessKit (e.g. our `Switch` → `Role::Switch`,
> our `TextInput`+`MULTILINE` → `Role::MultilineTextInput`); do **not** try to mirror all
> 182. This keeps widget authors' surface small while emitting a fully-detailed AT tree.

```rust
// new module: ui/src/a11y.rs
pub enum Role {
    // Structural / containers
    Window, Group, Strip, Workspace, Toolbar, TabBar, Tab, TabPanel,
    List, ListItem, Table, Row, Cell, Tree, TreeItem, Dialog, Menu, MenuItem,
    Landmark, Heading,           // for landmark/heading navigation
    // Controls
    Button, ToggleButton, Checkbox, RadioButton, Switch, Slider, SpinButton,
    ComboBox, TextInput, MultilineTextInput, Link, Image, ProgressBar, Tooltip,
    // Content
    StaticText, Paragraph, Document, Canvas, Unknown,
}

pub enum Action {                 // maps to AccessKit Action + AT-SPI actions
    Focus, Click, Increment, Decrement, ToggleExpand,
    SetValue(String), ScrollIntoView, ShowContextMenu, SetSelection { anchor: usize, focus: usize },
}

bitflags! {
    pub struct A11yState: u32 {
        const FOCUSED   = 1<<0;  const FOCUSABLE = 1<<1;  const SELECTED = 1<<2;
        const DISABLED  = 1<<3;  const EXPANDED  = 1<<4;  const COLLAPSED = 1<<5;
        const CHECKED   = 1<<6;  const MIXED     = 1<<7;  const READONLY = 1<<8;
        const REQUIRED  = 1<<9;  const INVALID   = 1<<10; const BUSY     = 1<<11;
        const MODAL     = 1<<12; const HIDDEN    = 1<<13; const MULTILINE = 1<<14;
    }
}

/// What a widget tells the a11y layer about itself. Geometry comes from
/// `Widget::get_rect()`; children come from `Widget::children()`. This is the
/// per-widget *local* semantics; the App assembles the tree.
pub struct AccessNode {
    pub role: Role,
    pub name: Option<String>,          // accessible_name (FTL-resolved)
    pub description: Option<String>,   // accessible_description
    pub value: Option<AccessValue>,    // text value / numeric range value
    pub state: A11yState,
    /// For text widgets: the caret + selection so caret-browsing & "read from
    /// cursor" work. Byte offsets into `value` text.
    pub text_selection: Option<(usize, usize)>,
    /// Live-region politeness: None = not live; Polite = announce when idle;
    /// Assertive = interrupt. (e.g. toast = Polite, error banner = Assertive.)
    pub live: Option<Live>,
    /// Optional explicit a11y child ordering when it differs from visual
    /// `children()` order (rare; default None = use children()).
    pub owns: Option<Vec<usize>>,
    /// Supported actions beyond the implicit Focus/Click.
    pub actions: ActionSet,
}
```

New `Widget` methods (all defaulted):

```rust
trait Widget {
    // ... existing ...

    /// Local accessibility semantics for THIS widget. Default `None` =
    /// "presentational / pass-through": the node is skipped and its children
    /// are reparented to its parent (so layout-only Rows/Columns/Containers
    /// don't pollute the a11y tree). Containers that ARE semantic (TabBar,
    /// List, Dialog, Toolbar) return Some(..).
    fn accessibility(&self) -> Option<AccessNode> { None }

    /// Perform an a11y action requested by an AT or the built-in reader.
    /// Mutates the CRDT just like `handle_event` does. Returns true if handled.
    /// Default: route Focus/Click through existing focus/click paths.
    fn perform_action(&mut self, _action: &Action, _state: &mut AutoCommit, _path: &ObjId) -> bool { false }

    /// Stable identity within a frame for diffing a11y tree updates. Reuse the
    /// existing `test_id()` when present; else the App assigns a path-stable id
    /// (same scheme as the e2e widget_registry `Parent/Child[index]`).
    fn a11y_id(&self) -> Option<&str> { self.test_id() }
}
```

Design notes:
- **Presentational by default.** Returning `None` collapses pure layout widgets
  (`Row`, `Column`, `Container`, `Stack`, `Spacer`) out of the a11y tree — ATs should not
  hear "group, group, group". Only widgets with meaning opt in. This keeps the tree clean
  and matches ARIA `role="presentation"` semantics.
- **Name resolution.** `name` must be the FTL-resolved string (via `i18n!`), not a key,
  because AccessKit/AT-SPI consume final text. Where a control's label is a *sibling*
  `Text` widget (e.g. a form field), the widget references it; the App resolves
  label-relationships (see 1.3).
- **State is derived, not stored.** `A11yState` is computed from existing widget fields
  (`is_focused()`, selection indices, disabled flags) at `accessibility()` time — no new
  source of truth, so it can't drift from `draw`.
- **Reuse existing signals.** `is_focusable()` → `FOCUSABLE`; `get_rect()` → bounds;
  `children()` → subtree; `presence_key()` already exists for collab and can annotate
  remote-presence into descriptions ("edited by Alice").

### 1.3 Building & incrementally updating the tree in `App`

The `App` already walks the widget tree to build `focusable_order` (`app.rs:513`). Add an
analogous pass that produces an **AccessKit `TreeUpdate`**:

1. **Tree assembly** (`ui/src/a11y_tree.rs`, called after layout in the frame): DFS over
   `root_widget` using `children()` + `accessibility()`. Skip `None` nodes (reparent their
   children). For each `Some` node, emit an AccessKit `Node` with a stable `NodeId`
   (derived from `a11y_id()` / structural path), role mapping, bounds (transform the
   widget `Rect` to window coords), name/description/value/state, and child id list.
   Resolve label-for relationships: a `TextInput` adjacent to a `Text` adopts it as its
   `labelled_by`.
2. **Focus.** Set the AccessKit tree `focus` to the node matching `App::focused_node`.
   This is the event Orca/our reader key off of. The mapping `NodeId(taffy) ↔
   AccessKit NodeId` is maintained alongside `node_map`.
3. **Incremental diff.** Keep last frame's node set; emit only changed/added/removed nodes
   in the `TreeUpdate` (AccessKit is designed for incremental updates — pushing the whole
   tree each frame is correct but wasteful; diff on the ~dozens of changed nodes). Trigger
   conditions: focus change, `needs_sync` (CRDT changed), structure-dirty rebuild, or a
   live-region content change.
4. **Adapters.** `accesskit_unix::Adapter` is **window-agnostic** (unlike the Win/macOS
   adapters it takes no `HWND`/`NSView`) — its entire contract is three handlers plus a few
   driver calls:
   - `ActivationHandler::request_initial_tree()` — called *lazily the first time an AT
     connects*; return the full initial `TreeUpdate`. (Build nothing until someone listens.)
   - `ActionHandler::do_action(ActionRequest)` — AT asks us to perform an action → route to
     `Widget::perform_action` on the target node.
   - `DeactivationHandler::deactivate_accessibility()` — all ATs gone → drop the tree.
   - Driver side: `update_if_active(|| build_tree_update())` (**zero cost when no AT is
     attached** — the hot path pays nothing), `set_root_window_bounds(outer, inner)` (we
     supply screen coords), `update_window_focus_state(bool)`.
   - **For app windows:** use `accesskit_winit::Adapter` (**v0.33.1, Apache-2.0, targets
     winit 0.30.x — matches the `ui` pin**), which wraps `accesskit_unix` and bridges the
     winit event loop (emits initial-tree/action/deactivation events; you `process_event`
     winit events and push `TreeUpdate`s). Lowest-friction for `ui` apps → Orca for free.
   - **For the compositor/shell:** drive `accesskit_unix` **directly** so the shell — which
     owns absolute screen geometry across tiled surfaces and remote-collab cursors — calls
     `set_root_window_bounds`/`update_window_focus_state` itself. (See §1.4: clean split =
     toolkit builds the per-surface tree in relative coords; compositor injects on-screen
     rect + focus. This is exactly the direction GNOME's next-gen protocol takes — §1.10.)
   - The **built-in reader** subscribes to the same `TreeUpdate` stream in-process via a
     channel (no D-Bus): `App` exposes `fn a11y_subscribe(&mut self, Sender<TreeUpdate>)`.

```
 winit WindowEvent / CRDT change
        │
        ▼
   App frame ── layout ──► a11y_tree::build()  ──► TreeUpdate (diffed)
        │                                            ├──► accesskit_winit::Adapter ──► AT-SPI2 (Orca, braille)
        │                                            └──► in-proc channel ──► built-in reader (1.5/1.6)
        └──► focus change ──► TreeUpdate.focus
```

### 1.4 Where the tree lives across the multi-process model

Each system app is its **own process** (its own `App`, its own winit window/Wayland
surface). So **each app exports its own AT-SPI tree** — exactly like every other Linux
app. This is good: it means a third-party Wayland client that already speaks AT-SPI
(GTK/Qt app run in a workspace) Just Works alongside ours, and Orca aggregates them via
the registry. Requirements:
- The **a11y bus must reach each sandbox** (W1, §0.3) — the proxied `org.a11y.Bus`.
- The **compositor** (`shell`) also runs an `App`-like chrome tree (`ShellOverlayHost`);
  it exports its *own* tree for the top bar / wheel disc / palette / permission prompts.
- The **built-in reader** is a separate user-session process that is an **AT-SPI client**
  (consumer) of the registry — so it can read *all* apps (ours and foreign), not just
  in-process trees. The in-process channel (1.3) is an *optimization/enrichment* path for
  qOS-native spatial cues, not the only path. Concretely: the built-in reader is an
  AT-SPI2 consumer (like Orca) **plus** it listens on a qOS-private socket for
  shell-specific enrichments (workspace moves, presence). This keeps it able to read
  foreign apps while still being first-party-smart about qOS.

### 1.5 TTS engine choice

**Recommendation: `speech-dispatcher` (speechd) as the abstraction layer, with Piper as
the primary voice and espeak-ng as the low-latency fallback.** Rationale:

- **speech-dispatcher** (`speechd`) is what Orca itself uses; making our reader speak
  through it means one system-wide voice configuration, consistent behavior whether the
  user runs Orca or our reader, and pluggable engines. It provides the primitives a screen
  reader needs: **priority levels** (`important`/`message`/`text`/`notification`/`progress`),
  **immediate interrupt** (stop-and-speak on focus change — essential), rate/pitch/volume,
  and per-message cancellation. Rust side: speak **SSIP** (its socket wire protocol) via
  **`ssip-client`** (pure-Rust, MIT; Odilia uses an async fork) — **no C linkage on the hot
  path.** speechd itself is GPL, but the **SSIP socket boundary** means GPL does not
  propagate into our binary. This is the canonical clean-abstraction boundary, by design.
- **Piper** as the quality voice: neural VITS TTS, ONNX models via onnxruntime,
  natural-sounding, fully offline, 35+ languages, ~tens of MB per voice (low/medium tiers
  are the screen-reader sweet spot), realtime even on Pi-class hardware. **License caveat
  (changed 2025):** the original `rhasspy/piper` (MIT) was **archived read-only Oct 2025**;
  active dev moved to **`OHF-Voice/piper1-gpl`** (**GPL-3.0**, v1.4.2 Apr 2026 — the GPL
  comes from embedded espeak-ng used for grapheme-to-phoneme). We integrate Piper as a
  **speech-dispatcher output module** (separate process behind the SSIP socket), so its GPL
  does **not** touch qOS code. **MIT escape hatch if we ever need in-process synthesis
  without the daemon:** `piper-plus` (`ayutaz/piper-plus`, **MIT**) drops espeak-ng and
  ships its own G2P for 8 languages (incl. EN/ES/FR/PT). Plan around speechd; keep
  piper-plus as the fallback for a daemon-less embedded path.
- **espeak-ng** (GPL-3.0) as the **low-latency fallback** and for rapid feedback (keystroke
  echo, fast focus changes, "character" mode, very high speech rates): near-instant,
  robotic but precise, 100+ languages, tiny footprint. Many blind power users *prefer* it
  for responsiveness. As a speechd output module it's the safe default that always works on
  first boot. (Rust direct-bind option exists — `espeakng`/`espeakng-sys` — but we reach it
  via speechd, so no GPL linkage.)

Default policy: **espeak-ng on first boot** (guaranteed available, instant), with Piper as
an opt-in/quality upgrade selectable in a11y settings. Both via speechd so switching is a
config change, not code.

> License note: espeak-ng (GPL-3.0), speechd (GPL), and current Piper (OHF-Voice, GPL-3.0)
> are all reached as *separate processes via the SSIP socket*, so they impose **no licensing
> constraint** on the qOS code (which links only the permissive `ssip-client`). Ship them in
> the OS image; document their licenses. `piper-plus` (MIT) is the GPL-free in-process
> escape hatch if a daemon-less path is ever needed.

### 1.6 How the reader speaks (behavior spec)

The built-in reader (`apps/screen-reader/` or `shell`-embedded service) consumes
`TreeUpdate`s/AT-SPI events and produces utterances:

- **Focus changes:** on focus move, **interrupt** current speech (speechd priority
  `important` with `cancel`), then speak: *role-appropriate template* = `name` + `role` +
  state ("Save, button, dimmed") + position-in-set where relevant ("item 3 of 12") +
  contextual container changes ("entered toolbar"). Templates are FTL-localized.
- **Reading order:** depth-first over the a11y tree honoring `owns`/`children` order; for
  documents/block editor, follow text order. "Read all"/"say all" walks from the caret
  forward, chunked by sentence so it can be interrupted cleanly.
- **Caret/selection:** text widgets expose `text_selection`; the reader echoes typed
  characters (configurable: characters / words / off), announces selection growth
  ("selected: the quick brown"), and supports **read-from-cursor**, **read current
  line/word/char**.
- **Live regions:** `Live::Polite` queued and spoken when idle (toasts, status); 
  `Live::Assertive` interrupts (errors, permission prompts). The qOS permission prompt
  (`ui/src/widget/permission_prompt.rs`) and toast (`toast.rs`) annotate `live`.
- **Interrupts:** any new key press or focus change cancels pending lower-priority speech;
  a dedicated **silence key** (e.g. Ctrl) stops speech instantly (Orca convention).
- **qOS-spatial enrichment:** when the active workspace/strip/group changes (shell
  events), announce the spatial move ("Workspace 2, strip Editing, 3 windows"). When a
  remote collaborator's caret/selection enters the focused document (existing presence
  events, `ui/src/presence/`), optionally announce ("Alice is editing this paragraph") —
  gated by a verbosity setting.

### 1.7 Keyboard navigation completeness

Today: linear Tab order (`app.rs:1471`) + 2D directional arrows (`navigate_focus`,
`app.rs:636`). For a screen-reader-grade experience we add:

- **Caret browsing** (read-only navigation through text/static content by char/word/
  line/paragraph without editing), toggled by a global key. Implemented in `App` for
  text-bearing focused widgets using the existing `selection`/`text_selection` model.
- **Structural navigation:** jump to next/previous **heading**, **landmark**, **list**,
  **table**, **form field**, **button** — the classic Orca single-key navigation. Built on
  the a11y tree `Role`s. `App` gains a navigation cursor independent of edit focus.
- **"Where am I"** (announce full focus context) and **flat review** (review the screen
  spatially without moving focus) — both read from the a11y tree + geometry.
- **Escape semantics & focus traps:** modal dialogs (`modal_dialog.rs`) must set
  `A11yState::MODAL` and trap focus; the reader announces modal entry/exit.
- **Shortcut discoverability:** expose app/shell keybindings as an AT-queryable list
  (the command palette already enumerates commands — surface it as an a11y action list).

### 1.8 Testing strategy (a11y tree + reader)
- **Unit (per widget):** snapshot each widget's `accessibility()` output for representative
  states (e.g. checkbox checked/unchecked/mixed/disabled) under `cargo test -p ui`. Pin
  role/name/state — a regression that drops a name fails the test. This mirrors the
  existing vello-scene pinning tests.
- **Tree integration:** a headless `App` builds a known widget tree and asserts the
  emitted `TreeUpdate` (node count, roles, focus target, parent/child structure, that
  presentational containers are collapsed). Reuse the offscreen `App` harness.
- **AT-SPI round-trip:** an integration test boots an app with the a11y bus, then uses the
  `atspi` Rust crate as a *client* to walk the exported tree and assert it matches —
  proving the export path, not just the in-process tree. (This is the conformance test.)
- **Reader behavior:** drive focus changes via the `UserEvent`/control-socket injection and
  assert the *utterance text* produced (mock the speechd client; assert SSIP messages +
  priorities/cancels). No audio needed.
- **Verification covenant:** assert on the production path (the real `TreeUpdate` Orca
  would see), never a tautological helper — per `CLAUDE.md` and
  `shell/tests/concept_parity_spec.rs`.

### 1.9 Genuinely hard parts (be honest)
- **AT-SPI conformance is a long tail.** "Tree appears in Accerciser" is week 1; "Orca
  reads every widget the way a blind user expects" is months of dogfooding. Tables, tree
  views, the block/rich-text editor, and combobox/listbox interaction patterns are the
  hard widgets. Budget real screen-reader-user testing — there is no substitute.
- **The sandbox a11y-bus proxy (W1)** is fiddly: get the `xdg-dbus-proxy` filter rules
  wrong and either nothing is accessible or you punch a hole in the sandbox. It must be
  per-workspace and least-privilege.
- **Caret browsing + the CRDT block editor**: mapping AT-SPI text-interface offsets to
  automerge `Text` objects and multi-block documents is non-trivial; do it once, well, in
  a shared text-a11y helper.

### 1.10 Built-in reader: base on Odilia; and the ecosystem is moving our way

- **Build the reader from `atspi` + Odilia.** The pure-Rust **`atspi`** crate (Odilia
  project, **MIT/Apache-2.0**, async over zbus, wire-types validated against the AT-SPI2
  spec via `zbus-lockstep`) is the consumer side; the **Odilia** screen reader itself
  (**MIT/Apache-2.0**, async pipeline `atspi` → event processing → `ssip-client` →
  speech-dispatcher) is the **best architectural reference and a plausible fork/embed base**
  for W6. Caveat: Odilia is explicitly *"absolutely not production ready"* (early/beta,
  daily churn) — treat it as a skeleton we co-develop, not a turnkey drop-in. Its design is
  exactly the pattern we want; reuse the pattern even where we write our own code.
- **Forward-looking alignment (GNOME "Newton").** GNOME is prototyping a **push-based**
  successor to the chatty AT-SPI pull model that builds **directly on AccessKit's schema**:
  providers push full trees + incremental `TreeUpdate`s; **per-surface trees** (not
  per-app), **relative coordinates** within a surface, **atomic updates synchronized with
  visual frames**, **client-side cached querying** (no per-property D-Bus round trips), and
  provider awareness of whether any AT is listening. Status: proposal/prototyping, no
  shipped timeline; first step is an AccessKit adapter + AT-SPI bridge via the shared
  `accesskit_atspi_common`. **Why it matters:** qOS's compositor-centric,
  frame-synchronized, per-surface model (toolkit builds relative-coord trees, compositor
  injects geometry via `set_root_window_bounds`) is *already aligned* with where the
  ecosystem is heading. Building on AccessKit now means minimal rework to adopt the push
  protocol later — and qOS could even be an early implementer.

---

## 2. Magnifier, high-contrast, voice control

### 2.1 High-contrast (easy; ship in P0)

> Goal: a fully high-contrast theme track plus the supporting toggles
> (reduce-motion, larger-text, increased focus-ring), selectable system-wide and honored
> by every `ui` app automatically.

- **Architecture:** add high-contrast `Theme` presets to `ui/src/theme.rs` (e.g.
  `Theme::high_contrast_dark()`, `Theme::high_contrast_light()`) built from new token
  values: maximal fg/bg contrast (WCAG AAA ≥ 7:1 for body text), thick high-visibility
  `border_focus`, removal of low-alpha "glass"/`surface_white_5/10/20` veneers (which kill
  contrast), solid (non-translucent) surfaces, and stronger `SemanticColors`. Because
  every widget already reads colors from the passed-in `Theme` (OS-controlled theming),
  **no per-widget changes are needed** — this is the cheap, high-value win.
- **Token additions:** a `ContrastMode` on `Tokens`, a `reduce_motion: bool` consulted by
  `Motion`/animation (`ui/src/animation/`, `needs_animation_frame`), a `focus_ring_px`
  scalar, and a global **text-scale** factor applied in `Typography`/`text_render` so
  "larger text" reflows via taffy.
- **Plumbing (W2):** an `A11ySettings` struct stored in the per-user settings CRDT and
  also exposed via env (`QOS_A11Y_*`) so apps spawned into a sandbox inherit it; the shell
  broadcasts theme changes (it already controls theming). The Settings app
  (`apps/settings/`) gets an Accessibility pane.
- **Testing:** offscreen `shots` snapshots per widget in high-contrast; an automated
  **contrast-ratio assertion** over the theme's fg/bg token pairs (compute WCAG ratio, fail
  if < threshold). This is a real, cheap correctness test.
- **Subagent:** W2 (small).

### 2.2 Magnifier (compositor-level)

> Goal: full-screen and lens magnification (2×–16×), smooth, that tracks keyboard focus,
> the text caret, and the pointer; with optional color-inversion / color-filter for low
> vision.

- **Architecture:** the magnifier belongs in the **compositor** because it must magnify
  *everything* (all app surfaces + chrome), and qOS already composes the entire screen into
  one `vello::Scene` per output (`shell/src/compositor/frame.rs`). Implement as a final
  **output transform** in the frame/scanout path: apply a scale+translate (an affine) to
  the composed scene/texture for the magnified region, sampling with good filtering. Two
  modes: full-screen (transform the whole output) and lens (a movable magnified rectangle
  composited over the unmagnified scene).
- **Focus/caret/cursor tracking:** the magnifier viewport recenters on (a) pointer
  position (already tracked in the seat), (b) the **focused a11y node's bounds** (consume
  the same focus events the reader uses — the a11y tree gives focus geometry across apps),
  and (c) the **text caret** (from the focused widget's `text_selection` bounds). This is
  why magnifier depends on P0/P1 focus events, not just raw cursor.
- **Color transforms:** invert / grayscale / contrast / color-blindness filters as a
  fragment-shader stage on the output. Cheap on the existing wgpu path.
- **Rendering note / risk:** on the **DRM backend** the current path renders via wgpu's GL
  fallback into a GBM buffer (`plan/compositor.md`); adding a post-composite scale stage is
  straightforward in wgpu but must stay within frame budget at high res. The deferred
  zero-copy DMA-BUF path (Phase 5.3) interacts here — coordinate so magnification is a
  shader pass before scanout, not a CPU copy.
- **CRITICAL — keep magnify on the scanout path, NOT the capture path.** This is a known
  pitfall (hit by GNOME and wlroots zoom tools): a naive output-zoom also scales
  screenshots *and screencasts*. qOS streams foreign-surface buffers to remote
  collaborators (openh264, `plan/collaboration.md`) and exposes a ScreenCast portal
  (`shell/src/portal/screencast_pipewire.rs`) — if the magnifier transform is applied
  before those taps, **every remote peer and every screen recording sees the local user's
  magnified viewport.** The magnifier transform MUST be the *final* output-stage transform,
  applied **after** the collab/screencast/`shots` buffers are tapped from the unmagnified
  composed scene. Surface streaming, ScreenCast, and the offscreen renderer all consume the
  **pre-magnify** scene; only the local scanout gets the zoom.
- **Prior art:** GNOME/Mutter (compositor-integrated fullscreen+lens with cursor/focus/caret
  tracking — the reference), KWin "Zoom" effect (Meta+Ctrl+Scroll), and wlroots standalone
  tools `wooz` / `hyprmag` (demonstrate the technique). Follow Mutter's tracking-mode UX.
- **Reduce-motion:** magnifier panning must respect `reduce_motion` (jump vs. smooth-pan).
- **Testing:** offscreen render a known scene, apply the magnifier transform, assert the
  output region/sampling via the existing `shots` offscreen path (byte-identical to live).
  Track-to-focus is tested by injecting focus changes and asserting the viewport center.
  **Add a regression test asserting the screencast/collab buffer is unmagnified** while the
  scanout is magnified — proving the capture/scanout split.
- **Subagent:** W10 (medium; touches the compositor scanout path — coordinate with the
  graphics/DRM owner).

### 2.3 Voice control (offline STT command grammar)

> Goal: hands-free operation — *"open terminal", "next workspace", "click Save",
> "scroll down", "type hello world", "go to heading"* — fully offline, low latency, not
> dictation-grade ASR but command-grade.

- **STT engine recommendation: Vosk** (`alphacep/vosk-api`, **Apache-2.0**) with the small
  (~50 MB) model, via the **`vosk` Rust crate** (`Bear-03/vosk-rs`, safe FFI over the Vosk
  C API). Rationale: **true streaming** recognition with zero-latency partial results, small
  footprint, fully offline, and — critically — supports a **constrained
  grammar/reconfigurable vocabulary** (give Vosk a fixed JSON phrase list = the command
  set), which massively boosts command accuracy and cuts latency vs. open-vocabulary ASR.
  Whisper, by contrast, is architecturally a *chunked/utterance* model (not natively
  streaming) and lacks grammar restriction — wrong shape for snappy command control. For
  optional free-form **dictation** (into the OSK / text fields), offer **whisper.cpp**
  (`whisper-rs` **v0.16, Unlicense/public-domain**; ggml MIT models — `tiny.en` ~75 MB,
  `base` ~142 MB, `small` ~466 MB, quantized variants smaller) as a separate, heavier
  on-demand mode — but the default always-listening command mode is Vosk-with-grammar.
- **Architecture:**
  ```
  mic (PipeWire) ──► VAD ──► Vosk (grammar=command set) ──► intent text
                                          │
                                          ▼
                        command grammar / intent parser
                                          │
                  ┌───────────────────────┼───────────────────────┐
                  ▼                        ▼                        ▼
        shell control socket       AT-SPI actions on          OSK / text injection
        (QOS_TEST_CONTROL:         focused a11y node          (input-method-v2)
        workspace/app/window       ("click Save",             ("type hello world")
        navigation, click_at)      "toggle checkbox")
  ```
- **Command grammar:** a data-driven grammar (FTL-localized phrase → intent) compiled to
  (a) the Vosk grammar word list and (b) an intent dispatcher. Intents fall into three
  sinks: **spatial/shell** commands → the existing `QOS_TEST_CONTROL` JSON control socket
  (it already does `list_targets`/`click_at`/navigation and is the production-grade input
  path); **per-widget** commands → AT-SPI `perform_action` on the focused/named node
  (reuses W3's `perform_action` + the a11y tree's named elements — "click Save" resolves
  the node named "Save"); **dictation** → the OSK's text-injection path (P3).
  - Naming targets by voice reuses the a11y **accessible names** — another payoff of W3.
- **Privacy:** 100% on-device, no network; mic access is gated by the existing portal
  permission model (`plan/security.md`), and there's a visible "listening" indicator +
  push-to-talk and wake-word options.
- **Testing:** the intent parser is unit-tested (phrase → intent, including localized
  phrases) without audio; end-to-end uses pre-recorded WAVs → assert emitted control-socket
  JSON / AT-SPI actions. No live mic in CI.
- **Subagent:** W11 (medium-large; STT integration + grammar + three dispatch sinks).

---

## 3. On-screen keyboard (priority #2; also the future phone keyboard)

> Goal: a first-class on-screen keyboard for touch and pointer use that is also the
> foundation for the eventual qOS **smartphone** keyboard: multi-layout, responsive
> (desktop ↔ phone), a customizable toolbar, **swipe-to-type** (FUTO), and **mini-LLM-grade
> next-word prediction** — fully offline.

### 3.1 Architecture: layer-shell IM client (recommended)

Three options were considered:

1. **A `ui` app rendered as compositor chrome** (inside `ShellOverlayHost`).
2. **A standalone `ui` app as a `wlr-layer-shell` surface** (overlay layer, anchored
   bottom). *(layer-shell is already supported — `shell/src/network/wayland.rs`.)*
3. **A `zwp_virtual_keyboard` / `input-method-v2` client.**

**Recommendation: a hybrid of (2)+(3): the OSK is a dedicated process that is both a
`wlr-layer-shell` surface (for its own UI, built with the `ui` toolkit) and the
`input-method-v2` client (for injecting text into the focused app).** Why:
- Layer-shell gives correct stacking, anchoring, exclusive-zone reservation (apps resize
  so the OSK doesn't cover the focused field), and per-output placement — all already
  modeled in the compositor.
- `input-method-v2` + `text-input-v3` is the **correct, protocol-clean way to inject
  text** and to *know what's focused* (the focused text field advertises content-type,
  surrounding text, and cursor — enabling smart prediction and auto-capitalization).
  `virtual-keyboard-v1` is the lower-level fallback for apps that don't speak
  `text-input-v3` (it injects raw keysyms via a virtual keyboard device).
- Building the OSK UI with `ui` means it inherits theming, high-contrast, i18n, and the
  a11y tree (the OSK keys themselves are accessible — important: the OSK must be operable
  and describable).

**Compositor work (W7):** implement the missing protocols in `shell`:
`zwp_input_method_manager_v2` / `zwp_input_method_v2` (server side — routes to the OSK
process), `zwp_virtual_keyboard_manager_v1` (fallback injection), and `zwp_text_input_v3`
(so focused apps advertise their text context). smithay provides building blocks for
text-input/input-method (verify the version in the `shell` pin and fill gaps). The
per-workspace **bridge socket** must expose these globals to sandboxed apps so a focused
sandboxed app can receive injected text and advertise its text context — coordinate with
`shell/src/compositor/bridge.rs`. Security: text injection is powerful; restrict the
input-method role to the trusted first-party OSK process (not arbitrary clients), and route
injected text only to the *currently focused* surface.

### 3.2 Text injection + cooperation with focus/a11y

- **Native `ui` apps:** must learn to be `text_input_v3` clients (new code in `ui/src/app.rs`
  — handle `Ime` events, advertise content-type/surrounding-text from the focused widget).
  Today `ui` has no IME path; add an `Ime`/`text-input` bridge so injected commit-strings
  splice into the focused widget's automerge `Text` exactly as typed characters do. The
  focused widget's `text_selection` (from W3) provides surrounding-text + cursor to the OSK
  for prediction/auto-cap.
- **Foreign apps:** handled by the compositor's `input-method-v2`/`virtual-keyboard`
  routing — no per-app work.
- **a11y cooperation:** the OSK consumes the **same focus stream** as the reader/magnifier
  to know *where* text is going and to position itself near the caret. The OSK keys are
  themselves in the a11y tree (so a switch-access or eye-gaze user can drive the OSK, and
  the reader can describe it). When the screen reader is active, OSK key presses are echoed.

### 3.3 Swipe-to-type via the FUTO `swipe-library`

Concrete findings (researched 2026-06-30):

- **`swipe-library`** (`gitlab.futo.org/keyboard/swipe-library`): a **C++17** inference
  engine. **License: GPL-3.0-or-later.** Build: **CMake 3.29+**, `make host` for desktop
  Linux. Inference runtime: **ExecuTorch v1.2.0** (vendored as a git submodule; one
  in-tree patch for `EXECUTORCH_OPTIMIZE_SIZE=ON`). Public API in
  `include/swipe_decoder/engine.hpp`:
  - `class SwipeEngine` — **not thread-safe** (serialize calls).
  - `SwipeEngine::Config { encoder_path (required), decoder_path (opt), lm_model_path
    (opt), lm_vocab_path (opt), dictionaries: vector, layout }`.
  - `recognize(x, y, t, num_points) -> vector<word>` where x,y are normalized 0..1 in the
    provided layout's coordinate space and t is ms since swipe start; optional
    `context_words` for the LM.
  - `recognize_multi()` for two-finger; `setMode(Mode)` to swap layout/dict/decoder/LM at
    runtime.
  - Layouts: `default_en_qwerty()` built-in or `load_layout_json()` (letters + normalized
    0..1 positions; duplicate letters not allowed) — see `include/swipe_decoder/layout.hpp`.
  - Dictionaries: "bring-your-own" via `ITrie` vtable
    (`include/swipe_decoder/itrie.h`); `load_trie_simple(path)` loads AOSP `.combined`
    dictionaries (`include/swipe_decoder/trie.hpp`), multiple dicts with per-dict weights.
- **Models** (`huggingface.co/futo-org/futo-swipe`): three **ExecuTorch `.pte`** files,
  ~**10 MB** total, each needs a sibling `metadata.json`; `models/scoring.json` tunes beam
  search. **License: FUTO Model Weights License 1.0** — permissive *with attribution*
  (commercial use + redistribution allowed; must ship the license, add a prominent "powered
  by FUTO Swipe" notice in an About/Settings screen, and note modifications; no use of FUTO
  trademarks beyond attribution; patent-claim and violation termination clauses).
  Components:
  - **Encoder** (`honorable_sturgeon`, 635K params, 2.65 MB): 1-D temporal CNN (TCN),
    **layout/language-agnostic** — produces per-timestep character scores for *any* layout
    you supply. This is the part that generalizes to AZERTY/other layouts.
  - **Decoder** (`magic_macaw`, 304K params, 1.25 MB): **English/QWERTY only** refinement.
  - **ContextLM** (`hungry_jellyfish`, 1.5M params, 6.25 MB): causal DFSMN LM with hash
    embedding for next-word prediction + beam-search reranking.
  - **Dataset** (1M+ English QWERTY swipes, 2024): **MIT** (for training, not needed at
    runtime).
- **Integration plan (W9a):**
  1. **Vendor `swipe-library` as a submodule** and build its `.pte`-loading static lib via
     a Rust **`cxx`** bridge (preferred over raw bindgen for the C++ `SwipeEngine` class) or
     a thin C-ABI shim if `cxx` friction with ExecuTorch is high. A `build.rs` drives the
     library's CMake (the `cc`/`cmake` crate) and links ExecuTorch. Wrap in a
     `swipe-decoder` Rust crate (`apps/osk/swipe-decoder/` or a workspace member).
  2. Ship the three `.pte` models + metadata + `scoring.json` + the AOSP `.combined`
     dictionary in the OS image (`/usr/share/qos/osk/`), with the FUTO attribution surfaced
     in the OSK's About pane (license requirement).
  3. **Decode pipeline:** touch/pointer path → resample/normalize to the active layout's
     0..1 space with ms timestamps → `SwipeEngine::recognize(..)` on a **worker thread**
     (engine is not thread-safe; one engine per worker) → candidate words → merge with the
     prediction module (3.4) → present as a candidate ribbon in the toolbar; top candidate
     committed on swipe-end, alternatives tappable.
  4. **Latency budget:** FUTO reports millisecond-scale inference on low-end devices
     (~2.5M params, 1.4M active). Target **< 50 ms** from swipe-end to candidate ribbon on
     desktop hardware; keep inference off the UI thread; cache the engine + warm it at OSK
     start. Validate on the target phone-class hardware early (P4 risk).
- **GPL note:** `swipe-library` is GPL-3.0. The OSK that links it must be GPL-compatible.
  Keep the OSK (and its swipe-decoder crate) in a **separate process/binary** that links
  the GPL library; the rest of qOS (`ui`, `shell`) is unaffected since they don't link it.
  Document this boundary. ExecuTorch is BSD-style; the FUTO models are the FUTO Model
  License (attribution).

### 3.4 Predictive word suggestions ("mini-LLM quality")

> Compose with swipe candidates; fully offline; phone-class latency.

Layered approach — ship the simple thing, upgrade behind the same interface:

- **Baseline (P4a):** the FUTO **ContextLM** already does next-word prediction + reranking
  given `context_words` — use it directly. Augment with a **classic n-gram** model
  (KenLM-style, or a compact word-trie with frequencies) for instant top-3 next-word
  suggestions and for typo/autocorrect on *tapped* (non-swipe) input. n-gram is tiny,
  microsecond-latency, and a strong baseline.
- **Upgrade (P4b, optional, behind a flag):** a **small on-device transformer** for higher-
  quality prediction, run via **`candle`** (pure-Rust ML, Apache/MIT — keeps the
  Rust toolchain, no extra C runtime) or **`llama.cpp`** (GGUF, via `llama-cpp-rs`) if a
  GGUF model is preferred. Use a **tiny** model (e.g. a distilled ~30–120 M-param model, or
  a purpose-built char/word-LM), **INT4/INT8-quantized**, target **< 30 ms** per
  next-token on phone-class hardware, predicting only the next 1–3 words (not free
  generation). Keep memory < ~150 MB.
- **Composition:** a single `Predictor` trait (`fn candidates(ctx, partial, swipe_cands)
  -> Vec<Suggestion>`) merges (a) swipe candidates from FUTO, (b) n-gram/transformer
  next-word predictions, (c) the user's personal dictionary (learned, stored in the
  per-user CRDT, **never** uploaded). Ranking blends model scores; the toolbar shows the
  top 3–5.
- **Honesty:** "mini-LLM quality" prediction at phone latency is achievable for *next-word*
  with small models, but true LLM-grade contextual rewriting is **not** within a tight
  on-device latency/battery budget — scope this to next-word + autocorrect + short-phrase
  completion, with the heavier transformer strictly optional and benchmarked before
  shipping. Personalization (learning the user's vocabulary) is what makes it *feel*
  smart and is cheaper than a bigger model.
- **Subagent:** W9b.

### 3.5 Layouts, sizing, toolbar (data-driven)

- **Layout format:** a **data-driven JSON/RON** layout spec (one file per
  layout/locale) describing rows, keys (label, output keysym/string, width units,
  modifiers, popup/long-press alternates for accents/emoji), and the normalized key
  positions that **double as the FUTO `load_layout_json()` geometry** (one source of truth
  for rendering *and* swipe decoding). Ship QWERTY, AZERTY, QWERTZ, Dvorak, a symbols
  layer, a numeric/keypad layer, and an **emoji** layer; community layouts are just data
  files. Layout switching uses `SwipeEngine::setMode` for the swipe side.
- **Responsive sizing:** the OSK is a `ui` app, so it uses taffy for layout. A
  `FormFactor` (phone / tablet / desktop) + output size/DPI selects key sizes, row counts,
  and whether to show the number row; min touch-target **≥ 9 mm / ~48 px**. The same code
  serves the desktop OSK and the future phone keyboard — only the form-factor token
  changes. layer-shell exclusive-zone reserves space so the focused app reflows.
- **Customizable toolbar (above the keys):** a configurable strip of actions — candidate
  ribbon, text-selection controls (select word/line/all, cut/copy/paste using the existing
  `arboard` clipboard path), cursor arrows, clipboard history, emoji/GIF, voice-input
  toggle (ties to §2.3), settings. Toolbar contents are **data-driven** (a list of
  action descriptors) and user-reorderable, stored in the per-user CRDT.
- **Subagent:** W8 (OSK app: rendering, layouts, toolbar, responsive, theming, a11y of the
  keys); W7 (compositor protocols) is its dependency.

### 3.6 OSK testing strategy
- **Layout data validation:** every layout file is parsed + checked (no duplicate swipe
  letters per FUTO constraint, all keysyms valid, positions in 0..1) in `cargo test`.
- **Injection:** headless test that the OSK as `input-method-v2` client commits a string
  and the focused (native + a fake foreign) client receives exactly that text — via the
  control socket / a fake wayland client (reuse `shell/tests/compositor_test.rs` harness).
- **Swipe decode:** golden-path test feeding a recorded touch path (x,y,t arrays) through
  the `swipe-decoder` crate and asserting the expected top candidate for a fixed
  model+layout — pins the FFI + model loading. Benchmark harness for the latency budget.
- **Prediction:** deterministic tests of the `Predictor` (context → expected top-k) with a
  fixed n-gram model; the transformer path benchmarked, not asserted on exact output.
- **a11y:** the OSK keys appear in its own a11y tree (Role::Button with key names) — tested
  like any `ui` app.

---

## Cross-cutting concerns

### CC-1: Settings & persistence
A single `A11ySettings` (screen-reader on/off + verbosity + voice + rate; magnifier on/off
+ level + tracking + color-filter; high-contrast + text-scale + reduce-motion;
voice-control on/off + wake-word; OSK enabled + layout + form-factor + toolbar config)
lives in the **per-user settings CRDT**, edited in `apps/settings/`'s new Accessibility
pane, and is mirrored to env vars (`QOS_A11Y_*`) at app-spawn so sandboxed apps inherit it.
The shell already owns theming and broadcasts theme changes — extend that channel.

### CC-2: The a11y bus across the sandbox (W1 — foundational, do first)
Stand up the AT-SPI accessibility bus for the graphical session and **proxy `org.a11y.Bus`
into every per-workspace sandbox** using a filtered `xdg-dbus-proxy` (Flatpak's model),
bound through the per-workspace bridge so a sandboxed app can (a) discover the a11y bus and
(b) register its tree, while the proxy denies everything else. Add the at-spi2 registry +
a11y bus launcher as session user services in `os/`. Without this, **no app is accessible**
— it gates P1's value.

### CC-3: First-boot & the greeter
Accessibility must work **before login**: the greeter (greetd/tuigreet today, or a `ui`
greeter) needs at minimum keyboard nav + the ability to **toggle the screen reader and
high-contrast with a known hotkey at the login screen** (and ideally speak the login
prompt). A blind user must be able to log in unaided on first boot. Sequence a small
greeter-a11y task into P2.

### CC-4: i18n
All reader utterances, OSK labels, and voice-command phrases are FTL keys
(`ui/src/i18n.rs`), localized per locale. TTS voice and STT model follow the locale.

### CC-5: Reduce-motion
The toolkit's animation system (`ui/src/animation/`, `needs_animation_frame`,
`Motion`/`SpringPresets`) must consult `reduce_motion` and degrade to instant transitions
— a real vestibular-accessibility need, and cheap once the setting exists (W2).

---

## Execution: large subagents

Each workstream below is sized for a large subagent. Dependencies reference other
workstreams. "Size" is rough implementation effort.

| ID | Workstream | Scope (one-liner) | Depends on | Size |
|----|------------|-------------------|-----------|------|
| **W0** | a11y semantic model in `ui` | Add `ui/src/a11y.rs` (`Role`, `Action`, `A11yState`, `AccessNode`, `Live`) + the defaulted `Widget` methods (`accessibility`, `perform_action`, `a11y_id`). No behavior; everything compiles. | — | S–M |
| **W1** | a11y D-Bus bus across sandbox | at-spi2 registry + a11y bus as session services in `os/`; filtered `xdg-dbus-proxy` of `org.a11y.Bus` into each per-workspace sandbox via the bridge; tests prove a sandboxed app can register and an external client can read it. | sandbox/bridge (exists) | M–L |
| **W2** | high-contrast + a11y settings + reduce-motion | High-contrast `Theme` presets + token additions (contrast mode, text-scale, focus-ring, reduce-motion); `A11ySettings` CRDT + env plumbing + Settings pane; animation honors reduce-motion; WCAG contrast tests. | — | S–M |
| **W3** | implement `accessibility()` on all widgets | Annotate ~55 widgets in `ui/src/widget/*` with roles/names/states/values/actions + `perform_action`; collapse presentational containers; per-widget snapshot tests. | W0 | L (broad) |
| **W4** | AccessKit tree + AT-SPI export | `ui/src/a11y_tree.rs` builds + diffs an AccessKit `TreeUpdate` from the widget tree each frame; wire `accesskit_winit`/`accesskit_unix` in `App`; focus mapping; AT-SPI round-trip test with the `atspi` client crate. | W0, W3, W1 | L |
| **W5** | TTS plumbing (speech-dispatcher) | SSIP client (`ssip-client`) wrapper with priorities/interrupts; ship speechd + espeak-ng + Piper modules in `os/`; voice/rate from settings; default espeak-on-first-boot. | W2 (settings) | M |
| **W6** | built-in screen reader | Reader process built on the `atspi` crate (MIT/Apache), **forking/referencing Odilia** (MIT/Apache) for the async event pipeline: AT-SPI consumer + qOS spatial-enrichment socket; focus/caret/live-region narration, reading order, "say all", interrupts; caret browsing + structural (heading/landmark) nav in `App`; greeter hotkeys. | W4, W5 | L |
| **W7** | compositor IM protocols | Implement `input-method-v2`, `virtual-keyboard-v1`, `text-input-v3` in `shell`; expose via the per-workspace bridge with first-party-only input-method role; route to focused surface; tests with a fake client. | compositor (exists) | L |
| **W8** | OSK app (base) | `apps/osk/` layer-shell + IM client built on `ui`: data-driven layouts (QWERTY/AZERTY/QWERTZ/Dvorak/symbols/numeric/emoji), responsive form-factors, customizable toolbar (selection/clipboard/cursor/voice), exclusive-zone; OSK keys accessible; injection tests. | W7; W2 (theming) | L |
| **W9a** | FUTO swipe FFI | `swipe-decoder` Rust crate: vendor `swipe-library` (GPL) as submodule, build ExecuTorch via `build.rs`, `cxx` bridge to `SwipeEngine`, ship `.pte` models + dicts + FUTO attribution; resample/normalize touch path; worker-thread decode; latency benchmark. | W8 | L (high external risk) |
| **W9b** | word prediction | `Predictor` trait merging FUTO ContextLM + n-gram (baseline) + optional `candle`/`llama.cpp` small transformer (flagged) + personal-dictionary learning in the CRDT; ranking; benchmarks. | W8 (W9a for swipe-cand merge) | M–L |
| **W10** | magnifier | Compositor output magnify (full-screen + lens, 2×–16×) with focus/caret/cursor tracking (consumes focus events from W4) + color filters; reduce-motion panning; offscreen tests. | W2; benefits from W4 (focus geometry); coordinate w/ DRM owner | M–L |
| **W11** | voice control | Vosk STT + constrained command grammar (FTL phrases → intents); three dispatch sinks (control socket / AT-SPI `perform_action` / OSK injection); push-to-talk + indicator; PipeWire mic via portal; intent unit tests + WAV e2e. | W3 (named actions), W7/W8 (dictation), control socket (exists) | M–L |

**Suggested wave scheduling** (maximize parallelism):
- **Wave A (parallel):** W0, W1, W2 — pure foundation, no interdeps.
- **Wave B (parallel after A):** W3 (needs W0), W7 (independent), W10 (needs W2).
- **Wave C:** W4 (needs W0+W3+W1), W8 (needs W7+W2), W5 (needs W2).
- **Wave D:** W6 (needs W4+W5), W9a/W9b (need W8), W11 (needs W3+W7/W8).

Critical path to first blind-usable build: **W0 → W1 → W3 → W4 → W5 → W6.**
Critical path to first OSK build: **W7 → W8** (swipe/prediction follow).

---

## Risk register (honest)

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|-----------|
| AT-SPI conformance long tail (tables, rich-text editor, comboboxes read wrong) | High | High | Dogfood with real screen-reader users; build a shared text-a11y helper for the CRDT block editor; budget months, not weeks, for "Orca-perfect". |
| a11y bus proxy into sandbox mis-scoped (nothing accessible, or sandbox hole) | Med | High | Model on Flatpak's `xdg-dbus-proxy --filter`; least-privilege; W1 tests assert both "app can register" and "proxy denies non-a11y traffic". |
| FUTO `swipe-library` GPL-3.0 ↔ rest-of-OS licensing | Med | Med | Isolate in a separate OSK binary that links GPL; rest of qOS doesn't link it; document the boundary; FUTO models need visible attribution (settings/About). |
| ExecuTorch + `cxx` build friction in the Rust/distrobox build | Med | Med | Spike W9a early as a standalone crate; C-ABI shim fallback if `cxx` fights ExecuTorch; pin ExecuTorch v1.2.0; vendor as submodule. |
| On-device prediction latency/battery on phone-class HW | Med | Med | Ship n-gram baseline first; transformer optional + benchmarked on target HW before enabling; cap model size + quantize. |
| Magnifier within frame budget on DRM/GL-fallback scanout | Med | Med | Shader post-pass before scanout, not CPU copy; coordinate with the deferred DMA-BUF Phase 5.3; test at high res. |
| **Magnifier scales collab streams / screenshots** (zoom leaks to remote peers & ScreenCast) | Med | High | Apply magnify as the *final* scanout transform, **after** the openh264/ScreenCast/`shots` taps consume the unmagnified scene; regression test the capture/scanout split. |
| `accesskit_winit` lagging the winit pin | **Low (resolved)** | Med | `accesskit_winit` **v0.33.1 targets winit 0.30.x** = the `ui` pin → adopt directly for apps. For the compositor, drive `accesskit_unix` directly (window-agnostic, 3-handler contract) feeding geometry via `set_root_window_bounds`. |
| Touch input not yet handled in `ui` (winit) blocks touch-OSK/swipe | Med | Med | Add `WindowEvent::Touch` handling in `App`; smithay seat already declares `TouchFocus`; small but on the OSK critical path. |
| Two screen readers (Orca + built-in) talking over each other | Low | Med | Mutually exclusive by default (one active); both read the same tree; settings enforce single active reader. |

---

## Licenses summary (offline, ship-in-image)

| Component | Version | License | Notes |
|-----------|---------|---------|-------|
| `accesskit` (core schema) | 0.24.1 | MIT OR Apache-2.0 | Linked directly. |
| `accesskit_unix` (AT-SPI provider) | 0.22.0 | MIT OR Apache-2.0 | Pure-Rust over zbus 5.5; window-agnostic. |
| `accesskit_winit` | 0.33.1 | Apache-2.0 | Targets winit 0.30.x = `ui` pin. |
| `atspi` (Odilia project; AT-SPI client) | current | MIT OR Apache-2.0 | Test client + built-in reader (consumer). |
| Odilia (reader reference/fork base) | beta | MIT OR Apache-2.0 | "Not production ready" — skeleton, not turnkey. |
| speech-dispatcher (`speechd`) | — | GPL | Reached over SSIP socket (not linked) → no entanglement. |
| `ssip-client` (Rust SSIP) | current | MIT | Linked; talks SSIP. Odilia uses an async fork. |
| espeak-ng | — | GPL-3.0 | Separate process via speechd → no entanglement; ship in image. |
| Piper (active = `OHF-Voice/piper1-gpl`) | 1.4.2 | **GPL-3.0** | GPL via embedded espeak-ng G2P; separate process via speechd → no entanglement. (rhasspy/piper MIT archived Oct 2025.) |
| `piper-plus` (MIT escape hatch) | current | **MIT** | Drops espeak-ng; 8 langs; only if a daemon-less in-process path is needed. |
| Vosk (`vosk-api` / `vosk` crate `Bear-03/vosk-rs`) | current | Apache-2.0 | STT command grammar; ~50 MB model offline; streaming + grammar. |
| whisper.cpp / `whisper-rs` | 0.16 | Unlicense (PD); ggml models MIT | Optional dictation mode only. |
| FUTO `swipe-library` | — | **GPL-3.0-or-later** | Isolate in the OSK binary; don't link from `ui`/`shell`. |
| FUTO swipe models (`.pte`) | — | **FUTO Model Weights License 1.0** | Permissive *with attribution*: ship license + visible "powered by FUTO Swipe" notice; commercial + redistribution OK. |
| FUTO swipe dataset | — | MIT | Training only; not shipped at runtime. |
| ExecuTorch | 1.2.0 | BSD-style | Bundled to load `.pte`. |
| `candle` (optional prediction) | current | MIT/Apache-2.0 | Pure-Rust small-transformer path. |

> All choices above are **offline-capable** and **license-compatible** with shipping in the
> Fedora-Bootc OS image, with the one caveat that the GPL swipe-library must stay in its own
> binary and the FUTO models require attribution in the OSK About screen.

---

## Where to read first (for implementers)
- `ui/src/widget/mod.rs` — the `Widget` trait (extend here: W0/W3).
- `ui/src/app.rs` — event loop, `focusable_order`, `navigate_focus`, `UserEvent` injection
  (W4/W6/voice/OSK-IME-in-`ui`).
- `ui/src/theme.rs`, `ui/src/tokens/` — high-contrast (W2).
- `ui/src/i18n.rs` — localized utterances/labels.
- `shell/src/network/wayland.rs` (layer-shell), `shell/src/compositor/{frame.rs,state.rs,
  bridge.rs,test_control.rs}` — OSK protocols (W7), magnifier (W10), voice dispatch (W11),
  sandbox a11y bus (W1).
- `plan/security.md` — the sandbox/D-Bus constraint (W1).
- `os/usr/lib/systemd/` + `os/usr/share/dbus-1/services/` — new a11y session services.
- FUTO: `gitlab.futo.org/keyboard/swipe-library` (`include/swipe_decoder/engine.hpp`),
  `huggingface.co/futo-org/futo-swipe` (W9a).
```
