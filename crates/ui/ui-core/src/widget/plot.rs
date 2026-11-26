//! The one widget shell every chart kind shares: layout, title, hover, keyboard and touch
//! inspection, activation, the legend toggles and the accessibility tree.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};

use crate::widget::chart_legend::SeriesToggles;
use crate::widget::chart_navigation::{
    ChartInspection, InteractiveChart, activated_key, chart_nav_action, handle_chart_event,
};
use crate::widget::chart_readout::Readout;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::{widget::ModelPath, widget::Widget};

/// Where a chart kind's marks sit and what is inspected: the shell lays the
/// frame out and drives its inspection; the marks read both.
#[derive(Clone, Debug)]
pub struct PlotFrame<H> {
    pub(crate) rect: Rect,
    pub(crate) title: String,
    /// What it says while it has nothing to show, and why; empty for the shared note.
    pub(crate) empty: String,
    /// A fixed height; `None` takes the height the parent gives, measuring
    /// [`PlotMarks::natural_height`] when it gives none.
    pub(crate) height: Option<f32>,
    /// The smallest height [`Plot::with_height`] accepts.
    pub(crate) min_height: f32,
    pub(crate) inspection: ChartInspection<H>,
    /// What the chart keeps between its box and its marks, measured as it is placed.
    pub(crate) margins: Margins,
}

impl<H> PlotFrame<H> {
    pub(crate) fn fill(min_height: f32) -> Self {
        Self {
            rect: Rect::ZERO,
            title: String::new(),
            empty: String::new(),
            height: None,
            min_height,
            inspection: ChartInspection::default(),
            margins: Margins::default(),
        }
    }
    pub(crate) fn new(height: f32, min_height: f32) -> Self {
        Self {
            rect: Rect::ZERO,
            title: String::new(),
            empty: String::new(),
            height: Some(height),
            min_height,
            inspection: ChartInspection::default(),
            margins: Margins::default(),
        }
    }
}

impl<H> PlotFrame<H> {
    /// The box the marks take: the frame inside its margins.
    pub(crate) fn inside(&self) -> Rect {
        self.margins.inside(self.rect)
    }

    /// Where a legend's rows start: across the frame, under its title.
    pub(crate) fn legend_area(&self) -> Rect {
        let y = self.rect.y0 + self.margins.legend;
        Rect::new(self.rect.x0, y, self.rect.x1, y)
    }

    /// How many rows the legend naming `names` takes across the frame.
    pub(crate) fn legend_rows<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> usize {
        crate::widget::chart_legend::rows(names, self.rect.x0, self.rect.x1)
    }
}

/// What every chart keeps between its box and its marks, in the theme's type and spacing: its
/// title and then its legend over the marks, and the words of its axes beside and under them.
/// The marks start on the box's edge wherever nothing stands beside them, so a chart lines up
/// with the words over and under it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Margins {
    /// Where the legend's rows start, under the title.
    pub(crate) legend: f64,
    pub(crate) top: f64,
    pub(crate) left: f64,
    pub(crate) right: f64,
    pub(crate) bottom: f64,
}

impl Margins {
    /// The title's band over the marks, then `legend_rows` rows of legend, each band a related
    /// gap over what follows it.
    pub(crate) fn header(theme: &Theme, title: &str, legend_rows: usize) -> Self {
        let legend = crate::widget::chart_navigation::title_band(theme, title);
        let rows = if legend_rows == 0 {
            0.0
        } else {
            tilo_ui_base::num::f64_of_usize(legend_rows) * crate::widget::chart_legend::ROW_HEIGHT
                + f64::from(theme.tokens.spacing.rhythm.related)
        };
        Self {
            legend,
            top: legend + rows,
            ..Self::default()
        }
    }

    /// The margins of a value axis whose widest label is `widest` wide: the label and a step before
    /// the marks, and half a label's line over the top rule and under the bottom one, so the
    /// end labels stand whole.
    pub(crate) fn value_axis(mut self, theme: &Theme, widest: f64) -> Self {
        let half = caption_line(theme) / 2.0;
        self.left += widest + step(theme);
        self.top += half;
        self.bottom = self.bottom.max(half);
        self
    }

