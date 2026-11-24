//! A file's bytes as a grid: an offset down the side, the byte's two hexadecimal digits and the
//! same byte as a character across, and the cursor, the selection and a search's matches drawn
//! over them. The rows are the app's, read from the device, so the grid costs only what it shows.
//!
//! The app decides what a press and a key mean; the grid reports them as data (a byte and a
//! column, a key as the person's keyboard names it), never a callback of its own.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::scrollbar::{PressOutcome, ScrollMetrics, Scrollbar};
use crate::widget::{TaffyTreeExt, Widget};

#[path = "hex_grid/hex_grid_component.rs"]
mod component;
pub use component::HexGridProps;

/// Which of the row's two columns the keys edit. The byte is the same; only what a key types does
/// differ.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HexColumn {
    #[default]
    Hex,
    Ascii,
}

impl HexColumn {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Hex => "hex",
            Self::Ascii => "ascii",
        }
    }
}

/// A byte's two digits, upper case as a dump prints them.
const DIGITS: &[u8; 16] = b"0123456789ABCDEF";

/// The bytes a row holds at least and at most, and the step between the sizes tried: a row holds
/// whole groups of eight bytes.
const MIN_COLUMNS: usize = 8;
const MAX_COLUMNS: usize = 64;
const COLUMN_STEP: usize = 4;
const GROUP: usize = 8;

/// How far a row's gaps may stretch or shrink to fill the width it is given.
const GAP_SCALE: (f64, f64) = (0.5, 2.0);

/// Bytes as a grid of hexadecimal pairs and text.
pub struct HexGrid {
    pub(crate) rows: Vec<String>,
    pub(crate) changed: Vec<String>,
    pub(crate) total: i64,
    pub(crate) first: i64,
    pub(crate) columns: i64,
    pub(crate) cursor: Option<i64>,
    pub(crate) anchor: Option<i64>,
    pub(crate) column: HexColumn,
    pub(crate) matches: Vec<(usize, usize)>,
    pub(crate) at_match: Option<i64>,
    pub(crate) label: Option<String>,
    rect: Rect,
    geo: Geo,
    last_cursor: Point,
    scrollbar: Scrollbar,
    focused: bool,
    dirty: bool,
    pub(crate) on_press: Option<Box<dyn FnMut(usize, HexColumn, bool)>>,
    /// Whether a press on a byte runs now, and why it cannot when it does not.
    pub(crate) enabled: bool,
    pub(crate) description: Option<String>,
    pub(crate) on_key: Option<Box<dyn FnMut(String)>>,
    pub(crate) on_wheel: Option<Box<dyn FnMut(i64)>>,
}

impl Default for HexGrid {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            changed: Vec::new(),
            total: 0,
            first: 0,
            columns: 0,
            cursor: None,
            anchor: None,
            column: HexColumn::default(),
            matches: Vec::new(),
            at_match: None,
            label: None,
            rect: Rect::ZERO,
            geo: Geo::default(),
            last_cursor: Point::ZERO,
            scrollbar: Scrollbar::vertical(),
            focused: false,
            dirty: false,
            on_press: None,
            enabled: true,
            description: None,
            on_key: None,
            on_wheel: None,
        }
    }
}

/// The monospace metrics every pitch derives from, measured with the cells' own `Code` style.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    glyph_w: f64,
    line_h: f64,
}

impl Metrics {
    /// Before the first draw has measured anything: the `Code` role's advance at 1×.
    const FALLBACK: Self = Self {
        glyph_w: 7.8,
        line_h: 19.5,
    };

    fn measure(tr: &mut TextRenderer, theme: &Theme) -> Self {
        let (w, h) = tr.measure_style("00000000", None, &theme.text(TextRole::Code));
        if w <= 0.0 || h <= 0.0 {
            return Self::FALLBACK;
        }
        Self {
            glyph_w: f64::from(w) / 8.0,
            line_h: f64::from(h),
        }
    }
}

/// Where the grid stands this frame: the header's labels, the rows, the scroll bar and the
/// pitches paint and hit-testing share.
#[derive(Clone, Debug, Default)]
struct Geo {
    header: Rect,
    grid: Rect,
    scroll: Rect,
    row_h: f64,
    line_h: f64,
    gutter_x0: f64,
    hex_x0: f64,
    ascii_x0: f64,
    ascii_x1: f64,
    byte_w: f64,
    byte_gap: f64,
    group_gap: f64,
    ascii_pitch: f64,
    rule_hex: f64,
    rule_ascii: f64,
    visible_rows: usize,
    columns: usize,
}

