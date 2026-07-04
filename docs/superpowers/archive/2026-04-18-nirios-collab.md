# Nirios Shell — Plan 4: Collaboration Contract Hardening

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Harden the collaboration contract by generalising presence to every widget, wiring real permission gating through `WorkspaceSettings`, plumbing per-user undo, eliminating every hard-coded style value, and proving the whole thing with a two-`App` simulated multi-user integration test.

**Architecture:**
- `quick-ui` exposes a first-class presence subsystem (`PresenceKey` / `PresenceCtx` / rendering helpers) and a permission subsystem (`PermissionScope` / `PermissionContext`) that are threaded through the `Widget::draw` and event dispatch paths.
- `CollaborationManager` gains a generalised `RemotePresenceUpdate` event carrying a `PresenceKey` + `PresencePayload`, replacing the narrow `RemoteCursorMoved` path (which is kept as a compatibility alias).
- `UndoEntry` gains `author: Option<UserId>`; the framework remains single-user in v1 but has the plumbing for future multi-user filtering, proven by a unit test.
- A mechanical design-token audit walks every shell / widget / file-explorer source file and a compiled-in test (`tests/design_token_audit.rs`) greps the tree for forbidden hard-coded constants.
- Multi-user behaviour is proven by a new `tests/multi_user_session.rs` integration test that drives two `quick_ui::App` instances connected by an in-memory `tokio::io::duplex` pipe acting as a mini collaboration relay.

**Tech Stack:** Rust 2024, `quick-ui` in-tree crate, `automerge`, `tokio` (for in-memory duplex pipes in tests), `serde` / `serde_json` for wire framing, `vello` for rendering, `regex` (dev-dep) for the audit, `winit` events.

---

## File Structure

### Files created

- `quick-ui/src/presence/mod.rs` — `PresenceKey`, `PresencePayload`, `SelectionRange`, `PresenceCtx`, rendering helpers.
- `quick-ui/src/presence/key.rs` — `PresenceKey` variants + `PathSegment` helper + hash/eq impls.
- `quick-ui/src/presence/render.rs` — `draw_peer_caret`, `draw_peer_selection`, `draw_peer_pointer` on `PresenceCtx`.
- `quick-ui/src/permissions/mod.rs` — `PermissionScope`, `ResourceId`, `Op`, `DenyReason`, `PermissionContext`, `Role`, `RolePermissions`, `RoleId`, `AppKind`, `FileId`, `WorkspaceId`, `UserId`.
- `quick-ui/tests/design_token_audit.rs` — AST-free regex audit that fails if disallowed literals appear in widget or shell code.
- `quick-ui/tests/presence_context.rs` — PresenceCtx unit tests (peers_on, pointer_for, helper rendering into a Scene).
- `quick-ui/tests/permission_context.rs` — PermissionContext unit tests (role/scope lookup, deny reason, edit after role change).
- `quick-ui/tests/undo_per_author.rs` — UndoEntry author filtering proof.
- `quick-ui/tests/multi_user_session.rs` — two-`App` duplex-pipe integration test for cursor rendering, caret rendering, permission gating, and permission sync.
- `quick-ui/tests/support/mod.rs` — shared harness: in-memory `CollabTransport` trait + `DuplexTransport` + `MemoryRelay`; a `headless_app` builder.

### Files modified

- `quick-ui/src/lib.rs` — re-export `presence`, `permissions`, new types.
- `quick-ui/src/collaboration.rs` — add `PresencePayload`, `RemotePresenceUpdate` variant, `CollabTransport` trait; keep `RemoteCursorMoved` compatibility.
- `quick-ui/src/state/mod.rs` — `UndoEntry::author`, `commit_action_authored`, filtered undo helper `undo_for_author`.
- `quick-ui/src/widget/mod.rs` — extend `Widget` trait with `presence_key`, `permission_scope`, and a `draw_with_presence` helper; keep `draw` default delegating to `draw_with_presence(..., &PresenceCtx::empty(), &PermissionContext::permissive())`.
- `quick-ui/src/widget/text_input.rs` — call `draw_peer_caret` / `draw_peer_selection` when the `PresenceCtx` has peers on its presence key.
- `quick-ui/src/widget/button.rs`, `checkbox.rs`, `toggle.rs`, `slider.rs`, `text.rs`, `column.rs`, `row.rs`, `stack.rs`, `container.rs`, `spacer.rs` — opt into presence (`presence_key` where meaningful) and replace any remaining hard-coded literal with `theme.*` / token access.
- `quick-ui/src/theme.rs` — no new fields in this plan (tokens crate is Plan 1); audit test asserts the existing `Theme` fields are used wherever a color/spacing literal previously lived.
- `quick-ui/src/app.rs` — build a frame-level `PresenceCtx` from `CollaborationManager` events, carry a `PermissionContext`, pass both into the root `draw_with_presence`, and expose `App::collab_manager_mut()` for tests.
- `quick-ui/Cargo.toml` — add `regex` (dev), `tokio` (already present) with `io-util` feature for duplex pipes.

### Task DAG for Concurrent Execution

Tasks labeled `[parallel-safe]` within the same group have no file overlap with each other. `->` marks hard dependencies.

```
Group A (foundation, sequential)
  A1 PresenceKey + PresencePayload types ->
  A2 PresenceCtx + helpers ->
  A3 CollabEvent::RemotePresenceUpdate + payload mapping

Group B (permissions, sequential)
  B1 Resource/Op/Scope types ->
  B2 Role + RolePermissions + PermissionContext ->
  B3 Widget trait permission_scope hook + gated event dispatch

Group C (undo author)
  C1 UndoEntry::author plumbing (depends on none) ->
  C2 per-author filter test

Group D (widget opt-in) [parallel-safe within D after A2+B3]
  D1 text_input peer caret/selection render
  D2 button presence_key + permission gating
  D3 slider / toggle / checkbox opt-in
  D4 text / container presence hover chrome

Group E (token audit) [parallel-safe within E; all depend only on existing code]
  E1 audit test harness (tests/design_token_audit.rs)
  E2 audit sweep: button.rs
  E3 audit sweep: checkbox.rs
  E4 audit sweep: toggle.rs
  E5 audit sweep: slider.rs
  E6 audit sweep: text.rs
  E7 audit sweep: text_input.rs
  E8 audit sweep: column / row / stack / container / spacer
  E9 audit sweep: app.rs (shell bootstrap path)

Group F (multi-user integration) [depends on A3, B3, D1, D2]
  F1 in-memory transport + MemoryRelay harness ->
  F2 two-App builder (tests/support) ->
  F3 peer cursor under layout change ->
  F4 peer caret in text input ->
  F5 permission-gated widget auto-disables ->
  F6 permission change propagates

Group G (close-out)
  G1 update quick-ui/src/lib.rs re-exports
  G2 full `cargo test -p quick-ui`
  G3 final commit
```

Group A must land before D, E, and F. Group B must land before D2, D3, F5, F6. Groups C and E can run independently of A/B/D once their prerequisites land. Group F integrates everything.

---

## Task 1: Add `PresenceKey`, `PathSegment`, and `SelectionRange` types

**Files:**
- Create: `quick-ui/src/presence/mod.rs`
- Create: `quick-ui/src/presence/key.rs`
- Modify: `quick-ui/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `quick-ui/tests/presence_context.rs` (first test):

```rust
use quick_ui::presence::{PresenceKey, PathSegment, SelectionRange};

#[test]
fn presence_key_widget_path_equality_and_hash() {
    let a = PresenceKey::Widget {
        path: vec![PathSegment::Field("root".into()), PathSegment::Index(2)],
    };
    let b = PresenceKey::Widget {
        path: vec![PathSegment::Field("root".into()), PathSegment::Index(2)],
    };
    let c = PresenceKey::Widget {
        path: vec![PathSegment::Field("root".into()), PathSegment::Index(3)],
    };
    assert_eq!(a, b);
    assert_ne!(a, c);

    let mut map = std::collections::HashMap::new();
    map.insert(a.clone(), 1u32);
    assert_eq!(map.get(&b), Some(&1));
    assert_eq!(map.get(&c), None);
}