    /// The margin `lines` lines of labels take under the marks, a step below them.
    pub(crate) fn labels_under(mut self, theme: &Theme, lines: usize) -> Self {
        if lines > 0 {
            let under = step(theme)
                + tilo_ui_base::num::f64_of_usize(lines) * caption_line(theme)
                + tilo_ui_base::num::f64_of_usize(lines - 1) * f64::from(theme.tokens.spacing.s1);
            self.bottom = self.bottom.max(under);
        }
        self
    }

    /// The box inside `rect` these margins leave, never turned inside out.
    pub(crate) fn inside(&self, rect: Rect) -> Rect {
        let x0 = (rect.x0 + self.left).min(rect.x1);
        let y0 = (rect.y0 + self.top).min(rect.y1);
        Rect::new(
            x0,
            y0,
            (rect.x1 - self.right).max(x0),
            (rect.y1 - self.bottom).max(y0),
        )
    }
}

/// The line a chart's small words take: its axis labels, ticks and captions.
pub(crate) fn caption_line(theme: &Theme) -> f64 {
    f64::from(
        theme
            .text(tilo_ui_base::tokens::TextRole::Caption)
            .line_height_px(),
    )
}

/// The step between a chart's marks and the words beside them.
pub(crate) fn step(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s2)
}

/// The widest of `labels` in the chart's small words.
pub(crate) fn widest<S: AsRef<str>>(
    tr: &mut TextRenderer,
    theme: &Theme,
    labels: impl IntoIterator<Item = S>,
) -> f64 {
    let style = theme.text(tilo_ui_base::tokens::TextRole::Caption);
    labels
        .into_iter()
        .map(|label| f64::from(tr.measure_style(label.as_ref(), None, &style).0))
        .fold(0.0, f64::max)
}

pub struct PlotItem<A> {
    pub(crate) target: String,
    pub(crate) name: String,
    pub(crate) description: String,
    /// What activating the item reports; `None` makes it static text.
    pub(crate) activates: Option<A>,
}

impl<A> PlotItem<A> {
    pub(crate) fn new(
        target: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        activates: A,
    ) -> Self {
        Self {
            target: target.into(),
            name: name.into(),
            description: description.into(),
            activates: Some(activates),
        }
    }
}

/// What a chart kind supplies to [`Plot`].
pub trait PlotMarks: 'static {
    type Hover: Clone + PartialEq + std::hash::Hash;
    /// What activating a mark reports through [`Plot::take_activated`].
    type Activated: Clone;
    const NAME: &'static str;
    const LABEL: &'static str;

    fn frame(&self) -> &PlotFrame<Self::Hover>;
    fn frame_mut(&mut self) -> &mut PlotFrame<Self::Hover>;
    fn hit(&self, point: Point) -> Option<Self::Hover>;
    /// The marks the keyboard steps through, in order.
    fn keyboard_items(&self) -> Vec<Self::Hover>;
    /// Where inspecting `item` puts the pointer; `None` leaves it.
    fn anchor(&self, item: &Self::Hover) -> Option<Point>;
    /// What activating `item` reports; `None` when it is not activatable.
    fn activation(&self, item: &Self::Hover) -> Option<Self::Activated>;
    fn items(&self) -> Vec<PlotItem<Self::Activated>>;
    fn label(&self) -> &'static str {
        Self::LABEL
    }
    /// The series that can be shown and hidden, as `(id, name)`.
    fn series(&self) -> Vec<(&str, &str)> {
        Vec::new()
    }
    fn legend_interactive(&self) -> bool {
        false
    }
    fn toggles(&self) -> Option<&SeriesToggles> {
        None
    }
    fn toggles_mut(&mut self) -> Option<&mut SeriesToggles> {
        None
    }
    fn series_of(&self, _item: &Self::Hover) -> Option<&str> {
        None
    }
    fn is_animating(&self) -> bool {
        false
    }
    /// What the chart reads at the inspected mark: painted over the marks in a window and composed
    /// as the page's own elements on the web. `None` while nothing is inspected.
    fn readout(&self, _theme: &Theme) -> Option<Readout> {
        None
    }
    /// What of the inspection the marks show themselves (a slice lifted out of its pie, a mark
    /// outlined), so the web's picture of them is drawn again when it changes: the inspected mark
    /// unless the readout's own highlights are all that shows it.
    fn shown_inspection(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.frame().inspection.hovered.hash(&mut hash);
        hash.finish()
    }
    /// The height the chart lays out at; `None` fills the parent.
    fn layout_height(&self) -> Option<f32> {
        self.frame().height
    }
    /// Whether the chart stands at its natural height rather than filling the parent's, when
    /// no height is set: a chart of rows, as tall as its rows are.
    fn keeps_natural_height(&self) -> bool {
        false
    }
    /// The height a filling chart wants at `width` when the parent leaves
    /// it open.
    fn natural_height(&self, _tr: &mut TextRenderer, _theme: &Theme, _width: f64) -> f64 {
        320.0
    }
    /// Places what the marks read before they paint: the plot's own rect, a legend's entries,
    /// each measured in `theme`'s own type.
    fn place(&mut self, _tr: &mut TextRenderer, _theme: &Theme) {}
    /// Paints the marks. Call [`Self::follow_keyboard_anchor`] once the
    /// geometry is current and before painting the inspector.
    fn paint(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme);
    /// Takes the events the shell's own inspection cannot drive — zoom,
    /// pan, brush, focus loss — before the shell inspects. Whether the
    /// event was consumed.
    fn event(
        &mut self,
        _event: &WindowEvent,
        _ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) -> bool {
        false
    }
    /// Whether a gesture is in flight and must keep receiving the pointer.
    fn wants_pointer_capture(&self) -> bool {
        false
    }
    fn visible_changed(&mut self) {}
    /// Keeps the pointer on a mark the keyboard chose while the geometry
    /// changes under it, so its inspector shows there.
    fn follow_keyboard_anchor(&mut self) {
        let inspection = &self.frame().inspection;
        if !inspection.keyboard {
            return;
        }
        if let Some(anchor) = inspection
            .hovered
            .as_ref()
            .and_then(|item| self.anchor(item))
        {
            self.frame_mut().inspection.pointer = anchor;
        }
    }
}

