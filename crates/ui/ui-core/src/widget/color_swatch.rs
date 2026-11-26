//! Colour swatches: one named swatch type, one painter, one picker.

use crate::widget::ModelPath;
use std::collections::HashMap;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
#[path = "color_swatch_component.rs"]
mod component;
use crate::widget::{TaffyTreeExt, Widget};
pub use component::ColorSwatchPickerProps;

#[derive(Clone, Debug, PartialEq)]
pub struct ColorSwatch {
    pub color: Color,
    pub name: Option<String>,
}

impl ColorSwatch {
    #[must_use]
    pub fn new(color: Color) -> Self {
        Self { color, name: None }
    }

    #[must_use]
    pub fn named(color: Color, name: impl Into<String>) -> Self {
        Self {
            color,
            name: Some(name.into()),
        }
    }

    /// What a reader hears for it: its name, or the colour it is in words ("Blue"): a hex
    /// string is a value, not a name.
    pub fn spoken_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| colour_name(self.color))
    }
}

/// The colour in words: the family a person names it by, or black, white or grey.
fn colour_name(color: Color) -> String {
    let (hue, saturation, value) = crate::color::rgb_to_hsv(color);
    crate::i18n::widget_text(colour_name_key(hue, saturation, value))
}

/// The word for a colour with this hue (degrees), saturation and value.
fn colour_name_key(hue: f32, saturation: f32, value: f32) -> &'static str {
    if value <= 0.10 {
        return "widget-colour-black";
    }
    if saturation <= 0.12 {
        return if value >= 0.92 {
            "widget-colour-white"
        } else {
            "widget-colour-grey"
        };
    }
    if value <= 0.50 && (15.0..55.0).contains(&hue) {
        return "widget-colour-brown";
    }
    match hue {
        h if h < 15.0 => "widget-colour-red",
        h if h < 45.0 => "widget-colour-orange",
        h if h < 70.0 => "widget-colour-yellow",
        h if h < 165.0 => "widget-colour-green",
        h if h < 195.0 => "widget-colour-teal",
        h if h < 255.0 => "widget-colour-blue",
        h if h < 290.0 => "widget-colour-violet",
        h if h < 330.0 => "widget-colour-magenta",
        h if h < 345.0 => "widget-colour-pink",
        _ => "widget-colour-red",
    }
}

pub struct ColorSwatchFace {
    rect: Rect,
    target_rect: Rect,
    color: Color,
    name: String,
    marker: Option<String>,
    none: bool,
    selected: bool,
    hovered: bool,
    focused: bool,
}

impl ColorSwatchFace {
    pub fn new(rect: Rect, color: Color, name: impl Into<String>) -> Self {
        Self {
            rect,
            target_rect: rect,
            color,
            name: name.into(),
            marker: None,
            none: false,
            selected: false,
            hovered: false,
            focused: false,
        }
    }

    /// The choice of no colour: an empty swatch on the panel's ground,
    /// crossed corner to corner, so it reads as "none" next to any colour,
    /// whatever it stands for (a slide, a page, a shape's fill).
    pub fn none(rect: Rect, name: impl Into<String>) -> Self {
        Self {
            none: true,
            ..Self::new(rect, Color::TRANSPARENT, name)
        }
    }

    #[must_use]
    pub fn is_none(&self) -> bool {
        self.none
    }

    pub fn of(rect: Rect, swatch: &ColorSwatch) -> Self {
        Self::new(rect, swatch.color, swatch.spoken_name())
    }

