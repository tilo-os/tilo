//! Small reusable custom widgets for the Themes app. They paint with the OS
//! theme (they are app chrome, not previewed content) and report local user
//! intent to [`crate::app::ThemesApp`] without touching the shared document.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::color_compat::ColorCompat;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::tokens::{Metrics, Spacing, TypeScale};
use ui::widget::Widget;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::draw;
use crate::model::{self, Rgb, ThemeSpec};

/// Panic-free leaf construction for [`Widget::build_layout`]. `new_leaf`
/// always inserts and returns `Ok`; `Widget::build_layout` returns a bare
/// `NodeId` (not a `Result`) across every implementor, so surfacing a
/// theoretical error here would mean threading a `Result` through the whole
/// toolkit for a branch that can't be taken.
#[allow(clippy::unwrap_used, reason = "new_leaf cannot fail; see doc comment")]
fn leaf_node(taffy: &mut TaffyTree<()>, style: Style) -> NodeId {
    taffy.new_leaf(style).unwrap()
}

/// Height of one sidebar theme row (logical px) at 1×. Mirrors weather's
/// saved-place chip height so the two apps' sidebars read the same. A row
/// whose two text lines measure taller than this seats — under a large-text
/// preference — grows to fit them (see [`ThemeRow::measure`]); at 1× every
/// row is exactly this.
pub const ROW_H: f32 = 58.0;

/// The palette swatch: a `bar_height` square, centred in the row whatever
/// the row's measured height.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const, and an f32 -> f64 widening is exact"
)]
const SWATCH: f64 = Metrics::BASE.bar_height as f64;

/// Inset from the row's edges to the swatch and to the trailing badge, and
/// the gap between the swatch and its text — the `s3` step.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const, and an f32 -> f64 widening is exact"
)]
const ROW_INSET: f64 = Spacing::BASE.s3 as f64;

/// Gap between the name line and the tag line beneath it — the caption step.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const, and an f32 -> f64 widening is exact"
)]
const LINE_GAP: f64 = Spacing::BASE.s1 as f64;

/// Least air above and below the two-line text block when it outgrows
/// [`ROW_H`] — the `s2` step.
const ROW_PAD: f32 = Spacing::BASE.s2;

/// Row width a bare `measure` (no parent width at all — neither known nor
/// available) shapes against: the docked gallery's 272 px column less its
/// `s4` inset on each side. Every hosted measure gets a real width instead.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const, and an f32 -> f64 widening is exact"
)]
const BARE_MEASURE_ROW_W: f64 = 272.0 - 2.0 * Spacing::BASE.s4 as f64;

/// A sidebar row for one theme — a small palette swatch, the theme's name
/// and a `Preset · Dark` tag, styled like weather's saved-place chip.
/// Selectable (click emits `select:<id>`); the currently *applied* theme is
/// badged "Active". The name of the selected custom theme is drawn by the
/// gallery's inline [`ui::widget::TextInput`] instead of here (suppressed
/// via [`ThemeRow::update`]).
pub struct ThemeRow {
    id: String,
    name: String,
    /// Static properties mirrored from the [`ThemeSpec`] this row
    /// represents, used only to paint the "Preset · Dark" tag.
    kind: ThemeKind,
    /// [base, raised, accent, success, warning, danger].
    swatch: [Rgb; 6],
    /// Transient view-state, read together when painting the row
    /// background and the "Active" badge.
    view: RowView,
    /// The gallery is drawing an inline name editor over this row, so the
    /// row must not paint its own static name.
    suppress_name: bool,
    rect: Rect,
    select_requested: bool,
    /// Theme type sizes for the name (`scale.base`), the tag (`scale.sm`)
    /// and the badge (`scale.xs`), captured in `build_layout` so `measure`,
    /// `name_rect` and `draw` shape the same text. Already text-scaled; handed
    /// to the renderer through [`TextRenderer::renderer_input_style`] so the
    /// scale is applied exactly once.
    name_size: f32,
    tag_size: f32,
    badge_size: f32,
    /// The name and tag line boxes as last measured, so `name_rect` (which
    /// has no renderer) places the inline editor on the drawn line.
    name_line: f64,
    tag_line: f64,
}

/// Static per-theme properties mirrored from the [`ThemeSpec`] a
/// [`ThemeRow`] represents — whether it's a built-in preset, and its base
/// contrast profile.
#[derive(Clone, Copy)]
struct ThemeKind {
    builtin: bool,
    dark_base: bool,
}