/// How tall a chart with nothing to show stands: its title, and its note with a line of room above
/// and below it, so an empty chart is no blank box as tall as its marks would be.
fn nothing_to_show_height<H>(frame: &PlotFrame<H>, theme: &Theme) -> f64 {
    let line = f64::from(
        theme
            .text(tilo_ui_base::tokens::TextRole::Body)
            .line_height_px(),
    );
    crate::widget::chart_navigation::title_band(theme, &frame.title) + 3.0 * line
}

/// A chart with nothing to show says so where its marks would stand, under its title: axes, rings
/// or a disc around no marks read as a chart of zeros.
fn paint_nothing_to_show<H>(
    frame: &PlotFrame<H>,
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
) {
    crate::widget::chart_navigation::draw_title(
        scene,
        transform,
        tr,
        theme,
        frame.rect,
        &frame.title,
    );
    let below = crate::widget::chart_navigation::title_band(theme, &frame.title);
    let room = Rect::new(
        frame.rect.x0,
        frame.rect.y0 + below,
        frame.rect.x1,
        frame.rect.y1.max(frame.rect.y0 + below),
    );
    let message = if frame.empty.is_empty() {
        tilo_ui_base::i18n::widget_format("widget-chart-nothing", &[])
    } else {
        frame.empty.clone()
    };
    let style = theme.text(tilo_ui_base::tokens::TextRole::Body);
    let width = tilo_ui_base::num::f32_of_f64(room.width().max(0.0));
    let (w, h) = tr.measure_style(&message, Some(width), &style);
    let at = Point::new(
        room.center().x - f64::from(w) / 2.0,
        room.center().y - f64::from(h) / 2.0,
    );
    tr.draw_style(
        scene,
        &message,
        theme.tokens.colors.text_dim,
        transform * Affine::translate((at.x, at.y)),
        Some(width),
        &style,
    );
}

/// The one chart widget; see the [module docs](self).
#[tilo_ui_base::component::not_a_component(
    part,
    "the body every chart shares; each chart is described under its own name"
)]
pub struct Plot<M: PlotMarks> {
    pub(crate) marks: M,
    activated: Option<M::Activated>,
    focused: bool,
    /// The theme the frame's marks are placed in: the layout is handed none,
    /// and a filling chart measures its height in it too.
    theme: Theme,

    /// Input or a new document value changed what this widget paints: it
    /// repaints once, and rests after the frame draws it.
    dirty: bool,
    /// What the marks place may have moved: input or a data change that the
    /// layout re-places before the next frame is read or drawn.
    layout_dirty: bool,
}

