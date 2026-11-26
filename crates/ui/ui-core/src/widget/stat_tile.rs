//! `StatTile`: one number and what it measures, on the content ground.

use std::collections::HashMap;

use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::kurbo::{Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use tilo_ui_base::icons::Icon;
use tilo_ui_base::paint::PaintCtx;

use crate::widget::activation::{Activation, Interaction, activation_controls};
use crate::widget::card_column_width;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::DrawChild;
use tilo_ui_base::{text_render::TextRenderer, text_render::TextStyle};
use tilo_ui_base::{widget::Element, widget::TaffyTreeExt, widget::Tone, widget::Widget};

#[path = "stat_tile_component.rs"]
mod component;
pub use component::{StatGroupProps, StatTileProps};

/// Direction of a tile's change line. The arrow says which way the number
/// moved; whether that is good is the tile's [`Tone`], not the arrow's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trend {
    Up,
    Down,
    Flat,
}

impl Trend {
    fn arrow(self) -> Option<Icon> {
        match self {
            Self::Up => Some(Icon::ArrowUp),
            Self::Down => Some(Icon::ArrowDown),
            Self::Flat => None,
        }
    }

    fn spoken(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Flat => "unchanged",
        }
    }
}

/// The resolved geometry of a tile in `theme`.
#[derive(Clone, Debug)]
struct TileMetrics {
    label: TextStyle,
    value: TextStyle,
    hero: TextStyle,
    unit: TextStyle,
    change: TextStyle,
    ticker: f64,
    ticker_gap: f64,
    tight: f64,
    unit_gap: f64,
    pad: f64,
    radius: f64,
}

impl TileMetrics {
    fn new(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        Self {
            label: theme.text(TextRole::Caption),
            value: theme.text(TextRole::Title).numeric(),
            hero: theme.text(TextRole::Display).numeric(),
            unit: theme.text(TextRole::Caption),
            change: theme.text(TextRole::Caption).numeric(),
            ticker: f64::from(theme.tokens.metrics.chrome_rule_px) * 3.0,
            ticker_gap: f64::from(spacing.s3),
            tight: f64::from(spacing.rhythm.tight),
            unit_gap: f64::from(spacing.s1),
            pad: f64::from(spacing.s2),
            radius: f64::from(theme.tokens.radii.item),
        }
    }

    fn line(text: &mut TextRenderer, style: &TextStyle) -> f64 {
        f64::from(text.measure_style("Ag", None, style).1)
    }

