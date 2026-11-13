//! The one widget shell every chart kind shares: layout, title, hover,
//! keyboard and touch inspection, activation, the legend toggles and the
//! accessibility tree. A chart kind only supplies its marks — geometry, hit
//! testing and painting — through [`PlotMarks`]; its builder name is an
//! alias of [`Plot`] over those marks (`Heatmap` is `Plot<HeatmapMarks>`).

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::a11y::{AccessNode, AccessState, Role};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::chart_legend::SeriesToggles;
use crate::widget::chart_navigation::{
    ChartInspection, InteractiveChart, activated_key, chart_nav_action, handle_chart_event,
};
use crate::widget::{ModelPath, Widget};

/// Where a chart kind's marks sit and what is inspected: the shell lays the
/// frame out and drives its inspection; the marks read both.
#[derive(Clone, Debug)]
pub struct PlotFrame<H> {
    pub(crate) rect: Rect,
    pub(crate) title: String,
    /// A fixed height; `None` takes the room the parent gives, measuring
    /// [`PlotMarks::natural_height`] when it gives none.
    pub(crate) height: Option<f32>,
    /// The smallest height [`Plot::with_height`] accepts.
    pub(crate) min_height: f32,
    pub(crate) inspection: ChartInspection<H>,
}

impl<H> PlotFrame<H> {
    /// A frame that takes the room it is given.
    pub(crate) fn fill(min_height: f32) -> Self {
        Self {
            rect: Rect::ZERO,
            title: String::new(),
            height: None,
            min_height,
            inspection: ChartInspection::default(),
        }
    }
    pub(crate) fn new(height: f32, min_height: f32) -> Self {
        Self {
            rect: Rect::ZERO,
            title: String::new(),
            height: Some(height),
            min_height,
            inspection: ChartInspection::default(),
        }
    }
}

/// One accessible, activatable mark: a point, bar, slice, node or link.
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
    /// The inspected mark.
    type Hover: Clone + PartialEq;
    /// What activating a mark reports through [`Plot::take_activated`].
    type Activated: Clone;
    /// The widget name.
    const NAME: &'static str;
    /// The accessible name of an untitled chart.
    const LABEL: &'static str;

    fn frame(&self) -> &PlotFrame<Self::Hover>;
    fn frame_mut(&mut self) -> &mut PlotFrame<Self::Hover>;
    /// The mark under `point`.
    fn hit(&self, point: Point) -> Option<Self::Hover>;
    /// The marks the keyboard steps through, in order.
    fn keyboard_items(&self) -> Vec<Self::Hover>;
    /// Where inspecting `item` puts the pointer; `None` leaves it.
    fn anchor(&self, item: &Self::Hover) -> Option<Point>;
    /// What activating `item` reports; `None` when it is not activatable.
    fn activation(&self, item: &Self::Hover) -> Option<Self::Activated>;
    /// Every mark the accessibility tree lists.
    fn items(&self) -> Vec<PlotItem<Self::Activated>>;
    /// The accessible name, which can depend on the marks' style.
    fn label(&self) -> &'static str {
        Self::LABEL
    }
    /// The series that can be shown and hidden, as `(id, name)`.
    fn series(&self) -> Vec<(&str, &str)> {
        Vec::new()
    }
    /// Whether clicking a legend entry shows or hides its series.
    fn legend_interactive(&self) -> bool {
        false
    }
    fn toggles(&self) -> Option<&SeriesToggles> {
        None
    }
    fn toggles_mut(&mut self) -> Option<&mut SeriesToggles> {
        None
    }
    /// The series `item` belongs to, if any.
    fn series_of(&self, _item: &Self::Hover) -> Option<&str> {
        None
    }
    fn is_animating(&self) -> bool {
        false
    }
    /// The height the chart lays out at; `None` fills the parent.
    fn layout_height(&self) -> Option<f32> {
        self.frame().height
    }
    /// The height a filling chart wants at `width` when the parent leaves
    /// it open.
    fn natural_height(&self, _tr: &mut TextRenderer, _theme: &Theme, _width: f64) -> f64 {
        320.0
    }
    /// Places what the marks read before they paint: the plot's own rect, a
    /// legend's entries, each measured in `theme`'s own type. The layout runs
    /// this, so a frame that is read before it is drawn — a press, a hover,
    /// assistive technology — finds every mark where it is drawn. Defaults to
    /// nothing for a chart kind whose marks are a pure function of the frame.
    fn place(&mut self, _tr: &mut TextRenderer, _theme: &Theme) {}
    /// Paints the marks. Call [`Self::follow_keyboard_anchor`] once the
    /// geometry is current and before painting the inspector.
    fn paint(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme);
    /// Takes the events the shell's own inspection cannot drive — zoom,
    /// pan, brush, focus loss — before the shell inspects. Whether the
    /// event was consumed.
    fn event(&mut self, _event: &WindowEvent, _ctx: &mut crate::widget::EventCtx<'_>) -> bool {
        false
    }
    /// Whether a gesture is in flight and must keep receiving the pointer.
    fn wants_pointer_capture(&self) -> bool {
        false
    }
    /// A series was shown or hidden: marks that cache their domain or their
    /// geometry drop it.
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