impl<M: PlotMarks> Plot<M> {
    pub(crate) fn from_marks(marks: M) -> Self {
        Self {
            dirty: true,
            marks,
            activated: None,
            focused: false,
            theme: Theme::default(),
            layout_dirty: true,
        }
    }
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.marks.frame_mut().title = title.into();
        self
    }
    /// What it says while it has nothing to show, and why; without it, the shared note.
    #[must_use]
    pub fn with_empty(mut self, note: impl Into<String>) -> Self {
        self.marks.frame_mut().empty = note.into();
        self
    }
    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        let frame = self.marks.frame_mut();
        frame.height = Some(height.max(frame.min_height));
        self
    }
    pub fn take_activated(&mut self) -> Option<M::Activated> {
        self.activated.take()
    }
    /// Repaints once on the next frame: for a mark that changed its data,
    /// whose places the layout works out again.
    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
        self.layout_dirty = true;
    }
    pub fn clear_inspection(&mut self) {
        self.marks.frame_mut().inspection.clear();
    }
    /// Inspects `item` as the keyboard would; whether one is inspected.
    pub(crate) fn inspect_item(&mut self, item: Option<M::Hover>) -> bool {
        let inspected = item.is_some();
        let inspection = &mut self.marks.frame_mut().inspection;
        inspection.hovered = item;
        inspection.keyboard = inspected;
        self.anchor_hovered();
        inspected
    }
    pub fn is_series_visible(&self, id: &str) -> bool {
        self.marks.series().iter().any(|(series, _)| *series == id)
            && !self
                .marks
                .toggles()
                .is_some_and(|toggles| toggles.is_hidden(id))
    }
    /// Shows or hides series `id`; whether that changed anything. Hiding
    /// the inspected mark's series ends the inspection.
    pub fn set_series_visible(&mut self, id: &str, visible: bool) -> bool {
        self.mark_dirty();
        if !self.marks.series().iter().any(|(series, _)| *series == id) {
            return false;
        }
        let Some(toggles) = self.marks.toggles_mut() else {
            return false;
        };
        if !toggles.set(id, visible) {
            return false;
        }
        let hovered_hidden = self
            .marks
            .frame()
            .inspection
            .hovered
            .as_ref()
            .and_then(|item| self.marks.series_of(item))
            .is_some_and(|series| series == id);
        if hovered_hidden {
            self.marks.frame_mut().inspection.hovered = None;
        }
        self.marks.visible_changed();
        true
    }
    pub fn take_visibility_changed(&mut self) -> Option<(String, bool)> {
        self.marks.toggles_mut()?.take_changed()
    }
    pub fn toggle_series(&mut self, id: &str) -> bool {
        let visible = self.is_series_visible(id);
        self.set_series_visible(id, !visible)
    }
    /// The series the person is on: the one whose legend entry the pointer is over, which the
    /// chart brings forward, else the one of the inspected mark, whose entry it names.
    fn refresh_focus(&mut self) {
        let inspection = &self.marks.frame().inspection;
        let on_legend = (!inspection.keyboard && self.marks.legend_interactive())
            .then(|| {
                self.marks
                    .toggles()
                    .and_then(|toggles| toggles.hit(inspection.pointer))
            })
            .flatten();
        let focus = on_legend.map(|id| (id, true)).or_else(|| {
            inspection
                .hovered
                .as_ref()
                .and_then(|item| self.marks.series_of(item))
                .map(|id| (id.to_owned(), false))
        });
        if let Some(toggles) = self.marks.toggles_mut() {
            toggles.set_focus(focus);
        }
    }
}