    fn sparkline(&self, text: &mut TextRenderer) -> f64 {
        2.0 * Self::line(text, &self.label)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StatTileFace<'a> {
    label: &'a str,
    value: &'a str,
    unit: Option<&'a str>,
    note: Option<&'a str>,
    change: Option<(&'a str, Trend)>,
    tone: Option<Tone>,
    hero: bool,
    sparkline: Option<&'a [f32]>,
    pressable: bool,
    selected: bool,
    interaction: Interaction,
}

impl<'a> StatTileFace<'a> {
    #[must_use]
    pub fn new(label: &'a str, value: &'a str) -> Self {
        Self {
            label,
            value,
            unit: None,
            note: None,
            change: None,
            tone: None,
            hero: false,
            sparkline: None,
            pressable: false,
            selected: false,
            interaction: Interaction::default(),
        }
    }

    /// The value's unit ("GB", "ms"), set smaller beside it.
    #[must_use]
    pub fn unit(mut self, unit: &'a str) -> Self {
        self.unit = Some(unit);
        self
    }

    /// A neutral qualifier read with the value ("of 2 TB", "estimated"),
    /// on its own line under it. It says nothing good or bad.
    #[must_use]
    pub fn note(mut self, note: &'a str) -> Self {
        self.note = Some(note);
        self
    }

    /// How the value changed: `("12% since yesterday", Trend::Up)`.
    #[must_use]
    pub fn change(mut self, text: &'a str, trend: Trend) -> Self {
        self.change = Some((text, trend));
        self
    }

    #[must_use]
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }

    /// The one hero number of a screen: the value in `Display`.
    #[must_use]
    pub fn hero(mut self) -> Self {
        self.hero = true;
        self
    }

    #[must_use]
    pub fn sparkline(mut self, samples: &'a [f32]) -> Self {
        self.sparkline = Some(samples);
        self
    }

    #[must_use]
    pub fn pressable(mut self, selected: bool) -> Self {
        self.pressable = true;
        self.selected = selected;
        self
    }

    #[must_use]
    pub fn interaction(mut self, interaction: Interaction) -> Self {
        self.interaction = interaction;
        self
    }

    fn value_style<'m>(&self, m: &'m TileMetrics) -> &'m TextStyle {
        if self.hero { &m.hero } else { &m.value }
    }

    /// How far the text stands in from the content edge: past the ticker.
    fn inset(&self, m: &TileMetrics) -> f64 {
        if self.tone.is_some() {
            m.ticker + m.ticker_gap
        } else {
            0.0
        }
    }

    pub fn height(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let m = TileMetrics::new(theme);
        self.content_height(text, &m) + if self.pressable { 2.0 * m.pad } else { 0.0 }
    }

    fn content_height(&self, text: &mut TextRenderer, m: &TileMetrics) -> f64 {
        let mut h = TileMetrics::line(text, &m.label)
            + m.tight
            + TileMetrics::line(text, self.value_style(m));
        if self.note.is_some() {
            h += TileMetrics::line(text, &m.unit);
        }
        if self.change.is_some() {
            h += m.tight + TileMetrics::line(text, &m.change);
        }
        if self.sparkline.is_some() {
            h += m.tight + m.sparkline(text);
        }
        h.ceil()
    }

    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let theme = ctx.theme;
        let m = TileMetrics::new(theme);
        let colors = &theme.tokens.colors;

        if self.pressable {
            let fill = if self.selected || self.interaction.pressed {
                Some(colors.intent.selection)
            } else if self.interaction.hovered && !self.interaction.disabled {
                Some(colors.intent.selection_hover)
            } else {
                None
            };
            if let Some(fill) = fill {
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    fill,
                    None,
                    &RoundedRect::from_rect(rect.inflate(m.pad, 0.0), m.radius),
                );
            }
        }
        let content = if self.pressable {
            rect.inset((0.0, -m.pad))
        } else {
            rect
        };
        let block = self.content_height(ctx.text, &m);
        if let Some(tone) = self.tone {
            let tick = Rect::new(
                content.x0,
                content.y0,
                content.x0 + m.ticker,
                content.y0 + block.min(content.height()),
            );
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                tone.colour(theme),
                None,
                &RoundedRect::from_rect(tick, m.ticker / 2.0),
            );
        }
        let x = rect.x0 + self.inset(&m);
        let width = (rect.x1 - x).max(0.0);
        let mut y = content.y0;

        let label = ctx.text.elide_to_width_style(
            self.label,
            tilo_ui_base::num::f32_of_f64(width),
            &m.label,
        );
        ctx.text(&label, &m.label, theme.text_dim, Point::new(x, y), None);
        y += TileMetrics::line(ctx.text, &m.label) + m.tight;

        // The value elides first; the unit keeps its place beside it.
        let value_style = self.value_style(&m).clone();
        let unit_w = self.unit.map_or(0.0, |unit| {
            f64::from(ctx.text.measure_style(unit, None, &m.unit).0) + m.unit_gap
        });
        let value = ctx.text.elide_to_width_style(
            self.value,
            tilo_ui_base::num::f32_of_f64((width - unit_w).max(1.0)),
            &value_style,
        );
        let (value_w, _) = ctx.text.measure_style(&value, None, &value_style);
        let value_line = TileMetrics::line(ctx.text, &value_style);
        ctx.text(
            &value,
            &value_style,
            theme.text_primary,
            Point::new(x, y),
            None,
        );
        if let Some(unit) = self.unit {
            let first_baseline = |text: &mut TextRenderer, s: &TextStyle, sample: &str| {
                f64::from(text.first_baseline_style(sample, None, s))
            };
            let value_base = first_baseline(ctx.text, &value_style, &value);
            let unit_base = first_baseline(ctx.text, &m.unit, unit);
            ctx.text(
                unit,
                &m.unit,
                theme.text_dim,
                Point::new(
                    x + f64::from(value_w) + m.unit_gap,
                    y + value_base - unit_base,
                ),
                None,
            );
        }
        y += value_line;

        if let Some(note) = self.note {
            let note = ctx.text.elide_to_width_style(
                note,
                tilo_ui_base::num::f32_of_f64(width.max(1.0)),
                &m.unit,
            );
            ctx.text(&note, &m.unit, theme.text_dim, Point::new(x, y), None);
            y += TileMetrics::line(ctx.text, &m.unit);
        }

        if let Some((change, trend)) = self.change {
            y += m.tight;
            let line = TileMetrics::line(ctx.text, &m.change);
            let mut cx = x;
            if let Some(arrow) = trend.arrow() {
                let side = f64::from(theme.tokens.metrics.icon_sm);
                let glyph = Rect::new(
                    cx,
                    y + (line - side) / 2.0,
                    cx + side,
                    y + (line + side) / 2.0,
                );
                arrow.draw(ctx.scene, ctx.transform, glyph, theme.text_dim);
                cx += side + m.unit_gap;
            }
            let change = ctx.text.elide_to_width_style(
                change,
                tilo_ui_base::num::f32_of_f64((x + width - cx).max(1.0)),
                &m.change,
            );
            ctx.text(&change, &m.change, theme.text_dim, Point::new(cx, y), None);
            y += line;
        }

        if let Some(samples) = self.sparkline {
            y += m.tight;
            let h = m.sparkline(ctx.text);
            let ink = self.tone.map_or(theme.text_dim, |tone| tone.colour(theme));
            crate::widget::Sparkline::paint(
                ctx.scene,
                ctx.transform,
                Rect::new(x, y, x + width, y + h),
                samples,
                crate::widget::chart::TrendRange::fit(samples),
                ink,
                &crate::widget::chart::TrendStyle::SPARKLINE,
            );
        }

        if self.pressable && self.interaction.focused && !self.interaction.disabled {
            crate::widget::paint_focus_ring(
                ctx.scene,
                ctx.transform,
                rect.inflate(m.pad, 0.0),
                m.radius,
                theme,
            );
        }
    }

    /// The tile's node for assistive technology, at `rect`: its label as
    /// the name and its value, unit, note and change as one spoken value.
    pub fn accessibility(&self, rect: Rect) -> tilo_ui_base::a11y::AccessNode {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        let role = if self.pressable {
            Role::Button
        } else {
            Role::Group
        };
        AccessNode::new(role)
            .with_name(self.label.to_owned())
            .with_value(self.spoken_value())
            .with_bounds(rect)
            .with_state(
                AccessState::new()
                    .focusable(self.pressable)
                    .focused(self.interaction.focused)
                    .selected(self.selected),
            )
    }

    fn spoken_value(&self) -> String {
        let mut value = match self.unit {
            Some(unit) => format!("{} {unit}", self.value),
            None => self.value.to_owned(),
        };
        if let Some(note) = self.note {
            value.push_str(&format!(" {note}"));
        }
        if let Some((change, trend)) = self.change {
            value.push_str(&format!(", {} {change}", trend.spoken()));
        }
        value
    }
}

