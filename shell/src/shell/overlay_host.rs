//! Shell overlay host — wraps the root content widget and draws modal
//! overlays (command palette for ⌘K, overview for ⌘Tab, etc.) on top.
//!
//! The `build_nirios_root` entry point returns this widget so that the
//! shell has exactly one place to route global keyboard events
//! (⌘K / ⌘Tab / Esc) and exactly one place to render the overlay stack
//! above the desktop content. Overlays each own their state — the host
//! just gates their visibility and paints a scrim when at least one is
//! showing.
//!
//! Phase 3 scope: ⌘K opens the command palette primitive. Overview /
//! quick-settings / workspace-settings wiring lands in follow-up tasks
//! alongside their click-surfaces.
//!
//! Key routing: `handle_event` consumes the palette-toggle and Esc
//! presses before propagating to the wrapped content. When the palette
//! is open, it also gets first crack at every event so typing-into-
//! query / Enter-to-select work without the desktop stealing focus.

use std::collections::HashMap;
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, LengthPercentageAuto, NodeId, Position, Rect as TaffyRect, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::kurbo::RoundedRect;
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use quick_ui::presence::PresenceCtx;
use quick_ui::text_render::TextRenderer;
use quick_ui::theme::Theme;
use quick_ui::widget::Widget;
use quick_ui::{CommandEntry, CommandPalette};

use crate::shell::chrome_state::Chrome;

pub struct ShellOverlayHost {
    rect: Rect,
    content: Box<dyn Widget>,
    palette: CommandPalette,
    palette_visible: bool,
    // Child node ids are captured once during `build_layout` so we can
    // forward layout/draw/event calls without re-walking the tree.
    content_node: Option<NodeId>,
    palette_node: Option<NodeId>,
    /// Shared chrome state; the host reads `overlays` flags from this to
    /// decide whether to draw overview / quick-settings / ws-settings on
    /// top of the content.
    chrome: Option<Chrome>,
    /// Last known cursor position in logical coords, for hit-testing
    /// overlay clicks (shared with the rest of the chrome).
    last_cursor: Option<Point>,
    /// Simulated remote peer cursor. The shell is collaborative-by-default,
    /// so we paint a deterministic fake peer ("Ada") over the desktop
    /// until the network layer feeds real `PresenceCtx::remote_cursors`
    /// into the widget tree — this keeps the collab chrome visible in
    /// single-process development. Lissajous-ish path seeded from `started`.
    started: Instant,
    /// Overview crossfade state: `overview_t` ∈ [0, 1] where 0 = desktop
    /// fully visible and 1 = overview fully visible. `overview_target` is
    /// the value `overview_t` is animating toward (latched on toggle).
    /// `transition_started`/`transition_from` bookend the in-flight
    /// animation so a re-toggle mid-fade picks up from the current visible
    /// alpha rather than popping back to 0 or 1.
    overview_t: f32,
    overview_target: f32,
    transition_started: Option<Instant>,
    transition_from: f32,
}

/// Crossfade duration between desktop ↔ overview. 200 ms reads as a
/// "soft swap" without dragging — matches the concept's spring preset
/// well enough without introducing a spring solver in the render loop.
const OVERVIEW_TRANSITION_MS: u64 = 200;

impl ShellOverlayHost {
    pub fn new(content: Box<dyn Widget>) -> Self {
        let mut palette = CommandPalette::new();
        for entry in [
            CommandEntry::new("search.open",        "Open global search"),
            CommandEntry::new("overview.toggle",    "Toggle overview"),
            CommandEntry::new("settings.open",      "Open workspace settings"),
            CommandEntry::new("quick-settings.open", "Open quick settings"),
        ] {
            palette.push(entry);
        }
        Self {
            rect: Rect::ZERO,
            content,
            palette,
            palette_visible: false,
            content_node: None,
            palette_node: None,
            chrome: None,
            last_cursor: None,
            started: Instant::now(),
            overview_t: 0.0,
            overview_target: 0.0,
            transition_started: None,
            transition_from: 0.0,
        }
    }

    /// Advance the overview crossfade toward `overview_target`. Latches
    /// a new transition if the target changed since last frame, then
    /// evaluates the ease-out cubic lerp. Returns the current `t` so the
    /// draw path can gate the content/overview alpha in lockstep.
    fn tick_overview_transition(&mut self, target: f32) -> f32 {
        if (target - self.overview_target).abs() > f32::EPSILON {
            self.transition_from = self.overview_t;
            self.overview_target = target;
            self.transition_started = Some(Instant::now());
        }
        if let Some(started) = self.transition_started {
            let elapsed = started.elapsed().as_millis() as f32;
            let duration = OVERVIEW_TRANSITION_MS as f32;
            if elapsed >= duration {
                self.overview_t = self.overview_target;
                self.transition_started = None;
            } else {
                let p = (elapsed / duration).clamp(0.0, 1.0);
                // Ease-out cubic: 1 - (1 - p)^3
                let eased = 1.0 - (1.0 - p).powi(3);
                self.overview_t =
                    self.transition_from + (self.overview_target - self.transition_from) * eased;
            }
        }
        self.overview_t
    }

    pub fn with_chrome(mut self, chrome: Chrome) -> Self {
        self.chrome = Some(chrome);
        self
    }

    fn toggle_palette(&mut self) {
        self.palette_visible = !self.palette_visible;
    }

    fn overlay_flags(&self) -> crate::shell::chrome_state::OverlayFlags {
        self.chrome
            .as_ref()
            .map(|c| c.borrow().overlays)
            .unwrap_or_default()
    }

    /// Mirror of the geometry in `draw_quick_settings` — the popover rect
    /// anchored under the top-right tray pill.
    fn quick_settings_rect(&self) -> Rect {
        let popover_w = 280.0_f64;
        let popover_h = 240.0_f64;
        let x1 = self.rect.x1 - 16.0;
        let y0 = self.rect.y0 + 48.0;
        Rect::new(x1 - popover_w, y0, x1, y0 + popover_h)
    }