    #[must_use]
    pub fn marker(mut self, marker: impl Into<String>) -> Self {
        self.marker = Some(marker.into());
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    #[must_use]
    pub fn hovered(mut self, hovered: bool) -> Self {
        self.hovered = hovered;
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[must_use]
    pub fn target_rect(mut self, target_rect: Rect) -> Self {
        self.target_rect = target_rect;
        self
    }

    pub fn hit_test(&self, point: Point) -> bool {
        self.target_rect.contains(point)
    }

    pub fn tooltip_target(&self, point: Point) -> Option<(Rect, String)> {
        self.hit_test(point).then(|| (self.rect, self.name.clone()))
    }

    pub fn accessibility(&self, action_target: impl Into<String>) -> AccessNode {
        AccessNode::new(Role::RadioButton)
            .with_name(self.name.clone())
            .with_action_target(action_target)
            .with_bounds(self.target_rect)
            .with_state(AccessState::new().focusable(true).checked(self.selected))
    }

    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.radii.item);
        let ground = colors.surfaces.content;
        if self.none {
            self.paint_none(scene, transform, theme);
            self.paint_mark(scene, transform, theme);
            return;
        }
        // The chosen swatch keeps its full fill; its mark is a ring outside
        // it (see `paint_mark`), so choosing never shrinks the colour.
        let fill_rect = self.rect;
        let fill_radius = radius;
        let shape = RoundedRect::from_rect(fill_rect, fill_radius);
        scene.fill(Fill::NonZero, transform, self.color, None, &shape);
        // The fill's edge is derived from the fill and the ground it stands
        // on, so a white swatch on paper or a black one on a dark panel keeps
        // its outline and a saturated one does not take a heavy one.
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            swatch_edge(theme, self.color, ground),
            None,
            &crate::surface::edge_outline(fill_rect, fill_radius),
        );
        self.paint_mark(scene, transform, theme);
        if let Some(marker) = &self.marker {
            let style = theme.text(TextRole::Caption).strong();
            let ink = colors.ink_on_any(&[self.color], colors.text_primary, 4.5);
            let (width, height) = text.measure_style(marker, None, &style);
            text.draw_style(
                scene,
                marker,
                ink,
                transform.pre_translate(vello::kurbo::Vec2::new(
                    self.rect.center().x - f64::from(width) / 2.0,
                    self.rect.center().y - f64::from(height) / 2.0,
                )),
                None,
                &style,
            );
        }
    }

    fn paint_none(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let colors = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.radii.item);
        let ground = colors.surfaces.content;
        let ink = colors.ink_on_any(
            &[ground],
            colors.text_dim,
            crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ink,
            None,
            &crate::surface::edge_outline(self.rect, radius),
        );
        let inset = radius * (1.0 - std::f64::consts::FRAC_1_SQRT_2);
        let r = self.rect.inset(-inset);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5).with_caps(vello::kurbo::Cap::Round),
            transform,
            ink,
            None,
            &vello::kurbo::Line::new((r.x0, r.y1), (r.x1, r.y0)),
        );
    }

    /// The swatch's edge and mark over a sample the host painted in its
    /// rect instead of the flat colour: an edge derived from the colour,
    /// and the chosen, focused or hovered ring as on a flat swatch.
    pub fn paint_ring(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let radius = f64::from(theme.tokens.radii.item);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            swatch_edge(theme, self.color, theme.tokens.colors.surfaces.content),
            None,
            &crate::surface::edge_outline(self.rect, radius),
        );
        self.paint_mark(scene, transform, theme);
    }

    fn paint_mark(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let colors = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.radii.item);
        let ground = colors.surfaces.content;
        let marked = self.selected || self.focused;
        if marked {
            // Offset outside the swatch: a gap of the ground, then the ring,
            // each half a spacing step, concentric with the swatch's corner.
            let gap = f64::from(theme.tokens.spacing.s1) / 2.0;
            let ring_width = gap;
            let reach = gap + ring_width / 2.0;
            let ink = if self.focused {
                colors.intent.you
            } else {
                colors.ink_on_any(&[ground], colors.text_primary, 3.0)
            };
            scene.stroke(
                &vello::kurbo::Stroke::new(ring_width),
                transform,
                ink,
                None,
                &RoundedRect::from_rect(self.rect.inset(reach), radius + reach),
            );
        } else if self.hovered {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                transform,
                colors.ink_on_any(&[self.color], colors.text_primary, 3.0),
                None,
                &crate::surface::edge_outline(self.rect, radius),
            );
        }
    }
}

pub type SwatchPainter = dyn FnMut(&mut Scene, Affine, Rect, usize, &Theme, &mut TextRenderer);

/// Works out a picker's swatch colours from the theme: one per swatch, in order.
pub type ThemeColours = dyn FnMut(&Theme) -> Vec<Color>;

/// A swatch fill's edge on `ground`: the edge derived from the fill, made to
/// part the fill from the ground where the two are close. The derived edge
/// is a translucent hairline, so it is measured as painted over the fill.
fn swatch_edge(theme: &Theme, fill: Color, ground: Color) -> Color {
    theme.tokens.colors.ink_on_any(
        &[fill, ground],
        crate::tokens::colors::composite(crate::surface::edge_on(fill), fill),
        SWATCH_EDGE_CONTRAST,
    )
}

const SWATCH_EDGE_CONTRAST: f64 = 1.4;

pub struct ColorSwatchPicker {
    swatches: Vec<ColorSwatch>,
    /// The most swatches a row takes.
    columns: usize,
    /// The swatches a row takes in the width it was laid out in: `columns`, or fewer where
    /// that many do not fit.
    fit: usize,
    /// For each swatch, the one drawn in its place: itself, or the earlier swatch the theme
    /// paints the same colour, so no colour is offered twice.
    drawn_as: Vec<usize>,
    selected: Option<usize>,
    choice: Option<usize>,
    rect: Rect,
    swatch_rects: Vec<Rect>,
    cell: f64,
    pad: f64,
    last_cursor: Point,
    hovered: Option<usize>,
    focused: bool,
    focus_index: usize,
    /// Told each chosen index, as well as [`ColorSwatchPicker::take_choice`].
    on_change: Option<Box<dyn FnMut(usize)>>,
    action: Option<crate::widget::ActionEmitter<usize>>,
    /// The cell's width (and height, unless `cell_height`), when it is not
    /// a pointer target.
    cell_width: Option<f64>,
    cell_height: Option<f64>,
    /// Paints each swatch in place of its flat colour.
    painter: Option<Box<SwatchPainter>>,
    /// Works out the swatches' colours from the theme they are drawn in.
    theme_colours: Option<Box<ThemeColours>>,
    /// The theme `theme_colours` last worked the colours out for.
    coloured_for: Option<(crate::theme::ThemeDocument, crate::tokens::PresenceHue)>,
    /// What the palette is called ("Accent"), said before the swatch a reader is on.
    name: Option<String>,
    /// The chosen ring's place: it slides from the swatch chosen before to the one chosen now.
    ring: crate::motion::MotionValue<Rect>,
    /// The swatch `ring` was last placed on.
    ring_for: Option<usize>,