pub struct StatTile {
    label: String,
    value: String,
    unit: Option<String>,
    note: Option<String>,
    change: Option<(String, Trend)>,
    tone: Option<Tone>,
    hero: bool,
    sparkline: Option<Vec<f32>>,
    selected: bool,
    activation: Activation,
    metrics: Option<TileMetrics>,
    rect: Rect,
    dirty: bool,
    layout_dirty: bool,
}

impl StatTile {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self::new_owned(label.into(), value.into())
    }

    /// [`Self::new`] once its text is owned: one copy in the toolkit
    /// rather than one in every app that calls the generic constructor.
    fn new_owned(label: String, value: String) -> Self {
        Self {
            label,
            value,
            unit: None,
            note: None,
            change: None,
            tone: None,
            hero: false,
            sparkline: None,
            selected: false,
            activation: Activation::new("stat"),
            metrics: None,
            rect: Rect::ZERO,
            dirty: true,
            layout_dirty: true,
        }
    }

    #[must_use]
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    pub fn set_value(&mut self, value: impl Into<String>) {
        let value = value.into();
        if value != self.value {
            self.value = value;
            self.dirty = true;
        }
    }

    pub fn set_note(&mut self, note: Option<String>) {
        if note != self.note {
            self.layout_dirty |= note.is_some() != self.note.is_some();
            self.note = note;
            self.dirty = true;
        }
    }

    pub fn set_change(&mut self, change: Option<(String, Trend)>) {
        if change != self.change {
            self.layout_dirty |= change.is_some() != self.change.is_some();
            self.change = change;
            self.dirty = true;
        }
    }

    #[must_use]
    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    #[must_use]
    pub fn change(mut self, text: impl Into<String>, trend: Trend) -> Self {
        self.change = Some((text.into(), trend));
        self
    }

    #[must_use]
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }

    #[must_use]
    pub fn hero(mut self) -> Self {
        self.hero = true;
        self
    }

    #[must_use]
    pub fn sparkline(mut self, samples: Vec<f32>) -> Self {
        self.sparkline = Some(samples);
        self
    }

    pub fn set_live(&mut self, value: impl Into<String>, samples: Vec<f32>) {
        self.value = value.into();
        self.sparkline = Some(samples);
        self.dirty = true;
    }

    /// Pressing the tile opens its detail; `id` names it for keyboard,
    /// tests and assistive technology.
    #[must_use]
    pub fn on_activate(mut self, id: impl Into<String>, f: impl FnMut() + 'static) -> Self {
        self.activation = Activation::new(id);
        self.activation.set_callback(Box::new(f));
        self
    }

    /// Pressing the tile opens its detail, once for a double-click; `id` names it for keyboard,
    /// tests and assistive technology.
    #[must_use]
    pub fn on_open(mut self, id: impl Into<String>, f: impl FnMut() + 'static) -> Self {
        self.activation = Activation::new(id);
        self.activation.set_open(Box::new(f));
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn set_selected(&mut self, selected: bool) {
        if self.selected != selected {
            self.selected = selected;
            self.dirty = true;
        }
    }

    fn face(&self) -> StatTileFace<'_> {
        let mut face =
            StatTileFace::new(&self.label, &self.value).interaction(self.activation.state);
        if let Some(unit) = &self.unit {
            face = face.unit(unit);
        }
        if let Some(note) = &self.note {
            face = face.note(note);
        }
        if let Some((text, trend)) = &self.change {
            face = face.change(text, *trend);
        }
        if let Some(tone) = self.tone {
            face = face.tone(tone);
        }
        if self.hero {
            face = face.hero();
        }
        if let Some(samples) = &self.sparkline {
            face = face.sparkline(samples);
        }
        if self.activation.is_bound() {
            face = face.pressable(self.selected);
        }
        face
    }
}