/// Transient per-row view-state for a [`ThemeRow`]: selection, the applied
/// OS theme, and pointer hover.
#[derive(Clone, Copy, Default)]
struct RowView {
    selected: bool,
    /// This row is the applied OS theme.
    active: bool,
    hover: bool,
}

impl ThemeRow {
    fn paint(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let radius = theme.radius_standard.min(12.0);
        // Row background: the raised card surface, hover a step up. The
        // selected row keeps the same surface — no accent wash (HIG §9.4:
        // accent means live, not selected) — and says "this is the theme
        // being edited" with one ring (HIG §7). It is a ring rather than the
        // shell's lift because these rows are already raised cards: a
        // `lift(1.0)` fill is the card's own colour and would not register.
        let bg = if self.view.hover {
            theme.bg_hover
        } else {
            theme.bg_primary
        };
        // The rest and hover edge derives from the row's own fill
        // (`ui::surface::card`, HIG §7/§9.5): a declared `border_standard`
        // at 60 % vanished against the raised row on the dark preset.
        ui::surface::card(scene, t, self.rect, radius, bg);
        if self.view.selected {
            draw::stroke_rrect(
                scene,
                t,
                self.rect.inset(-0.75),
                (radius - 0.75).max(0.0),
                theme.accent_primary,
                1.5,
            );
        }

        let sw = self.draw_swatch(scene, t);
        self.draw_name_and_tag(scene, t, tr, theme, sw);
        self.draw_active_badge(scene, t, tr, theme);
    }

    pub fn from_spec(spec: &ThemeSpec, selected: bool) -> Self {
        Self {
            id: spec.id.clone(),
            name: spec.name.clone(),
            kind: ThemeKind {
                builtin: spec.builtin,
                dark_base: spec.dark_base,
            },
            swatch: Self::swatch_of(spec),
            view: RowView {
                selected,
                ..RowView::default()
            },
            suppress_name: false,
            rect: Rect::ZERO,
            select_requested: false,
            name_size: TypeScale::default().base,
            tag_size: TypeScale::default().sm,
            badge_size: TypeScale::default().xs,
            // 1× line boxes for the frame before the first `measure`.
            name_line: f64::from(TypeScale::default().base) * 1.2,
            tag_line: f64::from(TypeScale::default().sm) * 1.2,
        }
    }

    /// Renderer-relative size for a theme-resolved type size.
    fn renderer_size(tr: &TextRenderer, size: f32) -> f32 {
        tr.renderer_input_style(&TextStyle::new(size)).size
    }

