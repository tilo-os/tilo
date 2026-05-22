//! `ShellDriver` — programmable, headless-friendly driver for the nirios
//! shell tree. Owns a `ShellOverlayHost` plus everything the layout +
//! draw + event passes need (taffy tree, text renderer, theme, viewport,
//! Automerge doc), and exposes a small API the three modes (window,
//! offscreen, headless) and the `Scenario` engine all share.
//!
//! Intentionally generous in surface so the same driver covers:
//!   - "render this scene to PNG" (offscreen)
//!   - "drive a scenario, dump state" (headless / AI-control / e2e)
//!   - "pre-run a scenario then go interactive" (window mode)

use std::collections::HashMap;
use std::time::Duration;

use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Point, Rect};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

use quick_desktop::compositor::frame::FrameBuilder;
use quick_desktop::shell::chrome_state::{Chrome, ChromeState, WsTab};
use quick_desktop::shell::overlay_host::ShellOverlayHost;
use quick_desktop::shell::quick_settings::QsLevel;
use quick_desktop::shell::state::WorkspaceTree;
use quick_ui::Widget;

use quick_ui::theme::Theme;
use taffy::prelude::NodeId;

/// Synthetic frame duration for `step_frames` — matches a 60 Hz monitor.
/// Used only as the unit for the spin-wait loop; animations themselves
/// read real wall time off `Instant::now()` inside the host so we still
/// have to spend that time. Kept short enough that sleeping 60 frames
/// (~1 s) doesn't blow scenario runtime.
const FRAME_MS: u64 = 16;

pub struct ShellDriver {
    /// Layout + paint core, shared with the native compositor mode.
    /// The driver wraps it and adds input dispatch, focus tracking,
    /// and modifier state on top.
    fb: FrameBuilder,
    state: AutoCommit,
    root_obj: ObjId,
    modifiers: Modifiers,
    focus: Option<NodeId>,
    last_cursor: Point,
}

impl ShellDriver {
    /// Build a driver around the canonical bootstrap workspace tree at
    /// `viewport` size. Equivalent to what `bin/shots`'s headless path
    /// did per-scene, but stateful across actions.
    pub fn new(viewport: (u32, u32)) -> Self {
        Self::with_tree(WorkspaceTree::bootstrap_default(), viewport)
    }

    pub fn with_tree(tree: WorkspaceTree, viewport: (u32, u32)) -> Self {
        Self {
            fb: FrameBuilder::with_tree(tree, viewport),
            state: AutoCommit::new(),
            root_obj: automerge::ROOT.clone(),
            modifiers: Modifiers::default(),
            focus: None,
            last_cursor: Point::ORIGIN,
        }
    }

    /// Run compute_layout + apply_layout against the current tree. The
    /// host's animation state is preserved because we don't rebuild the
    /// taffy tree — only the layout pass re-runs.
    ///
    /// If the host reports `take_structure_dirty()` (e.g. a tab drop
    /// spawned a new `WindowGroup`, or a closed group's fade elapsed),
    /// the taffy tree is rebuilt first so the new structure participates
    /// in layout. Mirrors `App::handle_window_event`'s post-event sweep.
    pub fn relayout(&mut self) {
        self.fb.relayout();
    }

    /// Mutate chrome state — opens overlays, switches tabs, tweaks
    /// quick-settings, etc. Borrows the chrome internally so callers
    /// don't have to juggle the `RefCell`.
    pub fn with_chrome<F: FnOnce(&mut ChromeState)>(&mut self, f: F) {
        self.fb.with_chrome(f);
    }