impl Widget for StatTile {
    tilo_ui_base::provides!(dyn tilo_ui_base::widget::controls::ActionControls,);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.metrics = Some(TileMetrics::new(theme));
        let style = Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: taffy::Size {
                width: length(0.0_f32),
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        let style = tilo_ui_base::widget::layout::filling(style, 1.0);
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(m) = self.metrics.clone() else {
            return taffy::Size::ZERO;
        };
        let face = self.face();
        let mut height = face.content_height(text_renderer, &m);
        if face.pressable {
            height += 2.0 * m.pad;
        }
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => tilo_ui_base::num::f32_of_f64(TileMetrics::line(text_renderer, &m.value) * 6.0),
        });
        taffy::Size {
            width,
            height: tilo_ui_base::num::f32_of_f64(height),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.dirty = true;
        }
        self.rect = rect;
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if env.should_cull(self.rect) {
            return;
        }
        let rect = self.rect;
        self.face().paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            rect,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        if self.activation.is_bound() && self.activation.handle_pointer(event, ctx, self.rect, &[])
        {
            self.dirty = true;
        }
    }

    tilo_ui_base::pressable!(rect);

    tilo_ui_base::redraw_flag!();

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty || self.rect == Rect::ZERO
    }

    fn layout_measured(&mut self) {
        self.layout_dirty = false;
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn name(&self) -> &'static str {
        "StatTile"
    }

    fn test_id(&self) -> Option<&str> {
        self.activation.is_bound().then(|| self.activation.id())
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        let mut node = self.face().accessibility(self.rect);
        node.bounds = None;
        Some(node)
    }
}

activation_controls!(StatTile, activation);

/// A set of stat tiles that reflows by width: all in one row when they fit,
/// then half per row (a 2×2 for four), then one per row. Tiles are separated
/// by space, never boxed.
pub struct StatGroup {
    tiles: Vec<Element>,
    nodes: Vec<NodeId>,
    min_tile: f64,
    column_gap: f64,
    row_gap: f64,
    rect: Rect,
    structure_dirty: bool,
}

impl StatGroup {
    pub fn new(tiles: Vec<Element>) -> Self {
        Self {
            tiles,
            nodes: Vec::new(),
            min_tile: 0.0,
            column_gap: 0.0,
            row_gap: 0.0,
            rect: Rect::ZERO,
            structure_dirty: false,
        }
    }

    /// The tile at `index`, to update its reading in place.
    pub fn tile_mut(&mut self, index: usize) -> Option<&mut StatTile> {
        self.tiles
            .get_mut(index)
            .and_then(|tile| tile.as_any_mut())
            .and_then(|tile| tile.downcast_mut::<StatTile>())
    }

    pub fn set_tiles(&mut self, tiles: Vec<Element>) {
        self.tiles = tiles;
        self.structure_dirty = true;
    }

    /// How many columns the group uses at `width`: the largest count of
    /// tiles that fit whose rows come out even (4, then 2, then 1 for four).
    pub fn columns_for_width(&self, width: f64) -> usize {
        columns_for(self.tiles.len(), width, self.min_tile, self.column_gap)
    }