    /// Collaborators dropdown rect — anchored to the top-right like quick-
    /// settings but narrower and taller-when-many-peers. Height is driven by
    /// the peer count so a roster of 8 doesn't clip.
    fn collaborators_rect(&self) -> Rect {
        let popover_w = 260.0_f64;
        let peers = self
            .chrome
            .as_ref()
            .map(|c| c.borrow().tree.collaborators.len())
            .unwrap_or(0);
        let rows = peers.max(1) as f64;
        let popover_h = 48.0 + rows * 36.0 + 16.0;
        let x1 = self.rect.x1 - 16.0;
        let y0 = self.rect.y0 + 48.0;
        Rect::new(x1 - popover_w, y0, x1, y0 + popover_h.min(420.0))
    }

    /// Name-input rect inside the ws-settings modal. Matches the drawn
    /// rect in `draw_ws_settings` so hit-test and draw stay in sync.
    fn ws_settings_name_rect(&self) -> Rect {
        let r = self.ws_settings_rect();
        let body_y = r.y0 + 168.0;
        Rect::new(r.x0 + 24.0, body_y + 20.0, r.x1 - 24.0, body_y + 48.0)
    }

    /// Mirror of the geometry in `draw_ws_settings` — centered modal card.
    fn ws_settings_rect(&self) -> Rect {
        let modal_w = 520.0_f64;
        let modal_h = 360.0_f64;
        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        Rect::new(
            cx - modal_w / 2.0,
            cy - modal_h / 2.0,
            cx + modal_w / 2.0,
            cy + modal_h / 2.0,
        )
    }

    /// Rect for the search-scope banner — sits 8 px above the palette with
    /// the same horizontal extent. Computed from the palette's post-layout
    /// rect so the banner tracks the palette even at different DPIs.
    fn search_banner_rect(&self) -> Rect {
        let p = self.palette.get_rect();
        if p.width() <= 0.0 {
            return Rect::ZERO;
        }
        let h = 32.0_f64;
        Rect::new(p.x0, p.y0 - h - 8.0, p.x1, p.y0 - 8.0)
    }

    /// Individual scope-pill rect inside the banner. `idx` = 0 → Workspace,
    /// 1 → All. Hit-test and draw consume this same helper.
    fn search_scope_pill_rect(&self, idx: usize) -> Rect {
        let b = self.search_banner_rect();
        if b.width() <= 0.0 {
            return Rect::ZERO;
        }
        let label_w = 96.0_f64; // "Search in:" prefix
        let pill_w = 90.0_f64;
        let pill_gap = 8.0_f64;
        let pill_h = 22.0_f64;
        let cy = (b.y0 + b.y1) / 2.0;
        let start = b.x0 + 16.0 + label_w;
        let x0 = start + idx as f64 * (pill_w + pill_gap);
        Rect::new(x0, cy - pill_h / 2.0, x0 + pill_w, cy + pill_h / 2.0)
    }

    /// Mirror of the geometry in `draw_overview`: return the index of the
    /// workspace card at logical position `p`, if any.
    fn hit_test_overview(&self, p: Point) -> Option<usize> {
        let chrome = self.chrome.as_ref()?;
        let state = chrome.borrow();
        let card_w = 240.0_f64;
        let card_h = 160.0_f64;
        let gap = 24.0_f64;
        let avail_w = self.rect.width() - 120.0;
        let cols = ((avail_w + gap) / (card_w + gap)).floor().max(1.0) as usize;
        let n = state.tree.workspaces.len();
        let rows = (n + cols - 1) / cols.max(1);
        let total_w = cols as f64 * card_w + (cols.saturating_sub(1)) as f64 * gap;
        let total_h = rows as f64 * card_h + (rows.saturating_sub(1)) as f64 * gap;
        let start_x = (self.rect.x0 + self.rect.x1) / 2.0 - total_w / 2.0;
        let start_y = (self.rect.y0 + self.rect.y1) / 2.0 - total_h / 2.0;
        for (i, _ws) in state.tree.workspaces.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let x0 = start_x + col as f64 * (card_w + gap);
            let y0 = start_y + row as f64 * (card_h + gap);
            let rect = Rect::new(x0, y0, x0 + card_w, y0 + card_h);
            if rect.contains(p) {
                return Some(i);
            }
        }
        None
    }
}

impl Widget for ShellOverlayHost {
    fn sync(&mut self, s: &mut AutoCommit, p: &ObjId) {
        self.content.sync(s, p);
        self.palette.sync(s, p);
    }

    fn update(&mut self, s: &mut AutoCommit, p: &ObjId) -> bool {
        let a = self.content.update(s, p);
        let b = self.palette.update(s, p);
        a || b
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let content_node = self.content.build_layout(taffy, theme, node_map);
        let palette_node = self.palette.build_layout(taffy, theme, node_map);
        // Overlay the palette above content: absolute positioning with
        // auto-margin horizontal centering and a fixed top offset so it
        // floats below the shell's top bar like a popover.
        let mut palette_style = taffy.style(palette_node).unwrap().clone();
        palette_style.position = Position::Absolute;
        palette_style.margin = TaffyRect {
            left: LengthPercentageAuto::auto(),
            right: LengthPercentageAuto::auto(),
            top: LengthPercentageAuto::length(0.0),
            bottom: LengthPercentageAuto::auto(),
        };
        palette_style.inset = TaffyRect {
            left: LengthPercentageAuto::auto(),
            right: LengthPercentageAuto::auto(),
            top: LengthPercentageAuto::length(120.0),
            bottom: LengthPercentageAuto::auto(),
        };
        taffy.set_style(palette_node, palette_style).unwrap();

        self.content_node = Some(content_node);
        self.palette_node = Some(palette_node);
        let node = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[content_node, palette_node],
            )
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        let children = taffy.children(node).unwrap();
        self.content
            .apply_layout(taffy, children[0], Point::new(self.rect.x0, self.rect.y0));
        self.palette
            .apply_layout(taffy, children[1], Point::new(self.rect.x0, self.rect.y0));
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let flags = self.overlay_flags();
        let target = if flags.overview { 1.0 } else { 0.0 };
        let t = self.tick_overview_transition(target);
        let content_alpha = 1.0 - t;
        let overview_alpha = t;

