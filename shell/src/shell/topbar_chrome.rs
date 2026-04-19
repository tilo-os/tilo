//! Rendering widget for the shell top bar.
//!
//! `TopBar` in `shell/topbar.rs` owns the logical state (workspace nav,
//! center-widget cooldown, tooltip timing, collaborator helpers). This file
//! provides a [`TopBarChrome`] that implements [`quick_ui::widget::Widget`]
//! and paints the bar onto the root scene. Phase 1 wires it into
//! `build_nirios_root` so there is finally a visible top-bar surface; later
//! phases will route clicks on its left/center/right clusters back into the
//! logical `TopBar` for workspace switch / overview toggle / quick-settings.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use chrono::Local;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use quick_ui::presence::PresenceCtx;
use quick_ui::text_render::TextRenderer;
use quick_ui::theme::Theme;
use quick_ui::widget::Widget;

use crate::shell::state::WorkspaceTree;

pub const TOPBAR_HEIGHT_PX: f32 = 40.0;

/// Snapshot of the data the top bar renders. Cached at construction and
/// refreshed when the shell calls [`TopBarChrome::sync_from_tree`] (e.g. in
/// response to a workspace switch). Keeping the render widget off
/// `WorkspaceTree` lets `build_nirios_root` stay a pure function of the
/// tree and avoids a circular borrow against the shell's owned logical
/// `TopBar`.
pub struct TopBarChrome {
    rect: Rect,
    workspace_name: String,
    workspace_index: usize,
    workspace_total: usize,
    collaborator_initials: Vec<char>,
}

impl TopBarChrome {
    pub fn from_tree(tree: &WorkspaceTree) -> Self {
        let mut this = Self {
            rect: Rect::ZERO,
            workspace_name: String::new(),
            workspace_index: 0,
            workspace_total: 1,
            collaborator_initials: Vec::new(),
        };
        this.sync_from_tree(tree);
        this
    }

    pub fn sync_from_tree(&mut self, tree: &WorkspaceTree) {
        self.workspace_total = tree.workspaces.len().max(1);
        let (idx, name) = tree
            .workspaces
            .iter()
            .enumerate()
            .find(|(_, w)| w.id == tree.active)
            .map(|(i, w)| (i, w.name.clone()))
            .unwrap_or((0, String::from("Workspace")));
        self.workspace_index = idx;
        self.workspace_name = name;
        self.collaborator_initials = tree
            .collaborators
            .iter()
            .take(3)
            .filter_map(|c| c.name.chars().next().map(|ch| ch.to_ascii_uppercase()))
            .collect();
    }

    fn current_clock_string() -> String {
        Local::now().format("%H:%M").to_string()
    }
}

impl Widget for TopBarChrome {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        // Clock updates once per second; we do not currently trigger a
        // re-paint on that cadence, but returning `true` here would flood
        // the frame loop. The shell's existing per-frame redraw in
        // `App::run` picks up the new HH:MM within ~16 ms anyway.
        false
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(TOPBAR_HEIGHT_PX),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        scene.fill(Fill::NonZero, transform, colors.surfaces.overlay, None, &self.rect);

        let border_rect = Rect::new(
            self.rect.x0,
            self.rect.y1 - 1.0,
            self.rect.x1,
            self.rect.y1,
        );
        scene.fill(Fill::NonZero, transform, colors.border_standard, None, &border_rect);

        let pad_x = tokens.spacing.s3 as f64;
        let center_y = (self.rect.y0 + self.rect.y1) / 2.0;
        let icon_size = 14.0_f64;

        // --- Left cluster: overview icon + chevrons + workspace name ---
        let mut x = self.rect.x0 + pad_x;

        // Overview "grid" icon: 2x2 mini-squares.
        let cell = (icon_size - 4.0) / 2.0;
        for (cx, cy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
            let r = Rect::new(
                x + cx * (cell + 2.0),
                center_y - icon_size / 2.0 + cy * (cell + 2.0),
                x + cx * (cell + 2.0) + cell,
                center_y - icon_size / 2.0 + cy * (cell + 2.0) + cell,
            );
            scene.fill(Fill::NonZero, transform, colors.text_dim, None, &r);
        }
        x += icon_size + tokens.spacing.s3 as f64;

        // Vertical divider
        let divider = Rect::new(x, center_y - 8.0, x + 1.0, center_y + 8.0);
        scene.fill(Fill::NonZero, transform, colors.border_standard, None, &divider);
        x += 1.0 + tokens.spacing.s2 as f64;

