//! A two-way comparison drawn line by line: each side's own line numbers down its gutter, the
//! changed rows washed in the theme's tone, and a strip down the side showing where in the text
//! the changes stand. The rows are the app's, aligned by the device, so the grid costs only what
//! it shows.
//!
//! The app decides what a press means; the grid reports it as data (the row pressed), never a
//! callback of its own. Which change the window is at is the app's, and the grid brings it into
//! view when it changes.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use crate::widget::code_editor::{HlState, Language, LineSpans};
use crate::widget::scrollbar::{PressOutcome, Scrollbar};
use tilo_ui_base::paint::with_alpha_scaled;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::ScrollMetrics;
use tilo_ui_base::{text_render::TextRenderer, text_render::TextStyle};
use tilo_ui_base::{widget::TaffyTreeExt, widget::Widget};

#[path = "diff_grid/diff_grid_component.rs"]
mod component;
pub use component::DiffGridProps;

/// Which side of the comparison shows: both, the left one alone or the right one alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiffSide {
    #[default]
    Both,
    Left,
    Right,
}

impl DiffSide {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

/// What kind of change a row is: what its wash and its mark read as.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum RowKind {
    /// An unchanged row: both sides hold the same line.
    #[default]
    Equal,
    /// A line only the right side has.
    Added,
    /// A line only the left side has.
    Removed,
    /// Both sides have a line here, and it differs.
    Modified,
}

impl RowKind {
    fn is_change(self) -> bool {
        self != Self::Equal
    }

    /// The tone a changed row is washed and marked in.
    fn colour(self, theme: &Theme) -> Option<Color> {
        let semantic = &theme.tokens.colors.semantic;
        match self {
            Self::Equal => None,
            Self::Added => Some(semantic.success),
            Self::Removed => Some(semantic.danger),
            Self::Modified => Some(semantic.warning),
        }
    }
}

/// Each line of `lines` as `language` colours it, or none where it colours nothing or the text is
/// too large to colour whole.
fn highlight(lines: &[String], language: Option<Language>) -> Vec<LineSpans> {
    let Some(highlighter) = language.and_then(Language::highlighter) else {
        return Vec::new();
    };
    if lines.len() > MAX_HIGHLIGHTED_LINES {
        return Vec::new();
    }
    let mut carry: HlState = 0;
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let spans = highlighter.spans(line, carry);
        carry = spans.carry;
        out.push(spans);
    }
    out
}

/// The most lines a comparison colours: past it the text reads uncoloured rather than costing a
/// frame to colour.
const MAX_HIGHLIGHTED_LINES: usize = 20_000;

/// The monospace metrics every pitch derives from, measured with the code role's own style.
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

/// Where the grid stands this frame: the gutters, the columns, the strip and the scroll bar that
/// painting and hit-testing share.
#[derive(Clone, Debug, Default)]
struct Geo {
    grid: Rect,
    scroll: Rect,
    strip: Rect,
    row_h: f64,
    line_h: f64,
    /// The left gutter's first x, and its width in glyphs.
    left_gutter_x0: f64,
    left_x0: f64,
    right_gutter_x0: f64,
    right_x0: f64,
    /// The column's right edge, where a long line is clipped.
    left_x1: f64,
    right_x1: f64,
    visible_rows: usize,
    both: bool,
}

impl Geo {
    fn row_at(&self, y: f64) -> Option<usize> {
        if y < self.grid.y0 {
            return None;
        }
        let row = ((y - self.grid.y0) / self.row_h) as usize;
        (row < self.visible_rows).then_some(row)
    }
}