        // Desktop content + simulated peer fade out as overview fades in.
        // Skip the layer entirely at full opacity to keep the common path
        // cheap (vello push_layer clips + allocates an intermediate).
        if content_alpha >= 0.999 {
            self.content
                .draw(scene, transform, text_renderer, theme, presence);
            self.draw_demo_peer(scene, transform, text_renderer, theme);
        } else if content_alpha > 0.0 {
            scene.push_layer(BlendMode::default(), content_alpha, transform, &self.rect);
            self.content
                .draw(scene, transform, text_renderer, theme, presence);
            self.draw_demo_peer(scene, transform, text_renderer, theme);
            scene.pop_layer();
        }

        // Overview grid (Cmd+Tab) crossfades opposite the desktop content.
        if overview_alpha > 0.0 {
            if overview_alpha >= 0.999 {
                draw_scrim(scene, transform, self.rect);
                self.draw_overview(scene, transform, text_renderer, theme);
            } else {
                scene.push_layer(BlendMode::default(), overview_alpha, transform, &self.rect);
                draw_scrim(scene, transform, self.rect);
                self.draw_overview(scene, transform, text_renderer, theme);
                scene.pop_layer();
            }
        }

        // Workspace settings modal
        if flags.ws_settings {
            draw_scrim(scene, transform, self.rect);
            self.draw_ws_settings(scene, transform, text_renderer, theme);
        }

        // Quick-settings popover (anchored top-right, no scrim so it reads
        // as a dropdown rather than a modal)
        if flags.quick_settings {
            self.draw_quick_settings(scene, transform, text_renderer, theme);
        }

        // Collaborators dropdown (anchored top-right, no scrim — matches the
        // quick-settings dropdown behaviour and the concept's avatar stack
        // "open peer list" affordance).
        if flags.collaborators {
            self.draw_collaborators(scene, transform, text_renderer, theme);
        }