impl Geo {
    fn hex_x(&self, c: usize) -> f64 {
        self.hex_x0 + c as f64 * (self.byte_w + self.byte_gap) + (c / GROUP) as f64 * self.group_gap
    }

    fn ascii_x(&self, c: usize) -> f64 {
        self.ascii_x0 + c as f64 * self.ascii_pitch
    }

    fn hex_column_at(&self, x: f64, columns: usize) -> Option<usize> {
        let half = self.byte_gap / 2.0;
        (0..columns).find(|&c| x >= self.hex_x(c) - half && x < self.hex_x(c) + self.byte_w + half)
    }

    fn ascii_column_at(&self, x: f64, columns: usize) -> Option<usize> {
        if x < self.ascii_x0 || x >= self.ascii_x1 || self.ascii_pitch <= 0.0 {
            return None;
        }
        let c = ((x - self.ascii_x0) / self.ascii_pitch) as usize;
        (c < columns).then_some(c)
    }

    fn row_at(&self, y: f64) -> Option<usize> {
        if y < self.grid.y0 || y >= self.grid.y0 + self.visible_rows as f64 * self.row_h {
            return None;
        }
        Some(((y - self.grid.y0) / self.row_h) as usize)
    }
}

/// A row's widths: glyphs that never change and gaps that stretch to fill the content width.
struct RowPlan {
    left: f64,
    right: f64,
    ascii_pitch: f64,
    gutter_w: f64,
    byte_w: f64,
    byte_gap: f64,
    group_gap: f64,
    col_gap: f64,
}

impl RowPlan {
    fn new(metrics: Metrics, theme: &Theme) -> Self {
        let sp = &theme.tokens.spacing;
        let s = |v: f32| f64::from(v);
        let ascii_gap = s(sp.s1) / 2.0;
        Self {
            left: s(sp.s4),
            right: s(theme.tokens.metrics.scrollbar_gutter) + s(sp.s1),
            ascii_pitch: metrics.glyph_w + ascii_gap,
            gutter_w: metrics.glyph_w * 8.0,
            byte_w: metrics.glyph_w * 2.0,
            byte_gap: s(sp.s2),
            group_gap: s(sp.s2),
            col_gap: s(sp.s6),
        }
    }

    fn fixed(&self, n: usize) -> f64 {
        self.left + self.gutter_w + n as f64 * (self.byte_w + self.ascii_pitch) + self.right
    }

    fn gaps(&self, n: usize) -> f64 {
        n.saturating_sub(1) as f64 * self.byte_gap
            + (n.saturating_sub(1) / GROUP) as f64 * self.group_gap
            + 2.0 * self.col_gap
    }

    fn width(&self, n: usize) -> f64 {
        self.fixed(n) + self.gaps(n)
    }
}

/// `byte` as the ASCII column shows it: a printable character, or the dot the column uses for one
/// that is not.
fn glyph(byte: u8) -> char {
    if (0x20..=0x7E).contains(&byte) {
        char::from(byte)
    } else {
        '·'
    }
}

/// One row's span of `lo..=hi` that falls in the row starting at `row_off`, as columns.
fn span_in_row(lo: usize, hi: usize, row_off: usize, columns: usize) -> Option<(usize, usize)> {
    let first = lo.max(row_off);
    let last = hi.min(row_off.saturating_add(columns).saturating_sub(1));
    (first <= last).then(|| (first - row_off, last - row_off))
}

impl HexGrid {
    /// The bytes a row holds: what the app asked for, as far as the width fits.
    fn columns_at(&self, theme: &Theme, metrics: Metrics) -> usize {
        let plan = RowPlan::new(metrics, theme);
        let fit = (MIN_COLUMNS / COLUMN_STEP..=MAX_COLUMNS / COLUMN_STEP)
            .rev()
            .map(|steps| steps * COLUMN_STEP)
            .find(|&n| plan.width(n) <= self.rect.width())
            .unwrap_or(MIN_COLUMNS);
        match usize::try_from(self.columns) {
            Ok(said) if said >= MIN_COLUMNS => said.min(fit.max(MIN_COLUMNS)),
            _ => fit,
        }
    }