    /// Read-only handle to the chrome cell. Use for assertions /
    /// serialisation — the caller is responsible for keeping the
    /// `Ref` short-lived.
    pub fn chrome(&self) -> std::cell::Ref<'_, ChromeState> {
        self.fb.chrome()
    }

    #[allow(dead_code)]
    pub fn chrome_handle(&self) -> Chrome {
        self.fb.chrome_handle()
    }

    pub fn host(&self) -> &ShellOverlayHost {
        self.fb.host()
    }

    #[allow(dead_code)]
    pub fn host_mut(&mut self) -> &mut ShellOverlayHost {
        self.fb.host_mut()
    }

    #[allow(dead_code)]
    pub fn theme(&self) -> &Theme {
        self.fb.theme()
    }

    pub fn viewport(&self) -> (f64, f64) {
        self.fb.viewport()
    }

    /// Replace the active modifier set. Scenarios call this when they
    /// need a chord (e.g. Cmd+K, Cmd+Tab); a `dispatch` of the resulting
    /// `Key(Tab)` then sees `is_mod = true` inside the host.
    pub fn set_modifiers(&mut self, state: ModifiersState) {
        self.modifiers = Modifiers::from(state);
    }

    /// Dispatch a single `WindowEvent` through the host's `handle_event`
    /// pipeline. Returns whether the event was consumed.
    pub fn dispatch(&mut self, event: WindowEvent) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = &event {
            self.last_cursor = Point::new(position.x, position.y);
        }
        if let WindowEvent::ModifiersChanged(m) = &event {
            self.modifiers = *m;
        }
        let mut handled = false;
        let child = self.fb.host_node();
        let (host, taffy) = self.fb.host_and_taffy_mut();
        host.handle_event(
            &event,
            1.0,
            &self.modifiers,
            &mut self.state,
            &self.root_obj,
            &mut handled,
            &mut self.focus,
            child,
            taffy,
        );
        handled
    }

    /// Synthesise a click at `p` — a `CursorMoved` to position, then a
    /// `Pressed`+`Released` left-button pair. Each step gets its own
    /// `dispatch` so per-event hit-tests run in the same order they
    /// would on real hardware.
    pub fn click_at(&mut self, p: Point) {
        self.dispatch(WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(p.x, p.y),
        });
        self.dispatch(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        self.dispatch(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }

    /// Press at `from`, drag through `steps` intermediate `CursorMoved`
    /// events, release at `to`. Used by scenarios that exercise the
    /// tab-drag / wheel-spin flows.
    pub fn drag(&mut self, from: Point, to: Point, steps: u32) {
        self.dispatch(WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(from.x, from.y),
        });
        self.dispatch(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        let n = steps.max(1) as f64;
        for s in 1..=steps.max(1) {
            let t = s as f64 / n;
            let p = Point::new(
                from.x + (to.x - from.x) * t,
                from.y + (to.y - from.y) * t,
            );
            self.dispatch(WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: PhysicalPosition::new(p.x, p.y),
            });
        }
        self.dispatch(WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }

    /// Synthesise a `KeyboardInput { state: Pressed }` for `key`.
    pub fn press_key(&mut self, key: Key) {
        let event = key_press_event(key);
        self.dispatch(event);
    }

    /// Type a literal string — one Key::Character event per non-control
    /// character. Triggers the same handle_event path the live binary's
    /// keyboard handler does. Caller is expected to have opened an
    /// input target (palette, name editor, etc.) first.
    pub fn type_str(&mut self, s: &str) {
        for c in s.chars() {
            if c.is_control() {
                continue;
            }
            let event = key_press_event(Key::Character(
                winit::keyboard::SmolStr::new(c.to_string()),
            ));
            self.dispatch(event);
        }
    }

    /// Step animation timers by `frames` × ~16 ms. Drives close fades,
    /// crossfades, palette/QS/WS-settings open animations, etc. Sleeps
    /// real time because the host reads `Instant::now()` directly —
    /// there's no virtual clock hook (yet).
    pub fn step_frames(&mut self, frames: u32) {
        for _ in 0..frames {
            std::thread::sleep(Duration::from_millis(FRAME_MS));
            // Drive the per-frame update + redraw so animation state
            // advances. `update` runs the per-tick reconciler too.
            self.fb
                .host_mut()
                .update(&mut self.state, &self.root_obj);
            // Re-draw into a throwaway scene so tick functions inside
            // `draw` (overview crossfade tick, etc.) see elapsed time.
            let _ = self.fb.paint();
        }
    }

    /// Render the current frame into a fresh Vello scene. Caller is
    /// responsible for passing it to a renderer (offscreen or live).
    pub fn paint(&mut self) -> Scene {
        self.fb.paint()
    }

    /// Convenience: relayout + draw. Used by the offscreen scene-set
    /// path (legacy `--scenes`) where state mutations between scenes
    /// can affect the structural layout (overlay reservations).
    pub fn paint_settled(&mut self) -> Scene {
        self.fb.paint_settled()
    }

    /// Apply the named "scene" mutation set (the legacy `--scenes`
    /// catalogue). Each scene is a one-shot mutation — call on a fresh
    /// driver per scene if a clean slate is required.
    pub fn scene_setup(&mut self, name: &str) -> Result<(), String> {
        match name {
            "desktop" => {}
            "overview" => {
                self.with_chrome(|st| st.overlays.overview = true);
                self.fb.host_mut().force_overview_settled(true);
            }
            "palette" => {
                self.fb.host_mut().set_palette_visible(true);
            }
            "quick-settings" => {
                self.with_chrome(|st| st.overlays.quick_settings = true);
            }
            "qs-wifi-drillin" => {
                self.with_chrome(|st| {
                    st.overlays.quick_settings = true;
                    st.quick_settings.push_level(QsLevel::Wifi);
                });
            }
            "qs-bt-drillin" => {
                self.with_chrome(|st| {
                    st.overlays.quick_settings = true;
                    st.quick_settings.push_level(QsLevel::Bluetooth);
                });
            }
            "ws-settings-general" => {
                self.with_chrome(|st| {
                    st.overlays.ws_settings = true;
                    st.ws_settings_tab = WsTab::General;
                });
            }
            "ws-settings-roles" => {
                self.with_chrome(|st| {
                    st.overlays.ws_settings = true;
                    st.ws_settings_tab = WsTab::Roles;
                });
            }
            "ws-settings-files" => {
                self.with_chrome(|st| {
                    st.overlays.ws_settings = true;
                    st.ws_settings_tab = WsTab::Files;
                });
            }
            "ws-settings-permissions" => {
                self.with_chrome(|st| {
                    st.overlays.ws_settings = true;
                    st.ws_settings_tab = WsTab::Permissions;
                });
            }
            other => return Err(format!("unknown scene '{other}'")),
        }
        Ok(())
    }

    /// Catalogue of scene names supported by `scene_setup`. Used by the
    /// legacy `--scenes` flag for default expansion + validation.
    pub const ALL_SCENES: &'static [&'static str] = &[
        "desktop",
        "overview",
        "palette",
        "quick-settings",
        "qs-wifi-drillin",
        "qs-bt-drillin",
        "ws-settings-general",
        "ws-settings-roles",
        "ws-settings-files",
        "ws-settings-permissions",
    ];

    /// Resolve a "named" UI target (button, pill, tab) to its on-screen
    /// rect in viewport-local coordinates. Returns `None` if the rect
    /// is `Rect::ZERO` (which generally means "this widget hasn't been
    /// drawn yet" — call `paint` once first to populate it).
    ///
    /// Resolution order (first non-zero match wins):
    /// 1. Hand-curated table — the five legacy keys that depend on
    ///    chrome-internal accessors that aren't reachable by tree walk.
    /// 2. Widget-tree registry — every widget with a `test_id` or
    ///    discoverable name/path key (see `widget_registry`).
    ///
    /// Names supported by the hand-curated table:
    ///   - `"add_workspace"` — the "+ Add workspace" tile in the overview
    ///     (only valid while the overview is open).
    ///   - `"ws_settings_tab[general|roles|files|permissions]"` — modal
    ///     tab pills (only valid while ws-settings is open).
    ///
    /// Beyond those, scenarios can address any widget by its `test_id`
    /// (e.g. `command_palette_input`, `wheel_disc`) or by its path key
    /// (e.g. `ShellOverlayHost/TopBarChrome` if the widget didn't opt in
    /// to a `test_id`).
    pub fn resolve_named_rect(&self, name: &str) -> Option<Rect> {
        let host = self.fb.host();
        let curated = match name {
            "add_workspace" => Some(host.add_workspace_rect_for_tests()),
            "ws_settings_tab[general]" => Some(host.ws_settings_tab_pill_rect(WsTab::General)),
            "ws_settings_tab[roles]" => Some(host.ws_settings_tab_pill_rect(WsTab::Roles)),
            "ws_settings_tab[files]" => Some(host.ws_settings_tab_pill_rect(WsTab::Files)),
            "ws_settings_tab[permissions]" => {
                Some(host.ws_settings_tab_pill_rect(WsTab::Permissions))
            }
            _ => None,
        };
        if let Some(r) = curated {
            if r.area() > 0.0 {
                return Some(r);
            }
            // Fall through to the registry — the curated key may have
            // missed because its prerequisite chrome state isn't set
            // (overview closed, ws-settings closed). Registry can still
            // succeed for an opt-in `test_id` regardless.
        }
        let registry = self.widget_registry();
        registry.get(name).copied().filter(|r| r.area() > 0.0)
    }

    /// Walk the widget tree from the overlay host and return a snapshot
    /// of every addressable widget's rect, keyed by:
    ///   - its `test_id()` if it opted in, OR
    ///   - a path key `Parent/Child[index]` reflecting the position in
    ///     the tree (index disambiguates siblings of the same name).
    ///
    /// The registry is **rebuilt on every call** — there's no hidden
    /// cache. Callers that need the registry across multiple actions
    /// in a scenario should re-fetch it after each `paint` because
    /// layout (and therefore rects) can shift between frames.
    ///
    /// Widgets at `Rect::ZERO` are still registered (a missing rect is
    /// information too — the caller can detect a widget that exists
    /// but isn't laid out yet). `resolve_named_rect` filters those out.
    pub fn widget_registry(&self) -> HashMap<String, Rect> {
        // Delegate to the shared walk in `compositor::frame` so the
        // out-of-process `compositor::test_control` and the in-process
        // ShellDriver see exactly the same target set.
        self.fb.widget_registry()
    }

    /// Centre of a named rect — convenience for the scenario engine.
    pub fn resolve_named_centre(&self, name: &str) -> Option<Point> {
        self.resolve_named_rect(name).map(|r| r.center())
    }

    /// Public read-through to the host's palette query, so scenarios /
    /// assertions don't have to drill through `host()`.
    #[allow(dead_code)]
    pub fn palette_query(&self) -> &str {
        self.fb.host().palette_query()
    }
}