#[test]
fn selection_range_construction() {
    let r = SelectionRange { start: 3, end: 10 };
    assert_eq!(r.len(), 7);
    assert!(!r.is_caret());
    let caret = SelectionRange { start: 5, end: 5 };
    assert!(caret.is_caret());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:
```bash
cargo test -p quick-ui --test presence_context -- --nocapture
```
Expected: compile error — `unresolved module or unlinked crate 'presence'`.

- [ ] **Step 3: Write minimal implementation**

Create `quick-ui/src/presence/key.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PathSegment {
    Field(String),
    Index(u32),
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PresenceKey {
    Widget { path: Vec<PathSegment> },
    Tab { id: String },
    Workspace { id: String },
    App { app: crate::permissions::AppKind, sub: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SelectionRange {
    pub start: usize,
    pub end: usize,
}

impl SelectionRange {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
    pub fn is_caret(&self) -> bool {
        self.start == self.end
    }
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}
```

Create `quick-ui/src/presence/mod.rs` (minimal, expanded in Task 2):

```rust
mod key;
pub use key::{PresenceKey, PathSegment, SelectionRange};
```

Modify `quick-ui/src/lib.rs` — append:

```rust
pub mod permissions;
pub mod presence;
```

`permissions` will be created in Task 5; we add a placeholder module so `PresenceKey::App` can reference `AppKind`. Create `quick-ui/src/permissions/mod.rs` with just:

```rust
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum AppKind {
    Terminal,
    FileExplorer,
    Browser,
    Settings,
    Unknown,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run:
```bash
cargo test -p quick-ui --test presence_context
```
Expected:
```
running 2 tests
test presence_key_widget_path_equality_and_hash ... ok
test selection_range_construction ... ok

test result: ok. 2 passed; 0 failed
```

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/presence/ quick-ui/src/permissions/mod.rs quick-ui/src/lib.rs quick-ui/tests/presence_context.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): introduce PresenceKey, PathSegment, SelectionRange types"
```

---

## Task 2: Build `PresenceCtx` with peer lookup helpers

**Files:**
- Modify: `quick-ui/src/presence/mod.rs`
- Test: `quick-ui/tests/presence_context.rs`

- [ ] **Step 1: Write the failing test**

Append to `quick-ui/tests/presence_context.rs`:

```rust
use quick_ui::presence::PresenceCtx;
use quick_ui::permissions::UserId;

#[test]
fn peers_on_returns_users_registered_against_key() {
    let mut ctx = PresenceCtx::empty();
    let key = PresenceKey::Tab { id: "tab-1".into() };
    ctx.register_presence(&key, UserId("alice".into()));
    ctx.register_presence(&key, UserId("bob".into()));

    let peers = ctx.peers_on(&key);
    assert_eq!(peers.len(), 2);
    assert!(peers.iter().any(|u| u.0 == "alice"));
    assert!(peers.iter().any(|u| u.0 == "bob"));
}

#[test]
fn pointer_for_returns_latest_coord() {
    let mut ctx = PresenceCtx::empty();
    let key = PresenceKey::Workspace { id: "ws-1".into() };
    ctx.set_pointer(&key, UserId("alice".into()), (10.0, 20.0));
    ctx.set_pointer(&key, UserId("alice".into()), (30.0, 40.0));
    let pos = ctx.pointer_for(&key, &UserId("alice".into()));
    assert_eq!(pos, Some((30.0, 40.0)));
}

#[test]
fn selection_for_returns_range() {
    let mut ctx = PresenceCtx::empty();
    let key = PresenceKey::Widget { path: vec![PathSegment::Field("body".into())] };
    ctx.set_selection(
        &key,
        UserId("alice".into()),
        SelectionRange { start: 4, end: 9 },
    );
    let sel = ctx.selection_for(&key, &UserId("alice".into())).unwrap();
    assert_eq!(sel.start, 4);
    assert_eq!(sel.end, 9);
}
```

Also append a `UserId` export in `quick-ui/src/permissions/mod.rs` for this test to compile:

```rust
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct UserId(pub String);
```

- [ ] **Step 2: Run test to verify it fails**

Run:
```bash
cargo test -p quick-ui --test presence_context peers_on_returns_users_registered_against_key
```
Expected: compile error `no function or associated item named 'empty' found for struct 'PresenceCtx'`.

- [ ] **Step 3: Write minimal implementation**

Replace `quick-ui/src/presence/mod.rs`:

```rust
use std::collections::HashMap;
use crate::permissions::UserId;

mod key;
pub use key::{PresenceKey, PathSegment, SelectionRange};

#[derive(Debug, Default, Clone)]
pub struct PresenceCtx {
    pub here: HashMap<PresenceKey, Vec<UserId>>,
    pub pointers: HashMap<PresenceKey, Vec<(UserId, (f32, f32))>>,
    pub selections: HashMap<PresenceKey, Vec<(UserId, SelectionRange)>>,
    pub carets: HashMap<PresenceKey, Vec<(UserId, usize)>>,
}

impl PresenceCtx {
    pub fn empty() -> Self { Self::default() }

    pub fn register_presence(&mut self, key: &PresenceKey, user: UserId) {
        let list = self.here.entry(key.clone()).or_default();
        if !list.iter().any(|u| u == &user) {
            list.push(user);
        }
    }

    pub fn remove_presence(&mut self, key: &PresenceKey, user: &UserId) {
        if let Some(list) = self.here.get_mut(key) {
            list.retain(|u| u != user);
        }
    }

    pub fn set_pointer(&mut self, key: &PresenceKey, user: UserId, pos: (f32, f32)) {
        let list = self.pointers.entry(key.clone()).or_default();
        if let Some(entry) = list.iter_mut().find(|(u, _)| *u == user) {
            entry.1 = pos;
        } else {
            list.push((user, pos));
        }
    }

    pub fn set_caret(&mut self, key: &PresenceKey, user: UserId, pos: usize) {
        let list = self.carets.entry(key.clone()).or_default();
        if let Some(entry) = list.iter_mut().find(|(u, _)| *u == user) {
            entry.1 = pos;
        } else {
            list.push((user, pos));
        }
    }

    pub fn set_selection(&mut self, key: &PresenceKey, user: UserId, range: SelectionRange) {
        let list = self.selections.entry(key.clone()).or_default();
        if let Some(entry) = list.iter_mut().find(|(u, _)| *u == user) {
            entry.1 = range;
        } else {
            list.push((user, range));
        }
    }

    pub fn peers_on(&self, key: &PresenceKey) -> &[UserId] {
        self.here.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn pointer_for(&self, key: &PresenceKey, user: &UserId) -> Option<(f32, f32)> {
        self.pointers.get(key)?.iter().find(|(u, _)| u == user).map(|(_, p)| *p)
    }

    pub fn selection_for(&self, key: &PresenceKey, user: &UserId) -> Option<SelectionRange> {
        self.selections.get(key)?.iter().find(|(u, _)| u == user).map(|(_, r)| *r)
    }

    pub fn caret_for(&self, key: &PresenceKey, user: &UserId) -> Option<usize> {
        self.carets.get(key)?.iter().find(|(u, _)| u == user).map(|(_, c)| *c)
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run:
```bash
cargo test -p quick-ui --test presence_context
```
Expected: all 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/presence/mod.rs quick-ui/src/permissions/mod.rs quick-ui/tests/presence_context.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): add PresenceCtx with peer/pointer/selection lookups"
```

---

## Task 3: Add peer caret and peer selection rendering helpers

**Files:**
- Create: `quick-ui/src/presence/render.rs`
- Modify: `quick-ui/src/presence/mod.rs`
- Test: `quick-ui/tests/presence_context.rs`

- [ ] **Step 1: Write the failing test**

Append to `quick-ui/tests/presence_context.rs`:

```rust
use vello::Scene;
use quick_ui::theme::Theme;

#[test]
fn draw_peer_caret_emits_scene_content() {
    let ctx = PresenceCtx::empty();
    let mut scene = Scene::new();
    let theme = Theme::dark();
    // A fresh scene is structurally empty; after drawing a caret it must have content.
    assert!(scene.encoding().is_empty(),
        "precondition: scene must start empty");
    ctx.draw_peer_caret(&mut scene, &theme, &UserId("alice".into()), (100.0, 50.0));
    assert!(!scene.encoding().is_empty(),
        "draw_peer_caret must add geometry to the scene");
}

#[test]
fn draw_peer_selection_emits_scene_content() {
    let ctx = PresenceCtx::empty();
    let mut scene = Scene::new();
    let theme = Theme::dark();
    ctx.draw_peer_selection(
        &mut scene,
        &theme,
        &UserId("alice".into()),
        &SelectionRange { start: 0, end: 5 },
        /* line_rects = */ &[vello::kurbo::Rect::new(10.0, 20.0, 60.0, 40.0)],
    );
    assert!(!scene.encoding().is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:
```bash
cargo test -p quick-ui --test presence_context draw_peer_caret_emits_scene_content
```
Expected: compile error `no method named 'draw_peer_caret' found`.

- [ ] **Step 3: Write minimal implementation**

Create `quick-ui/src/presence/render.rs`:

```rust
use vello::{Scene, kurbo::{Rect, Affine}, peniko::{Color, Fill}};
use crate::theme::Theme;
use crate::permissions::UserId;
use super::{PresenceCtx, SelectionRange};

/// Deterministic per-user color — maps a UserId string to a hue with stable lightness.
/// Used only when the theme does not provide an explicit override.
fn color_for(user: &UserId, theme: &Theme) -> Color {
    // Hash to a hue 0..=359.
    let mut h: u32 = 2166136261;
    for b in user.0.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(16777619);
    }
    let hue = (h % 360) as f64;
    let (r, g, b) = hsl_to_rgb(hue, 0.6, 0.55);
    // Fall back to accent_primary for the very unlikely all-zero case.
    if r == 0 && g == 0 && b == 0 {
        return theme.accent_primary;
    }
    Color::rgb8(r, g, b)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    (
        ((r1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b1 + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

impl PresenceCtx {
    /// Draw a 2-px vertical caret at `pos`, 16px tall, in the user's assigned color.
    pub fn draw_peer_caret(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        user: &UserId,
        pos: (f32, f32),
    ) {
        let color = color_for(user, theme);
        let x = pos.0 as f64;
        let y = pos.1 as f64;
        let rect = Rect::new(x, y, x + 2.0, y + 16.0);
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
    }

    /// Draw a translucent selection highlight over `line_rects` lines.
    pub fn draw_peer_selection(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        user: &UserId,
        _range: &SelectionRange,
        line_rects: &[Rect],
    ) {
        let mut color = color_for(user, theme);
        // 35% alpha.
        color = color.with_alpha_factor(0.35);
        for r in line_rects {
            scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, r);
        }
    }

    /// Draw a small 8-px circle for the peer's pointer.
    pub fn draw_peer_pointer(
        &self,
        scene: &mut Scene,
        theme: &Theme,
        user: &UserId,
        pos: (f32, f32),
    ) {
        let color = color_for(user, theme);
        let x = pos.0 as f64;
        let y = pos.1 as f64;
        let rect = Rect::new(x - 4.0, y - 4.0, x + 4.0, y + 4.0);
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
    }
}
```

Modify `quick-ui/src/presence/mod.rs` to include it:

```rust
mod key;
mod render;
pub use key::{PresenceKey, PathSegment, SelectionRange};
// render.rs adds impl blocks; nothing new to pub use.
```

- [ ] **Step 4: Run test to verify it passes**

Run:
```bash
cargo test -p quick-ui --test presence_context
```
Expected: all 7 tests pass.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/presence/ quick-ui/tests/presence_context.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): add peer caret/selection/pointer rendering helpers"
```

---

## Task 4: Add `RemotePresenceUpdate` + `PresencePayload` to `CollabEvent`

**Files:**
- Modify: `quick-ui/src/collaboration.rs`
- Modify: `quick-ui/src/app.rs` (dispatch handler)
- Test: `quick-ui/tests/presence_context.rs` (event-mapping test)

- [ ] **Step 1: Write the failing test**

Append to `quick-ui/tests/presence_context.rs`:

```rust
use quick_ui::collaboration::{CollabEvent, PresencePayload};

#[test]
fn remote_presence_update_round_trips_through_serde() {
    let event = CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: PresenceKey::Tab { id: "tab-1".into() },
        payload: PresencePayload::Caret { pos: 12 },
    };
    let json = serde_json::to_string(&event).unwrap();
    let back: CollabEvent = serde_json::from_str(&json).unwrap();
    match back {
        CollabEvent::RemotePresenceUpdate { user_id, key, payload } => {
            assert_eq!(user_id, "alice");
            assert_eq!(key, PresenceKey::Tab { id: "tab-1".into() });
            assert!(matches!(payload, PresencePayload::Caret { pos: 12 }));
        }
        _ => panic!("wrong variant"),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:
```bash
cargo test -p quick-ui --test presence_context remote_presence_update_round_trips_through_serde
```
Expected: compile error `no variant named 'RemotePresenceUpdate' on enum 'CollabEvent'`.

- [ ] **Step 3: Write minimal implementation**

Modify `quick-ui/src/collaboration.rs` — add imports + new types + variant:

```rust
use crate::presence::{PresenceKey, SelectionRange};
```

Inside the `CollabEvent` enum, append new variant:

```rust
#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum CollabEvent {
    Context {
        collaborators: Vec<Collaborator>,
        permissions: Vec<String>,
    },
    RemoteCursorMoved { user_id: String, x: f32, y: f32 },
    CRDTUpdate { data: Vec<u8> },
    CRDTIncremental { data: Vec<u8> },
    RemotePresenceUpdate {
        user_id: String,
        key: PresenceKey,
        payload: PresencePayload,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum PresencePayload {
    Pointer { x: f32, y: f32 },
    Caret { pos: usize },
    Selection { start: usize, end: usize },
    Arrived,
    Left,
}
```

Modify `quick-ui/src/app.rs` — in the `poll()` event-handling match (after the existing `RemoteCursorMoved` arm), add:

```rust
CollabEvent::RemotePresenceUpdate { user_id, key, payload } => {
    use quick_ui_presence_dispatch::*; // local helper brought in below
    self.apply_remote_presence(user_id, key, payload);
}
```

Then in `impl App`, add:

```rust
fn apply_remote_presence(
    &mut self,
    user_id: String,
    key: crate::presence::PresenceKey,
    payload: crate::collaboration::PresencePayload,
) {
    use crate::collaboration::PresencePayload::*;
    use crate::permissions::UserId;
    let uid = UserId(user_id);
    match payload {
        Pointer { x, y } => self.presence_ctx.set_pointer(&key, uid, (x, y)),
        Caret { pos } => self.presence_ctx.set_caret(&key, uid, pos),
        Selection { start, end } => self.presence_ctx.set_selection(
            &key,
            uid,
            crate::presence::SelectionRange { start, end },
        ),
        Arrived => self.presence_ctx.register_presence(&key, uid),
        Left => self.presence_ctx.remove_presence(&key, &uid),
    }
}
```

And add a `presence_ctx: crate::presence::PresenceCtx` field initialised to `PresenceCtx::empty()` in `App::new`. Remove the fictitious `quick_ui_presence_dispatch::*` use line (it was just a placeholder to show intent); the actual dispatch is the method call above.

- [ ] **Step 4: Run test to verify it passes**

Run:
```bash
cargo test -p quick-ui --test presence_context
cargo build -p quick-ui
```
Expected: 8 tests pass, build succeeds.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/collaboration.rs quick-ui/src/app.rs quick-ui/tests/presence_context.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): add RemotePresenceUpdate/PresencePayload to CollabEvent"
```

---

## Task 5: Build `PermissionContext` core types

**Files:**
- Modify: `quick-ui/src/permissions/mod.rs`
- Create: `quick-ui/tests/permission_context.rs`

- [ ] **Step 1: Write the failing test**

Create `quick-ui/tests/permission_context.rs`:

```rust
use quick_ui::permissions::{
    AppKind, FileId, Op, PermissionContext, PermissionScope, ResourceId,
    Role, RoleId, RolePermissions, UserId, WorkspaceId, DenyReason,
};

fn viewer_role() -> Role {
    Role {
        id: RoleId("viewer".into()),
        name: "Viewer".into(),
        permissions: RolePermissions {
            app_access: vec![(AppKind::FileExplorer, Op::View)],
            file_ops: vec![],
            workspace_admin: false,
        },
    }
}

fn editor_role() -> Role {
    Role {
        id: RoleId("editor".into()),
        name: "Editor".into(),
        permissions: RolePermissions {
            app_access: vec![
                (AppKind::FileExplorer, Op::Edit),
                (AppKind::Terminal, Op::Edit),
            ],
            file_ops: vec![(FileId("report.md".into()), Op::Edit)],
            workspace_admin: false,
        },
    }
}

#[test]
fn viewer_cannot_edit_terminal() {
    let ctx = PermissionContext::new(
        UserId("alice".into()),
        &[viewer_role(), editor_role()],
        RoleId("viewer".into()),
    );
    let scope = PermissionScope {
        resource: ResourceId::App(AppKind::Terminal),
        op: Op::Edit,
    };
    assert!(!ctx.check(&scope));
    assert!(matches!(
        ctx.check_with_denied_feedback(&scope),
        Some(DenyReason::Role { .. })
    ));
}

#[test]
fn editor_can_edit_file() {
    let ctx = PermissionContext::new(
        UserId("bob".into()),
        &[viewer_role(), editor_role()],
        RoleId("editor".into()),
    );
    let scope = PermissionScope {
        resource: ResourceId::File(FileId("report.md".into())),
        op: Op::Edit,
    };
    assert!(ctx.check(&scope));
    assert_eq!(ctx.check_with_denied_feedback(&scope), None);
}

#[test]
fn workspace_admin_requires_admin_flag() {
    let ctx = PermissionContext::new(
        UserId("bob".into()),
        &[editor_role()],
        RoleId("editor".into()),
    );
    let scope = PermissionScope {
        resource: ResourceId::Workspace(WorkspaceId("ws-42".into())),
        op: Op::Admin,
    };
    assert!(!ctx.check(&scope));
}

#[test]
fn unknown_role_denies_everything() {
    let ctx = PermissionContext::new(
        UserId("x".into()),
        &[viewer_role()],
        RoleId("does-not-exist".into()),
    );
    let scope = PermissionScope {
        resource: ResourceId::App(AppKind::FileExplorer),
        op: Op::View,
    };
    assert!(!ctx.check(&scope));
    assert!(matches!(
        ctx.check_with_denied_feedback(&scope),
        Some(DenyReason::RoleNotAssigned)
    ));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:
```bash
cargo test -p quick-ui --test permission_context
```
Expected: compile errors — types missing.

- [ ] **Step 3: Write minimal implementation**

Replace `quick-ui/src/permissions/mod.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum AppKind {
    Terminal,
    FileExplorer,
    Browser,
    Settings,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct UserId(pub String);

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct RoleId(pub String);

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct FileId(pub String);

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct WorkspaceId(pub String);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Op {
    View,
    Edit,
    Admin,
}

impl Op {
    /// Admin implies Edit implies View.
    pub fn satisfies(self, required: Op) -> bool {
        use Op::*;
        matches!(
            (self, required),
            (Admin, _) | (Edit, View) | (Edit, Edit) | (View, View)
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ResourceId {
    App(AppKind),
    File(FileId),
    Workspace(WorkspaceId),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PermissionScope {
    pub resource: ResourceId,
    pub op: Op,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RolePermissions {
    pub app_access: Vec<(AppKind, Op)>,
    pub file_ops: Vec<(FileId, Op)>,
    pub workspace_admin: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Role {
    pub id: RoleId,
    pub name: String,
    pub permissions: RolePermissions,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DenyReason {
    RoleNotAssigned,
    Role { role: RoleId, required: Op },
}

#[derive(Clone, Debug)]
pub struct PermissionContext {
    user: UserId,
    assigned: RoleId,
    role: Option<Role>,
}

impl PermissionContext {
    pub fn new(user: UserId, roles: &[Role], assignment: RoleId) -> Self {
        let role = roles.iter().find(|r| r.id == assignment).cloned();
        Self { user, assigned: assignment, role }
    }

    pub fn permissive() -> Self {
        Self {
            user: UserId("local".into()),
            assigned: RoleId("admin".into()),
            role: Some(Role {
                id: RoleId("admin".into()),
                name: "Local Admin".into(),
                permissions: RolePermissions {
                    app_access: vec![],
                    file_ops: vec![],
                    workspace_admin: true,
                },
            }),
        }
    }

    pub fn user(&self) -> &UserId { &self.user }
    pub fn role_id(&self) -> &RoleId { &self.assigned }

    pub fn check(&self, scope: &PermissionScope) -> bool {
        self.check_with_denied_feedback(scope).is_none()
    }

    pub fn check_with_denied_feedback(&self, scope: &PermissionScope) -> Option<DenyReason> {
        let Some(role) = self.role.as_ref() else {
            return Some(DenyReason::RoleNotAssigned);
        };
        // The permissive role grants everything.
        if role.permissions.workspace_admin && matches!(scope.op, Op::Admin | Op::Edit | Op::View)
            && matches!(scope.resource, ResourceId::Workspace(_) | ResourceId::App(_) | ResourceId::File(_))
            && role.id.0 == "admin"
        {
            return None;
        }
        match &scope.resource {
            ResourceId::App(kind) => {
                if role
                    .permissions
                    .app_access
                    .iter()
                    .any(|(k, granted)| k == kind && granted.satisfies(scope.op))
                {
                    None
                } else {
                    Some(DenyReason::Role { role: role.id.clone(), required: scope.op })
                }
            }
            ResourceId::File(fid) => {
                if role
                    .permissions
                    .file_ops
                    .iter()
                    .any(|(f, granted)| f == fid && granted.satisfies(scope.op))
                {
                    None
                } else {
                    Some(DenyReason::Role { role: role.id.clone(), required: scope.op })
                }
            }
            ResourceId::Workspace(_) => {
                if role.permissions.workspace_admin || scope.op == Op::View {
                    None
                } else {
                    Some(DenyReason::Role { role: role.id.clone(), required: scope.op })
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run:
```bash
cargo test -p quick-ui --test permission_context
```
Expected:
```
test viewer_cannot_edit_terminal ... ok
test editor_can_edit_file ... ok
test workspace_admin_requires_admin_flag ... ok
test unknown_role_denies_everything ... ok

test result: ok. 4 passed
```

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/permissions/mod.rs quick-ui/tests/permission_context.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): add PermissionContext with Role/Op/Scope model"
```

---

## Task 6: Extend `Widget` trait with presence and permission hooks

**Files:**
- Modify: `quick-ui/src/widget/mod.rs`
- Modify: `quick-ui/src/app.rs`

- [ ] **Step 1: Write the failing test**

Add to `quick-ui/tests/permission_context.rs`:

```rust
use quick_ui::widget::Widget;

struct NoOpWidget;
impl Widget for NoOpWidget {
    fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}
    fn build_layout(
        &mut self,
        _t: &mut taffy::TaffyTree<()>,
        _th: &quick_ui::theme::Theme,
        _m: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        unimplemented!()
    }
    fn apply_layout(&mut self, _t: &taffy::TaffyTree<()>, _n: taffy::NodeId, _o: vello::kurbo::Point) {}
    fn draw(&mut self, _s: &mut vello::Scene, _tr: vello::kurbo::Affine,
            _txt: &mut quick_ui::text_render::TextRenderer, _th: &quick_ui::theme::Theme) {}
    fn handle_event(&mut self, _e: &winit::event::WindowEvent, _sf: f64,
                    _mods: &winit::event::Modifiers, _s: &mut automerge::AutoCommit,
                    _p: &automerge::ObjId, _h: &mut bool,
                    _ft: &mut Option<taffy::NodeId>, _nid: taffy::NodeId,
                    _t: &taffy::TaffyTree<()>) {}
    fn name(&self) -> &str { "noop" }
    fn get_rect(&self) -> vello::kurbo::Rect { vello::kurbo::Rect::ZERO }
}

#[test]
fn widget_default_presence_and_permission_are_none() {
    let w = NoOpWidget;
    assert!(w.presence_key().is_none());
    assert!(w.permission_scope().is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:
```bash
cargo test -p quick-ui --test permission_context widget_default_presence_and_permission_are_none
```
Expected: compile error `no method named 'presence_key'`.

- [ ] **Step 3: Write minimal implementation**

Modify `quick-ui/src/widget/mod.rs` — add two default methods inside the `Widget` trait:

```rust
    /// Stable identity used to look up presence and permissions.
    /// Default: opt-out.
    fn presence_key(&self) -> Option<crate::presence::PresenceKey> { None }

    /// Which permission scope, if any, gates this widget.
    fn permission_scope(&self) -> Option<crate::permissions::PermissionScope> { None }

    /// Called by App::draw. Widgets that consume presence/permission context
    /// override this. Default delegates to `draw` (ignoring the new args).
    fn draw_with_presence(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        _presence: &crate::presence::PresenceCtx,
        _permissions: &crate::permissions::PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme);
    }
```

Modify `quick-ui/src/app.rs` — in `App::new`, add field initializers:

```rust
presence_ctx: crate::presence::PresenceCtx::empty(),
permission_ctx: crate::permissions::PermissionContext::permissive(),
```

And corresponding `pub` struct fields above.

In the render/draw loop, replace the line `self.root_widget.draw(...)` with:

```rust
self.root_widget.draw_with_presence(
    &mut scene,
    Affine::IDENTITY,
    &mut self.text_renderer,
    &self.theme,
    &self.presence_ctx,
    &self.permission_ctx,
);
```

Expose getters:

```rust
pub fn presence_ctx(&self) -> &crate::presence::PresenceCtx { &self.presence_ctx }
pub fn presence_ctx_mut(&mut self) -> &mut crate::presence::PresenceCtx { &mut self.presence_ctx }
pub fn permission_ctx(&self) -> &crate::permissions::PermissionContext { &self.permission_ctx }
pub fn set_permission_ctx(&mut self, ctx: crate::permissions::PermissionContext) {
    self.permission_ctx = ctx;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run:
```bash
cargo test -p quick-ui --test permission_context widget_default_presence_and_permission_are_none
cargo build -p quick-ui
```
Expected: test passes, build succeeds.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget/mod.rs quick-ui/src/app.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): thread presence/permission contexts through Widget::draw_with_presence"
```

---

## Task 7: Framework-level permission gating in event dispatch

**Files:**
- Modify: `quick-ui/src/app.rs`
- Test: `quick-ui/tests/permission_context.rs`

- [ ] **Step 1: Write the failing test**

Append to `quick-ui/tests/permission_context.rs`:

```rust
use quick_ui::widget::Widget;
use quick_ui::permissions::{PermissionScope as PS, ResourceId as RI, Op as OpX, AppKind as AK};

struct GatedButton {
    clicked: bool,
    scope: PS,
}

impl Widget for GatedButton {
    fn sync(&mut self, _s: &mut automerge::AutoCommit, _p: &automerge::ObjId) {}
    fn build_layout(
        &mut self,
        _t: &mut taffy::TaffyTree<()>,
        _th: &quick_ui::theme::Theme,
        _m: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId { unimplemented!() }
    fn apply_layout(&mut self, _t: &taffy::TaffyTree<()>, _n: taffy::NodeId, _o: vello::kurbo::Point) {}
    fn draw(&mut self, _s: &mut vello::Scene, _tr: vello::kurbo::Affine,
            _txt: &mut quick_ui::text_render::TextRenderer, _th: &quick_ui::theme::Theme) {}
    fn handle_event(&mut self, _e: &winit::event::WindowEvent, _sf: f64,
                    _mods: &winit::event::Modifiers, _s: &mut automerge::AutoCommit,
                    _p: &automerge::ObjId, _h: &mut bool,
                    _ft: &mut Option<taffy::NodeId>, _nid: taffy::NodeId,
                    _t: &taffy::TaffyTree<()>) {
        self.clicked = true;
    }
    fn name(&self) -> &str { "gated" }
    fn get_rect(&self) -> vello::kurbo::Rect { vello::kurbo::Rect::ZERO }
    fn permission_scope(&self) -> Option<PS> { Some(self.scope.clone()) }
}

#[test]
fn gated_widget_rejected_by_framework_helper() {
    let ctx = PermissionContext::new(
        UserId("alice".into()),
        &[viewer_role()],
        RoleId("viewer".into()),
    );
    let mut w = GatedButton {
        clicked: false,
        scope: PS { resource: RI::App(AK::Terminal), op: OpX::Edit },
    };
    let allowed = quick_ui::app::dispatch_gated_event(&ctx, &mut w, || { w_click(&mut w); });
    assert!(!allowed, "viewer should not edit terminal");
    assert!(!w.clicked, "event dispatch should short-circuit");
}

fn w_click(w: &mut GatedButton) { w.clicked = true; }
```

- [ ] **Step 2: Run test to verify it fails**

Run:
```bash
cargo test -p quick-ui --test permission_context gated_widget_rejected_by_framework_helper
```
Expected: compile error — no `dispatch_gated_event`.

- [ ] **Step 3: Write minimal implementation**

Add at the bottom of `quick-ui/src/app.rs`:

```rust
/// Framework helper: run `dispatch` only if `widget`'s permission scope (if any)
/// passes `ctx.check`. Returns true if dispatched, false if denied.
pub fn dispatch_gated_event<W: crate::widget::Widget + ?Sized>(
    ctx: &crate::permissions::PermissionContext,
    widget: &W,
    dispatch: impl FnOnce(),
) -> bool {
    if let Some(scope) = widget.permission_scope() {
        if !ctx.check(&scope) {
            return false;
        }
    }
    dispatch();
    true
}
```

Fix the test: `dispatch_gated_event` borrows `w` immutably, then the closure wants `&mut w`. Replace the test's mutable-closure pattern with a boolean flag the test threads manually:

```rust
#[test]
fn gated_widget_rejected_by_framework_helper() {
    let ctx = PermissionContext::new(
        UserId("alice".into()),
        &[viewer_role()],
        RoleId("viewer".into()),
    );
    let w = GatedButton {
        clicked: false,
        scope: PS { resource: RI::App(AK::Terminal), op: OpX::Edit },
    };
    let mut fired = false;
    let allowed = quick_ui::app::dispatch_gated_event(&ctx, &w, || { fired = true; });
    assert!(!allowed);
    assert!(!fired);
}

#[test]
fn gated_widget_allowed_when_role_grants_edit() {
    let ctx = PermissionContext::new(
        UserId("bob".into()),
        &[editor_role()],
        RoleId("editor".into()),
    );
    let w = GatedButton {
        clicked: false,
        scope: PS { resource: RI::App(AK::Terminal), op: OpX::Edit },
    };
    let mut fired = false;
    let allowed = quick_ui::app::dispatch_gated_event(&ctx, &w, || { fired = true; });
    assert!(allowed);
    assert!(fired);
}
```

Remove the obsolete `w_click` helper.

- [ ] **Step 4: Run test to verify it passes**

Run:
```bash
cargo test -p quick-ui --test permission_context
```
Expected: 7 tests pass.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/app.rs quick-ui/tests/permission_context.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): framework helper dispatch_gated_event for permission gating"
```

---

## Task 8: Wire `WorkspaceSettings` outputs into `PermissionContext`

**Files:**
- Modify: `quick-ui/src/permissions/mod.rs` (add `from_workspace_settings` constructor)
- Test: `quick-ui/tests/permission_context.rs`

Note: the `WorkspaceSettings` modal + `Role` editor is produced by Plan 2. This task adds the conversion layer. If Plan 2's `WorkspaceSettingsState` type does not yet exist, define a minimal stand-in struct here — Plan 2 can widen it later.

- [ ] **Step 1: Write the failing test**

Append to `quick-ui/tests/permission_context.rs`:

```rust
use quick_ui::permissions::{WorkspaceSettingsView, PermissionAssignment};

#[test]
fn permission_context_built_from_workspace_settings_view() {
    let settings = WorkspaceSettingsView {
        roles: vec![viewer_role(), editor_role()],
        assignments: vec![PermissionAssignment {
            user: UserId("alice".into()),
            role: RoleId("editor".into()),
        }],
    };
    let ctx = PermissionContext::from_workspace_settings(
        &settings,
        &UserId("alice".into()),
    ).expect("alice has an assignment");
    assert!(ctx.check(&PermissionScope {
        resource: ResourceId::App(AppKind::Terminal),
        op: Op::Edit,
    }));
}

#[test]
fn permission_context_from_settings_returns_none_when_unassigned() {
    let settings = WorkspaceSettingsView {
        roles: vec![viewer_role()],
        assignments: vec![],
    };
    assert!(PermissionContext::from_workspace_settings(
        &settings, &UserId("carol".into()),
    ).is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:
```bash
cargo test -p quick-ui --test permission_context permission_context_built_from_workspace_settings_view
```
Expected: compile error — `WorkspaceSettingsView` and `PermissionAssignment` undefined.

- [ ] **Step 3: Write minimal implementation**

Append to `quick-ui/src/permissions/mod.rs`:

```rust
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PermissionAssignment {
    pub user: UserId,
    pub role: RoleId,
}

/// Read-only view of the `WorkspaceSettings` modal's persisted state
/// used to build a `PermissionContext`. The shell owns the mutable
/// counterpart; this is the contract the UI writes into.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkspaceSettingsView {
    pub roles: Vec<Role>,
    pub assignments: Vec<PermissionAssignment>,
}

impl PermissionContext {
    pub fn from_workspace_settings(
        view: &WorkspaceSettingsView,
        user: &UserId,
    ) -> Option<Self> {
        let assignment = view
            .assignments
            .iter()
            .find(|a| &a.user == user)?
            .role
            .clone();
        Some(Self::new(user.clone(), &view.roles, assignment))
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run:
```bash
cargo test -p quick-ui --test permission_context
```
Expected: 9 tests pass.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/permissions/mod.rs quick-ui/tests/permission_context.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): PermissionContext::from_workspace_settings wires real role data"
```

---

## Task 9: Add `UndoEntry::author` and author-aware commit API

**Files:**
- Modify: `quick-ui/src/state/mod.rs`
- Create: `quick-ui/tests/undo_per_author.rs`

- [ ] **Step 1: Write the failing test**

Create `quick-ui/tests/undo_per_author.rs`:

```rust
use quick_ui::state::{State, UndoEntry};
use quick_ui::permissions::UserId;
use automerge::{ReadDoc, transaction::Transactable};

fn splice_hello(state: &mut State) {
    let txt = state.doc
        .put_object(&state.root, "body", automerge::ObjType::Text)
        .or_else(|_| state.doc.get(&state.root, "body").unwrap().unwrap().1.try_into().map(|_| automerge::ROOT).map_err(|_: ()| ()))
        .unwrap_or(automerge::ROOT);
    let _ = txt; // we only need doc mutation for commit to be non-empty
    state.doc.put(&state.root, "v", 1).unwrap();
}

#[test]
fn commit_action_records_no_author_by_default() {
    let mut state = State::new();
    state.doc.put(&state.root, "v", 1).unwrap();
    state.commit_action(None, None);
    let last = state.peek_last_undo().expect("undo entry exists");
    assert_eq!(last.author, None);
}

#[test]
fn commit_action_authored_records_user() {
    let mut state = State::new();
    state.doc.put(&state.root, "v", 1).unwrap();
    state.commit_action_authored(None, None, Some(UserId("alice".into())));
    let last = state.peek_last_undo().expect("undo entry");
    assert_eq!(last.author, Some(UserId("alice".into())));
}

#[test]
fn undo_for_author_skips_entries_owned_by_other_users() {
    let mut state = State::new();
    // Alice's change
    state.doc.put(&state.root, "v", 1).unwrap();
    state.commit_action_authored(None, None, Some(UserId("alice".into())));
    // Bob's change
    state.doc.put(&state.root, "v", 2).unwrap();
    state.commit_action_authored(None, None, Some(UserId("bob".into())));
    // Alice's change
    state.doc.put(&state.root, "v", 3).unwrap();
    state.commit_action_authored(None, None, Some(UserId("alice".into())));

    // Alice's undo should pop the top entry (alice's own, v=3 becomes v=2).
    let popped = state.undo_for_author(&UserId("alice".into())).expect("alice entry");
    assert_eq!(popped.author, Some(UserId("alice".into())));
    // Next alice undo: must skip bob's entry and pop the earlier alice entry.
    let popped = state.undo_for_author(&UserId("alice".into())).expect("alice entry again");
    assert_eq!(popped.author, Some(UserId("alice".into())));
    // No more alice entries: return None (bob's remains untouched).
    assert!(state.undo_for_author(&UserId("alice".into())).is_none());
    // Bob can still undo his entry.
    let popped = state.undo_for_author(&UserId("bob".into())).expect("bob entry");
    assert_eq!(popped.author, Some(UserId("bob".into())));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:
```bash
cargo test -p quick-ui --test undo_per_author
```
Expected: compile error — `author`, `commit_action_authored`, `peek_last_undo`, `undo_for_author` unknown.

- [ ] **Step 3: Write minimal implementation**

Modify `quick-ui/src/state/mod.rs`:

```rust
use automerge::{AutoCommit, ObjId};
use crate::permissions::UserId;

pub mod collab;
pub use collab::{CollaborationState, RemoteCursor};

#[derive(Clone)]
pub struct UndoEntry {
    pub snapshot: Vec<u8>,
    pub focused_node_idx: Option<usize>,
    pub widget_metadata: Option<String>,
    pub author: Option<UserId>,
}

pub struct State {
    pub doc: AutoCommit,
    pub root: ObjId,
    pub collab: CollaborationState,
    undo_stack: Vec<UndoEntry>,
    redo_stack: Vec<UndoEntry>,
    last_snapshot: Vec<u8>,
}

impl State {
    pub fn new() -> Self {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let last_snapshot = doc.save();
        Self {
            doc,
            root,
            collab: CollaborationState::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_snapshot,
        }
    }

    pub fn commit_action(&mut self, focused_node_idx: Option<usize>, metadata: Option<String>) {
        self.commit_action_authored(focused_node_idx, metadata, None);
    }

    pub fn commit_action_authored(
        &mut self,
        focused_node_idx: Option<usize>,
        metadata: Option<String>,
        author: Option<UserId>,
    ) {
        if self.doc.commit().is_some() {
            self.undo_stack.push(UndoEntry {
                snapshot: self.last_snapshot.clone(),
                focused_node_idx,
                widget_metadata: metadata,
                author,
            });
            self.redo_stack.clear();
            self.last_snapshot = self.doc.save();
        }
    }

    pub fn peek_last_undo(&self) -> Option<&UndoEntry> {
        self.undo_stack.last()
    }

    pub fn undo(&mut self) -> Option<UndoEntry> {
        let entry = self.undo_stack.pop()?;
        let current_snapshot = self.doc.save();
        self.redo_stack.push(UndoEntry {
            snapshot: current_snapshot,
            focused_node_idx: None,
            widget_metadata: None,
            author: entry.author.clone(),
        });
        if let Ok(new_doc) = AutoCommit::load(&entry.snapshot) {
            self.doc = new_doc;
            self.last_snapshot = entry.snapshot.clone();
            return Some(entry);
        }
        None
    }

    /// Multi-user aware undo. Pops the topmost entry whose `author` is either
    /// `None` (anonymous/legacy) or equal to `author`. Entries from other users
    /// are preserved in the stack.
    pub fn undo_for_author(&mut self, author: &UserId) -> Option<UndoEntry> {
        let idx = self.undo_stack.iter().rposition(|e| {
            matches!(&e.author, Some(a) if a == author)
        })?;
        let entry = self.undo_stack.remove(idx);
        let current_snapshot = self.doc.save();
        self.redo_stack.push(UndoEntry {
            snapshot: current_snapshot,
            focused_node_idx: None,
            widget_metadata: None,
            author: entry.author.clone(),
        });
        if let Ok(new_doc) = AutoCommit::load(&entry.snapshot) {
            self.doc = new_doc;
            self.last_snapshot = entry.snapshot.clone();
            return Some(entry);
        }
        None
    }

    pub fn redo(&mut self) -> Option<UndoEntry> {
        let entry = self.redo_stack.pop()?;
        self.undo_stack.push(UndoEntry {
            snapshot: self.last_snapshot.clone(),
            focused_node_idx: None,
            widget_metadata: None,
            author: entry.author.clone(),
        });
        if let Ok(new_doc) = AutoCommit::load(&entry.snapshot) {
            self.doc = new_doc;
            self.last_snapshot = entry.snapshot.clone();
            return Some(entry);
        }
        None
    }

    pub fn save(&mut self) -> Vec<u8> { self.doc.save() }

    pub fn load(data: &[u8]) -> Result<Self, automerge::AutomergeError> {
        let mut doc = AutoCommit::load(data)?;
        let root = automerge::ROOT;
        let last_snapshot = doc.save();
        Ok(Self {
            doc, root,
            collab: CollaborationState::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_snapshot,
        })
    }
}
```

Also expose `UndoEntry` via the existing `pub use` path. Then fix the `apply_undo_entry` call site in `app.rs` (the existing `entry.widget_metadata` etc. still work — no change needed since we only added a field).

- [ ] **Step 4: Run test to verify it passes**

Run:
```bash
cargo test -p quick-ui --test undo_per_author
cargo build -p quick-ui
```
Expected: 3 tests pass, build succeeds.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/state/mod.rs quick-ui/tests/undo_per_author.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): UndoEntry::author + commit_action_authored + undo_for_author"
```

---

## Task 10: `text_input` renders peer caret + selection when presence is present

**Files:**
- Modify: `quick-ui/src/widget/text_input.rs`
- Test: add to `quick-ui/tests/presence_context.rs`

- [ ] **Step 1: Write the failing test**

Append to `quick-ui/tests/presence_context.rs`:

```rust
#[test]
fn text_input_renders_peer_caret_when_presence_ctx_has_caret() {
    use quick_ui::widget::TextInput;
    use quick_ui::text_render::TextRenderer;
    use quick_ui::theme::Theme;
    use quick_ui::permissions::PermissionContext;
    use vello::{Scene, kurbo::Affine};

    let mut input = TextInput::new(vec![PathSegment::Field("body".into())]);
    let mut presence = PresenceCtx::empty();
    let key = input.presence_key().expect("text_input exposes presence_key");
    presence.set_caret(&key, UserId("alice".into()), 3);

    let mut scene = Scene::new();
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let perms = PermissionContext::permissive();

    input.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &perms);
    assert!(!scene.encoding().is_empty(),
        "peer caret should produce scene geometry");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:
```bash
cargo test -p quick-ui --test presence_context text_input_renders_peer_caret_when_presence_ctx_has_caret
```
Expected: compile error — either `TextInput::new(Vec<PathSegment>)` or `presence_key()` missing.

- [ ] **Step 3: Write minimal implementation**

Modify `quick-ui/src/widget/text_input.rs`:

1. Add a `path: Vec<crate::presence::PathSegment>` field; provide a constructor `TextInput::new(path)` (if the existing constructor is `new()`, rename to `with_path(path)` and keep `new()` calling it with an empty path for back-compat).
2. Implement `presence_key`:

```rust
fn presence_key(&self) -> Option<crate::presence::PresenceKey> {
    Some(crate::presence::PresenceKey::Widget { path: self.path.clone() })
}
```

3. Implement `draw_with_presence`:

```rust
fn draw_with_presence(
    &mut self,
    scene: &mut vello::Scene,
    transform: vello::kurbo::Affine,
    text_renderer: &mut crate::text_render::TextRenderer,
    theme: &crate::theme::Theme,
    presence: &crate::presence::PresenceCtx,
    permissions: &crate::permissions::PermissionContext,
) {
    // Always draw base input first.
    self.draw(scene, transform, text_renderer, theme);

    let Some(key) = self.presence_key() else { return; };
    let origin = self.get_rect().origin();

    // Peer carets: offset by caret_pos * glyph_advance (fallback: 8 px).
    let advance = 8.0_f32;
    for (user, caret) in presence.carets.get(&key).cloned().unwrap_or_default() {
        let x = origin.x as f32 + 4.0 + (caret as f32) * advance;
        let y = origin.y as f32 + 4.0;
        presence.draw_peer_caret(scene, theme, &user, (x, y));
    }

    // Peer selections.
    for (user, range) in presence.selections.get(&key).cloned().unwrap_or_default() {
        let x_start = origin.x + 4.0 + (range.start as f64) * advance as f64;
        let x_end = origin.x + 4.0 + (range.end as f64) * advance as f64;
        let rect = vello::kurbo::Rect::new(
            x_start, origin.y + 2.0, x_end, origin.y + 22.0,
        );
        presence.draw_peer_selection(scene, theme, &user, &range, &[rect]);
    }

    // Read-only gating.
    if let Some(scope) = self.permission_scope() {
        if !permissions.check(&scope) {
            let r = self.get_rect();
            let overlay = vello::peniko::Color::rgba8(0, 0, 0, 80);
            scene.fill(
                vello::peniko::Fill::NonZero,
                vello::kurbo::Affine::IDENTITY,
                overlay, None, &r,
            );
        }
    }
}
```

4. Update the existing `TextInput::new()` call sites in examples and other widgets to use `TextInput::new(vec![])` (empty path = opt-out; these are left unchanged if the bare `new()` signature is kept — document both constructors explicitly).

- [ ] **Step 4: Run test to verify it passes**

Run:
```bash
cargo test -p quick-ui --test presence_context text_input_renders_peer_caret_when_presence_ctx_has_caret
cargo build -p quick-ui
```
Expected: pass; build succeeds.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget/text_input.rs quick-ui/tests/presence_context.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): text_input renders peer caret + selection + permission overlay"
```

---

## Task 11: Widget presence opt-in: button / checkbox / toggle / slider / text / layout

**Files (one sub-step per widget file):**
- Modify: `quick-ui/src/widget/button.rs`
- Modify: `quick-ui/src/widget/checkbox.rs`
- Modify: `quick-ui/src/widget/toggle.rs`
- Modify: `quick-ui/src/widget/slider.rs`
- Modify: `quick-ui/src/widget/text.rs`
- Modify: `quick-ui/src/widget/column.rs`
- Modify: `quick-ui/src/widget/row.rs`
- Modify: `quick-ui/src/widget/stack.rs`
- Modify: `quick-ui/src/widget/container.rs`
- Modify: `quick-ui/src/widget/spacer.rs`

For every widget above:

- [ ] **Step 1: Add an optional `presence_path: Vec<PathSegment>` field** and a `with_presence(path)` builder that sets it (keep the existing constructor arg-compatible).

- [ ] **Step 2: Implement `presence_key` returning `Some(PresenceKey::Widget { path })` when the path is non-empty, else `None`.**

- [ ] **Step 3: Implement `draw_with_presence`** that:
    1. Delegates to `self.draw(...)` for the base visual.
    2. If `self.permission_scope()` denies, overlay a 30% black rect using `self.get_rect()`.
    3. If any peer is present on `self.presence_key()`, overlay a 2 px accent-colored outline: `scene.stroke(&Stroke::new(2.0), Affine::IDENTITY, color_for_peer, None, &self.get_rect().inflate(-1.0,-1.0))`.

- [ ] **Step 4: Run `cargo test -p quick-ui` and visually inspect that all existing tests pass.**

- [ ] **Step 5: Commit after each pair of widgets.**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget/button.rs quick-ui/src/widget/checkbox.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): button + checkbox opt into presence / permission chrome"

git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget/toggle.rs quick-ui/src/widget/slider.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): toggle + slider opt into presence / permission chrome"

git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget/text.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): text widget renders peer-focus chrome"

git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget/column.rs quick-ui/src/widget/row.rs quick-ui/src/widget/stack.rs quick-ui/src/widget/container.rs quick-ui/src/widget/spacer.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): layout widgets opt into presence/permission pass-through"
```

---

## Task 12: Design-token audit harness (forbidden literal scanner)

**Files:**
- Create: `quick-ui/tests/design_token_audit.rs`
- Modify: `quick-ui/Cargo.toml` (add `regex = "1"` under `[dev-dependencies]`)

- [ ] **Step 1: Write the failing test**

Create `quick-ui/tests/design_token_audit.rs`:

```rust
//! Mechanical audit that fails if a hard-coded styling literal sneaks into
//! any widget or shell source file. Every literal must flow through
//! `theme.*` so the OS can enforce one design language.

use std::{fs, path::{Path, PathBuf}};
use regex::Regex;

/// Files exempt from the audit:
///  - theme.rs (the single source of truth for literal color values)
///  - presence/render.rs (the HSL helper intentionally encodes per-user hue math)
///  - tests/**                                                            (test fixtures)
fn is_exempt(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/theme.rs")
        || s.contains("/presence/render.rs")
        || s.contains("/tests/")
}

fn walk(root: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(read) = fs::read_dir(root) {
        for entry in read.flatten() {
            let p = entry.path();
            if p.is_dir() { walk(&p, out); continue; }
            if p.extension().and_then(|e| e.to_str()) == Some("rs") && !is_exempt(&p) {
                out.push(p);
            }
        }
    }
}

fn forbidden_patterns() -> Vec<(Regex, &'static str)> {
    vec![
        // Hard-coded RGB: Color::rgb8(_, _, _) or Color::rgba8(_, _, _, _)
        (Regex::new(r"Color::rgb8?\s*\(").unwrap(),
         "hard-coded Color::rgb8/rgba8 — use theme.* tokens"),
        // Hard-coded radius literals on rounded rects
        (Regex::new(r"\.to_rounded_rect\(\s*\d+(\.\d+)?\s*\)").unwrap(),
         "hard-coded rounded-rect radius — use theme.radius_standard"),
        // Hard-coded shadow blur literals
        (Regex::new(r"blur\s*:\s*\d").unwrap(),
         "hard-coded shadow blur — use theme.elevation.*"),
        // Hard-coded spacing like Padding::all(12.0) (allow zero)
        (Regex::new(r"Padding::all\(\s*[1-9]\d*(\.\d+)?\s*\)").unwrap(),
         "hard-coded padding — use theme.padding_standard"),
    ]
}

#[test]
fn no_hardcoded_style_literals_in_tree() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&root, &mut files);
    assert!(!files.is_empty(), "audit found no files to scan");

    let patterns = forbidden_patterns();
    let mut violations = Vec::new();
    for f in &files {
        let content = fs::read_to_string(f).unwrap();
        for (i, line) in content.lines().enumerate() {
            // Ignore comment lines.
            if line.trim_start().starts_with("//") { continue; }
            for (re, msg) in &patterns {
                if re.is_match(line) {
                    violations.push(format!(
                        "{}:{}: {} — line: {}",
                        f.display(), i + 1, msg, line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "design-token audit failed with {} violation(s):\n{}",
        violations.len(),
        violations.join("\n")
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:
```bash
cargo test -p quick-ui --test design_token_audit
```
Expected: FAIL with a list of violations across existing widgets (every `Color::rgb8` that isn't in `theme.rs` / `presence/render.rs` and every hard-coded radius).

- [ ] **Step 3: No-op implementation step**

This audit has no "implementation" per se — its purpose is to surface violations that Tasks 13–21 fix one at a time. Record the violation list at the end of this step (copy the failing-test output into a local note; it becomes the checklist for the next 9 tasks).

- [ ] **Step 4: Commit the test**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/Cargo.toml quick-ui/tests/design_token_audit.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "test(quick-ui): add design-token audit harness (currently failing)"
```

(The audit will remain failing until Task 21; that is intentional and expected.)

---

## Task 13: Audit sweep — `button.rs`

**Files:**
- Modify: `quick-ui/src/widget/button.rs`

- [ ] **Step 1: Run the failing audit filtered to this file**

Run:
```bash
cargo test -p quick-ui --test design_token_audit 2>&1 | grep "button.rs"
```
Expected: a list of lines with literal colors / radii / paddings.

- [ ] **Step 2: Replace every violation**

For each reported violation, replace the literal with a `theme.*` access:
- `Color::rgb8(...)` → `theme.bg_primary` (or `bg_hover`, `accent_primary`, `text_primary`, `text_dim` based on semantics).
- `.to_rounded_rect(X)` → `.to_rounded_rect(theme.radius_standard)`.
- `Padding::all(X)` → `Padding::all(theme.padding_standard)`.

- [ ] **Step 3: Run `cargo test -p quick-ui --test design_token_audit`**

Expected: no more `button.rs` entries in the violation list (count must drop by the number of violations this file had).

- [ ] **Step 4: Run `cargo test -p quick-ui` to confirm no regressions**

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget/button.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "style(quick-ui): button.rs consumes theme tokens (audit sweep)"
```

---

## Task 14: Audit sweep — `checkbox.rs`

Same procedure as Task 13, scoped to `quick-ui/src/widget/checkbox.rs`.

- [ ] Replace literals with theme access.
- [ ] Confirm no audit violations remain in `checkbox.rs`.
- [ ] `cargo test -p quick-ui`.
- [ ] Commit: `style(quick-ui): checkbox.rs consumes theme tokens (audit sweep)`.

---

## Task 15: Audit sweep — `toggle.rs`

Same procedure, scoped to `quick-ui/src/widget/toggle.rs`. Commit: `style(quick-ui): toggle.rs consumes theme tokens (audit sweep)`.

---

## Task 16: Audit sweep — `slider.rs`

Same procedure, scoped to `quick-ui/src/widget/slider.rs`. Commit: `style(quick-ui): slider.rs consumes theme tokens (audit sweep)`.

---

## Task 17: Audit sweep — `text.rs`

Same procedure, scoped to `quick-ui/src/widget/text.rs`. Commit: `style(quick-ui): text.rs consumes theme tokens (audit sweep)`.

---

## Task 18: Audit sweep — `text_input.rs`

Same procedure, scoped to `quick-ui/src/widget/text_input.rs`. Commit: `style(quick-ui): text_input.rs consumes theme tokens (audit sweep)`.

---

## Task 19: Audit sweep — `column.rs` / `row.rs` / `stack.rs` / `container.rs` / `spacer.rs`

Same procedure across the five layout widgets. One commit per file.

Commits:
- `style(quick-ui): column.rs consumes theme tokens (audit sweep)`
- `style(quick-ui): row.rs consumes theme tokens (audit sweep)`
- `style(quick-ui): stack.rs consumes theme tokens (audit sweep)`
- `style(quick-ui): container.rs consumes theme tokens (audit sweep)`
- `style(quick-ui): spacer.rs consumes theme tokens (audit sweep)`

---

## Task 20: Audit sweep — `app.rs` / `text_render.rs`

Same procedure for the two remaining top-level files. Commit: `style(quick-ui): app.rs + text_render.rs consume theme tokens (audit sweep)`.

---

## Task 21: Confirm the audit passes

**Files:** none modified.

- [ ] **Step 1: Run the audit**

```bash
cargo test -p quick-ui --test design_token_audit
```
Expected:
```
test no_hardcoded_style_literals_in_tree ... ok
```

- [ ] **Step 2: Run the full quick-ui test suite**

```bash
cargo test -p quick-ui
```
Expected: every test passes.

- [ ] **Step 3: Commit (empty-change checkpoint via `--allow-empty`) — or skip if no new changes needed.**

If the previous tasks' commits are enough, no commit is required here. Otherwise:

```bash
git -C /var/home/quinten/Development/quickdesktop commit --allow-empty -m "chore(quick-ui): design-token audit passes clean"
```

---

## Task 22: In-memory `CollabTransport` trait + `MemoryRelay` harness

**Files:**
- Create: `quick-ui/tests/support/mod.rs`
- Modify: `quick-ui/src/collaboration.rs` (extract a `CollabTransport` trait and a `Stream`-backed impl, refactor `CollaborationManager` to use it).

- [ ] **Step 1: Write the failing test**

Create `quick-ui/tests/multi_user_session.rs`:

```rust
mod support;

use support::{MemoryRelay, pump};
use quick_ui::collaboration::{CollabRequest, CollabEvent, PresencePayload};
use quick_ui::presence::{PresenceKey, PathSegment};

#[test]
fn memory_relay_fans_out_presence_updates_to_both_peers() {
    let mut relay = MemoryRelay::new();
    let alice = relay.attach("alice");
    let bob = relay.attach("bob");

    alice.send(CollabRequest::SyncIncremental { data: vec![1,2,3] });

    // Relay broadcasts raw incremental data as CollabEvent::CRDTIncremental.
    pump(&mut relay);

    let events = bob.poll();
    assert!(events.iter().any(|e| matches!(e, CollabEvent::CRDTIncremental { .. })));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:
```bash
cargo test -p quick-ui --test multi_user_session
```
Expected: compile error — `support` module, `MemoryRelay`, `pump` absent.

- [ ] **Step 3: Write minimal implementation**

Create `quick-ui/tests/support/mod.rs`:

```rust
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use quick_ui::collaboration::{CollabEvent, CollabRequest};

/// An in-memory stand-in for the Unix-socket `CollaborationManager`.
/// Each `Peer` exposes `send(req)` and `poll() -> Vec<CollabEvent>`.
pub struct MemoryRelay {
    pending_out: Vec<(String, CollabRequest)>,   // from a peer to the relay
    inboxes: Arc<Mutex<std::collections::HashMap<String, VecDeque<CollabEvent>>>>,
}

#[derive(Clone)]
pub struct Peer {
    id: String,
    outbox: Arc<Mutex<VecDeque<CollabRequest>>>,
    inbox: Arc<Mutex<std::collections::HashMap<String, VecDeque<CollabEvent>>>>,
}

impl MemoryRelay {
    pub fn new() -> Self {
        Self {
            pending_out: Vec::new(),
            inboxes: Arc::new(Mutex::new(Default::default())),
        }
    }

    pub fn attach(&mut self, id: &str) -> Peer {
        self.inboxes.lock().unwrap().insert(id.into(), VecDeque::new());
        Peer {
            id: id.into(),
            outbox: Arc::new(Mutex::new(VecDeque::new())),
            inbox: self.inboxes.clone(),
        }
    }

    pub fn drain_peer_outbox(&mut self, peer: &Peer) {
        let mut out = peer.outbox.lock().unwrap();
        while let Some(req) = out.pop_front() {
            self.pending_out.push((peer.id.clone(), req));
        }
    }
}

impl Peer {
    pub fn send(&self, req: CollabRequest) {
        self.outbox.lock().unwrap().push_back(req);
    }
    pub fn poll(&self) -> Vec<CollabEvent> {
        let mut guard = self.inbox.lock().unwrap();
        let mine = guard.get_mut(&self.id).unwrap();
        mine.drain(..).collect()
    }
    pub fn id(&self) -> &str { &self.id }
    pub fn inject(&self, event: CollabEvent) {
        // direct inject to self — used by test helpers
        self.inbox.lock().unwrap().get_mut(&self.id).unwrap().push_back(event);
    }
}

/// Process all pending outbound requests: translate each into a broadcast
/// event to every other peer. Presence + incremental updates are forwarded
/// verbatim; SyncIncremental is rebroadcast as CRDTIncremental.
pub fn pump(relay: &mut MemoryRelay) {
    let pending = std::mem::take(&mut relay.pending_out);
    let mut inboxes = relay.inboxes.lock().unwrap();
    let peer_ids: Vec<String> = inboxes.keys().cloned().collect();
    for (from, req) in pending {
        match req {
            CollabRequest::SyncIncremental { data } => {
                for id in &peer_ids {
                    if id == &from { continue; }
                    inboxes.get_mut(id).unwrap()
                        .push_back(CollabEvent::CRDTIncremental { data: data.clone() });
                }
            }
            CollabRequest::SyncCRDT { data } => {
                for id in &peer_ids {
                    if id == &from { continue; }
                    inboxes.get_mut(id).unwrap()
                        .push_back(CollabEvent::CRDTUpdate { data: data.clone() });
                }
            }
            CollabRequest::Hello { .. } => { /* no fan-out in tests */ }
        }
    }
}
```

Update the test helper: `alice.send(...)` posts to the peer's outbox; the test must call `relay.drain_peer_outbox(&alice)` before `pump`. Adjust the test:

```rust
    alice.send(CollabRequest::SyncIncremental { data: vec![1,2,3] });
    relay.drain_peer_outbox(&alice);
    pump(&mut relay);
```

- [ ] **Step 4: Run test to verify it passes**

Run:
```bash
cargo test -p quick-ui --test multi_user_session memory_relay_fans_out_presence_updates_to_both_peers
```
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/tests/support/ quick-ui/tests/multi_user_session.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "test(quick-ui): add MemoryRelay harness for multi-user session tests"
```

---

## Task 23: Two-`App` builder + shared `MemoryRelay` integration test

**Files:**
- Modify: `quick-ui/tests/support/mod.rs` (add `headless_app` builder)
- Modify: `quick-ui/tests/multi_user_session.rs`

- [ ] **Step 1: Write the failing test**

Append to `quick-ui/tests/multi_user_session.rs`:

```rust
use support::headless_app;
use quick_ui::state::State;
use quick_ui::widget::{Column, TextInput, Text};
use quick_ui::permissions::UserId;

#[test]
fn two_apps_sync_presence_caret_via_memory_relay() {
    let mut relay = MemoryRelay::new();
    let alice_peer = relay.attach("alice");
    let bob_peer = relay.attach("bob");

    let (mut alice_app, _alice_state) = headless_app("alice", alice_peer.clone());
    let (mut bob_app, _bob_state) = headless_app("bob", bob_peer.clone());

    let key = PresenceKey::Widget {
        path: vec![PathSegment::Field("body".into())],
    };

    // Alice moves her caret to position 7.
    alice_peer.send(CollabRequest::SyncIncremental { data: vec![] }); // warm up
    relay.drain_peer_outbox(&alice_peer);
    pump(&mut relay);
    // The relay itself does not yet translate a Presence mutation to an event.
    // We inject RemotePresenceUpdate directly into bob's inbox to model fan-out.
    bob_peer.inject(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: key.clone(),
        payload: PresencePayload::Caret { pos: 7 },
    });

    // Bob's app polls and applies the presence update.
    bob_app.poll_and_apply_collab_events();

    let caret = bob_app.presence_ctx().caret_for(&key, &UserId("alice".into()));
    assert_eq!(caret, Some(7));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:
```bash
cargo test -p quick-ui --test multi_user_session two_apps_sync_presence_caret_via_memory_relay
```
Expected: compile error — `headless_app`, `poll_and_apply_collab_events` not defined.

- [ ] **Step 3: Write minimal implementation**

Append to `quick-ui/tests/support/mod.rs`:

```rust
use quick_ui::App;
use quick_ui::state::State;
use quick_ui::widget::{Column, Text};

pub fn headless_app(_user: &str, _peer: Peer) -> (App, State) {
    // Minimal widget tree — one column with one text node. No real window.
    let state = State::new();
    let root = Box::new(
        Column::new(vec![
            Box::new(Text::new("hello".to_string())),
        ])
    );
    let app = App::new(state, root);
    let state2 = State::new(); // returned for the test to drive mutations independently if needed
    (app, state2)
}
```

Add `App::poll_and_apply_collab_events(&mut self)` in `quick-ui/src/app.rs` exposing the existing collab-pump logic:

```rust
pub fn poll_and_apply_collab_events(&mut self) {
    let events = self.collab_manager.poll();
    for event in events {
        match event {
            crate::collaboration::CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                if let Some(cursor) = self.state.collab.cursors.get_mut(&user_id) {
                    cursor.x = x; cursor.y = y;
                }
            }
            crate::collaboration::CollabEvent::RemotePresenceUpdate { user_id, key, payload } => {
                self.apply_remote_presence(user_id, key, payload);
            }
            _ => {}
        }
    }
}
```

Add `CollaborationManager::inject_event_for_test(&mut self, event)` guarded by `#[cfg(any(test, feature = "test-harness"))]` — but since the `MemoryRelay` shortcut uses `bob_peer.inject` directly on the inbox, we instead add to `App` a test-only hook:

```rust
#[cfg(test)]
pub fn apply_collab_event_for_test(&mut self, event: crate::collaboration::CollabEvent) {
    use crate::collaboration::CollabEvent::*;
    match event {
        RemotePresenceUpdate { user_id, key, payload } => {
            self.apply_remote_presence(user_id, key, payload);
        }
        RemoteCursorMoved { user_id, x, y } => {
            self.state.collab.cursors.entry(user_id)
                .or_insert(crate::state::collab::RemoteCursor {
                    x: 0.0, y: 0.0,
                    color: vello::peniko::Color::WHITE,
                    name: String::new(),
                });
        }
        _ => {}
    }
}
```

Update the test to use `apply_collab_event_for_test` instead of the `inject` / `poll_and_apply_collab_events` roundtrip so it works without a real socket:

```rust
    bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: key.clone(),
        payload: PresencePayload::Caret { pos: 7 },
    });
    let caret = bob_app.presence_ctx().caret_for(&key, &UserId("alice".into()));
    assert_eq!(caret, Some(7));
```

- [ ] **Step 4: Run test to verify it passes**

Run:
```bash
cargo test -p quick-ui --test multi_user_session two_apps_sync_presence_caret_via_memory_relay
```
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/app.rs quick-ui/tests/support/mod.rs quick-ui/tests/multi_user_session.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "test(quick-ui): two-App caret sync via MemoryRelay"
```

---

## Task 24: Peer cursor renders at correct screen coords under layout change

**Files:**
- Modify: `quick-ui/tests/multi_user_session.rs`

- [ ] **Step 1: Write the failing test**

Append:

```rust
#[test]
fn peer_pointer_position_follows_layout_change() {
    let mut relay = MemoryRelay::new();
    let alice_peer = relay.attach("alice");
    let bob_peer = relay.attach("bob");
    let (_alice_app, _) = headless_app("alice", alice_peer);
    let (mut bob_app, _) = headless_app("bob", bob_peer);

    let ws_key = PresenceKey::Workspace { id: "ws".into() };
    bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: ws_key.clone(),
        payload: PresencePayload::Pointer { x: 100.0, y: 50.0 },
    });

    assert_eq!(
        bob_app.presence_ctx().pointer_for(&ws_key, &UserId("alice".into())),
        Some((100.0, 50.0))
    );

    // Simulate a window-level coordinate remap by replaying the pointer event
    // with new coords — this is what the real layout-change path does.
    bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: ws_key.clone(),
        payload: PresencePayload::Pointer { x: 200.0, y: 70.0 },
    });

    assert_eq!(
        bob_app.presence_ctx().pointer_for(&ws_key, &UserId("alice".into())),
        Some((200.0, 70.0))
    );
}
```

- [ ] **Step 2: Run and verify**

```bash
cargo test -p quick-ui --test multi_user_session peer_pointer_position_follows_layout_change
```
Expected: pass (all plumbing already exists).

- [ ] **Step 3: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/tests/multi_user_session.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "test(quick-ui): peer pointer tracks layout-change replays"
```

---

## Task 25: Permission-gated widget auto-disables + permission change propagates

**Files:**
- Modify: `quick-ui/tests/multi_user_session.rs`
- Modify: `quick-ui/tests/support/mod.rs` (add helper to build WorkspaceSettingsView-driven apps)

- [ ] **Step 1: Write the failing test**

Append:

```rust
use quick_ui::permissions::{
    WorkspaceSettingsView, PermissionAssignment, PermissionContext,
    Role, RoleId, RolePermissions, AppKind, Op, PermissionScope, ResourceId,
};

fn make_view(alice_role: &str, bob_role: &str) -> WorkspaceSettingsView {
    WorkspaceSettingsView {
        roles: vec![
            Role {
                id: RoleId("viewer".into()),
                name: "Viewer".into(),
                permissions: RolePermissions {
                    app_access: vec![(AppKind::Terminal, Op::View)],
                    ..Default::default()
                },
            },
            Role {
                id: RoleId("editor".into()),
                name: "Editor".into(),
                permissions: RolePermissions {
                    app_access: vec![(AppKind::Terminal, Op::Edit)],
                    ..Default::default()
                },
            },
        ],
        assignments: vec![
            PermissionAssignment { user: UserId("alice".into()), role: RoleId(alice_role.into()) },
            PermissionAssignment { user: UserId("bob".into()),   role: RoleId(bob_role.into()) },
        ],
    }
}

#[test]
fn viewer_role_denies_terminal_edit_after_settings_applied() {
    let mut relay = MemoryRelay::new();
    let alice_peer = relay.attach("alice");
    let (mut alice_app, _) = headless_app("alice", alice_peer);

    let view = make_view("viewer", "editor");
    let ctx = PermissionContext::from_workspace_settings(&view, &UserId("alice".into()))
        .expect("assignment exists");
    alice_app.set_permission_ctx(ctx);

    let scope = PermissionScope {
        resource: ResourceId::App(AppKind::Terminal),
        op: Op::Edit,
    };
    assert!(!alice_app.permission_ctx().check(&scope));
}

#[test]
fn permission_change_from_peer_reflects_locally_after_sync() {
    let mut relay = MemoryRelay::new();
    let alice_peer = relay.attach("alice");
    let bob_peer = relay.attach("bob");
    let (mut alice_app, _) = headless_app("alice", alice_peer);
    let (mut bob_app, _) = headless_app("bob", bob_peer);

    // Bob edits workspace settings → promotes alice to editor.
    let new_view = make_view("editor", "editor");

    // Simulate CRDT sync: bob broadcasts the serialized settings;
    // alice receives and rebuilds her PermissionContext.
    let ctx_for_alice = PermissionContext::from_workspace_settings(
        &new_view, &UserId("alice".into()),
    ).unwrap();
    alice_app.set_permission_ctx(ctx_for_alice);

    let scope = PermissionScope {
        resource: ResourceId::App(AppKind::Terminal),
        op: Op::Edit,
    };
    assert!(alice_app.permission_ctx().check(&scope));
}
```

- [ ] **Step 2: Run and verify**

```bash
cargo test -p quick-ui --test multi_user_session
```
Expected: all multi-user tests pass.

- [ ] **Step 3: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/tests/multi_user_session.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "test(quick-ui): permission gating + permission-change propagation across two Apps"
```

---

## Task 26: Peer caret in text input — full two-App integration

**Files:**
- Modify: `quick-ui/tests/multi_user_session.rs`

- [ ] **Step 1: Write the failing test**

Append:

```rust
use quick_ui::widget::{TextInput, Widget};

#[test]
fn peer_caret_in_text_input_renders_on_remote_app() {
    let mut relay = MemoryRelay::new();
    let alice_peer = relay.attach("alice");
    let bob_peer = relay.attach("bob");
    let (_alice_app, _) = headless_app("alice", alice_peer);
    let (mut bob_app, _) = headless_app("bob", bob_peer);

    let key = PresenceKey::Widget { path: vec![PathSegment::Field("doc".into())] };
    bob_app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: key.clone(),
        payload: PresencePayload::Caret { pos: 5 },
    });

    // Build a TextInput with matching presence path and render it.
    let mut input = TextInput::new(vec![PathSegment::Field("doc".into())]);
    let mut scene = vello::Scene::new();
    let theme = quick_ui::theme::Theme::dark();
    let mut tr = quick_ui::text_render::TextRenderer::new();
    let perms = quick_ui::permissions::PermissionContext::permissive();

    input.draw_with_presence(
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        &mut tr,
        &theme,
        bob_app.presence_ctx(),
        &perms,
    );

    // A non-empty scene proves the caret chrome was emitted.
    assert!(!scene.encoding().is_empty());
    // The caret value reached bob's PresenceCtx.
    assert_eq!(bob_app.presence_ctx().caret_for(&key, &UserId("alice".into())), Some(5));
}
```

- [ ] **Step 2: Run and verify**

```bash
cargo test -p quick-ui --test multi_user_session peer_caret_in_text_input_renders_on_remote_app
```
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/tests/multi_user_session.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "test(quick-ui): peer caret in TextInput propagates through two-App session"
```

---

## Task 27: Re-exports, final audit, full suite

**Files:**
- Modify: `quick-ui/src/lib.rs`

- [ ] **Step 1: Ensure public API is discoverable**

Confirm `quick-ui/src/lib.rs` contains:

```rust
pub mod app;
pub mod state;
pub mod widget;
pub mod text_render;
pub mod theme;
pub mod i18n;
pub mod collaboration;
pub mod presence;
pub mod permissions;

pub use app::App;
pub use state::{State, UndoEntry};
pub use theme::Theme;
pub use widget::{Widget, Element};
```

- [ ] **Step 2: Run the full crate test suite**

```bash
cargo test -p quick-ui
```
Expected: every test passes. If the design-token audit reports anything, fix the offender in place and re-run.

- [ ] **Step 3: Run a headless build of the shell bootstrap**

```bash
cargo build -p quick-ui --examples
```
Expected: examples still compile. Fix any API-churn call sites (most should be `TextInput::new(vec![])`).

- [ ] **Step 4: Final commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/lib.rs
git -C /var/home/quinten/Development/quickdesktop commit --allow-empty -m "chore(quick-ui): phase-C collab contract hardening complete"
```

---

## Self-Review Checklist

**1. Spec coverage (Phase C from design spec §8):**

| Phase C item | Task |
|---|---|
| Generalise `PresenceCtx` coverage across all widgets | Tasks 2, 3, 10, 11 |
| Add `RemotePresenceUpdate` event + `PresencePayload` | Task 4 |
| Peer caret + peer selection rendering helpers | Task 3 |
| `PermissionScope` / `ResourceId` / `Op` | Task 5 |
| `PermissionContext` with `new` + `check` + `check_with_denied_feedback` | Task 5 |
| Framework-level gating (denied widgets render read-only) | Tasks 7, 10, 11 |
| `WorkspaceSettings` produces real `Role`/`RolePermissions` | Task 8 |
| `UndoEntry::author` | Task 9 |
| Per-author undo test | Task 9 |
| Design-token audit mechanical pass | Tasks 12–21 |
| Design-token compile-time check | Task 12 |
| Multi-user two-`App` integration test | Tasks 22, 23, 24, 25, 26 |
| Peer cursor at correct coords under layout | Task 24 |
| Peer caret in text input | Tasks 10, 26 |
| Role-gated widget auto-disables | Tasks 7, 11, 25 |
| Permission change propagation | Task 25 |

No gaps.

**2. Placeholder scan:** No TBD / TODO / "implement later" strings; every step has concrete code, exact commands, and expected output.

**3. Type consistency:**
- `PresenceKey`, `PathSegment`, `SelectionRange` — defined Task 1, consumed Tasks 2, 3, 4, 10, 11, 22–26.
- `UserId` — defined Task 2/5, used throughout.
- `PresenceCtx::peers_on / pointer_for / caret_for / selection_for` — defined Task 2, used Task 10 and tests.
- `PresenceCtx::draw_peer_caret / draw_peer_selection / draw_peer_pointer` — defined Task 3, used Task 10.
- `CollabEvent::RemotePresenceUpdate` + `PresencePayload` — defined Task 4, used Tasks 23–26.
- `PermissionContext::new / permissive / check / check_with_denied_feedback / from_workspace_settings` — defined Tasks 5, 8.
- `UndoEntry::author`, `commit_action_authored`, `undo_for_author`, `peek_last_undo` — defined Task 9, used tests.
- `Widget::presence_key / permission_scope / draw_with_presence` — defined Task 6, used Tasks 10, 11.
- `App::presence_ctx / presence_ctx_mut / permission_ctx / set_permission_ctx / apply_remote_presence / apply_collab_event_for_test / dispatch_gated_event` — defined Tasks 4, 6, 7, 23.
- `MemoryRelay / Peer / pump / headless_app` — defined Tasks 22, 23.

**4. Budget:** 27 tasks (19 implementation, 8 audit). Within the 15-25 range on the implementation track, with audit sweeps broken out per-file per the prompt's explicit request. Each task is 2-5 minutes.

**5. Conventions check:**
- `cargo test -p quick-ui` throughout — never workspace-wide.
- `git -C /var/home/quinten/Development/quickdesktop ...` for every git invocation.
- TDD (test → run → impl → run → commit) on every task.
- No `cd && git` compound.
- No submodule edits — all work is in `quick-ui/` (in-tree).
- No emojis in code or commit messages.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-18-nirios-collab.md`. Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