/// The one chart widget; see the [module docs](self).
#[crate::component::not_a_component(
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
    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        let frame = self.marks.frame_mut();
        frame.height = Some(height.max(frame.min_height));
        self
    }
    pub fn take_activated(&mut self) -> Option<M::Activated> {
        self.activated.take()
    }
    /// The activation waiting to be read, without taking it.
    pub(crate) fn activation(&self) -> Option<&M::Activated> {
        self.activated.as_ref()
    }
    pub(crate) fn clear_activation(&mut self) {
        self.activated = None;
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
    /// The latest series a legend showed or hid, once.
    pub fn take_visibility_changed(&mut self) -> Option<(String, bool)> {
        self.marks.toggles_mut()?.take_changed()
    }
    pub fn toggle_series(&mut self, id: &str) -> bool {
        let visible = self.is_series_visible(id);
        self.set_series_visible(id, !visible)
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
        // A legend entry claims the click even when its series is gone.
        let _ = self.toggle_series(&id);
        true
    }
}

impl<M: PlotMarks> Widget for Plot<M> {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let height = self.marks.layout_height();
        self.theme = theme.clone();
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: height.map_or_else(taffy::Dimension::auto, taffy::Dimension::length),
            },
            flex_grow: if height.is_some() { 0.0 } else { 1.0 },
            ..Default::default()
        };
        crate::widget::TaffyTreeExt::widget_leaf(taffy, style, node_map, self)
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
        let height = match self.marks.layout_height() {
            Some(height) => height,
            None => crate::num::f32_of_f64(self.marks.natural_height(
                text,
                &self.theme,
                f64::from(width),
            )),
        };
        taffy::Size {
            width,
            height: known.height.unwrap_or(height),
        }
    }
    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.marks.frame_mut().rect = crate::widget::layout_rect(taffy, node, origin);
        let theme = self.theme.clone();
        crate::text_render::with_layout_renderer(&theme, |tr| self.marks.place(tr, &theme));
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
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // A page shown as HTML shows it as one image.
        crate::scene_parts::picture();
        self.marks.paint(scene, transform, tr, theme);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.dirty = true;
        if self.marks.event(event, ctx) {
            // A pan, a zoom or a brush moves the ticks and the legend the
            // marks place, not only what they paint.
            self.layout_dirty = true;
            return;
        }
        // Keys inspect the chart only while it has focus; otherwise they
        // belong to whatever does.
        if self.focused || chart_nav_action(event).is_none() {
            handle_chart_event(self, event, ctx);
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
                    node
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
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
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
            .find(|item| crate::a11y::hash_key(&item.target) == key)
            .and_then(|item| item.activates)
        else {
            return false;
        };
        self.activated = Some(activated);
        true
    }

    fn needs_redraw(&self) -> bool {
        crate::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        crate::widget::Widget::mark_children_drawn(self);
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

/// A value axis down the left of `plot`: a label per nice tick of `range`
/// (bottom to top), named by `label`, with a grid line across when `grid`.
pub(crate) struct ValueAxis<F> {
    pub(crate) plot: Rect,
    pub(crate) range: (f64, f64),
    pub(crate) grid: bool,
    pub(crate) label: F,
}

impl<F: Fn(f64) -> String> ValueAxis<F> {
    pub(crate) fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        use crate::tokens::TextRole;
        use vello::kurbo::{Line, Stroke};
        let (plot, (min, max)) = (self.plot, self.range);
        for value in crate::charts::nice_ticks(min, max, 5) {
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
            let label = (self.label)(value);
            let (width, _) = tr.measure_style(&label, None, &theme.text(TextRole::Caption));
            tr.draw_style(
                scene,
                &label,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((plot.x0 - f64::from(width) - 9.0, y - 6.0)),
                None,
                &theme.text(TextRole::Caption),
            );
        }
    }
}