/// A two-way comparison drawn as two aligned columns.
pub struct DiffGrid {
    pub(crate) left: Vec<String>,
    pub(crate) right: Vec<String>,
    /// One character per aligned row: `' '` where both sides hold the same line, `'a'` a line
    /// only the right has, `'r'` a line only the left has, `'m'` a line both have and differ.
    pub(crate) kinds: String,
    /// The number each row shows on each side, counted from the kinds: derived, not asked for.
    left_nos: Vec<String>,
    right_nos: Vec<String>,
    pub(crate) at: Option<i64>,
    pub(crate) side: DiffSide,
    pub(crate) language: Option<Language>,
    pub(crate) label: Option<String>,
    /// Why a press cannot run now, where the label does not say.
    pub(crate) description: Option<String>,
    /// What an empty row reads where only the other side has a line there, as one side shows.
    pub(crate) gap_words: Option<String>,
    /// The first of the comparison's rows this grid holds, as the window it was given starts.
    pub(crate) shown_first: i64,
    /// How many rows the whole comparison has; 0 holds every row the grid was given.
    pub(crate) total: i64,
    /// Each row's line number on the left, when the grid holds a window.
    pub(crate) left_numbers: Option<Vec<String>>,
    /// Each row's line number on the right, when the grid holds a window.
    pub(crate) right_numbers: Option<Vec<String>>,
    /// Each line of each side as its language colours it, once per change of the text.
    left_spans: Vec<LineSpans>,
    right_spans: Vec<LineSpans>,
    rect: Rect,
    geo: Geo,
    /// The first row shown.
    first: usize,
    last_cursor: Point,
    scrollbar: Scrollbar,
    focused: bool,
    dirty: bool,
    pub(crate) on_press: Option<Box<dyn FnMut(usize)>>,
    pub(crate) on_wheel: Option<Box<dyn FnMut(i64)>>,
}

impl Default for DiffGrid {
    fn default() -> Self {
        Self {
            left: Vec::new(),
            right: Vec::new(),
            kinds: String::new(),
            left_nos: Vec::new(),
            right_nos: Vec::new(),
            at: None,
            side: DiffSide::Both,
            language: None,
            label: None,
            description: None,
            gap_words: None,
            shown_first: 0,
            total: 0,
            left_numbers: None,
            right_numbers: None,
            left_spans: Vec::new(),
            right_spans: Vec::new(),
            rect: Rect::ZERO,
            geo: Geo::default(),
            first: 0,
            last_cursor: Point::ZERO,
            scrollbar: Scrollbar::vertical(),
            focused: false,
            dirty: false,
            on_press: None,
            on_wheel: None,
        }
    }
}

impl DiffGrid {
    fn rows(&self) -> usize {
        if self.total > 0 {
            usize::try_from(self.total).unwrap_or(0)
        } else {
            self.left.len().max(self.right.len())
        }
    }

    /// Where `row` of the comparison stands among the rows the grid holds: the whole comparison
    /// is held, or the window that starts at `first`.
    fn held(&self, row: usize) -> usize {
        if self.total > 0 {
            row.saturating_sub(self.first)
        } else {
            row
        }
    }

    fn kind_at(&self, row: usize) -> RowKind {
        match self.kinds.as_bytes().get(row) {
            Some(b'a') => RowKind::Added,
            Some(b'r') => RowKind::Removed,
            Some(b'm') => RowKind::Modified,
            _ => RowKind::Equal,
        }
    }

    /// Colours each side's lines with its language, once per change of the text: a comparison
    /// reads as the files do.
    pub(crate) fn highlight_rows(&mut self) {
        self.left_spans = highlight(&self.left, self.language);
        self.right_spans = highlight(&self.right, self.language);
    }