        if self.palette_visible {
            draw_scrim(scene, transform, self.rect);
            self.palette
                .draw(scene, transform, text_renderer, theme, presence);
            self.draw_search_banner(scene, transform, text_renderer, theme);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Track cursor position so we can hit-test overlay clicks in the
        // absence of position data on MouseInput.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Some(Point::new(
                position.x / scale_factor,
                position.y / scale_factor,
            ));
        }

        // Global shortcuts first — ⌘K / Ctrl+K toggles the palette, ⌘Tab
        // toggles the overview, and Esc closes whichever overlay is on top.
        if let WindowEvent::KeyboardInput {
            event: KeyEvent {
                logical_key,
                state: ElementState::Pressed,
                ..
            },
            ..
        } = event
        {
            let is_mod = modifiers.state().super_key() || modifiers.state().control_key();
            if is_mod {
                if let Key::Character(s) = logical_key {
                    if s.as_str().eq_ignore_ascii_case("k") {
                        self.toggle_palette();
                        *handled = true;
                        return;
                    }
                }
                if matches!(logical_key, Key::Named(NamedKey::Tab)) {
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        st.overlays.overview = !st.overlays.overview;
                        st.bump();
                        *handled = true;
                        return;
                    }
                }
                // Cmd+ArrowUp / Cmd+ArrowDown — switch active workspace.
                if matches!(logical_key, Key::Named(NamedKey::ArrowUp)) {
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        if st.switch_workspace(-1) {
                            *handled = true;
                        }
                        return;
                    }
                }
                if matches!(logical_key, Key::Named(NamedKey::ArrowDown)) {
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        if st.switch_workspace(1) {
                            *handled = true;
                        }
                        return;
                    }
                }
            }
            if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                // Priority order matches z-order: palette > overview > ws
                // settings > quick settings. Esc cancels an in-flight name
                // edit before closing the ws-settings modal.
                if self.palette_visible {
                    self.palette_visible = false;
                    *handled = true;
                    return;
                }
                if let Some(chrome) = self.chrome.clone() {
                    let mut st = chrome.borrow_mut();
                    if st.editing_workspace_name.is_some() {
                        st.editing_workspace_name = None;
                        st.bump();
                        *handled = true;
                        return;
                    }
                    if st.overlays.overview {
                        st.overlays.overview = false;
                        st.bump();
                        *handled = true;
                        return;
                    }
                    if st.overlays.ws_settings {
                        st.overlays.ws_settings = false;
                        st.bump();
                        *handled = true;
                        return;
                    }
                    if st.overlays.collaborators {
                        st.overlays.collaborators = false;
                        st.bump();
                        *handled = true;
                        return;
                    }
                    if st.overlays.quick_settings {
                        st.overlays.quick_settings = false;
                        st.bump();
                        *handled = true;
                        return;
                    }
                }
            }

            // Workspace-name editor. When a buffer is in flight we consume
            // character / backspace / enter before anything else gets a
            // chance at them — modifiers already handled above.
            if let Some(chrome) = self.chrome.clone() {
                let is_editing = chrome.borrow().editing_workspace_name.is_some();
                if is_editing && !is_mod {
                    match logical_key {
                        Key::Named(NamedKey::Enter) => {
                            chrome.borrow_mut().commit_workspace_name();
                            *handled = true;
                            return;
                        }
                        Key::Named(NamedKey::Backspace) => {
                            let mut st = chrome.borrow_mut();
                            if let Some(buf) = st.editing_workspace_name.as_mut() {
                                buf.pop();
                            }
                            st.bump();
                            *handled = true;
                            return;
                        }
                        Key::Named(NamedKey::Space) => {
                            let mut st = chrome.borrow_mut();
                            if let Some(buf) = st.editing_workspace_name.as_mut() {
                                buf.push(' ');
                            }
                            st.bump();
                            *handled = true;
                            return;
                        }
                        Key::Character(s) => {
                            let mut st = chrome.borrow_mut();
                            if let Some(buf) = st.editing_workspace_name.as_mut() {
                                for c in s.chars() {
                                    if !c.is_control() {
                                        buf.push(c);
                                    }
                                }
                            }
                            st.bump();
                            *handled = true;
                            return;
                        }
                        _ => {}
                    }
                }
            }
        }

        // Outside-click close: if a popover/modal is up and the click
        // lands outside its rect, dismiss it and swallow the click so the
        // content below doesn't react. Overview has its own dismissal path
        // below (click-to-activate-workspace) so we skip it here.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        {
            if let Some(p) = self.last_cursor {
                if self.palette_visible {
                    // Scope-pill click (runs before outside-click so users
                    // can swap scope without dismissing the palette).
                    for idx in 0..2 {
                        if self.search_scope_pill_rect(idx).contains(p) {
                            if let Some(chrome) = self.chrome.clone() {
                                use crate::shell::chrome_state::SearchScope;
                                let mut st = chrome.borrow_mut();
                                st.search_scope = if idx == 0 {
                                    SearchScope::Workspace
                                } else {
                                    SearchScope::All
                                };
                                st.bump();
                            }
                            *handled = true;
                            return;
                        }
                    }
                    let palette_rect = self.palette.get_rect();
                    let banner_rect = self.search_banner_rect();
                    if !palette_rect.contains(p) && !banner_rect.contains(p) {
                        self.palette_visible = false;
                        *handled = true;
                        return;
                    }
                }
                let flags = self.overlay_flags();
                if flags.collaborators {
                    let co_rect = self.collaborators_rect();
                    if !co_rect.contains(p) {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            st.overlays.collaborators = false;
                            st.bump();
                        }
                        *handled = true;
                        return;
                    }
                    // Inside the dropdown: swallow the click so the content
                    // beneath doesn't grab it. No per-row actions yet.
                    *handled = true;
                    return;
                }
                if flags.quick_settings {
                    let qs_rect = self.quick_settings_rect();
                    if !qs_rect.contains(p) {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            st.overlays.quick_settings = false;
                            st.bump();
                        }
                        *handled = true;
                        return;
                    }
                    // Click inside the popover: route to slider / toggle
                    // hit-testing. Sliders snap to click x; toggles flip.
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        let mut changed = false;
                        for i in 0..2 {
                            let (bar, _) = self.quick_settings_slider_geometry(i);
                            // Extend the hit region vertically so the whole
                            // slider row (including padding + label) is
                            // clickable, not just the 6 px bar.
                            let hit = Rect::new(bar.x0 - 4.0, bar.y0 - 12.0, bar.x1 + 4.0, bar.y1 + 12.0);
                            if hit.contains(p) {
                                let t = ((p.x - bar.x0) / bar.width()).clamp(0.0, 1.0) as f32;
                                if i == 0 {
                                    st.quick_settings.brightness = t;
                                } else {
                                    st.quick_settings.volume = t;
                                }
                                changed = true;
                            }
                        }
                        for i in 0..2 {
                            let prect = self.quick_settings_toggle_geometry(i);
                            let hit = Rect::new(prect.x0 - 4.0, prect.y0 - 4.0, prect.x1 + 4.0, prect.y1 + 4.0);
                            if hit.contains(p) {
                                if i == 0 {
                                    st.quick_settings.wifi = !st.quick_settings.wifi;
                                } else {
                                    st.quick_settings.bluetooth = !st.quick_settings.bluetooth;
                                }
                                changed = true;
                            }
                        }
                        if changed {
                            st.bump();
                            *handled = true;
                            return;
                        }
                    }
                    // Inside the popover but not on a control — swallow so
                    // the content below doesn't steal the click.
                    *handled = true;
                    return;
                }
                if flags.ws_settings {
                    let modal_rect = self.ws_settings_rect();
                    if !modal_rect.contains(p) {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut st = chrome.borrow_mut();
                            // Commit any in-flight name edit before closing
                            // so clicking away acts like "save & dismiss".
                            st.commit_workspace_name();
                            st.overlays.ws_settings = false;
                            st.bump();
                        }
                        *handled = true;
                        return;
                    }
                    // Click inside the modal: routes to the Name input rect
                    // (begins an edit) or — on any other click inside the
                    // modal — commits the pending edit if one is in flight.
                    if let Some(chrome) = self.chrome.clone() {
                        let name_rect = self.ws_settings_name_rect();
                        let mut st = chrome.borrow_mut();
                        if name_rect.contains(p) {
                            if st.editing_workspace_name.is_none() {
                                st.editing_workspace_name = st
                                    .tree
                                    .active_workspace()
                                    .map(|w| w.name.clone())
                                    .or_else(|| Some(String::new()));
                                st.bump();
                            }
                        } else {
                            st.commit_workspace_name();
                        }
                    }
                    *handled = true;
                    return;
                }
            }
        }

        // Click-on-overview-card: if the overview is visible, consume the
        // mouse click and — if it lands on a workspace card — activate
        // that workspace + close the overview.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        {
            let flags = self.overlay_flags();
            if flags.overview {
                if let Some(p) = self.last_cursor {
                    let hit = self.hit_test_overview(p);
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        if let Some(idx) = hit {
                            if let Some(ws) = st.tree.workspaces.get(idx) {
                                let id = ws.id;
                                st.tree.set_active(id);
                            }
                        }
                        // Clicking anywhere dismisses the overview once a
                        // workspace has been chosen (or missed).
                        st.overlays.overview = false;
                        st.bump();
                    }
                    *handled = true;
                    return;
                }
            }
        }

        let children = taffy.children(node_id).unwrap();
        if self.palette_visible {
            self.palette.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                children[1],
                taffy,
            );
            if *handled {
                return;
            }
        }
        self.content.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            children[0],
            taffy,
        );
    }

    fn name(&self) -> &str {
        "ShellOverlayHost"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn draw_scrim(scene: &mut Scene, transform: Affine, rect: Rect) {
    scene.fill(
        Fill::NonZero,
        transform,
        Color::rgba8(0, 0, 0, 154),
        None,
        &rect,
    );
}

impl ShellOverlayHost {
    fn draw_overview(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(chrome) = self.chrome.clone() else { return };
        let state = chrome.borrow();
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        // Centered grid of workspace cards — responsive column count based
        // on width; cards are 240×160 with 24 px gap.
        let card_w = 240.0_f64;
        let card_h = 160.0_f64;
        let gap = 24.0_f64;
        let avail_w = self.rect.width() - 120.0;
        let cols = ((avail_w + gap) / (card_w + gap)).floor().max(1.0) as usize;
        let n = state.tree.workspaces.len();
        let rows = (n + cols - 1) / cols.max(1);
        let total_w = cols as f64 * card_w + (cols.saturating_sub(1)) as f64 * gap;
        let total_h = rows as f64 * card_h + (rows.saturating_sub(1)) as f64 * gap;
        let start_x = (self.rect.x0 + self.rect.x1) / 2.0 - total_w / 2.0;
        let start_y = (self.rect.y0 + self.rect.y1) / 2.0 - total_h / 2.0;

        // Title: "Workspaces"
        let title = "Workspaces";
        let title_size = tokens.typography.scale.xl;
        let (tw, th) = text_renderer.measure(title, title_size, None);
        let tx = (self.rect.x0 + self.rect.x1) / 2.0 - tw as f64 / 2.0;
        let ty = start_y - 40.0 - th as f64;
        text_renderer.draw_text(
            scene,
            title,
            title_size,
            colors.text_primary,
            transform * Affine::translate((tx, ty)),
            None,
        );

        for (i, ws) in state.tree.workspaces.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let x0 = start_x + col as f64 * (card_w + gap);
            let y0 = start_y + row as f64 * (card_h + gap);
            let rect = Rect::new(x0, y0, x0 + card_w, y0 + card_h);
            let is_active = ws.id == state.tree.active;
            let fill = if is_active { colors.surfaces.raised } else { colors.neutral[1] };
            let card = RoundedRect::from_rect(rect, tokens.radii.lg as f64);
            scene.fill(Fill::NonZero, transform, fill, None, &card);
            if is_active {
                scene.stroke(
                    &vello::kurbo::Stroke::new(2.0),
                    transform,
                    colors.accent[5],
                    None,
                    &card,
                );
            } else {
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    colors.border_standard,
                    None,
                    &card,
                );
            }

            // Draw workspace name top-left
            let name_size = tokens.typography.scale.base;
            text_renderer.draw_text(
                scene,
                &ws.name,
                name_size,
                colors.text_primary,
                transform * Affine::translate((rect.x0 + 16.0, rect.y0 + 16.0)),
                None,
            );

            // Subtitle: strip / window counts
            let win_count: usize = ws.strips.iter().map(|s| s.window_groups.len()).sum();
            let sub = format!("{} strips · {} windows", ws.strips.len(), win_count);
            let sub_size = tokens.typography.scale.sm;
            text_renderer.draw_text(
                scene,
                &sub,
                sub_size,
                colors.text_dim,
                transform * Affine::translate((rect.x0 + 16.0, rect.y0 + 40.0)),
                None,
            );

            // Mini preview: draw each window-group as a rounded rectangle
            // inside the card, laid out horizontally.
            let preview_rect = Rect::new(
                rect.x0 + 16.0,
                rect.y0 + 70.0,
                rect.x1 - 16.0,
                rect.y1 - 16.0,
            );
            if win_count > 0 {
                let tile_gap = 6.0;
                let tile_w = (preview_rect.width() - tile_gap * (win_count.saturating_sub(1)) as f64)
                    / win_count as f64;
                let mut tx = preview_rect.x0;
                for strip in &ws.strips {
                    for g in &strip.window_groups {
                        let tile = Rect::new(tx, preview_rect.y0, tx + tile_w, preview_rect.y1);
                        let tile_rr =
                            RoundedRect::from_rect(tile, tokens.radii.sm as f64);
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            colors.surfaces.window_card,
                            None,
                            &tile_rr,
                        );
                        // Draw a little tab strip on top of each preview tile
                        let tab_h = 6.0;
                        let tab = Rect::new(tile.x0, tile.y0, tile.x1, tile.y0 + tab_h);
                        scene.fill(Fill::NonZero, transform, colors.accent[3], None, &tab);
                        let _ = g;
                        tx += tile_w + tile_gap;
                    }
                }
            } else {
                // Empty workspace placeholder dashed area.
                let pr = RoundedRect::from_rect(preview_rect, tokens.radii.sm as f64);
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0).with_dashes(0.0, [4.0, 4.0]),
                    transform,
                    colors.border_standard,
                    None,
                    &pr,
                );
                let empty_label = "empty";
                let (ew, eh) = text_renderer.measure(empty_label, sub_size, None);
                let ex = (preview_rect.x0 + preview_rect.x1) / 2.0 - ew as f64 / 2.0;
                let ey = (preview_rect.y0 + preview_rect.y1) / 2.0 - eh as f64 / 2.0;
                text_renderer.draw_text(
                    scene,
                    empty_label,
                    sub_size,
                    colors.text_dim,
                    transform * Affine::translate((ex, ey)),
                    None,
                );
            }
        }
    }

    fn draw_ws_settings(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(chrome) = self.chrome.clone() else { return };
        let state = chrome.borrow();
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        let modal_w = 520.0_f64;
        let modal_h = 360.0_f64;
        let cx = (self.rect.x0 + self.rect.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        let rect = Rect::new(
            cx - modal_w / 2.0,
            cy - modal_h / 2.0,
            cx + modal_w / 2.0,
            cy + modal_h / 2.0,
        );
        let card = RoundedRect::from_rect(rect, tokens.radii.lg as f64);
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &card);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &card,
        );

        // Title
        let title = "Workspace Settings";
        let tsize = tokens.typography.scale.xl;
        text_renderer.draw_text(
            scene,
            title,
            tsize,
            colors.text_primary,
            transform * Affine::translate((rect.x0 + 24.0, rect.y0 + 24.0)),
            None,
        );

        // Active workspace label
        let active_name = state
            .tree
            .active_workspace()
            .map(|w| w.name.clone())
            .unwrap_or_default();
        let label = format!("Active workspace: {}", active_name);
        let lsize = tokens.typography.scale.base;
        text_renderer.draw_text(
            scene,
            &label,
            lsize,
            colors.text_dim,
            transform * Affine::translate((rect.x0 + 24.0, rect.y0 + 72.0)),
            None,
        );

        // Tabs header (General / Roles / Files) — visual-only for now
        let tabs = ["General", "Roles", "Files", "Collaborators"];
        let mut tx = rect.x0 + 24.0;
        let ty = rect.y0 + 112.0;
        for (i, tab) in tabs.iter().enumerate() {
            let active = i == 0;
            let (w, h) = text_renderer.measure(tab, lsize, None);
            let pill_rect = Rect::new(tx - 8.0, ty - 4.0, tx + w as f64 + 8.0, ty + h as f64 + 4.0);
            let pill = RoundedRect::from_rect(pill_rect, tokens.radii.md as f64);
            if active {
                scene.fill(Fill::NonZero, transform, colors.surfaces.overlay, None, &pill);
            }
            let color = if active { colors.text_primary } else { colors.text_dim };
            text_renderer.draw_text(
                scene,
                tab,
                lsize,
                color,
                transform * Affine::translate((tx, ty)),
                None,
            );
            tx += w as f64 + 32.0;
        }

        // Name input — edits fall through to `editing_workspace_name`.
        let body_y = rect.y0 + 168.0;
        text_renderer.draw_text(
            scene,
            "Name",
            tokens.typography.scale.sm,
            colors.text_dim,
            transform * Affine::translate((rect.x0 + 24.0, body_y)),
            None,
        );
        let input_rect = self.ws_settings_name_rect();
        let input = RoundedRect::from_rect(input_rect, tokens.radii.sm as f64);
        scene.fill(Fill::NonZero, transform, colors.neutral[1], None, &input);
        let editing = state.editing_workspace_name.is_some();
        let border_color = if editing {
            colors.accent[5]
        } else {
            colors.border_standard
        };
        scene.stroke(
            &vello::kurbo::Stroke::new(if editing { 2.0 } else { 1.0 }),
            transform,
            border_color,
            None,
            &input,
        );
        let shown = state
            .editing_workspace_name
            .as_deref()
            .unwrap_or(active_name.as_str());
        let input_size = tokens.typography.scale.base;
        text_renderer.draw_text(
            scene,
            shown,
            input_size,
            colors.text_primary,
            transform * Affine::translate((input_rect.x0 + 12.0, input_rect.y0 + 6.0)),
            None,
        );
        if editing {
            let (tw, _th) = text_renderer.measure(shown, input_size, None);
            let caret_x = input_rect.x0 + 12.0 + tw as f64 + 1.0;
            let caret = Rect::new(
                caret_x,
                input_rect.y0 + 6.0,
                caret_x + 2.0,
                input_rect.y0 + 6.0 + input_size as f64,
            );
            scene.fill(Fill::NonZero, transform, colors.accent[5], None, &caret);
        }

        // Close hint
        let hint = if editing {
            "Enter to save · Esc to cancel"
        } else {
            "Press Esc to close"
        };
        let hsize = tokens.typography.scale.xs;
        let (hw, _hh) = text_renderer.measure(hint, hsize, None);
        text_renderer.draw_text(
            scene,
            hint,
            hsize,
            colors.text_dim,
            transform * Affine::translate((rect.x1 - hw as f64 - 24.0, rect.y1 - 24.0)),
            None,
        );
    }

    /// Paint a single deterministic simulated remote cursor — a "caret +
    /// name chip" pair — over the desktop content. Lissajous-ish motion
    /// from a seconds-since-started clock; path stays inside the host's
    /// rect with a 80 px margin so the peer remains visible regardless
    /// of window size.
    fn draw_demo_peer(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let t = self.started.elapsed().as_secs_f64();

        // Keep the peer inside the visible viewport minus an 80 px margin
        // so its label chip never clips against the edge.
        let margin = 80.0_f64;
        let avail_w = (self.rect.width() - margin * 2.0).max(0.0);
        let avail_h = (self.rect.height() - margin * 2.0).max(0.0);
        let cx = self.rect.x0 + margin + avail_w / 2.0;
        let cy = self.rect.y0 + margin + avail_h / 2.0;
        let px = cx + (avail_w / 2.0) * (t * 0.4).sin();
        let py = cy + (avail_h / 2.0) * (t * 0.31).cos();
        let apex = Point::new(px, py);

        // Indigo "Ada" peer — matches the concept's pinned-color convention.
        let peer = tokens.colors.accent[5];
        let caret_size = 10.0_f64;
        let chip_offset = 2.0_f64;
        let pad_x = 6.0_f64;
        let pad_y = 2.0_f64;
        let label_size = tokens.typography.scale.sm;

        let mut caret = vello::kurbo::BezPath::new();
        caret.move_to(apex);
        caret.line_to((apex.x + caret_size, apex.y + caret_size * 0.4));
        caret.line_to((apex.x + caret_size * 0.4, apex.y + caret_size));
        caret.close_path();
        scene.fill(Fill::NonZero, transform, peer, None, &caret);

        let label = "Ada";
        let (tw, _th) = text_renderer.measure(label, label_size, None);
        let chip_w = tw as f64 + pad_x * 2.0;
        let chip_h = label_size as f64 + pad_y * 2.0;
        let chip_x = apex.x + caret_size + chip_offset;
        let chip_y = apex.y + caret_size + chip_offset;
        let chip_rect = Rect::new(chip_x, chip_y, chip_x + chip_w, chip_y + chip_h);
        let chip = RoundedRect::from_rect(chip_rect, tokens.radii.sm as f64);
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &chip,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            tokens.colors.border_standard,
            None,
            &chip,
        );
        let underline = Rect::new(chip_rect.x0, chip_rect.y1 - 2.0, chip_rect.x1, chip_rect.y1);
        scene.fill(Fill::NonZero, transform, peer, None, &underline);
        text_renderer.draw_text(
            scene,
            label,
            label_size,
            tokens.colors.text_primary,
            transform * Affine::translate((chip_rect.x0 + pad_x, chip_rect.y0 + pad_y)),
            None,
        );
    }

    /// Scope banner drawn above the palette while ⌘K is open. Shows a
    /// "Search in:" label and two toggleable pills ("Workspace" / "All").
    /// The active pill uses the indigo accent fill so it reads as
    /// "selected"; the inactive pill sits on the raised surface.
    fn draw_search_banner(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        use crate::shell::chrome_state::SearchScope;
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let banner = self.search_banner_rect();
        if banner.width() <= 0.0 {
            return;
        }

        let card = RoundedRect::from_rect(banner, tokens.radii.sm as f64);
        scene.fill(Fill::NonZero, transform, colors.surfaces.overlay, None, &card);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &card,
        );

        // "Search in:" label
        let label_size = tokens.typography.scale.sm;
        let (_lw, lh) = text_renderer.measure("Search in:", label_size, None);
        let label_y = (banner.y0 + banner.y1) / 2.0 - lh as f64 / 2.0;
        text_renderer.draw_text(
            scene,
            "Search in:",
            label_size,
            colors.text_dim,
            transform * Affine::translate((banner.x0 + 16.0, label_y)),
            None,
        );

        // Active scope from chrome state (defaults to Workspace if the
        // overlay host was built without chrome — we still want the banner
        // to render sensibly for tests).
        let active = self
            .chrome
            .as_ref()
            .map(|c| c.borrow().search_scope)
            .unwrap_or_default();

        for (idx, scope) in [SearchScope::Workspace, SearchScope::All].iter().enumerate() {
            let pill = self.search_scope_pill_rect(idx);
            let is_active = *scope == active;
            let radius = pill.height() / 2.0;
            let pill_rr = RoundedRect::new(pill.x0, pill.y0, pill.x1, pill.y1, radius);
            let fill = if is_active { colors.accent[5] } else { colors.surfaces.raised };
            scene.fill(Fill::NonZero, transform, fill, None, &pill_rr);
            if !is_active {
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    colors.border_standard,
                    None,
                    &pill_rr,
                );
            }

            let text = scope.label();
            let (tw, th) = text_renderer.measure(text, tokens.typography.scale.sm, None);
            let tx = (pill.x0 + pill.x1) / 2.0 - tw as f64 / 2.0;
            let ty = (pill.y0 + pill.y1) / 2.0 - th as f64 / 2.0;
            text_renderer.draw_text(
                scene,
                text,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform * Affine::translate((tx, ty)),
                None,
            );
        }
    }

    fn draw_quick_settings(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let qs = self
            .chrome
            .as_ref()
            .map(|c| c.borrow().quick_settings)
            .unwrap_or_default();

        let rect = self.quick_settings_rect();
        let card = RoundedRect::from_rect(rect, tokens.radii.lg as f64);
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &card);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &card,
        );

        // Header
        text_renderer.draw_text(
            scene,
            "Quick Settings",
            tokens.typography.scale.lg,
            colors.text_primary,
            transform * Affine::translate((rect.x0 + 16.0, rect.y0 + 16.0)),
            None,
        );

        // Slider-ish rows: Brightness / Volume — values read from chrome.
        let slider_values = [qs.brightness, qs.volume];
        for (i, label) in ["Brightness", "Volume"].iter().enumerate() {
            let (bar, _ty) = self.quick_settings_slider_geometry(i);
            let ty = bar.y0 - 20.0;
            text_renderer.draw_text(
                scene,
                label,
                tokens.typography.scale.sm,
                colors.text_dim,
                transform * Affine::translate((rect.x0 + 16.0, ty)),
                None,
            );
            let bar_rr = RoundedRect::from_rect(bar, 3.0);
            scene.fill(Fill::NonZero, transform, colors.neutral[2], None, &bar_rr);
            let fill_w = bar.width() * slider_values[i].clamp(0.0, 1.0) as f64;
            let filled = Rect::new(bar.x0, bar.y0, bar.x0 + fill_w, bar.y1);
            let filled_rr = RoundedRect::from_rect(filled, 3.0);
            scene.fill(Fill::NonZero, transform, colors.accent[5], None, &filled_rr);
        }

        // Toggle rows: Wi-Fi / Bluetooth — values read from chrome.
        let toggle_values = [qs.wifi, qs.bluetooth];
        for (i, label) in ["Wi-Fi", "Bluetooth"].iter().enumerate() {
            let prect = self.quick_settings_toggle_geometry(i);
            let ty = prect.y0;
            text_renderer.draw_text(
                scene,
                label,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform * Affine::translate((rect.x0 + 16.0, ty)),
                None,
            );
            let pill = RoundedRect::from_rect(prect, prect.height() / 2.0);
            let on = toggle_values[i];
            let color = if on { colors.accent[5] } else { colors.neutral[3] };
            scene.fill(Fill::NonZero, transform, color, None, &pill);
            let knob_x = if on {
                prect.x1 - prect.height() / 2.0
            } else {
                prect.x0 + prect.height() / 2.0
            };
            let knob = vello::kurbo::Circle::new(
                Point::new(knob_x, (prect.y0 + prect.y1) / 2.0),
                prect.height() / 2.0 - 2.0,
            );
            scene.fill(Fill::NonZero, transform, colors.text_primary, None, &knob);
        }
    }

    /// Collaborators dropdown: a peer-roster popover anchored under the
    /// top-right avatar stack. Each row shows an indigo-filled initial
    /// circle + the peer's name; empty rosters show a "No peers" hint so
    /// the dropdown still reads as "here are the people" rather than
    /// "something broken".
    fn draw_collaborators(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let rect = self.collaborators_rect();
        let card = RoundedRect::from_rect(rect, tokens.radii.lg as f64);
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &card);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &card,
        );
        text_renderer.draw_text(
            scene,
            "Collaborators",
            tokens.typography.scale.lg,
            colors.text_primary,
            transform * Affine::translate((rect.x0 + 16.0, rect.y0 + 16.0)),
            None,
        );

        let peers: Vec<(String, char)> = self
            .chrome
            .as_ref()
            .map(|c| {
                c.borrow()
                    .tree
                    .collaborators
                    .iter()
                    .map(|p| {
                        let initial = p
                            .name
                            .chars()
                            .next()
                            .map(|c| c.to_ascii_uppercase())
                            .unwrap_or('•');
                        (p.name.clone(), initial)
                    })
                    .collect()
            })
            .unwrap_or_default();

        if peers.is_empty() {
            text_renderer.draw_text(
                scene,
                "No peers connected",
                tokens.typography.scale.sm,
                colors.text_dim,
                transform * Affine::translate((rect.x0 + 16.0, rect.y0 + 56.0)),
                None,
            );
            return;
        }

        for (i, (name, initial)) in peers.iter().enumerate() {
            let row_y = rect.y0 + 48.0 + i as f64 * 36.0;
            let center = Point::new(rect.x0 + 16.0 + 12.0, row_y + 14.0);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent[5],
                None,
                &vello::kurbo::Circle::new(center, 11.0),
            );
            let initial_str = initial.to_string();
            let (iw, ih) = text_renderer.measure(&initial_str, tokens.typography.scale.sm, None);
            text_renderer.draw_text(
                scene,
                &initial_str,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform * Affine::translate((center.x - iw as f64 / 2.0, center.y - ih as f64 / 2.0)),
                None,
            );
            text_renderer.draw_text(
                scene,
                name,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform * Affine::translate((rect.x0 + 44.0, row_y + 7.0)),
                None,
            );
        }
    }

    /// Slider bar rect for the i-th quick-settings slider (0 = Brightness,
    /// 1 = Volume). Returned together with the label baseline `y` so the
    /// draw path and the hit-test share one source of truth.
    pub(crate) fn quick_settings_slider_geometry(&self, i: usize) -> (Rect, f64) {
        let r = self.quick_settings_rect();
        let ty = r.y0 + 56.0 + i as f64 * 52.0;
        let bar = Rect::new(r.x0 + 16.0, ty + 20.0, r.x1 - 16.0, ty + 26.0);
        (bar, ty)
    }

    /// Toggle pill rect for the i-th quick-settings toggle (0 = Wi-Fi,
    /// 1 = Bluetooth).
    pub(crate) fn quick_settings_toggle_geometry(&self, i: usize) -> Rect {
        let r = self.quick_settings_rect();
        let ty = r.y0 + 160.0 + i as f64 * 28.0;
        let pill_w = 36.0;
        let pill_h = 18.0;
        Rect::new(r.x1 - 16.0 - pill_w, ty, r.x1 - 16.0, ty + pill_h)
    }
}

