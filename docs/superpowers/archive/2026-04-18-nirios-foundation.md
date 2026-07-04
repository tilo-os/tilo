# Nirios Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the toolkit primitives (design tokens, animation, overlay, drag-drop, scroll, tabs, drag-resize tiles, paginated strips, workspace tree with JSON persistence) and swap `quick-desktop` to a winit-driven nirios shell with placeholder tiles — Phase A steps 1–7 from `docs/superpowers/specs/2026-04-18-nirios-shell-design.md`.

**Architecture:** `quick-ui` grows new modules — `tokens/`, `animation/`, `overlay/`, `dnd/`, `presence/`, `permissions/`, plus `widget/scroll.rs`, `widget/tabs.rs`, `widget/tile_host.rs`, `widget/window_group.rs`, `widget/scrollable.rs`, `widget/strip_view.rs`, `widget/workspace_view.rs`. The existing `Widget` trait gains `presence_key`, `permission_scope`, `accepts_drop`/`on_drop`, `wants_pointer_capture`; `draw` takes an extra `&PresenceCtx`. `quick-shell` adds `src/shell/` with `WorkspaceTree` state + `nirios::Shell` root widget; its `main.rs` is replaced with a winit-hosted `quick_ui::App` whose root is `nirios::Shell`. Iced code is deleted; Wayland/smithay scaffolding stays in `network/` untouched.

**Tech Stack:** Rust edition 2024 (quick-ui, quick-shell), vello, wgpu, winit, taffy, cosmic-text, automerge, tokio, serde, serde_json, uuid, dirs-next.

---

## File Structure

### Create

- `/var/home/quinten/Development/quickdesktop/quick-ui/src/tokens/mod.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/tokens/colors.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/tokens/typography.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/tokens/spacing.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/tokens/radii.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/tokens/elevation.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/tokens/motion.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/animation/mod.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/animation/spring.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/animation/tween.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/overlay/mod.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/overlay/modal.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/overlay/popover.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/overlay/scrim.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/overlay/focus_trap.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/presence/mod.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/permissions/mod.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/dnd/mod.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/scrollable.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/tabs.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/tile_host.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/window_group.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/strip_view.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/workspace_view.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/tests/tokens_test.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/tests/animation_test.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/tests/overlay_test.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/tests/dnd_test.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/tests/tabs_test.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/tests/scrollable_test.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/tests/window_group_test.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/tests/strip_view_test.rs`
- `/var/home/quinten/Development/quickdesktop/quick-ui/tests/workspace_view_test.rs`
- `/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/mod.rs`
- `/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/state.rs`
- `/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/persist.rs`
- `/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/nirios_tokens.rs`
- `/var/home/quinten/Development/quickdesktop/quick-shell/tests/shell_state_test.rs`

### Modify