    /// Counts each side's line numbers from the kinds: a side's number moves only where it has a
    /// line, so both columns stand level while each keeps its own file's numbering.
    pub(crate) fn number_rows(&mut self) {
        if self.total > 0 {
            // A window's numbers come with it: the rows before it are not here to count from.
            self.left_nos = self
                .left_numbers
                .clone()
                .unwrap_or_else(|| vec![String::new(); self.left.len()]);
            self.right_nos = self
                .right_numbers
                .clone()
                .unwrap_or_else(|| vec![String::new(); self.right.len()]);
            return;
        }
        let rows = self.rows();
        let mut left = Vec::with_capacity(rows);
        let mut right = Vec::with_capacity(rows);
        let (mut left_no, mut right_no) = (0usize, 0usize);
        for row in 0..rows {
            match self.kind_at(row) {
                RowKind::Added => {
                    left.push(String::new());
                    right_no += 1;
                    right.push(right_no.to_string());
                }
                RowKind::Removed => {
                    left_no += 1;
                    left.push(left_no.to_string());
                    right.push(String::new());
                }
                RowKind::Equal | RowKind::Modified => {
                    left_no += 1;
                    right_no += 1;
                    left.push(left_no.to_string());
                    right.push(right_no.to_string());
                }
            }
        }
        self.left_nos = left;
        self.right_nos = right;
    }

    fn line(&self, side: DiffSide, row: usize) -> &str {
        let lines = match side {
            DiffSide::Right => &self.right,
            DiffSide::Both | DiffSide::Left => &self.left,
        };
        lines.get(self.held(row)).map_or("", String::as_str)
    }

    fn number(&self, side: DiffSide, row: usize) -> &str {
        let numbers = match side {
            DiffSide::Right => &self.right_nos,
            DiffSide::Both | DiffSide::Left => &self.left_nos,
        };
        numbers.get(self.held(row)).map_or("", String::as_str)
    }

    /// Resolve this frame's geometry from the rect and the measured cells.
    fn place(&self, theme: &Theme, metrics: Metrics) -> Geo {
        let sp = &theme.tokens.spacing;
        let s = |v: f32| f64::from(v);
        let both = self.side == DiffSide::Both;
        let inset = s(sp.s3);
        let gutter_w = metrics.glyph_w * 5.0;
        let gap = s(sp.s4);
        let strip_w = 3.0;
        let scroll_gutter = s(theme.tokens.metrics.scrollbar_gutter);
        let right_edge = self.rect.x1 - scroll_gutter - strip_w - s(sp.s1);
        let mut geo = Geo {
            row_h: (metrics.line_h + s(sp.s1) / 2.0).ceil(),
            line_h: metrics.line_h,
            both,
            ..Geo::default()
        };
        geo.grid = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y1);
        geo.left_gutter_x0 = self.rect.x0 + inset;
        geo.left_x0 = geo.left_gutter_x0 + gutter_w + s(sp.s2);
        if both {
            let column = (right_edge - geo.left_x0 - gap) / 2.0;
            geo.left_x1 = geo.left_x0 + column;
            geo.right_gutter_x0 = geo.left_x1 + gap;
            geo.right_x0 = geo.right_gutter_x0 + gutter_w + s(sp.s2);
            geo.right_x1 = right_edge;
        } else {
            geo.left_x1 = right_edge;
            geo.right_gutter_x0 = geo.left_gutter_x0;
            geo.right_x0 = geo.left_x0;
            geo.right_x1 = right_edge;
        }
        geo.visible_rows = ((geo.grid.height() / geo.row_h).floor().max(1.0)) as usize;
        geo.strip = Rect::new(
            self.rect.x1 - scroll_gutter - strip_w - s(sp.s1) / 2.0,
            geo.grid.y0,
            self.rect.x1 - scroll_gutter - s(sp.s1) / 2.0 - strip_w,
            geo.grid.y1,
        );
        geo.scroll = Rect::new(
            self.rect.x1 - scroll_gutter,
            geo.grid.y0,
            self.rect.x1,
            geo.grid.y0 + geo.visible_rows as f64 * geo.row_h,
        );
        geo
    }

    fn scroll_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            self.rows() as f64,
            self.geo.visible_rows as f64,
            self.first as f64,
        )
    }

    fn max_first(&self) -> usize {
        self.rows().saturating_sub(self.geo.visible_rows)
    }

    fn scroll_to(&mut self, first: usize) {
        let first = first.min(self.max_first());
        if self.total > 0 {
            // A window asks its app for the rows it needs: what this window holds is the app's,
            // and the next one comes as a change of it.
            let rows = i64::try_from(first).unwrap_or(i64::MAX)
                - i64::try_from(self.first).unwrap_or(i64::MAX);
            if rows != 0 {
                self.scrolled(rows);
            }
            return;
        }
        if first != self.first {
            self.first = first;
            self.dirty = true;
        }
    }

    /// Bring `row` into view, keeping the rows around it where they are when it already shows.
    fn reveal(&mut self, row: usize) {
        if self.total > 0 {
            // The app that holds a window brings the row into it; the grid paints what it holds.
            return;
        }
        if row < self.first {
            self.scroll_to(row);
        } else if row >= self.first.saturating_add(self.geo.visible_rows) {
            self.scroll_to(row.saturating_sub(self.geo.visible_rows) + 3);
        }
    }

    fn scrolled(&mut self, rows: i64) {
        if rows == 0 {
            return;
        }
        if let Some(wheel) = self.on_wheel.as_mut() {
            wheel(rows);
        }
    }

    fn pressed_at(&mut self, row: usize) {
        if let Some(press) = self.on_press.as_mut() {
            press(row);
        }
    }

    fn follow_scrollbar(&mut self, point: Point) {
        let (track, metrics) = (self.geo.scroll, self.scroll_metrics());
        if !metrics.overflows() {
            return;
        }
        if let Some(offset) = self.scrollbar.drag_to(track, metrics, point) {
            self.scroll_to(offset.round().max(0.0) as usize);
        }
        if self.scrollbar.update_hover(track, metrics, point) {
            self.dirty = true;
        }
    }

    fn press_scrollbar(&mut self, point: Point) -> bool {
        let (track, metrics) = (self.geo.scroll, self.scroll_metrics());
        if !metrics.overflows() || !track.contains(point) {
            return false;
        }
        match self.scrollbar.press(track, metrics, point) {
            PressOutcome::Miss => return false,
            PressOutcome::Thumb => {}
            PressOutcome::Page(offset) => self.scroll_to(offset.round().max(0.0) as usize),
        }
        self.dirty = true;
        true
    }
}