    /// Resolve this frame's geometry from the rect, the theme and the measured cells.
    fn place(&self, theme: &Theme, metrics: Metrics) -> Geo {
        let mut geo = Geo {
            line_h: metrics.line_h,
            row_h: (metrics.line_h + f64::from(theme.tokens.spacing.s1) / 2.0).ceil(),
            ..Geo::default()
        };
        let plan = RowPlan::new(metrics, theme);
        let columns = self.columns_at(theme, metrics);
        geo.columns = columns;
        let scale = if plan.gaps(columns) > 0.0 {
            ((self.rect.width() - plan.fixed(columns)) / plan.gaps(columns))
                .clamp(GAP_SCALE.0, GAP_SCALE.1)
        } else {
            1.0
        };
        let (byte_gap, group_gap, col_gap) = (
            plan.byte_gap * scale,
            plan.group_gap * scale,
            plan.col_gap * scale,
        );
        let hex_span = columns as f64 * (plan.byte_w + byte_gap) - byte_gap
            + (columns.saturating_sub(1) / GROUP) as f64 * group_gap;
        let gutter_x0 = self.rect.x0 + plan.left;
        let hex_x0 = gutter_x0 + plan.gutter_w + col_gap;
        let ascii_x0 = hex_x0 + hex_span + col_gap;
        let head_top = self.rect.y0 + f64::from(theme.tokens.spacing.s2);
        geo.header = Rect::new(self.rect.x0, head_top, self.rect.x1, head_top + geo.row_h);
        let grid_top = geo.header.y1 + f64::from(theme.tokens.spacing.s1);
        geo.grid = Rect::new(
            self.rect.x0,
            grid_top,
            self.rect.x1,
            self.rect.y1.max(grid_top),
        );
        geo.byte_w = plan.byte_w;
        geo.byte_gap = byte_gap;
        geo.group_gap = group_gap;
        geo.gutter_x0 = gutter_x0;
        geo.hex_x0 = hex_x0;
        geo.ascii_x0 = ascii_x0;
        geo.ascii_x1 = ascii_x0 + columns as f64 * plan.ascii_pitch;
        geo.rule_hex = hex_x0 - col_gap / 2.0;
        geo.rule_ascii = ascii_x0 - col_gap / 2.0;
        geo.ascii_pitch = plan.ascii_pitch;
        geo.visible_rows = ((geo.grid.height() / geo.row_h).floor().max(1.0)) as usize;
        let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        let inset = f64::from(theme.tokens.spacing.s1);
        geo.scroll = Rect::new(
            self.rect.x1 - inset - gutter * 2.0 / 3.0,
            geo.grid.y0,
            self.rect.x1 - inset,
            geo.grid.y0 + geo.visible_rows as f64 * geo.row_h,
        );
        geo
    }