    /// How many columns the group uses at `width`, and how wide each is.
    fn tracks_at(&self, width: f64) -> (usize, f32) {
        let columns = self.columns_for_width(width);
        let column =
            tilo_ui_base::num::f32_of_f64(card_column_width(width, columns, self.column_gap));
        (columns, column)
    }
}

pub fn columns_for(count: usize, width: f64, min_tile: f64, gap: f64) -> usize {
    let count = count.max(1);
    let fits = crate::widget::card_columns(width, min_tile, gap);
    (1..=count.min(fits))
        .rev()
        .find(|columns| count.is_multiple_of(*columns))
        .unwrap_or(1)
}

impl Widget for StatGroup {
    tilo_ui_base::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let rhythm = &theme.tokens.spacing.rhythm;
        self.column_gap = f64::from(rhythm.group);
        self.row_gap = f64::from(rhythm.section);
        self.min_tile = (f64::from(theme.text(TextRole::Body).line_height_px()) * 7.0).round();
        self.nodes = self
            .tiles
            .iter_mut()
            .map(|tile| tile.build_layout(taffy, theme, node_map))
            .collect();
        // A rebuilt group starts with the tracks of the width it was laid out
        // at, so its first solve is already its height: one column there
        // stacks the tiles, and a scroll view around it would see an
        // overflow that is not there.
        let grid_template_columns = if self.rect.width() > 0.0 {
            let (columns, column) = self.tracks_at(self.rect.width());
            vec![length(column); columns]
        } else {
            vec![taffy::style_helpers::fr(1.0_f32)]
        };
        let style = Style {
            display: taffy::Display::Grid,
            grid_template_columns,
            grid_auto_rows: vec![taffy::style_helpers::min_content()],
            gap: taffy::Size {
                width: length(tilo_ui_base::num::f32_of_f64(self.column_gap)),
                height: length(tilo_ui_base::num::f32_of_f64(self.row_gap)),
            },
            size: taffy::Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: taffy::Size {
                width: length(0.0_f32),
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        let node = taffy.parent_node(style, &self.nodes);
        let _ = taffy.set_restyled_for_width(node, true);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    /// As many columns as the width holds tiles of, each as wide as the width shares out.
    fn style_for_width(
        &mut self,
        styles: &mut taffy::Restyle<'_>,
        width: taffy::AvailableSpace,
        _text: &mut tilo_ui_base::text_render::TextRenderer,
    ) {
        let tracks = match width {
            taffy::AvailableSpace::Definite(width) => {
                let (columns, column) = self.tracks_at(f64::from(width));
                vec![length(column); columns]
            }
            _ => vec![taffy::style_helpers::fr(1.0_f32)],
        };
        let node = styles.node();
        let mut style = styles.style(node).clone();
        style.grid_template_columns = tracks;
        styles.set_style(node, style);
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        let origin = self.rect.origin();
        for (tile, &child) in self.tiles.iter_mut().zip(&self.nodes) {
            tile.apply_layout(taffy, child, origin);
        }
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        for tile in &mut self.tiles {
            tile.draw_child(scene, transform, text_renderer, theme, &env);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        for (tile, &node) in self.tiles.iter_mut().zip(&self.nodes) {
            tile.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                break;
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.tiles.iter().map(AsRef::as_ref).collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for tile in &self.tiles {
            visitor(tile.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.tiles
            .iter_mut()
            .map(|t| t.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    /// Its box is the rows its tiles make: it asks layout when a tile does, and a group holding
    /// none has no row to size.
    fn is_layout_dirty(&self) -> bool {
        self.tiles.iter().any(|tile| tile.is_layout_dirty())
    }

    fn mark_drawn(&mut self) {
        for tile in &mut self.tiles {
            tile.mark_drawn();
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure_dirty);
        for tile in &mut self.tiles {
            dirty |= tile.take_structure_dirty();
        }
        dirty
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "StatGroup"
    }
}

/// What a group of tiles' tests read of it from outside the module.
#[cfg(feature = "test-support")]
impl StatGroup {
    /// Stand the group in a box of a width its own solve did not give it.
    pub fn test_set_rect(&mut self, rect: vello::kurbo::Rect) {
        self.rect = rect;
    }
}

/// The style a tile's value is drawn in, for the theme's tests.
#[cfg(feature = "test-support")]
#[must_use]
pub fn test_tile_value_style(theme: &Theme) -> TextStyle {
    TileMetrics::new(theme).value
}