#[cfg(test)]
mod overview_transition_tests {
    use super::*;
    use quick_ui::widget::Container;

    fn host() -> ShellOverlayHost {
        // Dummy content widget — `Container::new()` produces an empty
        // layout node which is enough for the transition state tests.
        ShellOverlayHost::new(Box::new(Container::new()))
    }

    #[test]
    fn idle_holds_zero_until_target_changes() {
        let mut h = host();
        for _ in 0..5 {
            let t = h.tick_overview_transition(0.0);
            assert_eq!(t, 0.0);
        }
    }

    #[test]
    fn target_change_latches_transition_and_completes() {
        let mut h = host();
        h.tick_overview_transition(1.0);
        assert!(h.transition_started.is_some(), "should latch transition");
        assert_eq!(h.overview_target, 1.0);
        // Sleep past the full duration and tick once more; value should
        // settle exactly at the target with the transition cleared.
        std::thread::sleep(std::time::Duration::from_millis(
            OVERVIEW_TRANSITION_MS + 30,
        ));
        let t = h.tick_overview_transition(1.0);
        assert!((t - 1.0).abs() < f32::EPSILON);
        assert!(h.transition_started.is_none(), "transition should clear");
    }

    #[test]
    fn mid_flight_re_toggle_picks_up_from_current_t() {
        // Start a 0→1 fade, tick a bit, then flip target to 0. The
        // `transition_from` should be captured from the current `overview_t`
        // (not 0), so the reverse fade starts from wherever we were rather
        // than popping.
        let mut h = host();
        h.tick_overview_transition(1.0);
        std::thread::sleep(std::time::Duration::from_millis(
            OVERVIEW_TRANSITION_MS / 2,
        ));
        let t_mid = h.tick_overview_transition(1.0);
        assert!(
            t_mid > 0.0 && t_mid < 1.0,
            "mid-flight t should be between 0 and 1, got {}",
            t_mid
        );
        h.tick_overview_transition(0.0);
        assert!(
            (h.transition_from - t_mid).abs() < 0.01,
            "reverse should start from current alpha, got from={} t_mid={}",
            h.transition_from,
            t_mid
        );
        assert_eq!(h.overview_target, 0.0);
    }
}