    dirty: bool,
}

impl ColorSwatchPicker {
    /// `columns` swatches per row (at least one).
    pub fn new(swatches: impl IntoIterator<Item = ColorSwatch>, columns: usize) -> Self {
        let metrics = crate::tokens::Metrics::BASE;
        Self {
            dirty: true,
            swatches: swatches.into_iter().collect(),
            columns: columns.max(1),
            fit: columns.max(1),
            drawn_as: Vec::new(),
            selected: None,
            choice: None,
            rect: Rect::ZERO,
            swatch_rects: Vec::new(),
            cell: f64::from(metrics.min_pointer_target),
            pad: f64::from(crate::tokens::Spacing::BASE.s1),
            last_cursor: Point::ZERO,
            hovered: None,
            focused: false,
            focus_index: 0,
            on_change: None,
            action: None,
            cell_width: None,
            cell_height: None,
            painter: None,
            theme_colours: None,
            coloured_for: None,
            name: None,
            ring: crate::motion::MotionValue::new(Rect::ZERO),
            ring_for: None,
        }
    }

    /// Call the palette `name` for assistive technology ("Accent").
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.set_accessible_name(Some(name.into()));
        self
    }

    /// Where the chosen ring stands this frame, while it slides to the chosen swatch: `None`
    /// once it rests there (the chosen swatch draws its own ring) or when none is chosen.
    fn follow_ring(&mut self, motion: crate::tokens::EffectiveMotion) -> Option<Rect> {
        use crate::motion::{Direction, ListChange};
        let now = crate::motion::frame_now();
        let Some(target) = self
            .shown_selected()
            .and_then(|i| self.swatch_rects.get(i).copied())
        else {
            self.ring_for = None;
            return None;
        };
        let moved_from_another = self
            .ring_for
            .is_some_and(|before| Some(before) != self.selected)
            && self.ring.target().area() > 0.0;
        if moved_from_another {
            let timing =
                ListChange::MOVE.timing(Direction::Enter, motion, crate::motion::input_source());
            self.ring.set(target, timing, now);
            if let Some(until) = self.ring.settles_at() {
                crate::motion::want_frames_until(until);
            }
        } else if self.ring.target() != target {
            self.ring.snap(target);
        }
        self.ring_for = self.selected;
        self.ring.is_moving(now).then(|| self.ring.get(now))
    }

    /// What the palette is called now; `None` or empty leaves it unnamed.
    pub fn set_accessible_name(&mut self, name: Option<String>) {
        self.name = name.filter(|name| !name.is_empty());
    }

    /// Run `f` with each index the person chooses: a press, a key or a screen reader's.
    #[must_use]
    pub fn on_change(mut self, f: impl FnMut(usize) + 'static) -> Self {
        if self.action.is_none() {
            self.on_change = Some(Box::new(f));
        }
        self
    }

    /// Invoke `binding` with each index the person chooses, through the
    /// host like any action-bound control: the picker shows the choice until
    /// the state it changes answers.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<usize>) -> Self {
        self.action = Some(crate::widget::ActionEmitter::new(binding));
        self
    }

    /// Cells `cell` wide (and tall) rather than a pointer target, to fit a
    /// given number of columns across a panel.
    #[must_use]
    pub fn with_cell(mut self, cell: f64) -> Self {
        self.cell_width = Some(cell.max(1.0));
        self
    }

    /// Cells `height` tall rather than square, for samples wider than they
    /// are tall.
    #[must_use]
    pub fn with_cell_height(mut self, height: f64) -> Self {
        self.cell_height = Some(height.max(1.0));
        self
    }

    /// Paint each swatch with `painter` instead of its flat colour, for a
    /// choice among samples (brush strokes, patterns) rather than colours.
    /// The swatches' names still name the entries.
    #[must_use]
    pub fn with_painter(
        mut self,
        painter: impl FnMut(&mut Scene, Affine, Rect, usize, &Theme, &mut TextRenderer) + 'static,
    ) -> Self {
        self.painter = Some(Box::new(painter));
        self
    }

    /// Colour the swatches from the theme they are drawn in: `colours` answers one colour per
    /// swatch, in order, and is asked again whenever the theme or the person's colour changes.
    /// For a choice whose colours depend on the theme, such as an accent shown as it will look.
    #[must_use]
    pub fn with_theme_colours(
        mut self,
        colours: impl FnMut(&Theme) -> Vec<Color> + 'static,
    ) -> Self {
        self.theme_colours = Some(Box::new(colours));
        self.coloured_for = None;
        self
    }

    fn colour_for(&mut self, theme: &Theme) {
        let Some(colours) = self.theme_colours.as_mut() else {
            return;
        };
        let current = (theme.document.clone(), theme.tokens.colors.you);
        if self.coloured_for.as_ref() == Some(&current) {
            return;
        }
        for (swatch, colour) in self.swatches.iter_mut().zip(colours(theme)) {
            swatch.color = colour;
        }
        self.coloured_for = Some(current);
        // A theme can paint two choices alike, such as a look's own accent and the hue it is
        // made of: the later one is not offered again.
        self.drawn_as = (0..self.swatches.len())
            .map(|i| {
                let colour = self.swatches[i].color;
                (0..i)
                    .find(|j| self.swatches[*j].color == colour)
                    .unwrap_or(i)
            })
            .collect();
        self.dirty = true;
    }

    /// The swatch drawn for `index`: itself, or the earlier one painted the same colour.
    fn shown(&self, index: usize) -> usize {
        self.drawn_as.get(index).copied().unwrap_or(index)
    }

    /// The swatches drawn, in order.
    fn visible(&self) -> Vec<usize> {
        (0..self.swatches.len())
            .filter(|i| self.shown(*i) == *i)
            .collect()
    }

    /// The swatch drawn as chosen.
    fn shown_selected(&self) -> Option<usize> {
        self.selected.map(|i| self.shown(i))
    }

    pub fn set_selected(&mut self, idx: Option<usize>) {
        self.dirty = true;
        self.selected = idx.filter(|idx| *idx < self.swatches.len());
    }

    fn row_height(&self) -> f64 {
        self.cell_height.unwrap_or(self.cell)
    }

    #[must_use]
    pub fn with_selected(mut self, idx: usize) -> Self {
        self.selected = Some(idx);
        self
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.swatches.len()
    }

    pub fn take_choice(&mut self) -> Option<usize> {
        self.choice.take()
    }

    pub fn swatch(&self, idx: usize) -> Option<&ColorSwatch> {
        self.swatches.get(idx)
    }

    fn rows(&self) -> usize {
        self.visible().len().div_ceil(self.fit)
    }

    /// The swatches a row of `width` takes: at most as many whole cells as fit and at most
    /// `columns`, spread evenly over the lines that needs, so thirteen swatches wrap seven
    /// and six rather than nine and four.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a row holds a handful of on-screen cells"
    )]
    fn fit_in(&self, width: f64) -> usize {
        let fits = (((width + self.pad) / self.cell).floor().max(1.0) as usize).min(self.columns);
        let count = self.visible().len().max(1);
        count.div_ceil(count.div_ceil(fits))
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "row/column indices are bounded by the on-screen swatch \
                  grid, far below f64's exact-integer range"
    )]
    fn recompute_swatches(&mut self) {
        self.swatch_rects.clear();
        // Each swatch fills its pointer-target cell less the gap after it;
        // the first sits on the grid's (the content's) leading edge.
        let gap = self.pad;
        let row = self.row_height();
        let mut place = 0_usize;
        for i in 0..self.swatches.len() {
            if self.shown(i) != i {
                self.swatch_rects.push(Rect::ZERO);
                continue;
            }
            let r = place.checked_div(self.fit).unwrap_or(0) as f64;
            let c = place.checked_rem(self.fit).unwrap_or(0) as f64;
            let x = self.rect.x0 + c * self.cell;
            let y = self.rect.y0 + r * row;
            self.swatch_rects
                .push(Rect::new(x, y, x + self.cell - gap, y + row - gap));
            place += 1;
        }
    }

    /// `(width, height)` of the grid — shared by `build_layout` and
    /// `measure` so they can't disagree.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "column/row counts describe a small, on-screen swatch grid, \
                  far below f32's exact-integer range"
    )]
    fn grid_size(&self) -> (f32, f32) {
        let w = (self.fit as f64 * self.cell - self.pad).max(0.0);
        let h = (self.rows() as f64 * self.row_height() - self.pad).max(0.0);
        (w as f32, h as f32)
    }

    fn choose(&mut self, index: usize) {
        // A choice the person may not make keeps the one shown; it is sent to be refused with why.
        if self
            .action
            .as_ref()
            .is_none_or(crate::widget::ActionEmitter::is_available)
        {
            self.selected = Some(index);
            self.choice = Some(index);
        }
        // The person's own choice: one they may not make is sent all the same, to be refused with
        // why.
        if let Some(action) = self.action.as_mut()
            && let Err(error) = action.press(index)
        {
            tracing::warn!(%error, "swatch action refused its index");
        }
        if let Some(on_change) = &mut self.on_change {
            on_change(index);
        }
    }
}