    fn scroll_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            self.total.max(0) as f64,
            self.geo.visible_rows as f64,
            self.first.max(0) as f64,
        )
    }

    /// The byte at `point`, with the column pressed: `None` off a byte.
    fn hit_test(&self, point: Point) -> Option<(usize, HexColumn)> {
        let g = &self.geo;
        if point.x < g.grid.x0 || point.x >= g.grid.x1 || g.columns == 0 {
            return None;
        }
        let row = usize::try_from(self.first.max(0)).unwrap_or(0) + g.row_at(point.y)?;
        if row >= usize::try_from(self.total.max(0)).unwrap_or(0) {
            return None;
        }
        let row_off = row * g.columns;
        if let Some(col) = g.hex_column_at(point.x, g.columns) {
            return Some((row_off + col, HexColumn::Hex));
        }
        let col = g.ascii_column_at(point.x, g.columns)?;
        Some((row_off + col, HexColumn::Ascii))
    }

    /// The selection as an inclusive span, when one is made.
    fn selection(&self) -> Option<(usize, usize)> {
        let anchor = usize::try_from(self.anchor?).ok()?;
        let cursor = usize::try_from(self.cursor?).ok()?;
        Some((anchor.min(cursor), anchor.max(cursor)))
    }

    /// Report the wheel or a drag of the bar as the rows the first one moves by.
    fn scrolled(&mut self, rows: i64) {
        if rows == 0 {
            return;
        }
        if let Some(wheel) = self.on_wheel.as_mut() {
            wheel(rows);
        }
    }

    fn pressed(&mut self, at: usize, column: HexColumn, extend: bool) {
        if let Some(press) = self.on_press.as_mut() {
            press(at, column, extend);
        }
    }

    fn keyed(&mut self, key: String) {
        if let Some(handle) = self.on_key.as_mut() {
            handle(key);
        }
    }

    /// Keep the scroll bar's thumb in step with the pointer.
    fn follow_scrollbar(&mut self, point: Point) {
        let (track, metrics) = (self.geo.scroll, self.scroll_metrics());
        if !metrics.overflows() {
            return;
        }
        if let Some(offset) = self.scrollbar.drag_to(track, metrics, point) {
            let target = offset.round() as i64;
            let rows = target - self.first.max(0);
            if rows != 0 {
                self.first = target;
                self.scrolled(rows);
            }
            self.dirty = true;
        }
        if self.scrollbar.update_hover(track, metrics, point) {
            self.dirty = true;
        }
    }

    /// A press on the bar: grab the thumb or page toward the press.
    fn press_scrollbar(&mut self, point: Point) -> bool {
        let (track, metrics) = (self.geo.scroll, self.scroll_metrics());
        if !metrics.overflows() || !track.contains(point) {
            return false;
        }
        match self.scrollbar.press(track, metrics, point) {
            PressOutcome::Miss => return false,
            PressOutcome::Thumb => {}
            PressOutcome::Page(offset) => {
                let target = offset.round() as i64;
                let rows = target - self.first.max(0);
                self.first = target;
                self.scrolled(rows);
            }
        }
        self.dirty = true;
        true
    }

    /// The key as the person's keyboard names it: `a`, `ArrowLeft`, `Ctrl+S`.
    fn key_text(event: &winit::event::KeyEvent, modifiers: ModifiersState) -> Option<String> {
        let mut prefix = String::new();
        if modifiers.control_key() {
            prefix.push_str("Ctrl+");
        }
        if modifiers.alt_key() {
            prefix.push_str("Alt+");
        }
        if modifiers.super_key() {
            prefix.push_str("Super+");
        }
        let named = match &event.logical_key {
            Key::Named(named) => {
                let name = match named {
                    NamedKey::ArrowLeft => "ArrowLeft",
                    NamedKey::ArrowRight => "ArrowRight",
                    NamedKey::ArrowUp => "ArrowUp",
                    NamedKey::ArrowDown => "ArrowDown",
                    NamedKey::Home => "Home",
                    NamedKey::End => "End",
                    NamedKey::PageUp => "PageUp",
                    NamedKey::PageDown => "PageDown",
                    NamedKey::Tab => "Tab",
                    NamedKey::Insert => "Insert",
                    NamedKey::Backspace => "Backspace",
                    NamedKey::Delete => "Delete",
                    NamedKey::Escape => "Escape",
                    NamedKey::Enter => "Enter",
                    NamedKey::Space => " ",
                    NamedKey::Shift | NamedKey::Control | NamedKey::Alt | NamedKey::Super => {
                        return None;
                    }
                    _ => return None,
                };
                if modifiers.shift_key() {
                    prefix.push_str("Shift+");
                }
                name.to_owned()
            }
            Key::Character(text) => text.to_string(),
            _ => return None,
        };
        Some(prefix + &named)
    }
}

impl Widget for HexGrid {
    crate::described!();