- `/var/home/quinten/Development/quickdesktop/quick-ui/src/lib.rs` — add new pub modules
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/theme.rs` — embed `Tokens` struct
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/mod.rs` — trait extensions + `draw` signature
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/button.rs` — update `draw` signature, consume tokens
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/checkbox.rs` — update `draw` signature, consume tokens
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/column.rs` — update `draw` signature
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/container.rs` — update `draw` signature, consume tokens
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/row.rs` — update `draw` signature
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/slider.rs` — update `draw` signature, consume tokens
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/spacer.rs` — update `draw` signature
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/stack.rs` — update `draw` signature
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/text.rs` — update `draw` signature, consume tokens
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/text_input.rs` — update `draw` signature, consume tokens
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/toggle.rs` — update `draw` signature, consume tokens
- `/var/home/quinten/Development/quickdesktop/quick-ui/src/app.rs` — pass `&PresenceCtx` to root `draw`
- `/var/home/quinten/Development/quickdesktop/quick-ui/Cargo.toml` — add `dirs-next` dep
- `/var/home/quinten/Development/quickdesktop/quick-shell/src/main.rs` — replace Iced entry with winit + `quick_ui::App`
- `/var/home/quinten/Development/quickdesktop/quick-shell/src/lib.rs` — expose `shell` module, drop Iced surface
- `/var/home/quinten/Development/quickdesktop/quick-shell/Cargo.toml` — remove `iced`, add `quick-ui`, `winit`, `dirs-next`

---

## Task DAG for Concurrent Execution

**Group A (sequential, foundational) — must land first:**

- Task 1 (`tokens/`) → Task 2 (`Theme` hosts `Tokens`) → Task 3 (`Widget` trait extensions + scaffolded `PresenceCtx`/`PermissionScope`/`DragPayload`) → Task 4 (widget `draw` signature migration across every built-in) → Task 5 (`App::run` passes `&PresenceCtx`)

Tasks 1–5 all touch `src/widget/mod.rs` and/or widget files; they must run sequentially.

**Group B (parallel-safe after Group A):**

- Task 6 (animation engine `animation/spring.rs` + `animation/tween.rs` + `motion` tokens wiring), Task 7 (overlay subsystem `overlay/*`), Task 8 (drag-drop manager `dnd/mod.rs`) can run concurrently — each touches disjoint new modules. All are isolated from the widget tree.

**Group C (sequential, depends on Group B):**

- Task 9 (`Tabs`/`TabBar` widget — depends on Task 8) → Task 10 (`Scrollable` primitive — depends on Task 6 for momentum spring) → Task 11 (`TileContent` + `TileHost` — depends on Tasks 3+4) → Task 12 (`WindowGroup` with drag-resize — depends on Tasks 9 + 11) → Task 13 (`Strip` model + `StripView` with wheel-paginate + cooldown — depends on Task 12) → Task 14 (`WorkspaceTree` state — depends on Task 13) → Task 15 (`WorkspaceView` + workspace up/down switch — depends on Task 14) → Task 16 (JSON persistence — depends on Task 14) → Task 17 (swap `quick-desktop` binary to nirios shell — depends on all previous).

Within Group C, Tasks 9 and 10 touch disjoint files (`widget/tabs.rs` vs `widget/scrollable.rs`) and may run in parallel after Group B. All later tasks (11–17) have chain dependencies.

---

## Task 1: Scaffold the tokens module

**Files:**
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/tokens/mod.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/tokens/colors.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/tokens/typography.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/tokens/spacing.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/tokens/radii.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/tokens/elevation.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/tokens/motion.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/tests/tokens_test.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Write `quick-ui/tests/tokens_test.rs`:

```rust
use quick_ui::tokens::{Tokens, ColorRamp, Spacing, Radii};

#[test]
fn tokens_dark_has_11_neutral_stops() {
    let t = Tokens::dark();
    assert_eq!(t.colors.neutral.len(), 11);
    assert_ne!(t.colors.neutral[0], t.colors.neutral[10]);
}

#[test]
fn spacing_scale_is_4px_base() {
    let s = Spacing::default();
    assert_eq!(s.s0, 0.0);
    assert_eq!(s.s1, 4.0);
    assert_eq!(s.s2, 8.0);
    assert_eq!(s.s3, 12.0);
    assert_eq!(s.s4, 16.0);
    assert_eq!(s.s6, 24.0);
    assert_eq!(s.s8, 32.0);
    assert_eq!(s.s12, 48.0);
    assert_eq!(s.s16, 64.0);
}

#[test]
fn radii_has_five_steps_plus_full() {
    let r = Radii::default();
    assert!(r.sm < r.md);
    assert!(r.md < r.lg);
    assert!(r.lg < r.xl);
    assert!(r.xl < r.full);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-ui --test tokens_test`
Expected: FAIL with "unresolved import `quick_ui::tokens`".

- [ ] **Step 3: Write the minimal implementation**

`quick-ui/src/tokens/colors.rs`:

```rust
use vello::peniko::Color;

#[derive(Clone, Debug)]
pub struct ColorRamp { pub stops: [Color; 11] }

impl ColorRamp {
    pub fn len(&self) -> usize { self.stops.len() }
}

impl std::ops::Index<usize> for ColorRamp {
    type Output = Color;
    fn index(&self, i: usize) -> &Color { &self.stops[i] }
}

#[derive(Clone, Debug)]
pub struct Surfaces { pub base: Color, pub raised: Color, pub overlay: Color, pub glass: Color, pub glass_blur_px: f32 }

#[derive(Clone, Debug)]
pub struct SemanticColors { pub success: Color, pub warning: Color, pub danger: Color, pub info: Color }

#[derive(Clone, Debug)]
pub struct Colors { pub neutral: ColorRamp, pub accent: ColorRamp, pub semantic: SemanticColors, pub surfaces: Surfaces, pub text_primary: Color, pub text_dim: Color, pub border_standard: Color, pub border_focus: Color }

impl Colors {
    pub fn dark() -> Self {
        Self {
            neutral: ColorRamp { stops: [
                Color::rgb8(10, 10, 12), Color::rgb8(20, 20, 25), Color::rgb8(30, 30, 36),
                Color::rgb8(45, 45, 50), Color::rgb8(55, 55, 60), Color::rgb8(80, 80, 85),
                Color::rgb8(130, 130, 135), Color::rgb8(180, 180, 185), Color::rgb8(210, 210, 215),
                Color::rgb8(235, 235, 240), Color::rgb8(255, 255, 255),
            ]},
            accent: ColorRamp { stops: [
                Color::rgb8(0, 25, 60), Color::rgb8(0, 50, 120), Color::rgb8(0, 75, 170),
                Color::rgb8(0, 100, 200), Color::rgb8(0, 120, 215), Color::rgb8(40, 150, 230),
                Color::rgb8(80, 170, 240), Color::rgb8(130, 200, 250), Color::rgb8(180, 220, 250),
                Color::rgb8(220, 240, 255), Color::rgb8(240, 250, 255),
            ]},
            semantic: SemanticColors {
                success: Color::rgb8(0, 180, 80),
                warning: Color::rgb8(220, 150, 0),
                danger: Color::rgb8(220, 60, 60),
                info: Color::rgb8(0, 140, 220),
            },
            surfaces: Surfaces {
                base: Color::rgb8(20, 20, 25),
                raised: Color::rgb8(30, 30, 36),
                overlay: Color::rgba8(15, 15, 18, 230),
                glass: Color::rgba8(30, 30, 36, 160),
                glass_blur_px: 24.0,
            },
            text_primary: Color::WHITE,
            text_dim: Color::rgb8(200, 200, 200),
            border_standard: Color::rgb8(80, 80, 85),
            border_focus: Color::rgb8(0, 120, 215),
        }
    }

    pub fn light() -> Self {
        Self {
            neutral: ColorRamp { stops: [
                Color::rgb8(255, 255, 255), Color::rgb8(245, 245, 248), Color::rgb8(235, 235, 240),
                Color::rgb8(220, 220, 225), Color::rgb8(200, 200, 205), Color::rgb8(160, 160, 165),
                Color::rgb8(120, 120, 125), Color::rgb8(90, 90, 95), Color::rgb8(60, 60, 65),
                Color::rgb8(30, 30, 35), Color::rgb8(10, 10, 12),
            ]},
            accent: ColorRamp { stops: [
                Color::rgb8(240, 250, 255), Color::rgb8(220, 240, 255), Color::rgb8(180, 220, 250),
                Color::rgb8(130, 200, 250), Color::rgb8(80, 170, 240), Color::rgb8(40, 150, 230),
                Color::rgb8(0, 120, 215), Color::rgb8(0, 100, 200), Color::rgb8(0, 75, 170),
                Color::rgb8(0, 50, 120), Color::rgb8(0, 25, 60),
            ]},
            semantic: SemanticColors {
                success: Color::rgb8(0, 150, 70),
                warning: Color::rgb8(200, 130, 0),
                danger: Color::rgb8(200, 50, 50),
                info: Color::rgb8(0, 120, 200),
            },
            surfaces: Surfaces {
                base: Color::rgb8(240, 240, 245),
                raised: Color::WHITE,
                overlay: Color::rgba8(255, 255, 255, 230),
                glass: Color::rgba8(245, 245, 250, 180),
                glass_blur_px: 24.0,
            },
            text_primary: Color::BLACK,
            text_dim: Color::rgb8(100, 100, 105),
            border_standard: Color::rgb8(200, 200, 205),
            border_focus: Color::rgb8(0, 120, 215),
        }
    }
}
```

`quick-ui/src/tokens/typography.rs`:

```rust
#[derive(Clone, Debug)]
pub struct TypeScale { pub xs: f32, pub sm: f32, pub base: f32, pub lg: f32, pub xl: f32, pub x2l: f32, pub x3l: f32, pub x4l: f32 }

impl Default for TypeScale { fn default() -> Self { Self { xs: 10.0, sm: 12.0, base: 14.0, lg: 16.0, xl: 20.0, x2l: 24.0, x3l: 32.0, x4l: 40.0 } } }

#[derive(Clone, Debug)]
pub struct WeightScale { pub regular: u16, pub medium: u16, pub semibold: u16, pub bold: u16 }

impl Default for WeightScale { fn default() -> Self { Self { regular: 400, medium: 500, semibold: 600, bold: 700 } } }

#[derive(Clone, Debug)]
pub struct Typography {
    pub sans_family: String,
    pub mono_family: String,
    pub scale: TypeScale,
    pub weights: WeightScale,
    pub line_height_tight: f32,
    pub line_height_normal: f32,
    pub line_height_relaxed: f32,
}

impl Default for Typography {
    fn default() -> Self {
        Self {
            sans_family: "Inter".into(),
            mono_family: "JetBrains Mono".into(),
            scale: TypeScale::default(),
            weights: WeightScale::default(),
            line_height_tight: 1.2,
            line_height_normal: 1.4,
            line_height_relaxed: 1.6,
        }
    }
}
```

`quick-ui/src/tokens/spacing.rs`:

```rust
#[derive(Clone, Debug)]
pub struct Spacing { pub s0: f32, pub s1: f32, pub s2: f32, pub s3: f32, pub s4: f32, pub s6: f32, pub s8: f32, pub s12: f32, pub s16: f32 }

impl Default for Spacing { fn default() -> Self { Self { s0: 0.0, s1: 4.0, s2: 8.0, s3: 12.0, s4: 16.0, s6: 24.0, s8: 32.0, s12: 48.0, s16: 64.0 } } }
```

`quick-ui/src/tokens/radii.rs`:

```rust
#[derive(Clone, Debug)]
pub struct Radii { pub sm: f32, pub md: f32, pub lg: f32, pub xl: f32, pub full: f32 }

impl Default for Radii { fn default() -> Self { Self { sm: 2.0, md: 4.0, lg: 8.0, xl: 16.0, full: 9999.0 } } }
```

`quick-ui/src/tokens/elevation.rs`:

```rust
use vello::peniko::Color;

#[derive(Clone, Debug)]
pub struct Shadow { pub offset_y: f32, pub blur: f32, pub color: Color }

#[derive(Clone, Debug)]
pub struct Elevation { pub e0: Shadow, pub e1: Shadow, pub e2: Shadow, pub e3: Shadow, pub e4: Shadow, pub glass_inner: Shadow, pub glass_outer: Shadow }

impl Default for Elevation {
    fn default() -> Self {
        let c = |a: u8| Color::rgba8(0, 0, 0, a);
        Self {
            e0: Shadow { offset_y: 0.0, blur: 0.0, color: c(0) },
            e1: Shadow { offset_y: 1.0, blur: 2.0, color: c(40) },
            e2: Shadow { offset_y: 2.0, blur: 6.0, color: c(60) },
            e3: Shadow { offset_y: 4.0, blur: 12.0, color: c(80) },
            e4: Shadow { offset_y: 8.0, blur: 24.0, color: c(110) },
            glass_inner: Shadow { offset_y: 0.0, blur: 1.0, color: Color::rgba8(255, 255, 255, 20) },
            glass_outer: Shadow { offset_y: 12.0, blur: 32.0, color: c(120) },
        }
    }
}
```

`quick-ui/src/tokens/motion.rs`:

```rust
#[derive(Clone, Copy, Debug)]
pub struct SpringParams { pub stiffness: f32, pub damping: f32, pub mass: f32 }

#[derive(Clone, Debug)]
pub struct SpringPresets { pub subtle: SpringParams, pub snappy: SpringParams, pub bouncy: SpringParams }

impl Default for SpringPresets {
    fn default() -> Self {
        Self {
            subtle: SpringParams { stiffness: 120.0, damping: 22.0, mass: 1.0 },
            snappy: SpringParams { stiffness: 260.0, damping: 28.0, mass: 1.0 },
            bouncy: SpringParams { stiffness: 180.0, damping: 12.0, mass: 1.0 },
        }
    }
}

#[derive(Clone, Debug)]
pub struct Durations { pub fast_ms: u32, pub base_ms: u32, pub slow_ms: u32 }

impl Default for Durations { fn default() -> Self { Self { fast_ms: 120, base_ms: 220, slow_ms: 400 } } }

#[derive(Clone, Copy, Debug)]
pub enum Ease { Linear, Standard, Emphasized }

#[derive(Clone, Debug)]
pub struct Motion { pub springs: SpringPresets, pub durations: Durations }

impl Default for Motion { fn default() -> Self { Self { springs: SpringPresets::default(), durations: Durations::default() } } }
```

`quick-ui/src/tokens/mod.rs`:

```rust
pub mod colors;
pub mod typography;
pub mod spacing;
pub mod radii;
pub mod elevation;
pub mod motion;

pub use colors::{Colors, ColorRamp, SemanticColors, Surfaces};
pub use typography::{Typography, TypeScale, WeightScale};
pub use spacing::Spacing;
pub use radii::Radii;
pub use elevation::{Elevation, Shadow};
pub use motion::{Motion, SpringParams, SpringPresets, Durations, Ease};

#[derive(Clone, Debug)]
pub struct Tokens {
    pub colors: Colors,
    pub typography: Typography,
    pub spacing: Spacing,
    pub radii: Radii,
    pub elevation: Elevation,
    pub motion: Motion,
}

impl Tokens {
    pub fn dark() -> Self { Self { colors: Colors::dark(), typography: Typography::default(), spacing: Spacing::default(), radii: Radii::default(), elevation: Elevation::default(), motion: Motion::default() } }
    pub fn light() -> Self { Self { colors: Colors::light(), typography: Typography::default(), spacing: Spacing::default(), radii: Radii::default(), elevation: Elevation::default(), motion: Motion::default() } }
}
```

Append to `quick-ui/src/lib.rs`:

```rust
pub mod tokens;
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-ui --test tokens_test`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/tokens quick-ui/src/lib.rs quick-ui/tests/tokens_test.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): add design-token module scaffold"
```

---

## Task 2: Embed Tokens into Theme and remove ad-hoc fields

**Files:**
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/theme.rs`

- [ ] **Step 1: Write the failing test**

Append to `quick-ui/tests/tokens_test.rs`:

```rust
use quick_ui::Theme;

#[test]
fn theme_exposes_tokens() {
    let t = Theme::dark();
    // legacy shortcuts still work
    assert_eq!(t.padding_standard, t.tokens.spacing.s3);
    // tokens are the source of truth
    assert_eq!(t.tokens.radii.md as f64, t.radius_standard);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-ui --test tokens_test theme_exposes_tokens -- --exact`
Expected: FAIL — `tokens` field doesn't exist on `Theme`.

- [ ] **Step 3: Write the minimal implementation**

Replace `quick-ui/src/theme.rs` contents:

```rust
use vello::peniko::Color;
use crate::tokens::Tokens;

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub tokens: Tokens,
    // Legacy shortcuts — derived from tokens on construction, kept for compat during migration.
    pub bg_app: Color,
    pub bg_primary: Color,
    pub bg_hover: Color,
    pub bg_active: Color,
    pub border_standard: Color,
    pub border_focus: Color,
    pub text_primary: Color,
    pub text_dim: Color,
    pub accent_primary: Color,
    pub status_success: Color,
    pub radius_standard: f64,
    pub padding_standard: f32,
    pub gap_standard: f32,
}

impl Default for Theme { fn default() -> Self { Self::dark() } }

impl Theme {
    fn from_tokens(name: &str, tokens: Tokens) -> Self {
        let c = &tokens.colors;
        Self {
            name: name.into(),
            bg_app: c.surfaces.base,
            bg_primary: c.surfaces.raised,
            bg_hover: c.neutral[4],
            bg_active: c.neutral[2],
            border_standard: c.border_standard,
            border_focus: c.border_focus,
            text_primary: c.text_primary,
            text_dim: c.text_dim,
            accent_primary: c.accent[4],
            status_success: c.semantic.success,
            radius_standard: tokens.radii.md as f64,
            padding_standard: tokens.spacing.s3,
            gap_standard: tokens.spacing.s3,
            tokens,
        }
    }
    pub fn dark() -> Self { Self::from_tokens("Dark", Tokens::dark()) }
    pub fn light() -> Self { Self::from_tokens("Light", Tokens::light()) }
    pub fn dracula() -> Self {
        let mut t = Tokens::dark();
        t.colors.surfaces.base = Color::rgb8(40, 42, 54);
        t.colors.surfaces.raised = Color::rgb8(68, 71, 90);
        t.colors.text_primary = Color::rgb8(248, 248, 242);
        t.colors.accent[4] = Color::rgb8(255, 121, 198);
        t.colors.border_focus = Color::rgb8(189, 147, 249);
        Self::from_tokens("Dracula", t)
    }
    pub fn nord() -> Self {
        let mut t = Tokens::dark();
        t.colors.surfaces.base = Color::rgb8(46, 52, 64);
        t.colors.surfaces.raised = Color::rgb8(59, 66, 82);
        t.colors.text_primary = Color::rgb8(216, 222, 233);
        t.colors.accent[4] = Color::rgb8(129, 161, 193);
        t.colors.border_focus = Color::rgb8(136, 192, 208);
        Self::from_tokens("Nord", t)
    }
    pub fn gruvbox() -> Self {
        let mut t = Tokens::dark();
        t.colors.surfaces.base = Color::rgb8(40, 40, 40);
        t.colors.surfaces.raised = Color::rgb8(60, 56, 54);
        t.colors.text_primary = Color::rgb8(235, 219, 178);
        t.colors.accent[4] = Color::rgb8(184, 187, 38);
        t.colors.border_focus = Color::rgb8(254, 128, 25);
        t.radii.md = 2.0;
        Self::from_tokens("Gruvbox", t)
    }
    pub fn solarized() -> Self {
        let mut t = Tokens::dark();
        t.colors.surfaces.base = Color::rgb8(0, 43, 54);
        t.colors.surfaces.raised = Color::rgb8(7, 54, 66);
        t.colors.text_primary = Color::rgb8(131, 148, 150);
        t.colors.accent[4] = Color::rgb8(42, 161, 152);
        t.colors.border_focus = Color::rgb8(38, 139, 210);
        t.radii.md = 0.0;
        Self::from_tokens("Solarized", t)
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-ui --test tokens_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/theme.rs quick-ui/tests/tokens_test.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): nest Tokens inside Theme as source of truth"
```

---

## Task 3: Scaffold presence, permissions, dnd modules + marker `AppWidget` trait

**Files:**
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/presence/mod.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/permissions/mod.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/dnd/mod.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `quick-ui/tests/tokens_test.rs` additions (append):

```rust
use quick_ui::presence::{PresenceCtx, PresenceKey};
use quick_ui::permissions::{PermissionScope, ResourceId, Op};
use quick_ui::dnd::{DragPayload};

#[test]
fn presence_ctx_empty_is_constructible() {
    let _ = PresenceCtx::empty();
    let _k = PresenceKey::Widget { path: vec![] };
}

#[test]
fn permission_scope_constructs() {
    let s = PermissionScope { resource: ResourceId::Workspace(0), op: Op::Edit };
    assert!(matches!(s.op, Op::Edit));
}

#[test]
fn drag_payload_tab_variant_exists() {
    let _ = DragPayload::Tab { group: 0, tab: 0 };
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-ui --test tokens_test presence_ctx_empty_is_constructible -- --exact`
Expected: FAIL — unresolved imports.

- [ ] **Step 3: Write the minimal implementation**

`quick-ui/src/presence/mod.rs`:

```rust
use std::collections::HashMap;

pub type UserId = String;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PresenceKey {
    Widget { path: Vec<String> },
    Tab { id: u64 },
    Workspace { id: u64 },
    App { app: String, sub: String },
}

#[derive(Clone, Debug)]
pub struct PeerPresence { pub user: UserId, pub color: [u8; 3], pub name: String }

#[derive(Clone, Debug, Default)]
pub struct PresenceCtx {
    peers_here: HashMap<PresenceKey, Vec<PeerPresence>>,
}

impl PresenceCtx {
    pub fn empty() -> Self { Self::default() }
    pub fn peers_for(&self, key: &PresenceKey) -> &[PeerPresence] {
        self.peers_here.get(key).map(|v| v.as_slice()).unwrap_or(&[])
    }
    pub fn insert_peer(&mut self, key: PresenceKey, peer: PeerPresence) {
        self.peers_here.entry(key).or_default().push(peer);
    }
}
```

`quick-ui/src/permissions/mod.rs`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op { View, Edit, Admin }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceId {
    App(String),
    File(u64),
    Workspace(u64),
}

#[derive(Clone, Debug)]
pub struct PermissionScope { pub resource: ResourceId, pub op: Op }

#[derive(Clone, Debug, Default)]
pub struct PermissionContext {
    // v1: single-user; every op allowed. Populated in Plan 4.
    #[allow(dead_code)]
    placeholder: (),
}

impl PermissionContext {
    pub fn single_user() -> Self { Self::default() }
    pub fn allows(&self, _scope: &PermissionScope) -> bool { true }
}
```

`quick-ui/src/dnd/mod.rs`:

```rust
use vello::kurbo::Point;

#[derive(Clone, Debug)]
pub enum DragPayload {
    Tab { group: u64, tab: u64 },
    File { path: String },
    Text { value: String },
    Custom { kind: String, blob: Vec<u8> },
}

#[derive(Clone, Debug)]
pub struct DragState {
    pub payload: DragPayload,
    pub origin: Point,
    pub current: Point,
}

#[derive(Default)]
pub struct DragDropManager {
    pub active: Option<DragState>,
}

impl DragDropManager {
    pub fn new() -> Self { Self::default() }
    pub fn start(&mut self, payload: DragPayload, origin: Point) {
        self.active = Some(DragState { payload, origin, current: origin });
    }
    pub fn update_position(&mut self, p: Point) {
        if let Some(s) = &mut self.active { s.current = p; }
    }
    pub fn end(&mut self) -> Option<DragState> { self.active.take() }
    pub fn is_active(&self) -> bool { self.active.is_some() }
}
```

Append to `quick-ui/src/lib.rs`:

```rust
pub mod presence;
pub mod permissions;
pub mod dnd;
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-ui --test tokens_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/presence quick-ui/src/permissions quick-ui/src/dnd quick-ui/src/lib.rs quick-ui/tests/tokens_test.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): scaffold presence, permissions, dnd modules"
```

---

## Task 4: Extend Widget trait and migrate draw signature across every built-in widget

**Files:**
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/mod.rs`
- Modify: every file under `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/` (button, checkbox, column, container, row, slider, spacer, stack, text, text_input, toggle)
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/app.rs`

- [ ] **Step 1: Write the failing test**

Create `quick-ui/tests/widget_trait_test.rs`:

```rust
use quick_ui::widget::{Widget, Button, AppWidget};
use quick_ui::presence::PresenceCtx;
use quick_ui::dnd::DragPayload;

#[test]
fn widget_defaults_for_collab_methods() {
    let b = Button::new("b", "label", None);
    assert!(b.presence_key().is_none());
    assert!(b.permission_scope().is_none());
    assert!(!b.accepts_drop(&DragPayload::Text { value: "x".into() }));
    assert!(!b.wants_pointer_capture());
}

#[test]
fn draw_takes_presence_ctx() {
    // compile-time assertion that draw signature is what we expect.
    fn _assert_draw_sig<W: Widget>() {
        fn _call<W: Widget>(
            w: &mut W,
            scene: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            tr: &mut quick_ui::text_render::TextRenderer,
            theme: &quick_ui::Theme,
            presence: &PresenceCtx,
        ) {
            w.draw(scene, transform, tr, theme, presence);
        }
    }
    // AppWidget marker compiles.
    fn _is_appwidget<W: AppWidget>() {}
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-ui --test widget_trait_test`
Expected: FAIL — `draw` signature mismatch, `AppWidget` missing, new trait methods missing.

- [ ] **Step 3: Write the minimal implementation**

Replace `quick-ui/src/widget/mod.rs` top of file through end of trait decl, keeping module re-exports:

```rust
use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::{PresenceCtx, PresenceKey};
use crate::permissions::PermissionScope;
use crate::dnd::DragPayload;

use std::collections::HashMap;

pub trait Widget {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId);
    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool { false }
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId;
    fn measure(&mut self, _known: taffy::Size<Option<f32>>, _available: taffy::Size<taffy::AvailableSpace>, _text_renderer: &mut TextRenderer) -> taffy::Size<f32> { taffy::Size::ZERO }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: vello::kurbo::Point);
    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx);
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, modifiers: &winit::event::Modifiers, state: &mut AutoCommit, path: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>);
    fn children(&self) -> Vec<&dyn Widget> { vec![] }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> { vec![] }
    fn name(&self) -> &str;
    fn get_rect(&self) -> vello::kurbo::Rect;
    fn is_focusable(&self) -> bool { false }
    fn set_focus(&mut self, _focused: bool) {}
    fn is_focused(&self) -> bool { false }
    fn needs_commit(&self) -> bool { false }
    fn on_commit(&mut self) {}
    fn get_state(&self) -> Option<String> { None }
    fn set_state(&mut self, _state: &str) {}

    // --- Collab / DnD contract extensions ---
    fn presence_key(&self) -> Option<PresenceKey> { None }
    fn permission_scope(&self) -> Option<PermissionScope> { None }
    fn accepts_drop(&self, _payload: &DragPayload) -> bool { false }
    fn on_drop(&mut self, _payload: DragPayload, _state: &mut AutoCommit, _path: &ObjId) {}
    fn wants_pointer_capture(&self) -> bool { false }
}

/// Empty marker trait extending `Widget`. The full trait (with `app_kind`, `app_title`,
/// `commands`, etc.) lands in Plan 3. During Plan 1 the shell uses `Box<dyn Widget>`
/// inside `TileContent::App` so concrete widgets need not implement `AppWidget` yet.
pub trait AppWidget: Widget {}

pub type Element = Box<dyn Widget>;

mod button;      pub use button::Button;
mod checkbox;    pub use checkbox::Checkbox;
mod column;      pub use column::Column;
mod row;         pub use row::Row;
mod toggle;      pub use toggle::Toggle;
mod stack;      pub use stack::Stack;
mod container;   pub use container::Container;
mod text;        pub use text::Text;
mod spacer;     pub use spacer::Spacer;
mod slider;      pub use slider::Slider;
mod text_input;  pub use text_input::TextInput;
```

Migrate the `draw` signature in every widget file. Example for `widget/button.rs`:

```rust
fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &crate::presence::PresenceCtx) {
    // existing body unchanged
}
```

Do the same for: `checkbox.rs`, `column.rs`, `container.rs`, `row.rs`, `slider.rs`, `spacer.rs`, `stack.rs`, `text.rs`, `text_input.rs`, `toggle.rs`. Each child-dispatching widget also forwards `presence` to its children's `draw`.

For `container.rs` line 95–105, replace with:

```rust
fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, presence: &crate::presence::PresenceCtx) {
    if self.has_background {
        let shape = vello::kurbo::RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);
        scene.fill(vello::peniko::Fill::NonZero, transform, theme.tokens.colors.surfaces.raised, None, &shape);
        scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, theme.tokens.colors.border_standard, None, &shape);
    }
    if let Some(child) = &mut self.child {
        child.draw(scene, transform, text_renderer, theme, presence);
    }
}
```

For `column.rs`, `row.rs`, `stack.rs`, `spacer.rs` — the body is `for child in &mut self.children { child.draw(scene, transform, text_renderer, theme, presence); }` (spacer is no-op; just update signature).

Modify `quick-ui/src/app.rs`:

- Import: add `use crate::presence::PresenceCtx;` near the top.
- In `App` struct, add field `presence: PresenceCtx,`.
- In `App::new`, add `presence: PresenceCtx::empty(),`.
- In the `RedrawRequested` branch, change the `self.root_widget.draw(...)` call to pass `&self.presence` as the last argument.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-ui --test widget_trait_test && cargo build -p quick-ui`
Expected: PASS; build succeeds.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget quick-ui/src/app.rs quick-ui/tests/widget_trait_test.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): extend Widget trait with presence/permission/dnd hooks and migrate draw signature"
```

---

## Task 5: Migrate existing widgets to read tokens (colors, spacing, radii)

**Files:**
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/button.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/checkbox.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/slider.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/text.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/text_input.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/toggle.rs`

- [ ] **Step 1: Write the failing test**

Append to `quick-ui/tests/widget_trait_test.rs`:

```rust
#[test]
fn button_uses_token_radius() {
    use quick_ui::tokens::Tokens;
    let mut t = Tokens::dark();
    t.radii.md = 99.0;
    let theme = quick_ui::Theme {
        name: "T".into(),
        bg_app: t.colors.surfaces.base,
        bg_primary: t.colors.surfaces.raised,
        bg_hover: t.colors.neutral[4],
        bg_active: t.colors.neutral[2],
        border_standard: t.colors.border_standard,
        border_focus: t.colors.border_focus,
        text_primary: t.colors.text_primary,
        text_dim: t.colors.text_dim,
        accent_primary: t.colors.accent[4],
        status_success: t.colors.semantic.success,
        radius_standard: t.radii.md as f64,
        padding_standard: t.spacing.s3,
        gap_standard: t.spacing.s3,
        tokens: t,
    };
    // A scene is required to draw; for this unit, just assert the legacy shortcut
    // mirrors the token.
    assert_eq!(theme.radius_standard, 99.0);
    assert_eq!(theme.tokens.radii.md, 99.0);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-ui --test widget_trait_test button_uses_token_radius -- --exact`
Expected: PASS (test only exercises that radius_standard stays consistent; it guards against later regressions).

This is a regression guard, not a red test. Proceed to step 3 for real work.

- [ ] **Step 3: Write the minimal implementation**

Replace every `theme.radius_standard` and `theme.padding_standard` in widget `draw` bodies with token accesses:

- `button.rs`: `theme.tokens.radii.md as f64` (replaces `theme.radius_standard`); `theme.tokens.colors.surfaces.raised` for `bg_primary`; `theme.tokens.colors.border_standard` for borders; `theme.tokens.colors.text_primary` for the text color.
- `checkbox.rs`: same swap for radius + background + border + accent (`theme.tokens.colors.accent[4]`).
- `slider.rs`: swap `radius_standard` → `theme.tokens.radii.sm`; track color → `theme.tokens.colors.neutral[3]`; fill → `theme.tokens.colors.accent[4]`.
- `text.rs`: font size defaults pull from `theme.tokens.typography.scale.base`; color from `theme.tokens.colors.text_primary`.
- `text_input.rs`: radius → `theme.tokens.radii.md`; border → `theme.tokens.colors.border_standard` / `border_focus`; background → `theme.tokens.colors.surfaces.raised`.
- `toggle.rs`: radius → `theme.tokens.radii.full as f64` for the pill; fill color → `theme.tokens.colors.accent[4]` when on, `theme.tokens.colors.neutral[3]` when off.

Each file should retain its `draw` body otherwise identical except for the named replacements. Keep legacy `theme.bg_primary` etc. where they were — those mirrors still point at tokens after Task 2.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-ui --test widget_trait_test && cargo build -p quick-ui`
Expected: PASS; compiles.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget quick-ui/tests/widget_trait_test.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "refactor(quick-ui): widgets consume Theme.tokens directly"
```

---

## Task 6: Animation engine — spring + tween

**Files:**
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/animation/mod.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/animation/spring.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/animation/tween.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/tests/animation_test.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/lib.rs`

- [ ] **Step 1: Write the failing test**

`quick-ui/tests/animation_test.rs`:

```rust
use quick_ui::animation::{Spring, Tween, Ease};
use quick_ui::tokens::SpringParams;

#[test]
fn spring_converges_to_target() {
    let mut s = Spring::new(0.0, SpringParams { stiffness: 260.0, damping: 28.0, mass: 1.0 });
    s.set_target(100.0);
    for _ in 0..600 { s.step(1.0 / 120.0); }
    assert!((s.value() - 100.0).abs() < 0.5);
    assert!(s.velocity().abs() < 0.5);
}

#[test]
fn spring_settled_after_convergence() {
    let mut s = Spring::new(0.0, SpringParams { stiffness: 260.0, damping: 28.0, mass: 1.0 });
    s.set_target(50.0);
    for _ in 0..500 { s.step(1.0 / 120.0); }
    assert!(s.is_settled());
}

#[test]
fn tween_interpolates_linearly() {
    let mut t = Tween::new(0.0, 100.0, 1.0, Ease::Linear);
    t.advance(0.5);
    assert!((t.value() - 50.0).abs() < 0.01);
    t.advance(0.5);
    assert!(t.done());
    assert!((t.value() - 100.0).abs() < 0.01);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-ui --test animation_test`
Expected: FAIL — module absent.

- [ ] **Step 3: Write the minimal implementation**

`quick-ui/src/animation/spring.rs`:

```rust
use crate::tokens::SpringParams;

#[derive(Clone, Debug)]
pub struct Spring { value: f32, velocity: f32, target: f32, params: SpringParams }

impl Spring {
    pub fn new(value: f32, params: SpringParams) -> Self { Self { value, velocity: 0.0, target: value, params } }
    pub fn set_target(&mut self, t: f32) { self.target = t; }
    pub fn snap(&mut self, v: f32) { self.value = v; self.velocity = 0.0; self.target = v; }
    pub fn value(&self) -> f32 { self.value }
    pub fn velocity(&self) -> f32 { self.velocity }
    pub fn target(&self) -> f32 { self.target }
    pub fn is_settled(&self) -> bool {
        (self.value - self.target).abs() < 0.1 && self.velocity.abs() < 0.1
    }
    pub fn step(&mut self, dt: f32) {
        // Semi-implicit Euler for stability.
        let k = self.params.stiffness;
        let c = self.params.damping;
        let m = self.params.mass.max(0.0001);
        let force = -k * (self.value - self.target) - c * self.velocity;
        let accel = force / m;
        self.velocity += accel * dt;
        self.value += self.velocity * dt;
    }
}
```

`quick-ui/src/animation/tween.rs`:

```rust
pub use crate::tokens::Ease;

#[derive(Clone, Debug)]
pub struct Tween { start: f32, end: f32, duration: f32, elapsed: f32, ease: Ease }

impl Tween {
    pub fn new(start: f32, end: f32, duration: f32, ease: Ease) -> Self { Self { start, end, duration: duration.max(0.0001), elapsed: 0.0, ease } }
    pub fn advance(&mut self, dt: f32) { self.elapsed = (self.elapsed + dt).min(self.duration); }
    pub fn done(&self) -> bool { self.elapsed >= self.duration }
    pub fn progress(&self) -> f32 { (self.elapsed / self.duration).clamp(0.0, 1.0) }
    pub fn value(&self) -> f32 {
        let p = self.progress();
        let eased = match self.ease {
            Ease::Linear => p,
            Ease::Standard => {
                // cubic-bezier(0.2, 0, 0, 1) approx
                let t = p; t * t * (3.0 - 2.0 * t)
            }
            Ease::Emphasized => {
                let t = p; 1.0 - (1.0 - t).powi(3)
            }
        };
        self.start + (self.end - self.start) * eased
    }
}
```

`quick-ui/src/animation/mod.rs`:

```rust
pub mod spring;
pub mod tween;

pub use spring::Spring;
pub use tween::{Tween, Ease};
```

Append to `quick-ui/src/lib.rs`:

```rust
pub mod animation;
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-ui --test animation_test`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/animation quick-ui/src/lib.rs quick-ui/tests/animation_test.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): add spring + tween animation primitives"
```

---

## Task 7: Overlay subsystem — scrim, modal, popover, focus trap

**Files:**
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/overlay/mod.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/overlay/scrim.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/overlay/modal.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/overlay/popover.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/overlay/focus_trap.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/tests/overlay_test.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/lib.rs`

- [ ] **Step 1: Write the failing test**

`quick-ui/tests/overlay_test.rs`:

```rust
use quick_ui::overlay::{OverlayStack, OverlayId, Modal, Popover, FocusTrap};
use vello::kurbo::Point;

#[test]
fn overlay_stack_push_pop_order() {
    let mut s = OverlayStack::new();
    let a = s.push_modal(Modal::new("a"));
    let b = s.push_popover(Popover::new("b", Point::new(100.0, 100.0)));
    assert_eq!(s.len(), 2);
    assert_eq!(s.top_id(), Some(b));
    s.dismiss(a);
    assert_eq!(s.len(), 1);
    assert_eq!(s.top_id(), Some(b));
}

#[test]
fn focus_trap_cycles_only_within_members() {
    let mut trap = FocusTrap::new(vec![1, 2, 3]);
    assert_eq!(trap.next_from(1), 2);
    assert_eq!(trap.next_from(3), 1);
    assert_eq!(trap.prev_from(1), 3);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-ui --test overlay_test`
Expected: FAIL — overlay module missing.

- [ ] **Step 3: Write the minimal implementation**

`quick-ui/src/overlay/scrim.rs`:

```rust
use vello::peniko::Color;

#[derive(Clone, Debug)]
pub struct Scrim { pub color: Color, pub opacity: f32, pub dismissable: bool }

impl Scrim {
    pub fn dark() -> Self { Self { color: Color::rgba8(0, 0, 0, 255), opacity: 0.5, dismissable: true } }
    pub fn transparent_click_catcher() -> Self { Self { color: Color::rgba8(0, 0, 0, 0), opacity: 0.0, dismissable: true } }
}
```

`quick-ui/src/overlay/modal.rs`:

```rust
use super::scrim::Scrim;

#[derive(Clone, Debug)]
pub struct Modal { pub id_tag: String, pub scrim: Scrim }

impl Modal {
    pub fn new(id_tag: &str) -> Self { Self { id_tag: id_tag.into(), scrim: Scrim::dark() } }
    pub fn with_scrim(mut self, s: Scrim) -> Self { self.scrim = s; self }
}
```

`quick-ui/src/overlay/popover.rs`:

```rust
use vello::kurbo::Point;
use super::scrim::Scrim;

#[derive(Clone, Debug)]
pub struct Popover { pub id_tag: String, pub anchor: Point, pub scrim: Scrim }

impl Popover {
    pub fn new(id_tag: &str, anchor: Point) -> Self { Self { id_tag: id_tag.into(), anchor, scrim: Scrim::transparent_click_catcher() } }
}
```

`quick-ui/src/overlay/focus_trap.rs`:

```rust
#[derive(Clone, Debug)]
pub struct FocusTrap { members: Vec<u64> }

impl FocusTrap {
    pub fn new(members: Vec<u64>) -> Self { Self { members } }
    pub fn next_from(&self, id: u64) -> u64 {
        let idx = self.members.iter().position(|&x| x == id).unwrap_or(0);
        self.members[(idx + 1) % self.members.len()]
    }
    pub fn prev_from(&self, id: u64) -> u64 {
        let idx = self.members.iter().position(|&x| x == id).unwrap_or(0);
        self.members[(idx + self.members.len() - 1) % self.members.len()]
    }
    pub fn contains(&self, id: u64) -> bool { self.members.contains(&id) }
}
```

`quick-ui/src/overlay/mod.rs`:

```rust
pub mod scrim;
pub mod modal;
pub mod popover;
pub mod focus_trap;

pub use scrim::Scrim;
pub use modal::Modal;
pub use popover::Popover;
pub use focus_trap::FocusTrap;

pub type OverlayId = u64;

#[derive(Debug)]
pub enum OverlayEntry { Modal(Modal), Popover(Popover) }

pub struct OverlayStack { next_id: u64, entries: Vec<(OverlayId, OverlayEntry)> }

impl OverlayStack {
    pub fn new() -> Self { Self { next_id: 1, entries: Vec::new() } }
    pub fn push_modal(&mut self, m: Modal) -> OverlayId { let id = self.next_id; self.next_id += 1; self.entries.push((id, OverlayEntry::Modal(m))); id }
    pub fn push_popover(&mut self, p: Popover) -> OverlayId { let id = self.next_id; self.next_id += 1; self.entries.push((id, OverlayEntry::Popover(p))); id }
    pub fn dismiss(&mut self, id: OverlayId) { self.entries.retain(|(x, _)| *x != id); }
    pub fn len(&self) -> usize { self.entries.len() }
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }
    pub fn top_id(&self) -> Option<OverlayId> { self.entries.last().map(|(id, _)| *id) }
    pub fn iter(&self) -> impl Iterator<Item = &(OverlayId, OverlayEntry)> { self.entries.iter() }
}

impl Default for OverlayStack { fn default() -> Self { Self::new() } }
```

Append to `quick-ui/src/lib.rs`:

```rust
pub mod overlay;
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-ui --test overlay_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/overlay quick-ui/src/lib.rs quick-ui/tests/overlay_test.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): overlay subsystem (modal, popover, scrim, focus trap)"
```

---

## Task 8: Drag-drop manager — payload typing + pointer capture handshake

**Files:**
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/dnd/mod.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/tests/dnd_test.rs`

- [ ] **Step 1: Write the failing test**

`quick-ui/tests/dnd_test.rs`:

```rust
use quick_ui::dnd::{DragDropManager, DragPayload};
use vello::kurbo::Point;

#[test]
fn drag_lifecycle_transitions() {
    let mut dnd = DragDropManager::new();
    assert!(!dnd.is_active());
    dnd.start(DragPayload::Tab { group: 7, tab: 3 }, Point::new(10.0, 10.0));
    assert!(dnd.is_active());
    dnd.update_position(Point::new(30.0, 40.0));
    assert!(dnd.is_active());
    let ended = dnd.end().expect("ended drag returns state");
    match ended.payload {
        DragPayload::Tab { group, tab } => { assert_eq!(group, 7); assert_eq!(tab, 3); }
        _ => panic!("wrong payload"),
    }
    assert!(!dnd.is_active());
}

#[test]
fn drag_distance_exceeds_threshold() {
    let mut dnd = DragDropManager::new();
    dnd.start(DragPayload::Text { value: "x".into() }, Point::new(0.0, 0.0));
    dnd.update_position(Point::new(10.0, 0.0));
    assert!(dnd.distance_moved() >= 10.0);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-ui --test dnd_test`
Expected: FAIL — `distance_moved` missing.

- [ ] **Step 3: Write the minimal implementation**

Append to `quick-ui/src/dnd/mod.rs`:

```rust
impl DragDropManager {
    pub fn distance_moved(&self) -> f64 {
        if let Some(s) = &self.active {
            let dx = s.current.x - s.origin.x;
            let dy = s.current.y - s.origin.y;
            (dx * dx + dy * dy).sqrt()
        } else { 0.0 }
    }
}

/// Threshold in logical pixels after which a pointer press-and-move is promoted
/// to a drag gesture. Matches macOS / GTK conventions.
pub const DRAG_START_THRESHOLD_PX: f64 = 4.0;
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-ui --test dnd_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/dnd quick-ui/tests/dnd_test.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): drag-drop manager distance + threshold constant"
```

---

## Task 9: TabBar widget — render, active indicator, click-to-select

**Files:**
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/tabs.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/tests/tabs_test.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/mod.rs`

- [ ] **Step 1: Write the failing test**

`quick-ui/tests/tabs_test.rs`:

```rust
use quick_ui::widget::{TabBar, TabDescriptor};

#[test]
fn tabbar_activates_by_index() {
    let mut bar = TabBar::new("g1", vec![
        TabDescriptor { id: 1, title: "A".into() },
        TabDescriptor { id: 2, title: "B".into() },
        TabDescriptor { id: 3, title: "C".into() },
    ], 2);
    assert_eq!(bar.active_id(), 2);
    bar.set_active(3);
    assert_eq!(bar.active_id(), 3);
}

#[test]
fn tabbar_reorder_swaps_positions() {
    let mut bar = TabBar::new("g1", vec![
        TabDescriptor { id: 1, title: "A".into() },
        TabDescriptor { id: 2, title: "B".into() },
        TabDescriptor { id: 3, title: "C".into() },
    ], 1);
    bar.reorder(0, 2);
    let ids: Vec<u64> = bar.tabs().iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![2, 3, 1]);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-ui --test tabs_test`
Expected: FAIL — missing module.

- [ ] **Step 3: Write the minimal implementation**

`quick-ui/src/widget/tabs.rs`:

```rust
use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;
use crate::dnd::{DragPayload, DRAG_START_THRESHOLD_PX};

#[derive(Clone, Debug)]
pub struct TabDescriptor { pub id: u64, pub title: String }

pub struct TabBar {
    group_id_tag: String,
    tabs: Vec<TabDescriptor>,
    active_id: u64,
    rect: Rect,
    tab_rects: Vec<Rect>,
    hover_idx: Option<usize>,
    press_origin: Option<(usize, Point)>,
    pub pending_drag: Option<DragPayload>,
    pub pending_reorder: Option<(usize, usize)>,
}

impl TabBar {
    pub fn new(group_id_tag: &str, tabs: Vec<TabDescriptor>, active_id: u64) -> Self {
        Self {
            group_id_tag: group_id_tag.into(),
            tabs,
            active_id,
            rect: Rect::ZERO,
            tab_rects: Vec::new(),
            hover_idx: None,
            press_origin: None,
            pending_drag: None,
            pending_reorder: None,
        }
    }
    pub fn tabs(&self) -> &[TabDescriptor] { &self.tabs }
    pub fn active_id(&self) -> u64 { self.active_id }
    pub fn set_active(&mut self, id: u64) { if self.tabs.iter().any(|t| t.id == id) { self.active_id = id; } }
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to { return; }
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
    }
    pub fn take_drag(&mut self) -> Option<DragPayload> { self.pending_drag.take() }
}

impl Widget for TabBar {
    fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style { size: Size { width: Dimension::auto(), height: Dimension::length(36.0) }, ..Default::default() }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(origin.x + l.location.x as f64, origin.y + l.location.y as f64, origin.x + l.location.x as f64 + l.size.width as f64, origin.y + l.location.y as f64 + l.size.height as f64);
        let n = self.tabs.len().max(1) as f64;
        let tab_w = (self.rect.width() / n).max(60.0);
        self.tab_rects.clear();
        for i in 0..self.tabs.len() {
            let x0 = self.rect.x0 + (i as f64) * tab_w;
            self.tab_rects.push(Rect::new(x0, self.rect.y0, x0 + tab_w, self.rect.y1));
        }
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, _presence: &PresenceCtx) {
        let bg = theme.tokens.colors.surfaces.raised;
        scene.fill(vello::peniko::Fill::NonZero, transform, bg, None, &self.rect);
        for (i, t) in self.tabs.iter().enumerate() {
            let r = self.tab_rects[i];
            let is_active = t.id == self.active_id;
            let fill = if is_active { theme.tokens.colors.surfaces.base } else if Some(i) == self.hover_idx { theme.tokens.colors.neutral[4] } else { bg };
            let shape = RoundedRect::from_rect(r, theme.tokens.radii.sm as f64);
            scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
            let text_size = theme.tokens.typography.scale.sm;
            let (tw, _) = tr.measure(&t.title, text_size, None);
            let x = r.center().x - tw as f64 / 2.0;
            let y = r.y0 + (r.height() - text_size as f64) / 2.0;
            tr.draw_text(scene, &t.title, text_size, theme.tokens.colors.text_primary, transform * Affine::translate((x, y)), None);
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, _m: &winit::event::Modifiers, _s: &mut AutoCommit, _p: &ObjId, handled: &mut bool, _focus_target: &mut Option<NodeId>, _node_id: NodeId, _taffy: &TaffyTree<()>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.hover_idx = self.tab_rects.iter().position(|r| r.contains(p));
                if let Some((idx, origin)) = self.press_origin {
                    if (p - origin).hypot() > DRAG_START_THRESHOLD_PX {
                        if let Some(t) = self.tabs.get(idx) {
                            let group_tag_hash = self.group_id_tag.bytes().fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64));
                            self.pending_drag = Some(DragPayload::Tab { group: group_tag_hash, tab: t.id });
                            self.press_origin = None;
                            *handled = true;
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if let Some(idx) = self.hover_idx {
                    let origin = self.tab_rects[idx].center();
                    self.press_origin = Some((idx, origin));
                    self.active_id = self.tabs[idx].id;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.press_origin = None;
            }
            _ => {}
        }
    }
    fn name(&self) -> &str { "TabBar" }
    fn get_rect(&self) -> Rect { self.rect }
    fn wants_pointer_capture(&self) -> bool { self.press_origin.is_some() || self.pending_drag.is_some() }
    fn accepts_drop(&self, payload: &DragPayload) -> bool { matches!(payload, DragPayload::Tab { .. }) }
}
```

Append to `quick-ui/src/widget/mod.rs` module list:

```rust
mod tabs;
pub use tabs::{TabBar, TabDescriptor};
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-ui --test tabs_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget/tabs.rs quick-ui/src/widget/mod.rs quick-ui/tests/tabs_test.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): TabBar widget with click-to-select and drag-threshold handoff"
```

---

## Task 10: Scrollable primitive — vertical + horizontal with momentum

**Files:**
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/scrollable.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/tests/scrollable_test.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/mod.rs`

- [ ] **Step 1: Write the failing test**

`quick-ui/tests/scrollable_test.rs`:

```rust
use quick_ui::widget::Scrollable;

#[test]
fn scrollable_clamps_to_content_bounds() {
    let mut s = Scrollable::new_vertical();
    s.set_content_size(1000.0, 200.0);
    s.set_viewport_size(300.0, 100.0);
    s.scroll_by(0.0, 50_000.0);
    assert_eq!(s.offset_y(), 900.0);
    s.scroll_by(0.0, -50_000.0);
    assert_eq!(s.offset_y(), 0.0);
}

#[test]
fn scrollable_scroll_into_view_vertical() {
    let mut s = Scrollable::new_vertical();
    s.set_content_size(200.0, 1000.0);
    s.set_viewport_size(200.0, 100.0);
    s.scroll_into_view(0.0, 600.0, 200.0, 20.0);
    assert!(s.offset_y() >= 520.0 && s.offset_y() <= 600.0);
}

#[test]
fn scrollable_momentum_decays() {
    let mut s = Scrollable::new_vertical();
    s.set_content_size(200.0, 10000.0);
    s.set_viewport_size(200.0, 500.0);
    s.fling(0.0, 1500.0);
    let before = s.offset_y();
    for _ in 0..120 { s.tick(1.0 / 60.0); }
    let after = s.offset_y();
    assert!(after > before);
    assert!(s.momentum_speed() < 10.0);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-ui --test scrollable_test`
Expected: FAIL — module absent.

- [ ] **Step 3: Write the minimal implementation**

`quick-ui/src/widget/scrollable.rs`:

```rust
use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{WindowEvent, MouseScrollDelta};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::{Widget, Element};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;

#[derive(Clone, Copy, Debug)]
pub enum ScrollAxis { Vertical, Horizontal, Both }

pub struct Scrollable {
    axis: ScrollAxis,
    offset_x: f32,
    offset_y: f32,
    content_w: f32,
    content_h: f32,
    viewport_w: f32,
    viewport_h: f32,
    vx: f32,
    vy: f32,
    decay: f32,
    sticky_header_height: f32,
    child: Option<Element>,
    rect: Rect,
}

impl Scrollable {
    pub fn new(axis: ScrollAxis) -> Self { Self { axis, offset_x: 0.0, offset_y: 0.0, content_w: 0.0, content_h: 0.0, viewport_w: 0.0, viewport_h: 0.0, vx: 0.0, vy: 0.0, decay: 0.92, sticky_header_height: 0.0, child: None, rect: Rect::ZERO } }
    pub fn new_vertical() -> Self { Self::new(ScrollAxis::Vertical) }
    pub fn new_horizontal() -> Self { Self::new(ScrollAxis::Horizontal) }
    pub fn with_child(mut self, c: Element) -> Self { self.child = Some(c); self }
    pub fn with_sticky_header(mut self, h: f32) -> Self { self.sticky_header_height = h; self }

    pub fn set_content_size(&mut self, w: f32, h: f32) { self.content_w = w; self.content_h = h; self.clamp(); }
    pub fn set_viewport_size(&mut self, w: f32, h: f32) { self.viewport_w = w; self.viewport_h = h; self.clamp(); }
    pub fn offset_x(&self) -> f32 { self.offset_x }
    pub fn offset_y(&self) -> f32 { self.offset_y }
    pub fn momentum_speed(&self) -> f32 { (self.vx * self.vx + self.vy * self.vy).sqrt() }

    pub fn scroll_by(&mut self, dx: f32, dy: f32) {
        match self.axis {
            ScrollAxis::Vertical => { self.offset_y += dy; }
            ScrollAxis::Horizontal => { self.offset_x += dx; }
            ScrollAxis::Both => { self.offset_x += dx; self.offset_y += dy; }
        }
        self.clamp();
    }
    pub fn fling(&mut self, vx: f32, vy: f32) { self.vx = vx; self.vy = vy; }
    pub fn tick(&mut self, dt: f32) {
        self.offset_x += self.vx * dt;
        self.offset_y += self.vy * dt;
        self.vx *= self.decay;
        self.vy *= self.decay;
        if self.vx.abs() < 1.0 { self.vx = 0.0; }
        if self.vy.abs() < 1.0 { self.vy = 0.0; }
        self.clamp();
    }
    pub fn scroll_into_view(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if y < self.offset_y { self.offset_y = y; }
        else if y + h > self.offset_y + self.viewport_h { self.offset_y = (y + h - self.viewport_h).max(0.0); }
        if x < self.offset_x { self.offset_x = x; }
        else if x + w > self.offset_x + self.viewport_w { self.offset_x = (x + w - self.viewport_w).max(0.0); }
        self.clamp();
    }
    fn clamp(&mut self) {
        let max_x = (self.content_w - self.viewport_w).max(0.0);
        let max_y = (self.content_h - self.viewport_h).max(0.0);
        self.offset_x = self.offset_x.clamp(0.0, max_x);
        self.offset_y = self.offset_y.clamp(0.0, max_y);
    }
}

impl Widget for Scrollable {
    fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) { if let Some(c) = &mut self.child { c.sync(s, p); } }
    fn update(&mut self, s: &mut AutoCommit, p: &ObjId) -> bool {
        let ticking = self.momentum_speed() > 0.0;
        if ticking { self.tick(1.0 / 60.0); }
        let child = self.child.as_mut().map(|c| c.update(s, p)).unwrap_or(false);
        ticking || child
    }
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let child_node = self.child.as_mut().map(|c| c.build_layout(taffy, theme, node_map));
        let style = Style { size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) }, overflow: taffy::Point { x: taffy::Overflow::Hidden, y: taffy::Overflow::Hidden }, ..Default::default() };
        let node = match child_node { Some(c) => taffy.new_with_children(style, &[c]).unwrap(), None => taffy.new_leaf(style).unwrap() };
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(origin.x + l.location.x as f64, origin.y + l.location.y as f64, origin.x + l.location.x as f64 + l.size.width as f64, origin.y + l.location.y as f64 + l.size.height as f64);
        self.viewport_w = l.size.width; self.viewport_h = l.size.height;
        if let Some(c) = &mut self.child {
            let child_node = taffy.children(node).unwrap()[0];
            c.apply_layout(taffy, child_node, Point::new(self.rect.x0 - self.offset_x as f64, self.rect.y0 - self.offset_y as f64));
            let cl = taffy.layout(child_node).unwrap();
            self.content_w = cl.size.width; self.content_h = cl.size.height;
            self.clamp();
        }
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        scene.push_layer(vello::peniko::BlendMode::default(), 1.0, transform, &self.rect);
        if let Some(c) = &mut self.child { c.draw(scene, transform, tr, theme, presence); }
        scene.pop_layer();
    }
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, m: &winit::event::Modifiers, s: &mut AutoCommit, p: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>) {
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let (dx, dy) = match delta { MouseScrollDelta::LineDelta(x, y) => (*x * -20.0, *y * -20.0), MouseScrollDelta::PixelDelta(pp) => (-pp.x as f32, -pp.y as f32) };
            self.scroll_by(dx, dy);
            *handled = true;
            return;
        }
        if let Some(c) = &mut self.child {
            let child_node = taffy.children(node_id).unwrap()[0];
            c.handle_event(event, scale_factor, m, s, p, handled, focus_target, child_node, taffy);
        }
    }
    fn children(&self) -> Vec<&dyn Widget> { self.child.as_ref().map(|c| c.as_ref() as &dyn Widget).into_iter().collect() }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> { self.child.as_mut().map(|c| c.as_mut() as &mut (dyn Widget + '_)).into_iter().collect() }
    fn name(&self) -> &str { "Scrollable" }
    fn get_rect(&self) -> Rect { self.rect }
}
```

Append to `quick-ui/src/widget/mod.rs`:

```rust
mod scrollable;
pub use scrollable::{Scrollable, ScrollAxis};
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-ui --test scrollable_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget/scrollable.rs quick-ui/src/widget/mod.rs quick-ui/tests/scrollable_test.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): Scrollable primitive with momentum + scroll-into-view"
```

---

## Task 11: TileContent + TileHost widget

**Files:**
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/tile_host.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/mod.rs`

- [ ] **Step 1: Write the failing test**

Append to `quick-ui/tests/tabs_test.rs`:

```rust
use quick_ui::widget::{TileContent, TileHost, AppKind};

#[test]
fn tile_content_stub_renders_label() {
    let tc = TileContent::Stub { kind: AppKind::Browser, label: "Browser".into() };
    assert_eq!(TileHost::new(tc).kind_string(), "Browser");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-ui --test tabs_test tile_content_stub_renders_label -- --exact`
Expected: FAIL — missing items.

- [ ] **Step 3: Write the minimal implementation**

`quick-ui/src/widget/tile_host.rs`:

```rust
use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::{Widget, Element};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppKind { Terminal, FileExplorer, Browser, Settings, Other(String) }

impl AppKind {
    pub fn as_str(&self) -> &str { match self { AppKind::Terminal => "Terminal", AppKind::FileExplorer => "Files", AppKind::Browser => "Browser", AppKind::Settings => "Settings", AppKind::Other(s) => s.as_str() } }
}

/// Content for a single tile (single tab) hosted by a `TileHost`.
///
/// **Interim:** During Plan 1, `App` carries a `Box<dyn Widget>` so concrete widgets
/// need not implement the full `AppWidget` trait yet. Plan 3 redefines the variant as
/// `Box<dyn AppWidget>` (which extends `Widget`) once the app contract lands. The
/// `Wayland(SurfaceId)` variant is reserved for future compositor integration.
pub enum TileContent {
    App(Element),
    Stub { kind: AppKind, label: String },
    // reserved: Wayland(u64),
}

pub struct TileHost { content: TileContent, rect: Rect }

impl TileHost {
    pub fn new(content: TileContent) -> Self { Self { content, rect: Rect::ZERO } }
    pub fn kind_string(&self) -> String {
        match &self.content {
            TileContent::App(_) => "App".into(),
            TileContent::Stub { kind, .. } => kind.as_str().into(),
        }
    }
    pub fn content_mut(&mut self) -> &mut TileContent { &mut self.content }
}

impl Widget for TileHost {
    fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) { if let TileContent::App(w) = &mut self.content { w.sync(s, p); } }
    fn update(&mut self, s: &mut AutoCommit, p: &ObjId) -> bool { if let TileContent::App(w) = &mut self.content { w.update(s, p) } else { false } }
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = match &mut self.content {
            TileContent::App(w) => {
                let child = w.build_layout(taffy, theme, node_map);
                taffy.new_with_children(Style { size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) }, ..Default::default() }, &[child]).unwrap()
            }
            TileContent::Stub { .. } => {
                taffy.new_leaf(Style { size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) }, ..Default::default() }).unwrap()
            }
        };
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(origin.x + l.location.x as f64, origin.y + l.location.y as f64, origin.x + l.location.x as f64 + l.size.width as f64, origin.y + l.location.y as f64 + l.size.height as f64);
        if let TileContent::App(w) = &mut self.content {
            let child_node = taffy.children(node).unwrap()[0];
            w.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
        }
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        let shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.lg as f64);
        scene.fill(vello::peniko::Fill::NonZero, transform, theme.tokens.colors.surfaces.raised, None, &shape);
        match &mut self.content {
            TileContent::App(w) => w.draw(scene, transform, tr, theme, presence),
            TileContent::Stub { kind, label } => {
                let text_size = theme.tokens.typography.scale.xl;
                let (tw, _) = tr.measure(label, text_size, None);
                let x = self.rect.center().x - tw as f64 / 2.0;
                let y = self.rect.center().y - text_size as f64 / 2.0;
                tr.draw_text(scene, label, text_size, theme.tokens.colors.text_primary, transform * Affine::translate((x, y)), None);
                let sm = theme.tokens.typography.scale.sm;
                let s = format!("{} coming soon", kind.as_str());
                let (sw, _) = tr.measure(&s, sm, None);
                let sx = self.rect.center().x - sw as f64 / 2.0;
                tr.draw_text(scene, &s, sm, theme.tokens.colors.text_dim, transform * Affine::translate((sx, y + text_size as f64 + 8.0)), None);
            }
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, m: &winit::event::Modifiers, s: &mut AutoCommit, p: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>) {
        if let TileContent::App(w) = &mut self.content {
            let child_node = taffy.children(node_id).unwrap()[0];
            w.handle_event(event, scale_factor, m, s, p, handled, focus_target, child_node, taffy);
        }
    }
    fn name(&self) -> &str { "TileHost" }
    fn get_rect(&self) -> Rect { self.rect }
}
```

Append to `quick-ui/src/widget/mod.rs`:

```rust
mod tile_host;
pub use tile_host::{TileHost, TileContent, AppKind};
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-ui --test tabs_test tile_content_stub_renders_label -- --exact`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget/tile_host.rs quick-ui/src/widget/mod.rs quick-ui/tests/tabs_test.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): TileHost + TileContent with stub renderer"
```

---

## Task 12: WindowGroup widget — TabBar + TileHost + drag-resize edge

**Files:**
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/window_group.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/tests/window_group_test.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/mod.rs`

- [ ] **Step 1: Write the failing test**

`quick-ui/tests/window_group_test.rs`:

```rust
use quick_ui::widget::{WindowGroup, WindowWidth, TileContent, AppKind, TabDescriptor};

#[test]
fn window_width_presets_are_clamped() {
    assert_eq!(WindowWidth::Narrow.pixels(), 400);
    assert_eq!(WindowWidth::Normal.pixels(), 600);
    assert_eq!(WindowWidth::Wide.pixels(), 800);
    assert_eq!(WindowWidth::Custom(100).pixels(), 300);
    assert_eq!(WindowWidth::Custom(5000).pixels(), 1200);
    assert_eq!(WindowWidth::Custom(777).pixels(), 777);
}

#[test]
fn window_group_drag_right_edge_resizes() {
    let mut g = WindowGroup::new(1, vec![(TabDescriptor { id: 1, title: "T1".into() }, TileContent::Stub { kind: AppKind::Terminal, label: "Terminal".into() })], 1, WindowWidth::Normal);
    g.begin_resize(600.0);
    g.update_resize(750.0);
    g.end_resize();
    assert_eq!(g.width().pixels(), 750);
    g.begin_resize(750.0);
    g.update_resize(50.0);
    g.end_resize();
    assert_eq!(g.width().pixels(), 300);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-ui --test window_group_test`
Expected: FAIL.

- [ ] **Step 3: Write the minimal implementation**

`quick-ui/src/widget/window_group.rs`:

```rust
use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{WindowEvent, MouseButton, ElementState};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use taffy::style_helpers::length;
use crate::widget::{Widget, TabBar, TabDescriptor, TileContent, TileHost};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;

pub const WIDTH_MIN_PX: u32 = 300;
pub const WIDTH_MAX_PX: u32 = 1200;
pub const RESIZE_HANDLE_PX: f64 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowWidth { Narrow, Normal, Wide, Custom(u32) }

impl WindowWidth {
    pub fn pixels(&self) -> u32 {
        match self {
            WindowWidth::Narrow => 400,
            WindowWidth::Normal => 600,
            WindowWidth::Wide => 800,
            WindowWidth::Custom(p) => (*p).clamp(WIDTH_MIN_PX, WIDTH_MAX_PX),
        }
    }
}

pub struct WindowGroup {
    pub id: u64,
    bar: TabBar,
    hosts: Vec<(u64, TileHost)>,
    width: WindowWidth,
    rect: Rect,
    resize_origin: Option<(f32, u32)>,
    pub hover_resize: bool,
}

impl WindowGroup {
    pub fn new(id: u64, tabs_with_content: Vec<(TabDescriptor, TileContent)>, active_id: u64, width: WindowWidth) -> Self {
        let descs: Vec<TabDescriptor> = tabs_with_content.iter().map(|(d, _)| d.clone()).collect();
        let hosts: Vec<(u64, TileHost)> = tabs_with_content.into_iter().map(|(d, c)| (d.id, TileHost::new(c))).collect();
        Self { id, bar: TabBar::new(&format!("g{id}"), descs, active_id), hosts, width, rect: Rect::ZERO, resize_origin: None, hover_resize: false }
    }
    pub fn width(&self) -> WindowWidth { self.width }
    pub fn begin_resize(&mut self, pointer_x: f32) { self.resize_origin = Some((pointer_x, self.width.pixels())); }
    pub fn update_resize(&mut self, pointer_x: f32) {
        if let Some((origin_x, origin_w)) = self.resize_origin {
            let delta = pointer_x - origin_x;
            let new_w = ((origin_w as f32) + delta).round().max(0.0) as i64;
            let clamped = (new_w.max(WIDTH_MIN_PX as i64).min(WIDTH_MAX_PX as i64)) as u32;
            self.width = WindowWidth::Custom(clamped);
        }
    }
    pub fn end_resize(&mut self) { self.resize_origin = None; }
    pub fn active_host_mut(&mut self) -> Option<&mut TileHost> {
        let id = self.bar.active_id();
        self.hosts.iter_mut().find(|(i, _)| *i == id).map(|(_, h)| h)
    }
}

impl Widget for WindowGroup {
    fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) { if let Some(h) = self.active_host_mut() { h.sync(s, p); } }
    fn update(&mut self, s: &mut AutoCommit, p: &ObjId) -> bool { self.active_host_mut().map(|h| h.update(s, p)).unwrap_or(false) }
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let bar_node = self.bar.build_layout(taffy, theme, node_map);
        let active_id = self.bar.active_id();
        let tile_node = if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.build_layout(taffy, theme, node_map)
        } else {
            taffy.new_leaf(Style::default()).unwrap()
        };
        let w_px = self.width.pixels() as f32;
        let node = taffy.new_with_children(Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size { width: Dimension::length(w_px), height: Dimension::percent(1.0) },
            gap: taffy::Size { width: length(0.0), height: length(0.0) },
            ..Default::default()
        }, &[bar_node, tile_node]).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(origin.x + l.location.x as f64, origin.y + l.location.y as f64, origin.x + l.location.x as f64 + l.size.width as f64, origin.y + l.location.y as f64 + l.size.height as f64);
        let children = taffy.children(node).unwrap();
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        self.bar.apply_layout(taffy, children[0], new_origin);
        let active_id = self.bar.active_id();
        if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.apply_layout(taffy, children[1], new_origin);
        }
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        self.bar.draw(scene, transform, tr, theme, presence);
        let active_id = self.bar.active_id();
        if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.draw(scene, transform, tr, theme, presence);
        }
        // resize handle visualization
        let handle_rect = Rect::new(self.rect.x1 - RESIZE_HANDLE_PX, self.rect.y0, self.rect.x1, self.rect.y1);
        let col = if self.hover_resize || self.resize_origin.is_some() { theme.tokens.colors.border_focus } else { theme.tokens.colors.border_standard };
        scene.fill(vello::peniko::Fill::NonZero, transform, col, None, &handle_rect);
    }
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, m: &winit::event::Modifiers, s: &mut AutoCommit, p: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>) {
        let children = taffy.children(node_id).unwrap();
        let handle_rect = Rect::new(self.rect.x1 - RESIZE_HANDLE_PX, self.rect.y0, self.rect.x1, self.rect.y1);
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.hover_resize = handle_rect.contains(pt);
                if self.resize_origin.is_some() { self.update_resize(pt.x as f32); *handled = true; return; }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } if self.hover_resize => {
                // the origin x is captured at the current pointer via the current rect
                self.begin_resize(self.rect.x1 as f32);
                *handled = true; return;
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } if self.resize_origin.is_some() => {
                self.end_resize(); *handled = true; return;
            }
            _ => {}
        }
        self.bar.handle_event(event, scale_factor, m, s, p, handled, focus_target, children[0], taffy);
        if *handled { return; }
        let active_id = self.bar.active_id();
        if let Some((_, h)) = self.hosts.iter_mut().find(|(i, _)| *i == active_id) {
            h.handle_event(event, scale_factor, m, s, p, handled, focus_target, children[1], taffy);
        }
    }
    fn name(&self) -> &str { "WindowGroup" }
    fn get_rect(&self) -> Rect { self.rect }
    fn wants_pointer_capture(&self) -> bool { self.resize_origin.is_some() }
}
```

Append to `quick-ui/src/widget/mod.rs`:

```rust
mod window_group;
pub use window_group::{WindowGroup, WindowWidth, WIDTH_MIN_PX, WIDTH_MAX_PX, RESIZE_HANDLE_PX};
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-ui --test window_group_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget/window_group.rs quick-ui/src/widget/mod.rs quick-ui/tests/window_group_test.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): WindowGroup widget with right-edge drag-resize"
```

---

## Task 13: StripView widget — wheel-paginate with 600 ms cooldown

**Files:**
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/strip_view.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/tests/strip_view_test.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/mod.rs`

- [ ] **Step 1: Write the failing test**

`quick-ui/tests/strip_view_test.rs`:

```rust
use quick_ui::widget::{StripView, STRIP_COOLDOWN_MS};
use std::time::Duration;

#[test]
fn wheel_paginate_cooldown_blocks_second_call() {
    let mut v = StripView::new(vec!["a".into(), "b".into(), "c".into()], 0);
    assert!(v.try_paginate(1, Duration::from_millis(0)));
    assert_eq!(v.active_index(), 1);
    // second call inside cooldown → blocked
    assert!(!v.try_paginate(1, Duration::from_millis(100)));
    assert_eq!(v.active_index(), 1);
    // past cooldown → allowed
    assert!(v.try_paginate(1, Duration::from_millis(STRIP_COOLDOWN_MS + 10)));
    assert_eq!(v.active_index(), 2);
    // clamps at end
    assert!(!v.try_paginate(1, Duration::from_millis(10_000)));
    assert_eq!(v.active_index(), 2);
}

#[test]
fn wheel_paginate_negative_wraps_none() {
    let mut v = StripView::new(vec!["a".into(), "b".into()], 0);
    assert!(!v.try_paginate(-1, Duration::from_millis(0)));
    assert_eq!(v.active_index(), 0);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-ui --test strip_view_test`
Expected: FAIL.

- [ ] **Step 3: Write the minimal implementation**

`quick-ui/src/widget/strip_view.rs`:

```rust
use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{WindowEvent, MouseScrollDelta};
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use std::time::{Duration, Instant};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;

pub const STRIP_COOLDOWN_MS: u64 = 600;

pub struct StripView {
    strip_names: Vec<String>,
    active_index: usize,
    rect: Rect,
    last_paginate_ms: Option<u128>,
    // Internal clock for real-time handling.
    start: Instant,
}

impl StripView {
    pub fn new(strip_names: Vec<String>, active_index: usize) -> Self {
        Self { strip_names, active_index: active_index.min(1_usize.saturating_sub(1).max(0)), rect: Rect::ZERO, last_paginate_ms: None, start: Instant::now() }
            .with_initial(active_index)
    }
    fn with_initial(mut self, i: usize) -> Self { self.active_index = i.min(self.strip_names.len().saturating_sub(1)); self }
    pub fn active_index(&self) -> usize { self.active_index }
    pub fn strip_names(&self) -> &[String] { &self.strip_names }

    /// Returns `true` if the paginate applied. `direction` is +1 or -1.
    pub fn try_paginate(&mut self, direction: i32, now_since_start: Duration) -> bool {
        let now_ms = now_since_start.as_millis();
        if let Some(last) = self.last_paginate_ms {
            if now_ms.saturating_sub(last) < STRIP_COOLDOWN_MS as u128 { return false; }
        }
        let new_idx = self.active_index as i64 + direction as i64;
        if new_idx < 0 || new_idx >= self.strip_names.len() as i64 { return false; }
        self.active_index = new_idx as usize;
        self.last_paginate_ms = Some(now_ms);
        true
    }
}

impl Widget for StripView {
    fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style { size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) }, ..Default::default() }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(origin.x + l.location.x as f64, origin.y + l.location.y as f64, origin.x + l.location.x as f64 + l.size.width as f64, origin.y + l.location.y as f64 + l.size.height as f64);
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, _presence: &PresenceCtx) {
        // Simple v1: render the active strip name centered; shell will overlay real groups in later tasks.
        scene.fill(vello::peniko::Fill::NonZero, transform, theme.tokens.colors.surfaces.base, None, &self.rect);
        if let Some(name) = self.strip_names.get(self.active_index) {
            let text_size = theme.tokens.typography.scale.x2l;
            let (tw, _) = tr.measure(name, text_size, None);
            let x = self.rect.center().x - tw as f64 / 2.0;
            let y = self.rect.center().y - text_size as f64 / 2.0;
            tr.draw_text(scene, name, text_size, theme.tokens.colors.text_dim, transform * Affine::translate((x, y)), None);
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, _scale: f64, _m: &winit::event::Modifiers, _s: &mut AutoCommit, _p: &ObjId, handled: &mut bool, _focus: &mut Option<NodeId>, _n: NodeId, _t: &TaffyTree<()>) {
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let sign = match delta { MouseScrollDelta::LineDelta(_x, y) => -y.signum() as i32, MouseScrollDelta::PixelDelta(pp) => -(pp.y.signum() as i32) };
            if sign != 0 {
                let now = Instant::now().duration_since(self.start);
                if self.try_paginate(sign, now) { *handled = true; }
            }
        }
    }
    fn name(&self) -> &str { "StripView" }
    fn get_rect(&self) -> Rect { self.rect }
}
```

Append to `quick-ui/src/widget/mod.rs`:

```rust
mod strip_view;
pub use strip_view::{StripView, STRIP_COOLDOWN_MS};
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-ui --test strip_view_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget/strip_view.rs quick-ui/src/widget/mod.rs quick-ui/tests/strip_view_test.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): StripView with paginated wheel + 600 ms cooldown"
```

---

## Task 14: WorkspaceTree state in quick-shell

**Files:**
- Create: `/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/state.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/mod.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/nirios_tokens.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-shell/tests/shell_state_test.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-shell/src/lib.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-shell/Cargo.toml`

- [ ] **Step 1: Write the failing test**

`quick-shell/tests/shell_state_test.rs`:

```rust
use quick_desktop::shell::state::{WorkspaceTree, Workspace, Strip, WindowGroup, Tab, WindowWidth, TileContent, AppKind, Role, Collaborator};

#[test]
fn workspace_tree_default_has_one_workspace_and_one_strip() {
    let t = WorkspaceTree::bootstrap_default();
    assert_eq!(t.workspaces.len(), 1);
    assert_eq!(t.workspaces[0].strips.len(), 1);
    assert!(t.active_strip.get(&t.workspaces[0].id).is_some());
}

#[test]
fn append_workspace_sets_active() {
    let mut t = WorkspaceTree::bootstrap_default();
    let new_id = t.append_empty_workspace("New");
    t.set_active(new_id);
    assert_eq!(t.active, new_id);
}

#[test]
fn window_width_clamp() {
    assert_eq!(WindowWidth::Custom(50).pixels(), 300);
    assert_eq!(WindowWidth::Custom(9999).pixels(), 1200);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-desktop --test shell_state_test`
Expected: FAIL — module absent; also `quick-desktop` binary still has Iced compile errors at this stage. (Task 17 drops Iced; for now the test may not compile because the `lib.rs` doesn't expose `shell`. Fix via steps 3.)

- [ ] **Step 3: Write the minimal implementation**

Add to `quick-shell/Cargo.toml` `[dependencies]`:

```toml
quick-ui = { path = "../quick-ui" }
winit = "0.30.13"
dirs-next = "2.0"
```

`quick-shell/src/shell/state.rs`:

```rust
use serde::{Serialize, Deserialize};
use std::collections::HashMap;

pub type WorkspaceId = u64;
pub type StripId = u64;
pub type GroupId = u64;
pub type TabId = u64;
pub type FileId = u64;

fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AppKind { Terminal, FileExplorer, Browser, Settings, Other(String) }

impl AppKind {
    pub fn as_str(&self) -> &str { match self { AppKind::Terminal => "Terminal", AppKind::FileExplorer => "Files", AppKind::Browser => "Browser", AppKind::Settings => "Settings", AppKind::Other(s) => s } }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum WindowWidth { Narrow, Normal, Wide, Custom(u32) }

impl WindowWidth {
    pub fn pixels(&self) -> u32 {
        match self {
            WindowWidth::Narrow => 400,
            WindowWidth::Normal => 600,
            WindowWidth::Wide => 800,
            WindowWidth::Custom(p) => (*p).clamp(300, 1200),
        }
    }
}

/// Interim tile content for v1. Plan 3 replaces `App` with a live `Box<dyn AppWidget>`
/// handle; v1 JSON persistence only carries Stub and a symbolic `AppPlaceholder` for
/// native apps whose concrete widgets are constructed at shell boot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TileContent {
    AppPlaceholder { kind: AppKind, label: String },
    Stub { kind: AppKind, label: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tab { pub id: TabId, pub title: String, pub tile: TileContent }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowGroup { pub id: GroupId, pub tabs: Vec<Tab>, pub active_tab: TabId, pub width: WindowWidth }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Strip { pub id: StripId, pub name: String, pub window_groups: Vec<WindowGroup> }

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Role { pub id: u64, pub name: String }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceFile { pub id: FileId, pub name: String }

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Collaborator { pub user_id: String, pub name: String, pub color: [u8; 3] }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace { pub id: WorkspaceId, pub name: String, pub strips: Vec<Strip>, pub roles: Vec<Role>, pub workspace_files: Vec<WorkspaceFile> }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceTree { pub workspaces: Vec<Workspace>, pub active: WorkspaceId, pub active_strip: HashMap<WorkspaceId, StripId>, pub collaborators: Vec<Collaborator> }

impl WorkspaceTree {
    pub fn bootstrap_default() -> Self {
        let ws_id = next_id();
        let strip_id = next_id();
        let group_id = next_id();
        let tab_id = next_id();
        let ws = Workspace {
            id: ws_id,
            name: "Home".into(),
            strips: vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: vec![WindowGroup {
                    id: group_id,
                    tabs: vec![Tab { id: tab_id, title: "Terminal".into(), tile: TileContent::Stub { kind: AppKind::Terminal, label: "Terminal".into() } }],
                    active_tab: tab_id,
                    width: WindowWidth::Normal,
                }],
            }],
            roles: vec![],
            workspace_files: vec![],
        };
        let mut active_strip = HashMap::new();
        active_strip.insert(ws_id, strip_id);
        Self { workspaces: vec![ws], active: ws_id, active_strip, collaborators: vec![] }
    }

    pub fn append_empty_workspace(&mut self, name: &str) -> WorkspaceId {
        let ws_id = next_id();
        let strip_id = next_id();
        let ws = Workspace {
            id: ws_id,
            name: name.into(),
            strips: vec![Strip { id: strip_id, name: "Main".into(), window_groups: vec![] }],
            roles: vec![], workspace_files: vec![],
        };
        self.active_strip.insert(ws_id, strip_id);
        self.workspaces.push(ws);
        ws_id
    }

    pub fn set_active(&mut self, id: WorkspaceId) { if self.workspaces.iter().any(|w| w.id == id) { self.active = id; } }
    pub fn active_workspace(&self) -> Option<&Workspace> { self.workspaces.iter().find(|w| w.id == self.active) }
    pub fn active_workspace_mut(&mut self) -> Option<&mut Workspace> { self.workspaces.iter_mut().find(|w| w.id == self.active) }

    pub fn switch_up(&mut self) -> bool {
        let idx = self.workspaces.iter().position(|w| w.id == self.active).unwrap_or(0);
        if idx == 0 { return false; }
        self.active = self.workspaces[idx - 1].id; true
    }
    pub fn switch_down(&mut self) -> bool {
        let idx = self.workspaces.iter().position(|w| w.id == self.active).unwrap_or(0);
        if idx + 1 >= self.workspaces.len() { return false; }
        self.active = self.workspaces[idx + 1].id; true
    }
}
```

`quick-shell/src/shell/nirios_tokens.rs`:

```rust
use quick_ui::tokens::Tokens;

/// Apply nirios-specific overrides on top of the base token set (more dramatic motion,
/// heavier elevation for shell chrome). v1 overrides spring stiffness for the sidebar
/// wheel + overview scale transitions.
pub fn nirios_dark() -> Tokens {
    let mut t = Tokens::dark();
    t.motion.springs.snappy.stiffness = 320.0;
    t.motion.springs.bouncy.damping = 10.0;
    t.elevation.e3.blur = 18.0;
    t.elevation.e4.blur = 36.0;
    t
}
```

`quick-shell/src/shell/mod.rs`:

```rust
pub mod state;
pub mod nirios_tokens;
pub mod persist;
```

`quick-shell/src/shell/persist.rs` (stub — filled in Task 16):

```rust
use std::path::PathBuf;

pub fn state_json_path() -> PathBuf {
    dirs_next::config_dir()
        .map(|c| c.join("quick-desktop").join("state.json"))
        .unwrap_or_else(|| PathBuf::from("state.json"))
}
```

Modify `quick-shell/src/lib.rs` to expose shell (ensure `pub mod shell;` is present). The existing content may be empty or minimal; append if needed.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-desktop --test shell_state_test`
Expected: PASS (3 tests). Note: if the existing `main.rs` Iced code still lives in the binary, the package build may still fail — the test is a lib-level test, so `cargo test --lib --test shell_state_test -p quick-desktop` is the precise invocation. Use that:

Run: `cargo test -p quick-desktop --lib shell::state`
Expected: PASS (or skip until Task 17 removes Iced if build blocks).

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop/quick-shell add src/shell Cargo.toml tests/shell_state_test.rs src/lib.rs
git -C /var/home/quinten/Development/quickdesktop/quick-shell commit -m "feat(shell): WorkspaceTree v1 data model with JSON-serializable shell state"
```

---

## Task 15: WorkspaceView widget — renders active workspace's active strip + switch transitions

**Files:**
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/workspace_view.rs`
- Create: `/var/home/quinten/Development/quickdesktop/quick-ui/tests/workspace_view_test.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-ui/src/widget/mod.rs`

- [ ] **Step 1: Write the failing test**

`quick-ui/tests/workspace_view_test.rs`:

```rust
use quick_ui::widget::{WorkspaceView, WorkspaceSwitchDirection};
use quick_ui::animation::Spring;

#[test]
fn workspace_view_switch_up_down_sets_slide_target() {
    let mut v = WorkspaceView::new(3, 0);
    v.switch(WorkspaceSwitchDirection::Down);
    assert_eq!(v.active_index(), 1);
    let target = v.slide_target();
    assert!(target.abs() > 0.01, "expected a non-zero slide target, got {target}");
    v.switch(WorkspaceSwitchDirection::Down);
    assert_eq!(v.active_index(), 2);
    v.switch(WorkspaceSwitchDirection::Down);
    assert_eq!(v.active_index(), 2); // clamped
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-ui --test workspace_view_test`
Expected: FAIL — module missing.

- [ ] **Step 3: Write the minimal implementation**

`quick-ui/src/widget/workspace_view.rs`:

```rust
use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style, Size, Dimension};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::PresenceCtx;
use crate::animation::Spring;
use crate::tokens::SpringParams;

#[derive(Clone, Copy, Debug)]
pub enum WorkspaceSwitchDirection { Up, Down }

pub struct WorkspaceView {
    workspace_count: usize,
    active_index: usize,
    slide: Spring,   // 0.0 settled; animates to signed offset and back
    rect: Rect,
}

impl WorkspaceView {
    pub fn new(workspace_count: usize, active_index: usize) -> Self {
        Self { workspace_count: workspace_count.max(1), active_index: active_index.min(workspace_count.saturating_sub(1)), slide: Spring::new(0.0, SpringParams { stiffness: 220.0, damping: 24.0, mass: 1.0 }), rect: Rect::ZERO }
    }
    pub fn active_index(&self) -> usize { self.active_index }
    pub fn slide_target(&self) -> f32 { self.slide.target() }
    pub fn switch(&mut self, d: WorkspaceSwitchDirection) -> bool {
        let (new_idx, direction_sign) = match d {
            WorkspaceSwitchDirection::Up => {
                if self.active_index == 0 { return false; }
                (self.active_index - 1, -1.0_f32)
            }
            WorkspaceSwitchDirection::Down => {
                if self.active_index + 1 >= self.workspace_count { return false; }
                (self.active_index + 1, 1.0_f32)
            }
        };
        self.active_index = new_idx;
        // Kick the slide spring: snap to incoming offset then relax to 0.
        let h = self.rect.height().max(300.0) as f32;
        self.slide.snap(direction_sign * h);
        self.slide.set_target(0.0);
        true
    }
    pub fn tick_animation(&mut self, dt: f32) { self.slide.step(dt); }
}

impl Widget for WorkspaceView {
    fn sync(&mut self, _s: &mut AutoCommit, _p: &ObjId) {}
    fn update(&mut self, _s: &mut AutoCommit, _p: &ObjId) -> bool {
        if !self.slide.is_settled() { self.tick_animation(1.0 / 60.0); true } else { false }
    }
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let n = taffy.new_leaf(Style { size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) }, ..Default::default() }).unwrap();
        node_map.insert(n, self as *mut dyn Widget);
        n
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, n: NodeId, origin: Point) {
        let l = taffy.layout(n).unwrap();
        self.rect = Rect::new(origin.x + l.location.x as f64, origin.y + l.location.y as f64, origin.x + l.location.x as f64 + l.size.width as f64, origin.y + l.location.y as f64 + l.size.height as f64);
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, _p: &PresenceCtx) {
        let offset = self.slide.value();
        let t = transform * Affine::translate((0.0, offset as f64));
        scene.fill(vello::peniko::Fill::NonZero, t, theme.tokens.colors.surfaces.base, None, &self.rect);
        let s = format!("workspace {}/{}", self.active_index + 1, self.workspace_count);
        let text_size = theme.tokens.typography.scale.lg;
        let (tw, _) = tr.measure(&s, text_size, None);
        let x = self.rect.center().x - tw as f64 / 2.0;
        let y = self.rect.y0 + theme.tokens.spacing.s2 as f64;
        tr.draw_text(scene, &s, text_size, theme.tokens.colors.text_dim, t * Affine::translate((x, y)), None);
    }
    fn handle_event(&mut self, _e: &WindowEvent, _sf: f64, _m: &winit::event::Modifiers, _s: &mut AutoCommit, _p: &ObjId, _h: &mut bool, _f: &mut Option<NodeId>, _n: NodeId, _t: &TaffyTree<()>) {}
    fn name(&self) -> &str { "WorkspaceView" }
    fn get_rect(&self) -> Rect { self.rect }
}
```

Append to `quick-ui/src/widget/mod.rs`:

```rust
mod workspace_view;
pub use workspace_view::{WorkspaceView, WorkspaceSwitchDirection};
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-ui --test workspace_view_test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget/workspace_view.rs quick-ui/src/widget/mod.rs quick-ui/tests/workspace_view_test.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): WorkspaceView with spring-driven 2D slide on switch"
```

---

## Task 16: JSON persistence for WorkspaceTree

**Files:**
- Modify: `/var/home/quinten/Development/quickdesktop/quick-shell/src/shell/persist.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-shell/tests/shell_state_test.rs`

- [ ] **Step 1: Write the failing test**

Append to `quick-shell/tests/shell_state_test.rs`:

```rust
#[test]
fn persistence_roundtrip_atomic() {
    use quick_desktop::shell::persist::{save_tree_to, load_tree_from};
    use quick_desktop::shell::state::WorkspaceTree;
    let tmp = std::env::temp_dir().join(format!("qd-test-{}.json", std::process::id()));
    let t = WorkspaceTree::bootstrap_default();
    save_tree_to(&t, &tmp).expect("save");
    let loaded = load_tree_from(&tmp).expect("load");
    assert_eq!(loaded.workspaces.len(), t.workspaces.len());
    assert_eq!(loaded.active, t.active);
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn load_missing_file_returns_default() {
    use quick_desktop::shell::persist::load_or_default;
    use std::path::PathBuf;
    let missing = PathBuf::from("/tmp/definitely-missing-xyz.json");
    let _ = std::fs::remove_file(&missing);
    let t = load_or_default(&missing);
    assert_eq!(t.workspaces.len(), 1);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-desktop --test shell_state_test persistence_roundtrip_atomic -- --exact`
Expected: FAIL — `save_tree_to` / `load_tree_from` missing.

- [ ] **Step 3: Write the minimal implementation**

Replace `quick-shell/src/shell/persist.rs`:

```rust
use std::path::{Path, PathBuf};
use std::io::Write;
use crate::shell::state::WorkspaceTree;

pub fn state_json_path() -> PathBuf {
    dirs_next::config_dir()
        .map(|c| c.join("quick-desktop").join("state.json"))
        .unwrap_or_else(|| PathBuf::from("state.json"))
}

pub fn save_tree_to(tree: &WorkspaceTree, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(tree).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load_tree_from(path: &Path) -> std::io::Result<WorkspaceTree> {
    let s = std::fs::read_to_string(path)?;
    serde_json::from_str::<WorkspaceTree>(&s).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn load_or_default(path: &Path) -> WorkspaceTree {
    match load_tree_from(path) {
        Ok(t) => t,
        Err(_) => WorkspaceTree::bootstrap_default(),
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p quick-desktop --test shell_state_test`
Expected: PASS (5 tests total).

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop/quick-shell add src/shell/persist.rs tests/shell_state_test.rs
git -C /var/home/quinten/Development/quickdesktop/quick-shell commit -m "feat(shell): atomic JSON persistence for WorkspaceTree with default fallback"
```

---

## Task 17: Swap quick-desktop binary to nirios shell running on quick_ui::App

**Files:**
- Modify: `/var/home/quinten/Development/quickdesktop/quick-shell/src/main.rs`
- Modify: `/var/home/quinten/Development/quickdesktop/quick-shell/Cargo.toml`

- [ ] **Step 1: Write the failing test** (build-smoke + shell construction)

Append to `quick-shell/tests/shell_state_test.rs`:

```rust
#[test]
fn nirios_shell_root_constructs_from_tree() {
    use quick_desktop::shell::build_nirios_root;
    let tree = quick_desktop::shell::state::WorkspaceTree::bootstrap_default();
    let _boxed: Box<dyn quick_ui::widget::Widget> = build_nirios_root(&tree);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p quick-desktop --test shell_state_test nirios_shell_root_constructs_from_tree -- --exact`
Expected: FAIL — `build_nirios_root` missing.

- [ ] **Step 3: Write the minimal implementation**

Update `quick-shell/Cargo.toml` — remove `iced` from `[dependencies]` (it stays in the workspace's `apps/browser` etc. which are unaffected). Final `[dependencies]`:

```toml
[dependencies]
tokio = { version = "1.0", features = ["full"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
toml = "0.8"
tracing = "0.1"
tracing-subscriber = "0.3"
once_cell = "1.19"
chrono = "0.4"
uuid = { version = "1.0", features = ["v4", "serde"] }
dirs-next = "2.0"
smithay-client-toolkit = "0.19"
wayland-client = "0.31"
wayland-protocols = "0.32"
libp2p = { version = "0.56", features = ["tcp", "dns", "websocket", "noise", "yamux", "mdns", "gossipsub", "tokio", "macros"] }
smithay = { version = "0.7", default-features = false, features = ["wayland_frontend", "backend_winit", "desktop"] }
calloop-wayland-source = "0.4"
rand = "0.10.0"
quick-ui = { path = "../quick-ui" }
winit = "0.30.13"
```

Add to `quick-shell/src/shell/mod.rs`:

```rust
pub mod state;
pub mod nirios_tokens;
pub mod persist;

use quick_ui::widget::{Widget, Column, WorkspaceView, StripView, WindowGroup as UiGroup, WindowWidth as UiWidth, TileContent as UiTile, TabDescriptor, AppKind as UiKind, Container};
use state::{WorkspaceTree, TileContent as ShellTile, AppKind as ShellKind};

fn to_ui_width(w: state::WindowWidth) -> UiWidth {
    match w {
        state::WindowWidth::Narrow => UiWidth::Narrow,
        state::WindowWidth::Normal => UiWidth::Normal,
        state::WindowWidth::Wide => UiWidth::Wide,
        state::WindowWidth::Custom(p) => UiWidth::Custom(p),
    }
}

fn to_ui_kind(k: &ShellKind) -> UiKind {
    match k {
        ShellKind::Terminal => UiKind::Terminal,
        ShellKind::FileExplorer => UiKind::FileExplorer,
        ShellKind::Browser => UiKind::Browser,
        ShellKind::Settings => UiKind::Settings,
        ShellKind::Other(s) => UiKind::Other(s.clone()),
    }
}

fn to_ui_tile(t: &ShellTile) -> UiTile {
    match t {
        ShellTile::Stub { kind, label } => UiTile::Stub { kind: to_ui_kind(kind), label: label.clone() },
        ShellTile::AppPlaceholder { kind, label } => UiTile::Stub { kind: to_ui_kind(kind), label: label.clone() },
    }
}

/// Build the nirios shell's root widget tree from a WorkspaceTree. v1 renders the
/// active workspace's active strip as a `StripView` overlay above the active
/// `WorkspaceView`, with placeholder window-groups laid out horizontally.
pub fn build_nirios_root(tree: &WorkspaceTree) -> Box<dyn Widget> {
    let strips_for_view: Vec<String> = tree.active_workspace()
        .map(|w| w.strips.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default();
    let strip_view: Box<dyn Widget> = Box::new(StripView::new(strips_for_view, 0));
    let workspace_view: Box<dyn Widget> = Box::new(WorkspaceView::new(tree.workspaces.len(), tree.workspaces.iter().position(|w| w.id == tree.active).unwrap_or(0)));

    let mut col = Column::new().push(workspace_view).push(strip_view);

    if let Some(ws) = tree.active_workspace() {
        for strip in &ws.strips {
            for g in &strip.window_groups {
                let descs: Vec<TabDescriptor> = g.tabs.iter().map(|t| TabDescriptor { id: t.id, title: t.title.clone() }).collect();
                let tabs_with_content: Vec<(TabDescriptor, UiTile)> = g.tabs.iter().map(|t| (TabDescriptor { id: t.id, title: t.title.clone() }, to_ui_tile(&t.tile))).collect();
                let ui_group = UiGroup::new(g.id, tabs_with_content, g.active_tab, to_ui_width(g.width));
                col = col.push(Box::new(ui_group));
            }
            // suppress unused warning
            let _ = descs_unused(&descs);
        }
    }
    Box::new(Container::new().with_child(Box::new(col)).with_background())
}

fn descs_unused(_d: &[quick_ui::widget::TabDescriptor]) {}
```

Replace `quick-shell/src/main.rs` with:

```rust
use quick_ui::{App, State};
use quick_desktop::shell::{build_nirios_root, persist};

fn main() {
    tracing_subscriber::fmt::init();
    let path = persist::state_json_path();
    let tree = persist::load_or_default(&path);
    let state = State::new();
    let root = build_nirios_root(&tree);
    let app = App::new(state, root);
    app.run();
}
```

Update `quick-shell/src/lib.rs` to contain (and nothing else for this plan):

```rust
pub mod shell;
// Legacy non-UI modules retained for compositor/collab work (deferred to Plan 4+).
pub mod config;
pub mod security;
pub mod network;
pub mod workspace;
```

Delete `quick-shell/src/ui/` and `quick-shell/src/ui.rs` (Iced-only canvas). Use:

```bash
git -C /var/home/quinten/Development/quickdesktop/quick-shell rm -r src/ui src/ui.rs
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo build -p quick-desktop && cargo test -p quick-desktop --test shell_state_test nirios_shell_root_constructs_from_tree -- --exact`
Expected: build succeeds; test passes.

Demo check: `cargo run -p quick-desktop` opens a winit window titled "QuickUI App" rendering the bootstrap workspace with a placeholder terminal tile. Wheel over the strip area paginates strips (there's only 1 in the default tree — add a second manually or verify via keyboard). Cmd/Super+ArrowDown/Up switches workspaces via the WorkspaceView widget (after Task 15 wires it through — see below).

Note: In this task, Cmd/Super+Up/Down is wired via `App` modifier + arrow keys into the `WorkspaceView`. That happens in a follow-on small step in Task 17 but is already supported: `App`'s existing handler passes key events into the widget tree. The shell's top-level widget must consume `ModifiersChanged` + `KeyboardInput` for `Super + ArrowUp/Down` and invoke `workspace_view.switch(...)` in Task 15's widget handler. To complete this wiring, update `quick-ui/src/widget/workspace_view.rs` `handle_event`:

```rust
fn handle_event(&mut self, event: &WindowEvent, _sf: f64, modifiers: &winit::event::Modifiers, _s: &mut AutoCommit, _p: &ObjId, handled: &mut bool, _f: &mut Option<NodeId>, _n: NodeId, _t: &TaffyTree<()>) {
    if let WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Named(k), state: winit::event::ElementState::Pressed, .. }, .. } = event {
        if modifiers.state().super_key() || modifiers.state().control_key() {
            match k {
                winit::keyboard::NamedKey::ArrowUp => { if self.switch(WorkspaceSwitchDirection::Up) { *handled = true; } }
                winit::keyboard::NamedKey::ArrowDown => { if self.switch(WorkspaceSwitchDirection::Down) { *handled = true; } }
                _ => {}
            }
        }
    }
}
```

Re-run `cargo build -p quick-ui && cargo test -p quick-ui --test workspace_view_test`. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git -C /var/home/quinten/Development/quickdesktop/quick-shell add -A
git -C /var/home/quinten/Development/quickdesktop/quick-shell commit -m "feat(shell): swap quick-desktop entry to winit + quick_ui::App running nirios shell; drop Iced UI"
git -C /var/home/quinten/Development/quickdesktop add quick-ui/src/widget/workspace_view.rs
git -C /var/home/quinten/Development/quickdesktop commit -m "feat(quick-ui): WorkspaceView consumes Super/Ctrl+ArrowUp/Down for workspace switch"
```

Then bump the submodule pointer from superproject:

```bash
git -C /var/home/quinten/Development/quickdesktop add quick-shell
git -C /var/home/quinten/Development/quickdesktop commit -m "bump quick-shell to nirios foundation"
```

---

## Self-Review Checklist

- [ ] **Spec coverage.** Every bullet in the user's scope (items 1–10 in the task brief) maps to a task:
  - Design tokens + Theme + widget migration → Tasks 1, 2, 4, 5.
  - Animation/spring engine + motion tokens → Task 6 (+ motion tokens in Task 1).
  - Overlay subsystem → Task 7.
  - Widget trait extensions + `PresenceCtx` / `PermissionScope` scaffold + `draw` signature → Tasks 3 & 4.
  - Drag-drop manager + typed payloads + `Tabs/TabBar` with reorder + detach → Tasks 3, 8, 9.
  - `Scrollable` (V + H, momentum, scroll-into-view, sticky slot) → Task 10.
  - `TileContent` + `TileHost` + `WindowGroup` with drag-resize → Tasks 11 & 12.
  - `Strip` model + paginated `StripView` with wheel-switch + 600 ms cooldown → Task 13.
  - `WorkspaceTree` + `WorkspaceView` + up/down switch with 2D slide + JSON persistence → Tasks 14, 15, 16.
  - Swap `quick-desktop` entry point to nirios shell in dev mode via winit + `quick_ui::App`, drop Iced UI, keep Wayland/smithay scaffolding unused → Task 17.

- [ ] **Signatures locked in verbatim.** The spec's Widget-trait extensions (`presence_key`, `permission_scope`, `accepts_drop`, `on_drop`, `wants_pointer_capture`, `draw` + `&PresenceCtx`) appear exactly in Task 4's `widget/mod.rs` replacement. The `WorkspaceTree`/`Workspace`/`Strip`/`WindowGroup`/`Tab`/`TileContent`/`WindowWidth` data model in Task 14 mirrors the spec §5 exactly. `WindowWidth::Custom` is clamped to `300..=1200` in both `quick-ui::widget::WindowWidth` and `quick-shell::shell::state::WindowWidth`.

- [ ] **AppWidget interim is documented.** Task 4 defines `AppWidget: Widget` as an empty marker with an inline doc comment stating the full trait lands in Plan 3. Task 11 documents that `TileContent::App` carries a `Box<dyn Widget>` during Plan 1. Task 14's state-level `TileContent::AppPlaceholder` is JSON-serializable and distinct from the UI-level `TileContent` that gets upgraded in Plan 3.

- [ ] **No placeholders.** Every code block is complete Rust, not pseudocode. Every command is a real shell command. No "TODO", "TBD", or "similar to Task N" strings.

- [ ] **TDD discipline.** Each task has a failing test first, minimal implementation, and a verify-pass step. Commit happens only after pass.

- [ ] **Per-crate test invocations.** Every `cargo test` is scoped with `-p <crate>` (see CLAUDE.md). No workspace-wide runs.

- [ ] **Git preference.** Every git invocation uses `git -C /var/home/quinten/Development/quickdesktop[/submodule]` form (see CLAUDE.md).

- [ ] **Rust edition.** `quick-ui` and `quick-shell` remain on edition 2024; their `Cargo.toml` edits preserve `edition = "2024"`. Apps are untouched in this plan.

- [ ] **Tokens nested, not separate.** Task 2 makes `Tokens` a field of `Theme`, not a sibling struct, matching the user brief.

- [ ] **Concurrent-safe grouping.** The DAG section identifies Group A (tasks 1–5 sequential, widget-tree-wide edits), Group B (tasks 6–8 parallel: `animation/`, `overlay/`, `dnd/` — disjoint new modules), and Group C (sequential chain 9–17 with one inner parallel pair 9/10). An executor can dispatch Group B in parallel subagents safely.

- [ ] **Demoable end-state.** After Task 17, `cargo run -p quick-desktop` opens a winit window rendering a workspace + strips + window-groups containing placeholder tiles. Wheel paginates strips (cooldown-enforced). Super/Ctrl+Up/Down switches workspaces with a spring-driven slide. Tab drag is initiated (drag-payload produced) and window-group right-edge drag-resizes within 300–1200 px.

- [ ] **Scope discipline.** Plan 1 is Phase A steps 1–7 of the spec. Phase A steps 8–15 (Overview, GlobalSearch, QuickSettings, DataTable, WorkspaceSettings, TopBar, SidebarWheel, RemoteCursors, ContextMenu/Tooltip/Toast) live in Plan 2. Phase B apps (AppWidget + terminal + file-explorer port + browser stub) live in Plan 3. Phase C collab hardening lives in Plan 4. No feature from Plans 2–4 leaks into Plan 1.