    /// The width the name and tag may take in a row `row_w` wide: from the
    /// text's left edge to the row's trailing inset, less the "Active" badge
    /// when this row carries it. `measure` and `draw` shape the text against
    /// this same width, so a long name that wraps under large text grows the
    /// row instead of running into its tag or across the badge.
    fn text_max_width(&self, row_w: f64, tr: &mut TextRenderer) -> f32 {
        let badge_w = if self.view.active {
            self.badge_width(tr) + ROW_INSET
        } else {
            0.0
        };
        let taken = ROW_INSET + SWATCH + ROW_INSET + badge_w + ROW_INSET;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a logical-pixel width comfortably representable by f32"
        )]
        let width = (row_w - taken).max(0.0) as f32;
        width
    }

    /// Measure the name and tag line boxes with the styles and width they
    /// are drawn in, and remember them for `name_rect`.
    fn measure_lines(&mut self, tr: &mut TextRenderer, row_w: f64) {
        let max_w = Some(self.text_max_width(row_w, tr));
        let name_size = Self::renderer_size(tr, self.name_size);
        let tag_size = Self::renderer_size(tr, self.tag_size);
        let (_, name_h) = tr.measure_weighted(&self.name, name_size, max_w, TextWeight::Semibold);
        let (_, tag_h) = tr.measure(&self.tag_label(), tag_size, max_w);
        self.name_line = f64::from(name_h);
        self.tag_line = f64::from(tag_h);
    }

    /// Height of the two-line text block: name, caption gap, tag.
    fn text_block_height(&self) -> f64 {
        self.name_line + LINE_GAP + self.tag_line
    }

    /// Top of the text block, centred in the row.
    fn text_block_top(&self) -> f64 {
        self.rect.y0 + (self.rect.height() - self.text_block_height()) / 2.0
    }

    /// Left edge of the name and tag: past the swatch and one inset.
    fn text_x(&self) -> f64 {
        self.rect.x0 + ROW_INSET + SWATCH + ROW_INSET
    }

    /// The row height the text needs: its block with [`ROW_PAD`] above and
    /// below, never below [`ROW_H`].
    fn measured_height(&self) -> f32 {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a logical-pixel height comfortably representable by f32"
        )]
        let block = self.text_block_height() as f32;
        (block + 2.0 * ROW_PAD).max(ROW_H)
    }

    fn tag_label(&self) -> String {
        let tag = if self.kind.builtin {
            "Preset"
        } else {
            "Custom"
        };
        let base = if self.kind.dark_base { "Dark" } else { "Light" };
        format!("{tag} · {base}")
    }

    pub fn take_select_request(&mut self) -> bool {
        std::mem::take(&mut self.select_requested)
    }

    fn swatch_of(spec: &ThemeSpec) -> [Rgb; 6] {
        [
            spec.surface_base,
            spec.surface_raised,
            spec.accent,
            spec.success,
            spec.warning,
            spec.danger,
        ]
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn is_custom(&self) -> bool {
        !self.kind.builtin
    }
    pub fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Refresh visual fields in place (preserves hover / rect so the gallery
    /// can push fresh state every frame without flicker).
    pub fn update(&mut self, spec: &ThemeSpec, selected: bool, active: bool, suppress_name: bool) {
        self.name.clone_from(&spec.name);
        self.kind.builtin = spec.builtin;
        self.kind.dark_base = spec.dark_base;
        self.swatch = Self::swatch_of(spec);
        self.view.selected = selected;
        self.view.active = active;
        self.suppress_name = suppress_name;
    }

    /// The name-text region of the row, in scene coords — where the gallery
    /// overlays its inline editor for the selected custom theme: the drawn
    /// name line, from the text's left edge to the row's trailing inset.
    pub fn name_rect(&self) -> Rect {
        let top = self.text_block_top();
        Rect::new(
            self.text_x(),
            top,
            self.rect.x1 - ROW_INSET,
            top + self.name_line,
        )
    }

    /// Palette swatch: surface tile with a raised inset and a row of
    /// accent + semantic dots (part of [`Widget::draw`]). A fixed square
    /// centred in the row, so a row that grows for its text keeps the
    /// swatch's proportions. Returns its rect so the name/tag text can
    /// anchor beside it.
    fn draw_swatch(&self, scene: &mut Scene, t: Affine) -> Rect {
        let cy = self.rect.center().y;
        let sw = Rect::new(
            self.rect.x0 + ROW_INSET,
            cy - SWATCH / 2.0,
            self.rect.x0 + ROW_INSET + SWATCH,
            cy + SWATCH / 2.0,
        );
        // The swatch is the previewed theme's base surface, so its edge is
        // measured against *that* colour, not the app theme's declared
        // border: a light preset's swatch on a dark row (and the reverse)
        // keeps a visible rim either way.
        ui::surface::card(scene, t, sw, 7.0, model::color(self.swatch[0]));
        let inset = Rect::new(
            sw.x0 + 5.0,
            sw.y0 + 5.0,
            sw.x1 - 5.0,
            sw.y0 + sw.height() * 0.42,
        );
        draw::fill_rrect(scene, t, inset, 4.0, model::color(self.swatch[1]));
        let dot_y = sw.y1 - 8.5;
        for (i, c) in self.swatch[2..].iter().enumerate() {
            draw::circle(
                scene,
                t,
                Point::new(sw.x0 + 8.0 + i as f64 * 8.6, dot_y),
                3.0,
                model::color(*c),
            );
        }
        sw
    }

    /// Name (unless the gallery's inline editor covers it) plus the
    /// preset/custom · dark/light tag beside the swatch `sw` (part of
    /// [`Widget::draw`]).
    fn draw_name_and_tag(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        sw: Rect,
    ) {
        let tx = sw.x1 + ROW_INSET;
        let top = self.text_block_top();
        // The name wraps before the trailing badge (or the row's inset) — the
        // same width the row was measured for — never painted across the
        // badge or the row's edge.
        let name_max_w = self.text_max_width(self.rect.width(), tr);
        if !self.suppress_name {
            tr.draw_text_weighted(
                scene,
                &self.name,
                Self::renderer_size(tr, self.name_size),
                theme.text_primary,
                t * Affine::translate((tx, top)),
                Some(name_max_w),
                TextWeight::Semibold,
            );
        }
        tr.draw_text(
            scene,
            &self.tag_label(),
            Self::renderer_size(tr, self.tag_size),
            theme.text_dim,
            t * Affine::translate((tx, top + self.name_line + LINE_GAP)),
            Some(name_max_w),
        );
    }

    /// Width of the "Active" pill: its label plus one `s2` of padding each
    /// side.
    fn badge_width(&self, tr: &mut TextRenderer) -> f64 {
        let (w, _) = tr.measure_weighted(
            "Active",
            Self::renderer_size(tr, self.badge_size),
            None,
            TextWeight::Bold,
        );
        f64::from(w) + 2.0 * f64::from(Spacing::BASE.s2)
    }

    /// The "Active" badge on the row's right edge when this theme is the
    /// applied OS theme (part of [`Widget::draw`]).
    fn draw_active_badge(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.view.active {
            return;
        }
        let lab = "Active";
        let size = Self::renderer_size(tr, self.badge_size);
        let pw = self.badge_width(tr);
        // The pill is its label's line box plus one `s2` of air, so it grows
        // with the type instead of clipping it.
        let (_, lh) = tr.measure_weighted(lab, size, None, TextWeight::Bold);
        let ph = f64::from(lh) + f64::from(Spacing::BASE.s2);
        let cy = self.rect.center().y;
        let pill = Rect::new(
            self.rect.x1 - pw - ROW_INSET,
            cy - ph / 2.0,
            self.rect.x1 - ROW_INSET,
            cy + ph / 2.0,
        );
        draw::fill_rrect(scene, t, pill, ph / 2.0, theme.accent_primary);
        draw::text_center(
            &mut PaintCtx::new(scene, t, tr, theme),
            lab,
            &TextStyle::new(size).with_weight(TextWeight::Bold),
            on_accent(theme),
            pill,
        );
    }
}

impl Widget for ThemeRow {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let scale = &theme.tokens.typography.scale;
        self.name_size = scale.base;
        self.tag_size = scale.sm;
        self.badge_size = scale.xs;
        // The height comes from `measure` (the two text lines, never below
        // ROW_H), so 1× rows stay exactly ROW_H and large text grows the row
        // instead of stacking the tag over the name.
        let node = leaf_node(
            taffy,
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::auto(),
                },
                min_size: Size {
                    width: Dimension::auto(),
                    height: Dimension::length(ROW_H),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        // The row's width: the known dimension when the parent has resolved
        // it, else the definite available width taffy hands the final layout
        // pass, else (a bare measure with no parent at all) the docked
        // gallery's content width. The text wraps at this width in `draw`.
        let available_w = match available.width {
            taffy::AvailableSpace::Definite(w) => Some(w),
            _ => None,
        };
        let row_w = known
            .width
            .or(available_w)
            .map_or(BARE_MEASURE_ROW_W, f64::from);
        self.measure_lines(text_renderer, row_w);
        taffy::Size {
            width: known.width.unwrap_or(0.0),
            height: known.height.unwrap_or_else(|| self.measured_height()),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else { return };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _p: &PresenceCtx,
    ) {
        self.paint(scene, t, tr, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let h = self.rect.contains(p);
                if h != self.view.hover {
                    self.view.hover = h;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.view.hover => {
                self.select_requested = true;
                *ctx.focus_target = Some(ctx.node_id);
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ThemeRow"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn test_id(&self) -> Option<&str> {
        Some(&self.id)
    }
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::RadioButton)
                .with_name(self.name.clone())
                .with_action_target(format!("themes.select:{}", self.id))
                .with_state(ui::a11y::AccessState {
                    focusable: true,
                    checked: Some(self.view.selected),
                    selected: self.view.selected,
                    ..Default::default()
                }),
        )
    }
}

// --- shared colour helpers ---------------------------------------------

use vello::Scene;

/// Pick a legible on-colour for an accent fill.
pub fn on_accent(theme: &Theme) -> Color {
    let a = [
        theme.accent_primary.r(),
        theme.accent_primary.g(),
        theme.accent_primary.b(),
    ];
    model::color(model::on_color(a))
}

/// Linearly mix colour `a` toward `b` by `t` (0..1).
pub fn lerp(a: Color, b: Color, t: f32) -> Color {
    model::color(model::mix([a.r(), a.g(), a.b()], [b.r(), b.g(), b.b()], t))
}