impl<M: PlotMarks> InteractiveChart for Plot<M> {
    type Hover = M::Hover;
    fn inspection(&mut self) -> &mut ChartInspection<M::Hover> {
        &mut self.marks.frame_mut().inspection
    }
    fn keyboard_items(&self) -> Vec<M::Hover> {
        self.marks.keyboard_items()
    }
    fn anchor(&self, item: &M::Hover) -> Option<Point> {
        self.marks.anchor(item)
    }
    fn update_hover(&mut self) {
        let hovered = self.marks.hit(self.marks.frame().inspection.pointer);
        self.marks.frame_mut().inspection.hovered = hovered;
    }
    fn clear_hover(&mut self) {
        self.marks.frame_mut().inspection.hovered = None;
        if let Some(toggles) = self.marks.toggles_mut() {
            toggles.set_focus(None);
        }
    }
    fn activate_hovered(&mut self) -> bool {
        let Some(activated) = self
            .marks
            .frame()
            .inspection
            .hovered
            .as_ref()
            .and_then(|item| self.marks.activation(item))
        else {
            return false;
        };
        self.activated = Some(activated);
        true
    }
    fn toggle_legend_at(&mut self, point: Point) -> bool {
        if !self.marks.legend_interactive() {
            return false;
        }
        let Some(id) = self.marks.toggles().and_then(|toggles| toggles.hit(point)) else {
            return false;
        };
        let _ = self.toggle_series(&id);
        true
    }
}

impl<M: PlotMarks> Widget for Plot<M> {
    tilo_ui_base::described!();

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.theme = theme.clone();
        // The height is measured ([`Widget::measure`]), never fixed here: a chart with nothing to
        // show stands only as tall as it takes to say so, and grows back when readings come.
        let fills = self.marks.layout_height().is_none() && !self.marks.keeps_natural_height();
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            flex_grow: if fills { 1.0 } else { 0.0 },
            ..Default::default()
        };
        tilo_ui_base::widget::TaffyTreeExt::widget_leaf(taffy, style, node_map, self)
    }
    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(width) => width,
            _ => 320.0,
        });
        let height = if self.marks.items().is_empty() {
            tilo_ui_base::num::f32_of_f64(nothing_to_show_height(self.marks.frame(), &self.theme))
        } else {
            match self.marks.layout_height() {
                Some(height) => height,
                None => tilo_ui_base::num::f32_of_f64(self.marks.natural_height(
                    text,
                    &self.theme,
                    f64::from(width),
                )),
            }
        };
        taffy::Size {
            width,
            height: known.height.unwrap_or(height),
        }
    }
    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.marks.frame_mut().rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        let theme = self.theme.clone();
        tilo_ui_base::text_render::with_layout_renderer(&theme, |tr| self.marks.place(tr, &theme));
    }
    fn get_rect(&self) -> Rect {
        self.marks.frame().rect
    }
    fn is_layout_dirty(&self) -> bool {
        self.marks.frame().rect == Rect::ZERO || self.layout_dirty
    }
    fn layout_needs_measure(&self) -> bool {
        self.is_layout_dirty()
    }
    fn layout_measured(&mut self) {
        self.layout_dirty = false;
    }
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, tr, theme, mut env) = ctx.parts();
        tilo_ui_base::scene_parts::picture();
        if self.marks.items().is_empty() {
            paint_nothing_to_show(self.marks.frame(), scene, transform, tr, theme);
            return;
        }
        self.marks.paint(scene, transform, tr, theme);
        if let Some(readout) = self.marks.readout(theme) {
            readout.paint(scene, transform, tr, theme);
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        self.dirty = true;
        if self.marks.event(event, ctx) {
            self.layout_dirty = true;
            return;
        }
        // Keys inspect the chart only while it has focus; otherwise they
        // belong to whatever does.
        if self.focused || chart_nav_action(event).is_none() {
            handle_chart_event(self, event, ctx);
            self.refresh_focus();
        }
    }
    fn wants_pointer_capture(&self) -> bool {
        self.marks.wants_pointer_capture()
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let legend = self.marks.series();
        let toggles = self
            .marks
            .toggles()
            .filter(|_| self.marks.legend_interactive());
        let children = self
            .marks
            .items()
            .into_iter()
            .map(|item| {
                let node = AccessNode::new(if item.activates.is_some() {
                    Role::Button
                } else {
                    Role::StaticText
                })
                .with_name(item.name)
                .with_description(item.description);
                if item.activates.is_some() {
                    node.with_action_target(item.target)
                        .with_state(AccessState::new().focusable(true))
                } else {
                    // A reading keeps its identity by the target it is read at,
                    // so it is keyed as the legend's toggles beside it are: a
                    // list mixes no keyed and position-addressed siblings.
                    node.with_hashed_key(&item.target)
                }
            })
            .chain(toggles.into_iter().flat_map(|toggles| {
                legend
                    .iter()
                    .map(|(id, name)| toggles.access_node(id, name))
            }))
            .collect();
        let title = &self.marks.frame().title;
        Some(
            AccessNode::new(Role::Canvas)
                .with_name(if title.is_empty() {
                    self.marks.label()
                } else {
                    title
                })
                .with_children(children),
        )
    }
    fn perform_action(
        &mut self,
        action: &tilo_ui_base::a11y::Action,
        key_path: &[u64],
        _state: &mut tilo_ui_base::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        let Some(key) = activated_key(action, key_path) else {
            return false;
        };
        let legend_hit = self
            .marks
            .legend_interactive()
            .then(|| {
                SeriesToggles::activated(self.marks.series().into_iter().map(|(id, _)| id), key)
            })
            .flatten();
        if let Some(id) = legend_hit {
            return self.toggle_series(&id);
        }
        let Some(activated) = self
            .marks
            .items()
            .into_iter()
            .find(|item| tilo_ui_base::a11y::hash_key(&item.target) == key)
            .and_then(|item| item.activates)
        else {
            return false;
        };
        self.activated = Some(activated);
        true
    }

    fn needs_redraw(&self) -> bool {
        tilo_ui_base::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        tilo_ui_base::widget::Widget::mark_children_drawn(self);
    }
    fn name(&self) -> &'static str {
        M::NAME
    }
    fn needs_animation_frame(&self) -> bool {
        self.marks.is_animating()
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.dirty = true;
        self.focused = focused;
        if !focused && self.marks.frame().inspection.keyboard {
            self.clear_inspection();
        }
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
}