    fn name(&self) -> &'static str {
        "HexGrid"
    }

    fn test_id(&self) -> Option<&str> {
        Some("hex_grid")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    /// Its size is its slot's, from its style alone: it asks for a layout only until it has
    /// one, and what it shows inside that slot is placed as it draws.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(unused_variables, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text_renderer, theme, _env) = ctx.parts();
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let metrics = Metrics::measure(text_renderer, theme);
        self.geo = self.place(theme, metrics);
        let colors = &theme.tokens.colors;
        let code = theme.text(TextRole::Code);
        let code_strong = code.clone().strong();
        let g = self.geo.clone();
        let head_y = g.header.y0 + (g.row_h - g.line_h) / 2.0;

        // The column labels share the cells' pitch, so each label stands over its column.
        let mut labels: Vec<(String, f64)> = Vec::with_capacity(g.columns + 2);
        labels.push(("Offset".to_owned(), g.gutter_x0));
        for c in 0..g.columns {
            let digits = [DIGITS[(c >> 4) & 0xF], DIGITS[c & 0xF]];
            labels.push((String::from_utf8_lossy(&digits).into_owned(), g.hex_x(c)));
        }
        labels.push(("ASCII".to_owned(), g.ascii_x0));
        for (text, x) in &labels {
            text_renderer.draw_style(
                scene,
                text,
                colors.text_dim,
                transform * Affine::translate(Vec2::new(*x, head_y)),
                None,
                &code,
            );
        }

        // The rules between the columns run from the labels down the rows.
        let rule = f64::from(theme.tokens.metrics.chrome_rule_px);
        let edge = crate::surface::edge_on(colors.surfaces.content);
        for x in [g.rule_hex, g.rule_ascii] {
            scene.fill(
                Fill::NonZero,
                transform,
                edge,
                None,
                &Rect::new(x, g.header.y0, x + rule, g.grid.y1),
            );
        }

        let selected = self.selection();
        let cursor = self.cursor.and_then(|c| usize::try_from(c).ok());
        let total_rows = usize::try_from(self.total.max(0)).unwrap_or(0);
        for vr in 0..g.visible_rows {
            let row = usize::try_from(self.first.max(0)).unwrap_or(0) + vr;
            if row >= total_rows {
                break;
            }
            let row_off = row * g.columns;
            let y = g.grid.y0 + vr as f64 * g.row_h;
            let at = RowAt {
                vr,
                row_off,
                y,
                selected,
                cursor,
            };
            self.paint_row(
                scene,
                transform,
                text_renderer,
                theme,
                (&code, &code_strong),
                at,
            );
        }

        let metrics = self.scroll_metrics();
        if metrics.overflows() {
            self.scrollbar
                .draw(scene, transform, g.scroll, metrics, theme, self.focused);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                self.follow_scrollbar(self.last_cursor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.scrollbar.release() {
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let point = self.last_cursor;
                if !self.rect.contains(point) {
                    return;
                }
                // A press in the grid puts the keyboard in it, so the bytes can be typed at once.
                *ctx.focus_target = Some(ctx.node_id);
                if self.press_scrollbar(point) {
                    *ctx.handled = true;
                    return;
                }
                if let Some((at, column)) = self.hit_test(point) {
                    self.pressed(at, column, ctx.modifiers.state().shift_key());
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let rows = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (-*y).round() as i64 * 3,
                    MouseScrollDelta::PixelDelta(p) => {
                        -(p.y / self.geo.row_h.max(1.0)).round() as i64
                    }
                };
                self.scrolled(rows);
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput { event, .. } if self.focused => {
                if event.state != ElementState::Pressed {
                    return;
                }
                if let Some(key) = Self::key_text(event, ctx.modifiers.state()) {
                    self.keyed(key);
                    // Tab switches columns here, so the window neither walks its focus nor gives
                    // the key to another widget.
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::Role;
        let node = crate::a11y::AccessNode::new(Role::Table)
            .with_optional_name(self.label.clone())
            .with_hashed_key("hex_grid");
        // Why it cannot be pressed now, if it says: read beside its name, never instead.
        Some(match &self.description {
            Some(why) => node.with_description(why.clone()),
            None => node,
        })
    }

    /// A grid takes the keyboard from a press: the keys it types land in the byte under the
    /// cursor.
    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.rect
            .contains(point)
            .then_some(winit::window::CursorIcon::Text)
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

/// One visible row: where it stands and what is drawn over its bytes.
#[derive(Clone, Copy)]
struct RowAt {
    /// Its index among the visible rows.
    vr: usize,
    /// The offset of its first byte.
    row_off: usize,
    /// Its top.
    y: f64,
    /// The bytes chosen, first to last.
    selected: Option<(usize, usize)>,
    /// The byte the cursor is on.
    cursor: Option<usize>,
}

impl HexGrid {
    fn paint_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        (code, code_strong): (&TextStyle, &TextStyle),
        at: RowAt,
    ) {
        let RowAt { vr, row_off, y, .. } = at;
        let g = &self.geo;
        let colors = &theme.tokens.colors;
        let text_y = y + (g.row_h - g.line_h) / 2.0;
        let offset = format!("{row_off:08X}");
        text_renderer.draw_style(
            scene,
            &offset,
            colors.text_dim,
            transform * Affine::translate(Vec2::new(g.gutter_x0, text_y)),
            None,
            code,
        );

        self.paint_tints(scene, transform, theme, at);

        let hex = self.rows.get(vr).map_or("", String::as_str);
        let changed = self.changed.get(vr).map_or("", String::as_str);
        let mut glyphs = String::new();
        for c in 0..g.columns {
            let Some(pair) = hex.get(c * 2..c * 2 + 2) else {
                break;
            };
            let Ok(byte) = u8::from_str_radix(pair, 16) else {
                break;
            };
            let marked = changed.as_bytes().get(c) == Some(&b'1');
            let style = if marked { code_strong } else { code };
            let ink = if byte == 0 && !marked {
                colors.text_tertiary
            } else {
                colors.text_primary
            };
            text_renderer.draw_style(
                scene,
                pair,
                ink,
                transform * Affine::translate(Vec2::new(g.hex_x(c), text_y)),
                None,
                style,
            );
            let ascii_ink = if marked || (0x20..=0x7E).contains(&byte) {
                colors.text_primary
            } else {
                colors.text_tertiary
            };
            glyphs.clear();
            glyphs.push(glyph(byte));
            text_renderer.draw_style(
                scene,
                &glyphs,
                ascii_ink,
                transform * Affine::translate(Vec2::new(g.ascii_x(c), text_y)),
                None,
                style,
            );
        }
    }

    /// The bands under one row's glyphs, lowest to highest: the bytes edited, a search's matches,
    /// the selection, then the cursor.
    fn paint_tints(&self, scene: &mut Scene, transform: Affine, theme: &Theme, at: RowAt) {
        let RowAt {
            vr,
            row_off,
            y,
            selected,
            cursor,
        } = at;
        let g = &self.geo;
        let colors = &theme.tokens.colors;
        let columns = g.columns;
        let radius = f64::from(theme.tokens.radii.detail);
        let half = g.byte_gap / 2.0;
        let hex_band = |first: usize, last: usize| {
            Rect::new(
                g.hex_x(first) - half,
                y,
                g.hex_x(last) + g.byte_w + half,
                y + g.row_h,
            )
        };
        let ascii_band = |first: usize, last: usize| {
            Rect::new(
                g.ascii_x(first),
                y,
                g.ascii_x(last) + g.ascii_pitch,
                y + g.row_h,
            )
        };
        let fill = |scene: &mut Scene, rect: Rect, color: Color| {
            scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &RoundedRect::from_rect(rect, radius),
            );
        };

        let changed = self.changed.get(vr).map_or("", String::as_str);
        let edited = colors.lift_on(colors.surfaces.content, 1.0);
        for c in 0..columns {
            if changed.as_bytes().get(c) == Some(&b'1') {
                fill(scene, hex_band(c, c), edited);
                fill(scene, ascii_band(c, c), edited);
            }
        }

        let row_end = row_off.saturating_add(columns);
        for (index, &(start, end)) in self.matches.iter().enumerate() {
            if end <= row_off || start >= row_end {
                continue;
            }
            let Some((a, b)) = span_in_row(start, end.saturating_sub(1), row_off, columns) else {
                continue;
            };
            let current = self
                .at_match
                .is_some_and(|at| usize::try_from(at) == Ok(index));
            crate::widget::paint_find_match(scene, transform, hex_band(a, b), current, theme);
            crate::widget::paint_find_match(scene, transform, ascii_band(a, b), current, theme);
        }

        if let Some((lo, hi)) = selected
            && let Some((a, b)) = span_in_row(lo, hi, row_off, columns)
        {
            fill(scene, hex_band(a, b), colors.intent.selection_tint);
            fill(scene, ascii_band(a, b), colors.intent.selection_tint);
        }

        // The cursor: the person's colour round the cell they type in, a quiet outline on its
        // mirror in the other column.
        if let Some(c) = cursor
            .and_then(|cursor| cursor.checked_sub(row_off))
            .filter(|&c| c < columns)
        {
            let (active, mirror) = if self.column == HexColumn::Hex {
                (hex_band(c, c), ascii_band(c, c))
            } else {
                (ascii_band(c, c), hex_band(c, c))
            };
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                colors.intent.you,
                None,
                &RoundedRect::from_rect(active, radius),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.text_tertiary,
                None,
                &RoundedRect::from_rect(mirror, radius),
            );
        }
    }
}