        // Up-chevron ▲
        let chev = 10.0_f64;
        let up = vello::kurbo::BezPath::from_iter([
            vello::kurbo::PathEl::MoveTo(Point::new(x, center_y + chev * 0.35)),
            vello::kurbo::PathEl::LineTo(Point::new(x + chev / 2.0, center_y - chev * 0.35)),
            vello::kurbo::PathEl::LineTo(Point::new(x + chev, center_y + chev * 0.35)),
        ]);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            transform,
            if self.workspace_index == 0 { colors.border_standard } else { colors.text_dim },
            None,
            &up,
        );
        x += chev + tokens.spacing.s2 as f64;

        // Down-chevron ▼
        let dn = vello::kurbo::BezPath::from_iter([
            vello::kurbo::PathEl::MoveTo(Point::new(x, center_y - chev * 0.35)),
            vello::kurbo::PathEl::LineTo(Point::new(x + chev / 2.0, center_y + chev * 0.35)),
            vello::kurbo::PathEl::LineTo(Point::new(x + chev, center_y - chev * 0.35)),
        ]);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            transform,
            if self.workspace_index + 1 >= self.workspace_total {
                colors.border_standard
            } else {
                colors.text_dim
            },
            None,
            &dn,
        );
        x += chev + tokens.spacing.s3 as f64;

        // Workspace-name button: text + gear glyph
        let ws_text_size = tokens.typography.scale.sm;
        let ws_label = format!(
            "{}  {}/{}",
            self.workspace_name, self.workspace_index + 1, self.workspace_total
        );
        let (ws_w, ws_h) = text_renderer.measure(&ws_label, ws_text_size, None);
        let ws_y = center_y - ws_h as f64 / 2.0;
        // Subtle pill backdrop so the workspace name reads as clickable.
        let pill = RoundedRect::from_rect(
            Rect::new(
                x - 6.0,
                center_y - 12.0,
                x + ws_w as f64 + 18.0,
                center_y + 12.0,
            ),
            tokens.radii.sm as f64,
        );
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &pill);
        text_renderer.draw_text(
            scene,
            &ws_label,
            ws_text_size,
            colors.text_primary,
            transform * Affine::translate((x, ws_y)),
            None,
        );
        let gear_x = x + ws_w as f64 + 6.0;
        // Gear placeholder: small circle with a dot inside.
        let gear_ring = vello::kurbo::Circle::new(Point::new(gear_x + 4.0, center_y), 4.5);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.2),
            transform,
            colors.text_dim,
            None,
            &gear_ring,
        );
        let gear_hole = vello::kurbo::Circle::new(Point::new(gear_x + 4.0, center_y), 1.3);
        scene.fill(Fill::NonZero, transform, colors.text_dim, None, &gear_hole);

        // --- Center cluster: clock HH:MM ---
        let clock = Self::current_clock_string();
        let clock_size = tokens.typography.scale.base;
        let (cw, ch) = text_renderer.measure(&clock, clock_size, None);
        let cx = (self.rect.x0 + self.rect.x1) / 2.0 - cw as f64 / 2.0;
        let cy = center_y - ch as f64 / 2.0;
        text_renderer.draw_text(
            scene,
            &clock,
            clock_size,
            colors.text_primary,
            transform * Affine::translate((cx, cy)),
            None,
        );

        // --- Right cluster: avatar stack + tray pill ---
        let mut rx = self.rect.x1 - pad_x;

        // Tray pill (rounded-full) with three dots for wifi / monitor / battery.
        let tray_w = 52.0_f64;
        let tray_h = 22.0_f64;
        let tray = RoundedRect::new(
            rx - tray_w,
            center_y - tray_h / 2.0,
            rx,
            center_y + tray_h / 2.0,
            tray_h / 2.0,
        );
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &tray);
        for i in 0..3 {
            let dx = rx - tray_w + 12.0 + i as f64 * 12.0;
            let c = vello::kurbo::Circle::new(Point::new(dx, center_y), 2.0);
            scene.fill(Fill::NonZero, transform, colors.text_dim, None, &c);
        }
        rx -= tray_w + tokens.spacing.s3 as f64;

        // Avatar stack: up to 3 circles, overlapping by 4 px, newest on top.
        for (i, initial) in self.collaborator_initials.iter().enumerate() {
            let cx = rx - i as f64 * (14.0 - 4.0);
            let center = Point::new(cx - 7.0, center_y);
            // Background plate (slight ring so overlapping circles stay readable).
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surfaces.overlay,
                None,
                &vello::kurbo::Circle::new(center, 9.0),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent[5],
                None,
                &vello::kurbo::Circle::new(center, 7.5),
            );
            let s = initial.to_string();
            let (tw, th) = text_renderer.measure(&s, tokens.typography.scale.sm, None);
            text_renderer.draw_text(
                scene,
                &s,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform * Affine::translate((center.x - tw as f64 / 2.0, center.y - th as f64 / 2.0)),
                None,
            );
        }
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Click wiring lands in a follow-up phase alongside the overlay host.
    }

    fn name(&self) -> &str {
        "TopBarChrome"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