/// The usual item of a valued mark: its id as target and activation, its
/// label as name, and its detail — or its value — as description.
pub(crate) fn value_item(id: &str, label: &str, detail: &str, value: f64) -> PlotItem<String> {
    PlotItem::new(
        id,
        label,
        if detail.is_empty() {
            format!("Value {value}")
        } else {
            detail.to_owned()
        },
        id.to_owned(),
    )
}

/// How many ticks at most a value axis labels.
pub(crate) const AXIS_TICKS: usize = 5;

/// A value axis down the left of `plot`: a label per nice tick of `range`
/// (bottom to top), named by `label` from the tick's value and the step between
/// ticks, with a grid line across when `grid`.
pub(crate) struct ValueAxis<F> {
    pub(crate) plot: Rect,
    pub(crate) range: (f64, f64),
    pub(crate) grid: bool,
    pub(crate) label: F,
}

/// The step between nice `ticks`: what a label's precision follows, so ticks a whole number
/// apart read `0, 2, 4` and ticks a quarter apart `0.25, 0.5`.
pub(crate) fn tick_step(ticks: &[f64]) -> f64 {
    match ticks {
        [first, second, ..] => second - first,
        _ => 1.0,
    }
}

impl<F: Fn(f64, f64) -> String> ValueAxis<F> {
    /// The widest label the axis over `range` writes.
    pub(crate) fn widest(tr: &mut TextRenderer, theme: &Theme, range: (f64, f64), label: F) -> f64 {
        let ticks = crate::charts::nice_ticks(range.0, range.1, AXIS_TICKS);
        let step = tick_step(&ticks);
        widest(tr, theme, ticks.into_iter().map(|value| label(value, step)))
    }

    pub(crate) fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        use tilo_ui_base::tokens::TextRole;
        use vello::kurbo::{Line, Stroke};
        let (plot, (min, max)) = (self.plot, self.range);
        let ticks = crate::charts::nice_ticks(min, max, AXIS_TICKS);
        let tick_step = tick_step(&ticks);
        for value in ticks {
            let fraction = if max > min {
                (value - min) / (max - min)
            } else {
                0.0
            };
            let y = plot.y1 - fraction * plot.height();
            if self.grid {
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    theme.tokens.colors.border_standard,
                    None,
                    &Line::new((plot.x0, y), (plot.x1, y)),
                );
            }
            let label = (self.label)(value, tick_step);
            // Each label ends a step before the plot, its line centred on its rule.
            let (width, height) = tr.measure_style(&label, None, &theme.text(TextRole::Caption));
            let step = f64::from(theme.tokens.spacing.s2);
            tr.draw_style(
                scene,
                &label,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((
                        plot.x0 - f64::from(width) - step,
                        y - f64::from(height) / 2.0,
                    )),
                None,
                &theme.text(TextRole::Caption),
            );
        }
    }
}
