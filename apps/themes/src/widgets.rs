//! Small reusable custom widgets for the Themes app: a command-bus
//! button and a gallery preset card. Both paint with the OS theme (they
//! are app chrome, not previewed content) and emit user intent by writing
//! a one-shot command string into the CRDT, which [`crate::app::ThemesApp`]
//! drains and dispatches each frame.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::color_compat::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::draw;
use crate::model::{self, Rgb, ThemeSpec};

#[derive(Clone, Copy, PartialEq)]
pub enum BtnVariant {
    Primary,
    Ghost,
    Danger,
}

/// A labelled button that writes `command` into the CRDT command bus on
/// click. Sizing is either content-approximate or full-width.
pub struct Btn {
    command: String,
    label: String,
    variant: BtnVariant,
    full_width: bool,
    rect: Rect,
    hover: bool,
    pressed: bool,
    enabled: bool,
}

impl Btn {
    pub fn new(command: impl Into<String>, label: impl Into<String>, variant: BtnVariant) -> Self {
        Self {
            command: command.into(),
            label: label.into(),
            variant,
            full_width: false,
            rect: Rect::ZERO,
            hover: false,
            pressed: false,
            enabled: true,
        }
    }

    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn approx_width(&self) -> f32 {
        (self.label.chars().count() as f32 * 8.0 + 30.0).max(64.0)
    }
}

