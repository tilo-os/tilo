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

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, LengthPercentageAuto, NodeId, Position, Rect as TaffyRect, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::kurbo::RoundedRect;
use vello::peniko::{Color, Fill};
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
}

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
        }
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
        self.content
            .draw(scene, transform, text_renderer, theme, presence);
        let flags = self.overlay_flags();

        // Overview grid (Cmd+Tab)
        if flags.overview {
            draw_scrim(scene, transform, self.rect);
            self.draw_overview(scene, transform, text_renderer, theme);
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

        if self.palette_visible {
            draw_scrim(scene, transform, self.rect);
            self.palette
                .draw(scene, transform, text_renderer, theme, presence);
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
                // settings > quick settings.
                if self.palette_visible {
                    self.palette_visible = false;
                    *handled = true;
                    return;
                }
                if let Some(chrome) = self.chrome.clone() {
                    let mut st = chrome.borrow_mut();
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
                    if st.overlays.quick_settings {
                        st.overlays.quick_settings = false;
                        st.bump();
                        *handled = true;
                        return;
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
                    let palette_rect = self.palette.get_rect();
                    if !palette_rect.contains(p) {
                        self.palette_visible = false;
                        *handled = true;
                        return;
                    }
                }
                let flags = self.overlay_flags();
                if flags.quick_settings && !self.quick_settings_rect().contains(p) {
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        st.overlays.quick_settings = false;
                        st.bump();
                    }
                    *handled = true;
                    return;
                }
                if flags.ws_settings && !self.ws_settings_rect().contains(p) {
                    if let Some(chrome) = self.chrome.clone() {
                        let mut st = chrome.borrow_mut();
                        st.overlays.ws_settings = false;
                        st.bump();
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

        // General-tab body placeholder
        let body_y = rect.y0 + 168.0;
        text_renderer.draw_text(
            scene,
            "Name",
            tokens.typography.scale.sm,
            colors.text_dim,
            transform * Affine::translate((rect.x0 + 24.0, body_y)),
            None,
        );
        let input_rect = Rect::new(rect.x0 + 24.0, body_y + 20.0, rect.x1 - 24.0, body_y + 48.0);
        let input = RoundedRect::from_rect(input_rect, tokens.radii.sm as f64);
        scene.fill(Fill::NonZero, transform, colors.neutral[1], None, &input);
        scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, colors.border_standard, None, &input);
        text_renderer.draw_text(
            scene,
            &active_name,
            tokens.typography.scale.base,
            colors.text_primary,
            transform * Affine::translate((input_rect.x0 + 12.0, input_rect.y0 + 6.0)),
            None,
        );

        // Close hint
        let hint = "Press Esc to close";
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

    fn draw_quick_settings(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        let popover_w = 280.0_f64;
        let popover_h = 240.0_f64;
        // Anchor under the top-right tray pill.
        let x1 = self.rect.x1 - 16.0;
        let y0 = self.rect.y0 + 48.0; // below the 40 px top bar + 8 px gap
        let rect = Rect::new(x1 - popover_w, y0, x1, y0 + popover_h);
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

        // Slider-ish rows: Brightness / Volume
        for (i, label) in ["Brightness", "Volume"].iter().enumerate() {
            let ty = rect.y0 + 56.0 + i as f64 * 52.0;
            text_renderer.draw_text(
                scene,
                label,
                tokens.typography.scale.sm,
                colors.text_dim,
                transform * Affine::translate((rect.x0 + 16.0, ty)),
                None,
            );
            let bar = Rect::new(rect.x0 + 16.0, ty + 20.0, rect.x1 - 16.0, ty + 26.0);
            let bar_rr = RoundedRect::from_rect(bar, 3.0);
            scene.fill(Fill::NonZero, transform, colors.neutral[2], None, &bar_rr);
            // Filled portion
            let fill_w = bar.width() * if i == 0 { 0.6 } else { 0.4 };
            let filled = Rect::new(bar.x0, bar.y0, bar.x0 + fill_w, bar.y1);
            let filled_rr = RoundedRect::from_rect(filled, 3.0);
            scene.fill(Fill::NonZero, transform, colors.accent[5], None, &filled_rr);
        }

        // Toggle rows: Wi-Fi / Bluetooth
        for (i, label) in ["Wi-Fi", "Bluetooth"].iter().enumerate() {
            let ty = rect.y0 + 160.0 + i as f64 * 28.0;
            text_renderer.draw_text(
                scene,
                label,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform * Affine::translate((rect.x0 + 16.0, ty)),
                None,
            );
            // Toggle pill on the right
            let pill_w = 36.0;
            let pill_h = 18.0;
            let prect = Rect::new(rect.x1 - 16.0 - pill_w, ty, rect.x1 - 16.0, ty + pill_h);
            let pill = RoundedRect::from_rect(prect, pill_h / 2.0);
            let on = i == 0;
            let color = if on { colors.accent[5] } else { colors.neutral[3] };
            scene.fill(Fill::NonZero, transform, color, None, &pill);
            let knob_x = if on { prect.x1 - pill_h / 2.0 } else { prect.x0 + pill_h / 2.0 };
            let knob = vello::kurbo::Circle::new(Point::new(knob_x, (prect.y0 + prect.y1) / 2.0), pill_h / 2.0 - 2.0);
            scene.fill(Fill::NonZero, transform, colors.text_primary, None, &knob);
        }
    }
}