impl Widget for DiffGrid {
    tilo_ui_base::described!();

    fn name(&self) -> &'static str {
        "DiffGrid"
    }

    fn test_id(&self) -> Option<&str> {
        Some("diff_grid")
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
        let rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(unused_variables, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text_renderer, theme, _env) = ctx.parts();
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let metrics = Metrics::measure(text_renderer, theme);
        self.geo = self.place(theme, metrics);
        self.first = self.first.min(self.max_first());
        let colors = &theme.tokens.colors;
        let (code, code_strong) = (
            theme.text(TextRole::Code),
            theme.text(TextRole::Code).strong(),
        );
        let g = self.geo.clone();
        for vr in 0..g.visible_rows {
            let row = self.first + vr;
            if row >= self.rows() {
                break;
            }
            let y = g.grid.y0 + vr as f64 * g.row_h;
            self.paint_row(
                scene,
                transform,
                text_renderer,
                theme,
                (&code, &code_strong),
                (row, y),
            );
        }
        // What stands under the pointer reads as one line under it.
        if self.rect.contains(self.last_cursor)
            && let Some(vr) = g.row_at(self.last_cursor.y)
        {
            let row = self.first + vr;
            if row < self.rows() {
                let y = g.grid.y0 + vr as f64 * g.row_h;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.intent.selection_tint,
                    None,
                    &Rect::new(g.grid.x0, y, g.grid.x1, y + g.row_h),
                );
            }
        }
        self.paint_strip(scene, transform, theme);
        let metrics = self.scroll_metrics();
        if metrics.overflows() {
            self.scrollbar
                .draw(scene, transform, g.scroll, metrics, theme, self.focused);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = ctx.logical(position);
                if self.rect.contains(point) {
                    self.last_cursor = point;
                    self.dirty = true;
                }
                self.follow_scrollbar(point);
            }
            WindowEvent::CursorLeft { .. } => {
                self.dirty = true;
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
                *ctx.focus_target = Some(ctx.node_id);
                if self.press_scrollbar(point) {
                    *ctx.handled = true;
                    return;
                }
                if let Some(vr) = self.geo.row_at(point.y) {
                    let row = self.first + vr;
                    if row < self.rows() {
                        self.pressed_at(row);
                        *ctx.handled = true;
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let rows = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (-*y).round() as i64 * 3,
                    MouseScrollDelta::PixelDelta(p) => {
                        -(p.y / self.geo.row_h.max(1.0)).round() as i64
                    }
                };
                if rows != 0 {
                    let first = i64::try_from(self.first).unwrap_or(i64::MAX);
                    let target = usize::try_from((first + rows).max(0)).unwrap_or(0);
                    self.scroll_to(target);
                    self.scrolled(rows);
                }
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::a11y::{AccessState, Role};
        let node = tilo_ui_base::a11y::AccessNode::new(Role::Table)
            .with_optional_name(self.label.clone())
            .with_hashed_key("diff_grid")
            .with_state(
                AccessState::new().disabled(self.on_press.is_none() && self.description.is_some()),
            );
        // Why it cannot be pressed now, if it says: read beside its name, never instead.
        Some(match &self.description {
            Some(why) => node.with_description(why.clone()),
            None => node,
        })
    }

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

impl DiffGrid {
    /// One row, at `at`: its place among the aligned rows and where it stands down the grid.
    fn paint_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        codes: (&TextStyle, &TextStyle),
        at: (usize, f64),
    ) {
        let (row, y) = at;
        let g = &self.geo;
        let colors = &theme.tokens.colors;
        let kind = self.kind_at(row);
        let text_y = y + (g.row_h - g.line_h) / 2.0;
        let radius = f64::from(theme.tokens.radii.detail);
        if let Some(tone) = kind.colour(theme) {
            scene.fill(
                Fill::NonZero,
                transform,
                with_alpha_scaled(tone, 0.13),
                None,
                &Rect::new(g.grid.x0, y, g.grid.x1, y + g.row_h),
            );
            // A mark on the row's leading edge, in the same tone, so a change reads even where
            // the wash is faint.
            scene.fill(
                Fill::NonZero,
                transform,
                tone,
                None,
                &RoundedRect::from_rect(
                    Rect::new(g.grid.x0, y, g.grid.x0 + 2.0, y + g.row_h),
                    radius,
                ),
            );
        }
        let (code, code_strong) = codes;
        let (number_ink, ink, strong) = if kind.is_change() {
            (colors.text_dim, colors.text_primary, code_strong)
        } else {
            (colors.text_tertiary, colors.text_primary, code)
        };
        let syntax = &theme.tokens.syntax;
        let width = self.geo_number_width();
        let line = |text_renderer: &mut TextRenderer,
                    scene: &mut Scene,
                    number: &str,
                    text: &str,
                    spans: Option<&LineSpans>,
                    number_x: f64,
                    text_x: f64,
                    edge: f64| {
            if !number.is_empty() {
                let padded = format!("{number:>width$}");
                text_renderer.draw_style(
                    scene,
                    &padded,
                    number_ink,
                    transform * Affine::translate(Vec2::new(number_x, text_y)),
                    None,
                    code,
                );
            }
            if text.is_empty() {
                return;
            }
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a column's width on screen is far below f32's exact integer range"
            )]
            let max_width = (edge - text_x).max(0.0) as f32;
            // Each run the language colours is drawn in its role's ink; the gaps between them
            // read in the row's own.
            let mut runs: Vec<(usize, usize, Color)> = Vec::new();
            match spans {
                Some(line_spans) if !line_spans.spans.is_empty() => {
                    let mut at = 0usize;
                    for span in &line_spans.spans {
                        let start = span.range.start.min(text.len());
                        let end = span.range.end.min(text.len());
                        if start > at {
                            runs.push((at, start, ink));
                        }
                        if end > start {
                            runs.push((start, end, syntax.color(span.kind)));
                        }
                        at = at.max(end);
                    }
                    if at < text.len() {
                        runs.push((at, text.len(), ink));
                    }
                }
                _ => runs.push((0, text.len(), ink)),
            }
            let mut x = text_x;
            for (start, end, colour) in runs {
                let Some(run) = text.get(start..end) else {
                    continue;
                };
                if run.is_empty() {
                    continue;
                }
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "a column's width on screen is far below f32's exact integer range"
                )]
                let room = f64::from(max_width).min((edge - x).max(0.0)) as f32;
                text_renderer.draw_style(
                    scene,
                    run,
                    colour,
                    transform * Affine::translate(Vec2::new(x, text_y)),
                    Some(room),
                    strong,
                );
                let advance = text_renderer.measure_style(run, None, strong).0;
                x += f64::from(advance);
            }
        };
        // One side alone takes the left column: its own lines, numbered as its file has them.
        let shown = if g.both || self.side != DiffSide::Right {
            DiffSide::Left
        } else {
            DiffSide::Right
        };
        let hidden = if shown == DiffSide::Right {
            DiffSide::Left
        } else {
            DiffSide::Right
        };
        let spans = if shown == DiffSide::Right {
            self.right_spans.get(row)
        } else {
            self.left_spans.get(row)
        };
        line(
            text_renderer,
            scene,
            self.number(shown, row),
            self.line(shown, row),
            spans,
            g.left_gutter_x0,
            g.left_x0,
            g.left_x1,
        );
        if g.both {
            line(
                text_renderer,
                scene,
                self.number(DiffSide::Right, row),
                self.line(DiffSide::Right, row),
                self.right_spans.get(row),
                g.right_gutter_x0,
                g.right_x0,
                g.right_x1,
            );
        } else if let Some(words) = &self.gap_words
            && self.line(shown, row).is_empty()
            && !self.line(hidden, row).is_empty()
        {
            // The row is the other side's line: say so, in the quiet ink of a note.
            text_renderer.draw_style(
                scene,
                words,
                colors.text_tertiary,
                transform * Affine::translate(Vec2::new(g.left_x0, text_y)),
                None,
                code,
            );
        }
    }

    /// The glyphs a line number takes: the widest number the grid holds.
    fn geo_number_width(&self) -> usize {
        self.left_nos
            .iter()
            .chain(self.right_nos.iter())
            .map(String::len)
            .max()
            .unwrap_or(1)
            .clamp(1, 7)
    }

    /// The strip down the side: where each change stands in the text, so a person sees how the
    /// file's changes are spread before scrolling to them.
    fn paint_strip(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let g = &self.geo;
        let rows = self.rows();
        if rows == 0 || g.strip.height() <= 0.0 {
            return;
        }
        let height = g.strip.height();
        let mut row = 0usize;
        while row < rows {
            let kind = self.kind_at(row);
            if !kind.is_change() {
                row += 1;
                continue;
            }
            let first = row;
            while row < rows && self.kind_at(row).is_change() {
                row += 1;
            }
            let Some(tone) = kind.colour(theme) else {
                continue;
            };
            let y0 = g.strip.y0 + first as f64 / rows as f64 * height;
            let y1 = (g.strip.y0 + row as f64 / rows as f64 * height).max(y0 + 2.0);
            scene.fill(
                Fill::NonZero,
                transform,
                with_alpha_scaled(tone, 0.85),
                None,
                &RoundedRect::from_rect(Rect::new(g.strip.x0, y0, g.strip.x1, y1), 1.5),
            );
        }
        if let Some(at) = self.at.and_then(|at| usize::try_from(at).ok()) {
            let y = g.strip.y0 + at.min(rows) as f64 / rows as f64 * height;
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.intent.accent,
                None,
                &Rect::new(g.strip.x0 - 1.0, y, g.strip.x1 + 1.0, y + 2.0),
            );
        }
    }
}