impl crate::widget::controls::ActionControls for ColorSwatchPicker {
    crate::widget::controls::optional_emitter_controls!();

    /// Each swatch is the control its semantic target names, bound to the
    /// picker's action.
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let Some(action) = &self.action else {
            return crate::widget::controls::default_bindings(self);
        };
        (0..self.swatches.len())
            .map(|index| action.metadata(format!("swatch:{index}")))
            .collect()
    }
}

impl Widget for ColorSwatchPicker {
    /// Its size follows from what it was made with and the theme, which builds its node anew:
    /// a laid-out one asks for no solve of its own.
    fn is_layout_dirty(&self) -> bool {
        false
    }

    crate::described!();

    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.cell = self
            .cell_width
            .unwrap_or_else(|| f64::from(theme.tokens.metrics.min_pointer_target));
        self.pad = f64::from(theme.tokens.spacing.s1);
        self.colour_for(theme);
        // As wide as every column, narrowing a whole cell at a time where the row is
        // narrower: the swatches wrap onto more lines rather than overflow.
        self.fit = self.columns;
        let (widest, _) = self.grid_size();
        let style = Style {
            flex_shrink: 1.0,
            // Its own size across the row too: stretched, its bounds would take in height it does
            // not draw in.
            align_self: Some(taffy::AlignSelf::FlexStart),
            min_size: taffy::Size {
                width: taffy::Dimension::length(crate::num::f32_of_f64(self.cell - self.pad)),
                height: taffy::Dimension::auto(),
            },
            max_size: taffy::Size {
                width: taffy::Dimension::length(widest),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        _tr: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = known.width.map(f64::from).unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(width) => f64::from(width),
            taffy::AvailableSpace::MinContent => 0.0,
            taffy::AvailableSpace::MaxContent => f64::INFINITY,
        });
        self.fit = self.fit_in(width);
        let (width, height) = self.grid_size();
        taffy::Size { width, height }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.fit = self.fit_in(self.rect.width());
        self.recompute_swatches();
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.colour_for(theme);
        let sliding = self.follow_ring(theme.motion());
        let chosen = self.shown_selected();
        for (i, (swatch, rect)) in self.swatches.iter().zip(&self.swatch_rects).enumerate() {
            if self.shown(i) != i {
                continue;
            }
            let face = ColorSwatchFace::of(*rect, swatch)
                .selected(chosen == Some(i) && sliding.is_none())
                .focused(self.focused && self.focus_index == i)
                .hovered(self.hovered == Some(i));
            match self.painter.as_mut() {
                Some(painter) => {
                    painter(scene, transform, *rect, i, theme, text_renderer);
                    face.paint_ring(scene, transform, theme);
                }
                None => face.paint(scene, transform, text_renderer, theme),
            }
        }
        if let (Some(at), Some(swatch)) = (
            sliding,
            self.shown_selected().and_then(|i| self.swatches.get(i)),
        ) {
            ColorSwatchFace::of(at, swatch)
                .selected(true)
                .paint_mark(scene, transform, theme);
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.ring.is_moving(crate::motion::frame_now())
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.dirty = true;
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                self.hovered = self
                    .swatch_rects
                    .iter()
                    .position(|r| r.contains(self.last_cursor));
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(i) = self
                    .swatch_rects
                    .iter()
                    .position(|r| r.contains(self.last_cursor))
                {
                    self.choose(i);
                    self.focused = true;
                    self.focus_index = i;
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.focused && !self.swatches.is_empty() => {
                // Arrows step through the swatches drawn, not those left out.
                let visible = self.visible();
                let at = visible
                    .iter()
                    .position(|i| *i == self.shown(self.focus_index))
                    .unwrap_or(0);
                let last = visible.len().saturating_sub(1);
                let next = match logical_key {
                    Key::Named(NamedKey::ArrowLeft) => at.saturating_sub(1),
                    Key::Named(NamedKey::ArrowRight) => at.saturating_add(1).min(last),
                    Key::Named(NamedKey::ArrowUp) => at.saturating_sub(self.fit),
                    Key::Named(NamedKey::ArrowDown) => at.saturating_add(self.fit).min(last),
                    Key::Named(NamedKey::Home) => 0,
                    Key::Named(NamedKey::End) => last,
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        self.choose(self.focus_index);
                        *ctx.handled = true;
                        return;
                    }
                    _ => return,
                };
                self.focus_index = visible.get(next).copied().unwrap_or(self.focus_index);
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn needs_redraw(&self) -> bool {
        let sliding = self.ring.is_moving(crate::motion::frame_now());
        crate::widget::redraw_or_children(self.dirty || sliding, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        crate::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "ColorSwatchPicker"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Each swatch names itself on hover.
    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.swatches
            .iter()
            .zip(&self.swatch_rects)
            .find_map(|(swatch, rect)| ColorSwatchFace::of(*rect, swatch).tooltip_target(point))
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.dirty = true;
        self.focused = focused;
        if focused {
            self.focus_index = self
                .shown_selected()
                .unwrap_or(0)
                .min(self.swatches.len().saturating_sub(1));
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn accessibility(&self) -> Option<AccessNode> {
        // Each swatch is drawn (not a child widget), so it becomes a
        // synthetic `ListItem` named like its tooltip.
        let visible = self.visible();
        let count = visible.len();
        let chosen = self.shown_selected();
        let value = chosen
            .and_then(|i| self.swatches.get(i))
            .map(ColorSwatch::spoken_name);
        let items: Vec<AccessNode> = visible
            .iter()
            .enumerate()
            .map(|(place, &i)| {
                let swatch = &self.swatches[i];
                AccessNode::new(Role::ListItem)
                    .with_action_target(format!("swatch:{i}"))
                    .with_name(swatch.spoken_name())
                    // The cells are custom-painted synthetic descendants, so
                    // export their real logical-pixel rects for AT hit testing
                    // and overlay inspection just like the shared pointer path.
                    .with_bounds(self.swatch_rects.get(i).copied().unwrap_or(Rect::ZERO))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(chosen == Some(i))
                            .focused(self.focused && self.focus_index == i),
                    )
                    .with_position(place.saturating_add(1), count)
            })
            .collect();
        Some(
            AccessNode::new(Role::List)
                .with_optional_name(self.name.clone())
                .with_optional_value(value)
                .with_state(
                    AccessState::new()
                        .focusable(self.is_focusable())
                        .focused(self.is_focused()),
                )
                .with_children(items),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [place] = path else {
            return None;
        };
        let swatch = *self.visible().get(*place)?;
        self.swatch_rects.get(swatch).copied()
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        let Some(index) = target
            .strip_prefix("swatch:")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|index| *index < self.swatches.len())
        else {
            return false;
        };
        if matches!(action, crate::a11y::Action::Focus) {
            self.focused = true;
            self.focus_index = index;
            return true;
        }
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        self.choose(index);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;
    use crate::testing::dispatch;
    use automerge::AutoCommit;

    fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color::from_rgb8(r, g, b)
    }

    fn palette() -> Vec<ColorSwatch> {
        vec![
            ColorSwatch::named(rgb(0x11, 0x22, 0x33), "Ink"),
            ColorSwatch::new(rgb(0x44, 0x55, 0x66)),
            ColorSwatch::named(rgb(0x77, 0x88, 0x99), "Slate"),
        ]
    }

    fn lay_out(w: &mut dyn Widget) {
        let theme = Theme::dark();
        crate::widget::layout::lay_out_detached(
            w,
            vello::kurbo::Rect::new(0.0, 0.0, 300.0, 300.0),
            &theme,
            &mut TextRenderer::new(),
        )
        .unwrap();
    }

    use crate::synthetic_input::named_key;

    tilo_test_env::scenario! {
        /// A swatch and the picker of swatches.
        /// Each phase was a test of its own and builds its own fixture.
        fn colour_swatches_paint_name_and_choose;

        phase a_sampled_grid_tells_each_choice_and_paints_its_samples;
        {
            let chosen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let told = std::rc::Rc::clone(&chosen);
            let painted = std::rc::Rc::new(std::cell::Cell::new(0_usize));
            let counted = std::rc::Rc::clone(&painted);
            let mut picker = ColorSwatchPicker::new(palette(), 3)
                .with_cell(60.0)
                .with_cell_height(40.0)
                .with_painter(move |_, _, _, _, _, _| counted.set(counted.get() + 1))
                .on_change(move |index| told.borrow_mut().push(index));
            lay_out(&mut picker);
            // Three cells of 60 by 40, less the gap after the last: the grid
            // has no outer inset.
            let gap = f64::from(crate::tokens::Spacing::BASE.s1);
            assert_eq!(picker.get_rect().size(), (180.0 - gap, 40.0 - gap).into(),);
            let first = picker.swatch_rects[0];
            assert!(first.width() > first.height(), "cells are wider than tall");
            let third = picker.swatch_rects[2].center();

            dispatch(
                &mut picker,
                &crate::synthetic_input::cursor_moved(third.x, third.y),
            );
            dispatch(&mut picker, &crate::synthetic_input::mouse_press());
            assert_eq!(*chosen.borrow(), vec![2]);

            picker.set_selected(Some(0));
            assert_eq!(picker.selected, Some(0));
            assert_eq!(*chosen.borrow(), vec![2]);

            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            picker.draw(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut text, &Theme::dark(), PresenceCtx::default().draw_env()));
            assert_eq!(painted.get(), 3, "every cell is painted by the sampler");
        }

        phase a_none_swatch_paints_its_diagonal_and_no_fill;
        {
            let rect = Rect::new(10.0, 10.0, 40.0, 30.0);
            let painted = |face: &ColorSwatchFace| {
                let mut scene = Scene::new();
                face.paint(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut TextRenderer::new(),
                    &Theme::dark(),
                );
                let encoding = scene.encoding();
                let fills = encoding
                    .styles
                    .iter()
                    .filter(|style| {
                        style.flags_and_miter_limit & vello_encoding::Style::FLAGS_STYLE_BIT == 0
                    })
                    .count();
                (encoding.draw_tags.len(), encoding.n_paths, fills)
            };
            let none = ColorSwatchFace::none(rect, "No colour");
            assert_eq!(painted(&none), (2, 2, 0), "the edge and the diagonal");
            let ink = ColorSwatchFace::new(rect, rgb(0x11, 0x22, 0x33), "Ink");
            let (_, _, fills) = painted(&ink);
            assert_eq!(fills, 1, "a colour swatch is filled");
            assert_eq!(
                none.accessibility("none").name.as_deref(),
                Some("No colour")
            );
        }

        phase the_chosen_ring_slides_to_the_swatch_chosen_next;
        {
            let motion =
                crate::tokens::EffectiveMotion::new(crate::tokens::Motion::default(), false);
            let mut picker = ColorSwatchPicker::new(
                [rgb(0x11, 0x22, 0x33), rgb(0x33, 0x22, 0x11)].map(ColorSwatch::new),
                2,
            )
            .with_selected(0);
            let (first, second) = (
                Rect::new(0.0, 0.0, 32.0, 32.0),
                Rect::new(40.0, 0.0, 72.0, 32.0),
            );
            picker.swatch_rects = vec![first, second];
            use crate::time::testing::with_monotonic_time;
            let t0 = crate::time::now();
            let at = |ms: u64| t0 + crate::time::Duration::from_millis(ms);
            with_monotonic_time(t0, || {
                assert_eq!(picker.follow_ring(motion), None, "the first ring is placed, not slid");
                crate::motion::note_source(crate::tokens::MotionSource::Pointer);
                picker.choose(1);
                let _ = picker.follow_ring(motion);
            });
            with_monotonic_time(at(60), || {
                let at = picker.follow_ring(motion).expect("the ring slides to the next choice");
                assert!(at.x0 > first.x0 && at.x0 < second.x0, "on its way: {at:?}");
            });
            with_monotonic_time(at(800), || {
                assert_eq!(picker.follow_ring(motion), None, "it lands on the chosen swatch");
            });
        }

                phase one_face_drives_hit_help_and_checked_semantics;
        {
            let visual = Rect::new(10.0, 10.0, 30.0, 30.0);
            let target = Rect::new(6.0, 4.0, 34.0, 36.0);
            let face = ColorSwatchFace::new(visual, rgb(0xff, 0xff, 0xff), "Text color #FFFFFF")
                .selected(true)
                .target_rect(target);
            assert!(face.hit_test(Point::new(7.0, 20.0)));
            assert_eq!(
                face.tooltip_target(Point::new(7.0, 20.0)),
                Some((visual, "Text color #FFFFFF".to_owned())),
            );
            let node = face.accessibility("text-color:ffffff");
            assert_eq!(node.role, Role::RadioButton);
            assert_eq!(node.state.checked, Some(true));
            assert_eq!(node.bounds.expect("target bounds").to_rect(), target);
        }

        phase the_picker_names_each_swatch_and_chooses_by_pointer_keyboard_and_semantics;
        {
            let mut picker = ColorSwatchPicker::new(palette(), 2);
            lay_out(&mut picker);
            let tree = Widget::accessibility(&picker).expect("picker semantics");
            let names: Vec<_> = tree.children.iter().map(|c| c.name.clone()).collect();
            assert_eq!(
                names,
                [
                    Some("Ink".to_owned()),
                    Some("Blue".to_owned()),
                    Some("Slate".to_owned())
                ],
                "a nameless swatch is spoken as the colour it is"
            );
            let centre = picker.swatch_rects[2].center();
            assert_eq!(
                Widget::tooltip_target_at(&picker, centre).map(|(_, name)| name),
                Some("Slate".to_owned())
            );
            let unnamed = picker.swatch_rects[1].center();
            assert_eq!(
                Widget::tooltip_target_at(&picker, unnamed).map(|(_, name)| name),
                Some("Blue".to_owned()),
                "a nameless swatch names itself on hover too"
            );

            dispatch(
                &mut picker,
                &crate::synthetic_input::cursor_moved(centre.x, centre.y),
            );
            assert!(dispatch(
                &mut picker,
                &crate::synthetic_input::mouse_press()
            ));
            assert_eq!(picker.take_choice(), Some(2));

            Widget::set_focus(&mut picker, true);
            assert!(dispatch(&mut picker, &named_key(NamedKey::Home)));
            assert!(dispatch(&mut picker, &named_key(NamedKey::ArrowRight)));
            assert!(dispatch(&mut picker, &named_key(NamedKey::Space)));
            assert_eq!(picker.take_choice(), Some(1));

            assert!(Widget::perform_semantic_action(
                &mut picker,
                "swatch:0",
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(picker.take_choice(), Some(0));
            assert_eq!(
                picker.swatch(0).map(|s| s.color),
                Some(rgb(0x11, 0x22, 0x33))
            );
            assert!(
                Widget::accessibility(&picker).unwrap().children[0]
                    .state
                    .selected
            );
        }

        phase a_row_too_narrow_for_every_column_wraps_onto_more_lines;
        {
            let theme = Theme::dark();
            let cell = f64::from(theme.tokens.metrics.min_pointer_target);
            let swatches = (0..9).map(|_| ColorSwatch::new(Color::from_rgb8(90, 90, 90)));
            let mut picker = ColorSwatchPicker::new(swatches, 9);
            crate::widget::layout::lay_out_detached(
                &mut picker,
                vello::kurbo::Rect::new(0.0, 0.0, cell * 4.5, 400.0),
                &theme,
                &mut TextRenderer::new(),
            )
            .unwrap();
            let rows = |picker: &ColorSwatchPicker| {
                let mut tops: Vec<i64> = picker
                    .swatch_rects
                    .iter()
                    .map(|rect| rect.y0.round() as i64)
                    .collect();
                tops.dedup();
                tops.len()
            };
            assert_eq!(rows(&picker), 3, "four fit a line, so nine take three lines of three");
            assert_eq!(picker.fit, 3, "the lines are even");
            assert!(picker.rect.width() <= cell * 4.5, "the grid stays in its row");
            lay_out(&mut picker);
            assert_eq!(rows(&picker), 1, "wide enough, every swatch stands on one line");
        }

        phase a_colour_the_theme_paints_twice_is_offered_once;
        {
            let blue = Color::from_rgb8(40, 90, 220);
            let swatches = ["Own", "Red", "Blue", "Green"]
                .map(|name| ColorSwatch::named(Color::TRANSPARENT, name));
            let mut picker = ColorSwatchPicker::new(swatches, 4)
                .with_selected(2)
                .with_theme_colours(move |_| {
                    vec![blue, Color::from_rgb8(220, 40, 40), blue, Color::from_rgb8(40, 200, 40)]
                });
            lay_out(&mut picker);
            assert_eq!(picker.visible(), [0, 1, 3], "the second blue is left out");
            assert_eq!(picker.swatch_rects[2], Rect::ZERO, "and takes no cell");
            assert_eq!(
                picker.swatch_rects[3].x0 - picker.swatch_rects[1].x0,
                picker.cell,
                "the next swatch closes up"
            );
            assert_eq!(picker.shown_selected(), Some(0), "a choice of it shows on its twin");
            let node = picker.accessibility().unwrap();
            assert_eq!(node.children.len(), 3, "a reader hears each colour once");
            picker.set_focus(true);
            dispatch(&mut picker, &named_key(NamedKey::ArrowRight));
            assert_eq!(picker.focus_index, 1);
            dispatch(&mut picker, &named_key(NamedKey::ArrowRight));
            assert_eq!(picker.focus_index, 3, "the arrows step over it");
        }

        phase the_first_swatch_sits_on_the_content_edge;
        {
            let mut picker = ColorSwatchPicker::new(
                [
                    ColorSwatch::named(Color::from_rgb8(200, 40, 40), "Red"),
                    ColorSwatch::named(Color::from_rgb8(40, 200, 40), "Green"),
                ],
                2,
            );
            let theme = Theme::dark();
            let mut tree = TaffyTree::new();
            let node = picker.build_layout(&mut tree, &theme, &mut HashMap::new());
            tree.compute_layout(node, taffy::prelude::TaffyMaxContent::MAX_CONTENT)
                .unwrap();
            picker.apply_layout(&tree, node, Point::new(24.0, 10.0));
            assert_eq!(picker.swatch_rects[0].x0, 24.0);
            assert_eq!(picker.swatch_rects[0].y0, 10.0);
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            ColorSwatchFace::new(Rect::new(0.0, 0.0, 24.0, 24.0), Color::WHITE, "White")
                .selected(true)
                .paint(&mut scene, Affine::IDENTITY, &mut text, &Theme::light());
            let edge = swatch_edge(
                &Theme::light(),
                Color::WHITE,
                Theme::light().tokens.colors.surfaces.content,
            );
            assert!(
                crate::a11y::contrast_ratio(edge, Color::WHITE) >= SWATCH_EDGE_CONTRAST - 1e-6,
                "a white swatch keeps an edge on paper"
            );
        }
    }
}