impl Widget for Btn {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let width = if self.full_width {
            Dimension::percent(1.0)
        } else {
            Dimension::length(self.approx_width())
        };
        let node = taffy
            .new_leaf(Style {
                size: Size { width, height: Dimension::length(34.0) },
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

    fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, _p: &PresenceCtx) {
        let radius = theme.radius_standard;
        let (mut bg, mut fg, border) = match self.variant {
            BtnVariant::Primary => (theme.accent_primary, on_accent(theme), Color::TRANSPARENT),
            BtnVariant::Ghost => (theme.bg_primary, theme.text_primary, theme.border_standard),
            BtnVariant::Danger => (theme.tokens.colors.semantic.danger, Color::WHITE, Color::TRANSPARENT),
        };
        if !self.enabled {
            bg = bg.multiply_alpha(0.4);
            fg = fg.multiply_alpha(0.5);
        } else if self.pressed {
            bg = bg.multiply_alpha(0.82);
        } else if self.hover {
            match self.variant {
                BtnVariant::Ghost => bg = theme.bg_hover,
                _ => bg = lighten(bg, 0.08),
            }
        }
        draw::fill_rrect(scene, t, self.rect, radius, bg);
        if border != Color::TRANSPARENT {
            draw::stroke_rrect(scene, t, self.rect, radius, border, 1.0);
        }
        draw::text_center(
            tr, scene, t, &self.label, 13.0, fg,
            self.rect.x0, self.rect.x1, self.rect.y0, self.rect.y1, TextWeight::Semibold,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_event(&mut self, event: &WindowEvent, scale: f64, _m: &winit::event::Modifiers, state: &mut AutoCommit, path: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, _taffy: &TaffyTree<()>) {
        if !self.enabled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale, position.y / scale);
                let h = self.rect.contains(p);
                if h != self.hover {
                    self.hover = h;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: es, button: MouseButton::Left, .. } => {
                if *es == ElementState::Pressed && self.hover {
                    self.pressed = true;
                    *focus_target = Some(node_id);
                    *handled = true;
                } else if *es == ElementState::Released {
                    if self.pressed && self.hover {
                        crate::doc::put_str(state, path, crate::doc::KEY_COMMAND, &self.command);
                        *handled = true;
                    }
                    self.pressed = false;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "Btn" }
    fn get_rect(&self) -> Rect { self.rect }
    fn is_focusable(&self) -> bool { self.enabled }
}

/// Height of one sidebar theme row (logical px). Mirrors weather's
/// saved-place chip height so the two apps' sidebars read the same.
pub const ROW_H: f32 = 58.0;

/// A sidebar row for one theme — a small palette swatch, the theme's name
/// and a `Preset · Dark` tag, styled like weather's saved-place chip.
/// Selectable (click emits `select:<id>`); the currently *applied* theme is
/// badged "Active". The name of the selected custom theme is drawn by the
/// gallery's inline [`ui::widget::TextInput`] instead of here (suppressed
/// via [`ThemeRow::update`]).
pub struct ThemeRow {
    id: String,
    name: String,
    builtin: bool,
    dark_base: bool,
    /// [base, raised, accent, success, warning, danger].
    swatch: [Rgb; 6],
    selected: bool,
    /// This row is the applied OS theme.
    active: bool,
    /// The gallery is drawing an inline name editor over this row, so the
    /// row must not paint its own static name.
    suppress_name: bool,
    rect: Rect,
    hover: bool,
}

impl ThemeRow {
    pub fn from_spec(spec: &ThemeSpec, selected: bool) -> Self {
        Self {
            id: spec.id.clone(),
            name: spec.name.clone(),
            builtin: spec.builtin,
            dark_base: spec.dark_base,
            swatch: Self::swatch_of(spec),
            selected,
            active: false,
            suppress_name: false,
            rect: Rect::ZERO,
            hover: false,
        }
    }

    fn swatch_of(spec: &ThemeSpec) -> [Rgb; 6] {
        [spec.surface_base, spec.surface_raised, spec.accent, spec.success, spec.warning, spec.danger]
    }

    pub fn id(&self) -> &str { &self.id }
    pub fn is_custom(&self) -> bool { !self.builtin }
    pub fn get_rect(&self) -> Rect { self.rect }

    /// Refresh visual fields in place (preserves hover / rect so the gallery
    /// can push fresh state every frame without flicker).
    pub fn update(&mut self, spec: &ThemeSpec, selected: bool, active: bool, suppress_name: bool) {
        self.name = spec.name.clone();
        self.builtin = spec.builtin;
        self.dark_base = spec.dark_base;
        self.swatch = Self::swatch_of(spec);
        self.selected = selected;
        self.active = active;
        self.suppress_name = suppress_name;
    }

    /// The name-text region of the row, in scene coords — where the gallery
    /// overlays its inline editor for the selected custom theme.
    pub fn name_rect(&self) -> Rect {
        let tx = self.rect.x0 + 62.0;
        Rect::new(tx, self.rect.y0 + 9.0, self.rect.x1 - 12.0, self.rect.y0 + 33.0)
    }
}

impl Widget for ThemeRow {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size { width: Dimension::percent(1.0), height: Dimension::length(ROW_H) },
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

    fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, _p: &PresenceCtx) {
        let radius = theme.radius_standard.min(12.0);
        // Row background: selected → accent-tinted; else hover / raised —
        // the same tiering weather's saved-place chip uses.
        let bg = if self.selected {
            lerp(theme.bg_app, theme.accent_primary, 0.18)
        } else if self.hover {
            theme.bg_hover
        } else {
            theme.bg_primary
        };
        draw::fill_rrect(scene, t, self.rect, radius, bg);
        let (border, bw) = if self.selected {
            (theme.accent_primary, 1.5)
        } else {
            (theme.border_standard.multiply_alpha(0.6), 1.0)
        };
        draw::stroke_rrect(scene, t, self.rect, radius, border, bw);

        // Palette swatch: a compact tile of the theme's base surface with a
        // raised inset and a row of accent + semantic dots.
        let sw = Rect::new(self.rect.x0 + 11.0, self.rect.y0 + 9.0, self.rect.x0 + 51.0, self.rect.y1 - 9.0);
        draw::fill_rrect(scene, t, sw, 7.0, model::color(self.swatch[0]));
        draw::stroke_rrect(scene, t, sw, 7.0, theme.border_standard.multiply_alpha(0.5), 1.0);
        let inset = Rect::new(sw.x0 + 5.0, sw.y0 + 5.0, sw.x1 - 5.0, sw.y0 + sw.height() * 0.42);
        draw::fill_rrect(scene, t, inset, 4.0, model::color(self.swatch[1]));
        let dot_y = sw.y1 - 8.5;
        for (i, c) in self.swatch[2..].iter().enumerate() {
            draw::circle(scene, t, Point::new(sw.x0 + 8.0 + i as f64 * 8.6, dot_y), 3.0, model::color(*c));
        }

        // Name (unless the gallery's inline editor covers it) + tag. The
        // name-over-tag pair is optically centred against the row so it sits
        // level with the swatch tile (shared two-line helper).
        let tx = sw.x1 + 11.0;
        let (name_y, tag_y) = draw::two_line_tops(self.rect.y0, ROW_H as f64, 14.0, 11.0, 22.0);
        if !self.suppress_name {
            draw::text_w(tr, scene, t, &self.name, 14.0, theme.text_primary, tx, name_y, TextWeight::Semibold);
        }
        let tag = if self.builtin { "Preset" } else { "Custom" };
        let base = if self.dark_base { "Dark" } else { "Light" };
        draw::text(tr, scene, t, &format!("{tag} · {base}"), 11.0, theme.text_dim, tx, tag_y);

        // Applied-theme badge on the right edge.
        if self.active {
            let lab = "Active";
            let (w, _) = tr.measure(lab, 10.0, None);
            let pw = w as f64 + 16.0;
            let pill = Rect::new(self.rect.x1 - pw - 10.0, self.rect.center().y - 9.0, self.rect.x1 - 10.0, self.rect.center().y + 9.0);
            draw::fill_rrect(scene, t, pill, 9.0, theme.accent_primary);
            draw::text_center(tr, scene, t, lab, 10.0, on_accent(theme), pill.x0, pill.x1, pill.y0, pill.y1, TextWeight::Bold);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_event(&mut self, event: &WindowEvent, scale: f64, _m: &winit::event::Modifiers, state: &mut AutoCommit, path: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, _taffy: &TaffyTree<()>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale, position.y / scale);
                let h = self.rect.contains(p);
                if h != self.hover {
                    self.hover = h;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if self.hover {
                    crate::doc::put_str(state, path, crate::doc::KEY_COMMAND, &format!("select:{}", self.id));
                    *focus_target = Some(node_id);
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "ThemeRow" }
    fn get_rect(&self) -> Rect { self.rect }
    fn is_focusable(&self) -> bool { true }
    fn test_id(&self) -> Option<&str> { Some(&self.id) }
}

// --- shared colour helpers ---------------------------------------------

use vello::Scene;

/// Pick a legible on-colour for an accent fill.
pub fn on_accent(theme: &Theme) -> Color {
    let a = [theme.accent_primary.r(), theme.accent_primary.g(), theme.accent_primary.b()];
    model::color(model::on_color(a))
}

/// Lighten a colour toward white by `t` (0..1).
pub fn lighten(c: Color, t: f32) -> Color {
    let rgb = [c.r(), c.g(), c.b()];
    model::color(model::mix(rgb, [255, 255, 255], t))
}

/// Linearly mix colour `a` toward `b` by `t` (0..1).
pub fn lerp(a: Color, b: Color, t: f32) -> Color {
    model::color(model::mix([a.r(), a.g(), a.b()], [b.r(), b.g(), b.b()], t))
}