// ---------- helpers ----------

// `walk_widget` lives in `compositor::frame` so the out-of-process
// `compositor::test_control` and the in-process ShellDriver share one
// implementation. See `quick-shell/src/compositor/frame.rs`.

/// Synthesise a `WindowEvent::KeyboardInput { state: Pressed }` for
/// `key`. winit's `KeyEvent` has a `pub(crate)` field
/// (`platform_specific`) so we zero-init the struct + overwrite the
/// fields the shell actually reads. Same workaround the e2e harness
/// uses (see `tests/e2e_shell_flows.rs::escape_event`).
pub fn key_press_event(key: Key) -> WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = key.clone();
    ev.physical_key =
        PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    // `text` is `Option<SmolStr>` — for ASCII characters we mirror what
    // winit normally fills so the palette's character-input branch
    // sees the same glyph it would on real hardware.
    ev.text = if let Key::Character(s) = &key {
        Some(s.clone())
    } else {
        None
    };
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

/// Convenience: a Pressed key event for a [`NamedKey`]. Used by the
/// scenario engine's `Key(NamedKey::Foo)` action variant.
#[allow(dead_code)]
pub fn named_key_event(key: NamedKey) -> WindowEvent {
    key_press_event(Key::Named(key))
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive Cmd+Tab through `dispatch` and verify the chrome's
    /// `overlays.overview` flag flips. Mirrors the live binary's
    /// hotkey path through real `handle_event` dispatch on the host
    /// — exercises the full `dispatch → ShellOverlayHost::handle_event
    /// → chrome.borrow_mut()` chain.
    #[test]
    fn driver_dispatch_routes_click_to_chrome() {
        let mut d = ShellDriver::new((1600, 900));
        let _ = d.paint();
        assert!(!d.chrome().overlays.overview);
        d.set_modifiers(ModifiersState::SUPER);
        d.press_key(Key::Named(NamedKey::Tab));
        assert!(
            d.chrome().overlays.overview,
            "Cmd+Tab should toggle the overview overlay on through driver dispatch"
        );
    }

    /// Toggle the palette open via the public host API, step a few
    /// frames so the palette open animation can register, then assert
    /// the palette is still visible. Pinned smoke for `step_frames`
    /// not panicking and not nuking visibility flags.
    #[test]
    fn driver_step_frames_advances_palette_fade() {
        let mut d = ShellDriver::new((1600, 900));
        assert!(!d.host().palette_visible());
        d.host_mut().set_palette_visible(true);
        d.step_frames(6);
        assert!(
            d.host().palette_visible(),
            "step_frames should advance animation timers without flipping visibility"
        );
        let _ = d.paint();
    }

    /// Paint the overview, then verify the named-rect resolver can pick
    /// out the "+ Add workspace" tile (which `draw_overview` populates
    /// each visible frame).
    #[test]
    fn driver_resolve_named_rect_for_overview_add_workspace() {
        let mut d = ShellDriver::new((1600, 900));
        d.with_chrome(|st| st.overlays.overview = true);
        d.host_mut().force_overview_settled(true);
        let _ = d.paint_settled();
        let rect = d.resolve_named_rect("add_workspace");
        assert!(
            rect.is_some(),
            "overview should populate the add_workspace rect after a settled paint"
        );
    }

    /// End-to-end pin for the user's "drag-tab-out replaces instead of
    /// creating a sibling" report: with the bootstrap default's 4-tab
    /// Wide group, dragging the second tab pill (Files) past the source
    /// group's right edge and releasing in the empty strip area must
    /// produce TWO groups — the 3-tab source and a 1-tab new group —
    /// not one.
    ///
    /// The earlier `tab_drag_out_to_new_group` headless scenario used
    /// guessed coords (420, 80) → (1100, 80) which missed the actual
    /// pill rect (the leading icon column shifts pill 1 well past
    /// x=420 inside a Wide=800 group laid out from x≈224). This test
    /// reads the live `tab_rects()` off the bar so the press lands
    /// exactly on a pill regardless of layout drift.
    #[test]
    fn driver_drag_tab_out_creates_sibling_group() {
        let mut d = ShellDriver::new((1600, 900));
        let _ = d.paint(); // seed any draw-only caches the layout missed
        d.relayout();

        // Snapshot pre-drag layout: bootstrap default seeds one strip
        // with one Wide group carrying Notes/Files/Terminal/Browser.
        let strip_id = {
            let st = d.chrome();
            let ws_id = st.tree.active;
            *st.tree.active_strip.get(&ws_id).expect("active strip")
        };
        let pre = d.host().groups_per_strip();
        let pre_strip = pre
            .iter()
            .find(|(s, _)| *s == strip_id)
            .expect("strip in groups_per_strip");
        assert_eq!(
            pre_strip.1.len(),
            1,
            "precondition: bootstrap default has exactly one group"
        );
        assert_eq!(
            pre_strip.1[0].1.len(),
            4,
            "precondition: that group has four tabs"
        );

        // Read the second pill's rect off the live bar so the press
        // hit-tests against the pill the layout actually painted.
        let (from, to) = {
            let group = d
                .host_mut()
                .group_at_for_tests(strip_id, 0)
                .expect("group at idx 0");
            let group_rect = group.get_rect();
            let rects = group.tab_rects().to_vec();
            assert_eq!(rects.len(), 4, "bar must have 4 pill rects after layout");
            // Pill 1 (Files) — non-active source.
            let pill = rects[1];
            let from = pill.center();
            // Release point: 100 px right of the source group's right
            // edge, same y as the bar — squarely in the trailing gap.
            let to = Point::new(group_rect.x1 + 100.0, from.y);
            (from, to)
        };

        d.drag(from, to, 16);
        d.relayout();

        let post = d.host().groups_per_strip();
        let post_strip = post
            .iter()
            .find(|(s, _)| *s == strip_id)
            .expect("strip still present after drag");
        assert_eq!(
            post_strip.1.len(),
            2,
            "drag-out from a 4-tab group must produce exactly two groups \
             (3-tab source + 1-tab sibling), not one (the 'replace' bug). \
             post = {:?}",
            post_strip.1,
        );
        // Source kept 3 tabs, new group has 1.
        let counts: Vec<usize> = post_strip.1.iter().map(|(_, t)| t.len()).collect();
        assert!(
            counts.contains(&3) && counts.contains(&1),
            "expected one group with 3 tabs and one with 1 tab; got {:?}",
            counts,
        );

        // Both groups must be laid out — the user's visible "replace"
        // perception was the new group living at `Rect::ZERO` because
        // `GroupRow::structure_dirty` never bubbled up to `App` /
        // driver-level `relayout`. Pin both rects > 0 so a regression
        // would surface as a failure here, not as a silent invisible
        // sibling group at runtime.
        for i in 0..2 {
            let g = d
                .host_mut()
                .group_at_for_tests(strip_id, i)
                .expect("group laid out");
            let r = g.get_rect();
            assert!(
                r.area() > 0.0,
                "group[{i}] must have a non-zero rect after relayout; got {r:?}",
            );
        }
    }

    /// Smoke for `scene_setup` — drive every named scene and verify it
    /// returns Ok, then verify a fresh driver returns Err for an
    /// unknown name.
    #[test]
    fn driver_scene_setup_covers_catalogue() {
        for &name in ShellDriver::ALL_SCENES {
            let mut d = ShellDriver::new((1600, 900));
            d.scene_setup(name)
                .unwrap_or_else(|e| panic!("scene_setup({name}) failed: {e}"));
            let _ = d.paint_settled();
        }
        let mut d = ShellDriver::new((1600, 900));
        assert!(d.scene_setup("nope").is_err());
    }

    /// `widget_registry` must enumerate the live widget tree after a
    /// paint. The bootstrap default seeds a topbar inside the content
    /// chrome, so a registry walk should surface a `TopBarChrome` entry
    /// (either as a bare name or with a `[0]` suffix).
    ///
    /// Pinned so a future regression that breaks `ShellOverlayHost::
    /// children()` — the recursion entry-point — fails this test instead
    /// of silently returning an empty registry.
    #[test]
    fn widget_registry_finds_topbar_buttons() {
        let mut d = ShellDriver::new((1600, 900));
        let _ = d.paint();
        d.relayout();
        let _ = d.paint();
        let registry = d.widget_registry();
        assert!(
            !registry.is_empty(),
            "registry must not be empty after paint"
        );
        // The host (root) should be there.
        assert!(
            registry.keys().any(|k| k.contains("ShellOverlayHost")),
            "registry should contain ShellOverlayHost; keys={:?}",
            registry.keys().collect::<Vec<_>>()
        );
        // And the topbar chrome should be reachable through the content
        // subtree (added in Addition B once host.children() forwards
        // content + sidebar + palette).
        assert!(
            registry.keys().any(|k| k.contains("TopBarChrome")),
            "registry should contain TopBarChrome; keys={:?}",
            registry.keys().collect::<Vec<_>>()
        );
    }

    /// `resolve_named_rect` must fall through to the widget tree when
    /// the hand-curated table misses. The CommandPalette widget opts in
    /// to `test_id() == "command_palette_input"` (a high-value target);
    /// after opening the palette, the resolver should return its rect
    /// even though `command_palette_input` is not in the hand-curated
    /// match arm.
    #[test]
    fn resolve_named_rect_falls_back_to_registry() {
        let mut d = ShellDriver::new((1600, 900));
        // Open the palette — drives the same code path scenarios use,
        // so the palette's rect gets populated on the subsequent paint.
        d.host_mut().set_palette_visible(true);
        let _ = d.paint();
        d.relayout();
        let _ = d.paint();

        // Sanity: hand-curated table doesn't know this key.
        // (No direct assertion — we can't call the private inner match —
        // but the registry-fallback path is what makes the next assertion
        // pass; if a future edit accidentally adds this key to the
        // curated table, the test still passes.)
        let rect = d.resolve_named_rect("command_palette_input");
        assert!(
            rect.is_some(),
            "command_palette_input must resolve through the registry once \
             the palette is visible; registry keys: {:?}",
            d.widget_registry().keys().collect::<Vec<_>>()
        );
        let r = rect.unwrap();
        assert!(
            r.area() > 0.0,
            "resolved command_palette_input rect must be non-zero; got {r:?}"
        );
    }
}
