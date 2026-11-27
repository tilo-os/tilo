//! One view swapped for another whole, where it stands: a stage's page, a slide, the content of
//! the tab a window shows. The view that was shown stays laid out while it fades out, and the one
//! that replaces it fades in over it (`motion::SWAP`), both at the same place; once the swap has
//! played only the new one stands.
//!
//! A view writes it as `ui.Swapped(shown: what) { … }`: when `shown` names something else, what
//! stood under the new content stays drawn while it fades out. A host builds one too when a view
//! that declares its content swapped whole (`transition: .cross_fade`) changes what it shows: the
//! reconciler hands the widget it had to [`Swapped::new`] and what it built for the new content
//! beside it. The pace is the view's `motion:`, the theme's own otherwise; a change no hand made,
//! a still theme and reduced motion land at once, and then the old view is not kept at all.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

use crate::time::Instant;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::{
    component::Component, component::Described, component::PartViews, widget::AppModel,
    widget::DrawChild, widget::DrawCtx, widget::Element, widget::EventCtx, widget::ModelPath,
    widget::Spacer, widget::TaffyTreeExt, widget::Widget,
};
use tilo_ui_base::{motion::CrossFade, motion::Feel, motion::frame_now, motion::recent_input};
use tilo_ui_base::{tokens::EffectiveMotion, tokens::MotionSource};

/// What a view that swaps its content whole takes: what it shows, its name, and the pace the
/// swap plays at. A change of [`Self::shown`] keeps the content it showed under the new one while
/// it fades out, then lets it go.
#[derive(Component, Default)]
#[component(name = "Swapped", role = "Group", label = "name")]
pub struct SwappedProps {
    /// What the content is: a view that names another swaps what it shows, the one it showed
    /// fading out under the new one. Any text does, as a key: the window's front tab, a slide's
    /// id, a word the view changes. Empty is the same as any other.
    #[component(data)]
    pub shown: Option<String>,
    /// What it shows, for assistive technology.
    pub name: Option<String>,
    /// The weight it fills its room with, as a container's `fill`: the whole room for a window's
    /// content, its own height where it hugs what it holds.
    pub fill: Option<f64>,
    /// The pace the swap plays at (`.standard`, `.brisk`, `.calm`, `.still`): the theme's own
    /// otherwise.
    pub motion: Option<Feel>,
    /// What it shows.
    #[component(part(default))]
    pub body: Option<Element>,
}

pub struct Swapped {
    current: Element,
    /// The view that was shown while it fades out, laid out over the current one.
    previous: Option<Element>,
    /// How opaque the outgoing view was drawn when the swap began: a swap that interrupts one
    /// carries the view it caught part-way, never a jump to full.
    previous_opacity: f32,
    /// The swap's progress: 0 at the moment it began, 1 when only the current view stands.
    fade: CrossFade<u64>,
    generation: u64,
    /// The pace the swap plays at, as the view's `motion:` names it.
    feel: Feel,
    theme: Option<Theme>,
    /// A swap the reconciler just made, which starts on the next frame: only then is the theme
    /// known, and the theme is what says how the person has motion set.
    pending: bool,
    /// The weight the view fills its room with (`fill`), so the swap takes it too.
    fill: f32,
    /// Reported to the host when this widget's composition changed: the old view kept, or let go.
    structure_dirty: bool,
    /// What the content is, as the view names it (`ui.Swapped(shown:)`): a view that names
    /// another swaps what it shows.
    shown: String,
    /// A change of [`Self::shown`] the part's reconciliation has not come for yet: the content
    /// it showed is kept and fades out when the reconciler reaches the view it updates.
    swap_owed: bool,
    /// What it shows, for assistive technology.
    name: Option<String>,
    id: Option<String>,
    rect: Rect,
}

impl Swapped {
    /// `current` standing on its own, having swapped nothing.
    #[must_use]
    pub fn standing(current: Element) -> Self {
        Self {
            current,
            previous: None,
            previous_opacity: 1.0,
            fade: CrossFade::of_spec(tilo_ui_base::motion::SWAP, 0),
            generation: 0,
            feel: Feel::Standard,
            theme: None,
            pending: false,
            fill: 0.0,
            structure_dirty: true,
            shown: String::new(),
            swap_owed: false,
            name: None,
            id: None,
            rect: Rect::ZERO,
        }
    }

    /// `previous` (what the view showed) swapped for `current` (what it shows now).
    #[must_use]
    pub fn new(previous: Element, current: Element) -> Self {
        Self {
            current,
            previous: Some(previous),
            previous_opacity: 1.0,
            fade: CrossFade::of_spec(tilo_ui_base::motion::SWAP, 0),
            generation: 1,
            feel: Feel::Standard,
            theme: None,
            pending: true,
            fill: 0.0,
            structure_dirty: true,
            shown: String::new(),
            swap_owed: false,
            name: None,
            id: None,
            rect: Rect::ZERO,
        }
    }

    /// Swap what is shown for `next`, the view showing now fading out where it stands. The swap
    /// already fading (if any) is dropped: the view the person sees is the one that goes on
    /// fading from where it is drawn.
    pub fn swap(&mut self, next: Element) {
        let now = frame_now();
        self.previous_opacity = self.fade.incoming(now);
        self.previous = Some(std::mem::replace(&mut self.current, next));
        self.pending = true;
        self.structure_dirty = true;
    }

    /// What the view shows now.
    #[must_use]
    pub fn current(&self) -> &dyn Widget {
        self.current.as_ref()
    }

    /// What the view shows now, to change or take.
    pub fn current_mut(&mut self) -> &mut Element {
        &mut self.current
    }

    /// The view that is fading out, while it still stands.
    #[must_use]
    pub fn previous(&self) -> Option<&dyn Widget> {
        self.previous.as_deref()
    }

    /// Whether a swap is playing or about to: the old view still stands.
    #[must_use]
    pub fn is_swapping(&self) -> bool {
        self.previous.is_some()
    }

    /// How opaque the view that is going out and the one coming in are drawn at `now`, while a
    /// swap plays: the old at its own opacity fading out over the first 60 %, the new fading in
    /// over the whole motion.
    #[must_use]
    pub fn opacities(&self, now: Instant) -> Option<(f32, f32)> {
        if !self.fade.is_moving(now) {
            return None;
        }
        let outgoing = self
            .fade
            .outgoing(now)
            .map_or(0.0, |(_, gliding)| self.previous_opacity * gliding);
        Some((outgoing, self.fade.incoming(now)))
    }

    /// The pace the swap plays at (`motion:` on the view).
    pub fn set_feel(&mut self, feel: Feel) {
        self.feel = feel;
    }

    /// The weight the view fills its room with.
    pub fn set_fill(&mut self, fill: f32) {
        self.fill = fill;
    }

    /// Address this widget by a stable id, for tests and inspection.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Start the swap the reconciler made, now that the theme is known, and let the old view go
    /// once it has faded (immediately, for one that lands).
    fn begin(&mut self, motion: EffectiveMotion, now: Instant) {
        if !std::mem::take(&mut self.pending) {
            return;
        }
        // A press or a gesture plays the swap; a key plays it at `fast` (the whole view is what
        // changed, which is what a reader watches), and a change no hand made lands.
        let source = recent_input(now).unwrap_or(MotionSource::Action);
        self.generation = self.generation.wrapping_add(1);
        self.fade
            .show(self.generation, self.feel.over(motion), source, now);
        self.settle(now);
    }

    /// Let the old view go once it has faded out. A swap still waiting to begin has not faded: the
    /// old view stays until it does, so a sync before the first layout never drops it.
    fn settle(&mut self, now: Instant) {
        if self.pending {
            return;
        }
        if self.previous.is_some() && !self.fade.is_moving(now) {
            self.previous = None;
            self.structure_dirty = true;
        }
    }

    fn starts(&mut self) {
        if self.pending
            && let Some(theme) = self.theme.clone()
        {
            self.begin(theme.motion(), frame_now());
        }
    }
}

/// A view that says what it shows (`ui.Swapped(shown:)`): a change of that name swaps the content
/// it holds whole, keeping what it showed under the new content while it fades out.
impl Described for Swapped {
    type Props = SwappedProps;

    fn from_props(key: &str, props: SwappedProps) -> Self {
        let SwappedProps {
            shown,
            name,
            fill,
            motion,
            body,
        } = props;
        let body = body.unwrap_or_else(|| Box::new(Spacer::fixed(0.0)) as Element);
        let mut built = Self::standing(body).with_id(key);
        built.shown = shown.unwrap_or_default();
        built.name = name;
        built.set_fill(weight(fill));
        built.set_feel(motion.unwrap_or_default());
        built
    }

    fn set_props(&mut self, props: SwappedProps) -> bool {
        let SwappedProps {
            shown,
            name,
            fill,
            motion,
            ..
        } = props;
        self.name = name;
        self.set_fill(weight(fill));
        self.set_feel(motion.unwrap_or_default());
        // The content in the part is reconciled where it stands, so a change of what it shows is
        // only known to be one when that reconciliation comes: the part swaps then. A view that
        // shows the same content under another name swaps nothing (see `view_mut`).
        let shown = shown.unwrap_or_default();
        if shown != self.shown {
            self.shown = shown;
            self.swap_owed = true;
        }
        true
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        (part == "body").then_some(self as &mut dyn PartViews)
    }
}

impl PartViews for Swapped {
    fn len(&self) -> usize {
        1
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        if index != 0 {
            return None;
        }
        if std::mem::take(&mut self.swap_owed) {
            // The content the part showed is kept and fades out while what comes fades in: the
            // reconciler updates what this returns, so it updates an empty stand-in and builds
            // the new content after, which `replace` takes.
            self.swap(Box::new(Spacer::fixed(0.0)));
        }
        Some(&mut self.current)
    }

    fn insert(&mut self, _index: usize, view: Element) {
        self.current = view;
    }

    fn remove(&mut self, _index: usize) {
        self.current = Box::new(Spacer::fixed(0.0));
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, _index: usize, view: Element) {
        self.current = view;
    }
}

/// The weight a `fill` prop gives, as the layout solver takes it.
fn weight(fill: Option<f64>) -> f32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a view's share of its room, far inside f32"
    )]
    {
        fill.unwrap_or(0.0) as f32
    }
}

/// The container's style: one cell both views fill, as tall as the taller of them where the
/// view's own height is open.
fn container(fill: f32) -> taffy::Style {
    let style = taffy::Style {
        display: taffy::Display::Grid,
        grid_template_columns: vec![taffy::style_helpers::minmax(
            taffy::style_helpers::length(0.0_f32),
            taffy::style_helpers::fr(1.0_f32),
        )],
        grid_template_rows: vec![taffy::style_helpers::minmax(
            taffy::style_helpers::length(0.0_f32),
            taffy::style_helpers::fr(1.0_f32),
        )],
        size: taffy::Size {
            width: taffy::style_helpers::percent(1.0_f32),
            // A view given room fills it: its own height is the room, never what the pages it
            // holds happen to be as tall as.
            height: if fill > 0.0 {
                taffy::style_helpers::percent(1.0_f32)
            } else {
                taffy::Dimension::auto()
            },
        },
        min_size: taffy::Size {
            width: taffy::style_helpers::length(0.0_f32),
            height: taffy::style_helpers::length(0.0_f32),
        },
        ..taffy::Style::default()
    };
    tilo_ui_base::widget::layout::filling(style, fill)
}

/// The view shown now, taking the whole of the one cell: its size is what the view is.
fn cell() -> taffy::Style {
    let line = taffy::Line {
        start: taffy::style_helpers::line(1),
        end: taffy::GridPlacement::Auto,
    };
    taffy::Style {
        grid_column: line.clone(),
        grid_row: line,
        size: taffy::Size {
            width: taffy::style_helpers::percent(1.0_f32),
            height: taffy::style_helpers::percent(1.0_f32),
        },
        min_size: taffy::Size {
            width: taffy::style_helpers::length(0.0_f32),
            height: taffy::style_helpers::length(0.0_f32),
        },
        ..taffy::Style::default()
    }
}

/// The view that is fading out, drawn over the cell the view shown takes: out of the flow, so
/// what it holds never makes the swap bigger, smaller or wider than the view that stands.
fn overlay() -> taffy::Style {
    taffy::Style {
        position: taffy::Position::Absolute,
        inset: taffy::Rect {
            left: taffy::style_helpers::length(0.0_f32),
            right: taffy::style_helpers::length(0.0_f32),
            top: taffy::style_helpers::length(0.0_f32),
            bottom: taffy::style_helpers::length(0.0_f32),
        },
        size: taffy::Size {
            width: taffy::style_helpers::percent(1.0_f32),
            height: taffy::style_helpers::percent(1.0_f32),
        },
        min_size: taffy::Size {
            width: taffy::style_helpers::length(0.0_f32),
            height: taffy::style_helpers::length(0.0_f32),
        },
        ..taffy::Style::default()
    }
}

impl Widget for Swapped {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        // A change of what the view says it shows, whose part the reconciler found unchanged,
        // swaps nothing: the frame after it clears what the part did not take, so an edit inside
        // the tab in front never fades the whole content.
        self.swap_owed = false;
        self.starts();
        self.settle(frame_now());
        self.current.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.starts();
        let now = frame_now();
        let was = self.previous.is_some();
        self.settle(now);
        self.current.update(state, path) | (was != self.previous.is_some())
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        self.begin(theme.motion(), frame_now());
        let current = self.current.build_layout(taffy, theme, node_map);
        let previous = self
            .previous
            .as_mut()
            .map(|previous| previous.build_layout(taffy, theme, node_map));
        let mut children = Vec::with_capacity(2);
        if let Some(previous) = previous {
            let _ = taffy.set_style(previous, overlay());
            children.push(previous);
        }
        let _ = taffy.set_style(current, cell());
        children.push(current);
        taffy.widget_node(container(self.fill), &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        let origin = self.rect.origin();
        let mut index = 0;
        if let Some(previous) = self.previous.as_mut()
            && let Ok(child) = taffy.child_at_index(node, index)
        {
            previous.apply_layout(taffy, child, origin);
            index += 1;
        }
        if let Ok(child) = taffy.child_at_index(node, index) {
            self.current.apply_layout(taffy, child, origin);
        }
    }

    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let (scene, transform, text, theme, env) = ctx.parts();
        let now = frame_now();
        let rect = self.rect;
        let Some((_, incoming)) = self.opacities(now) else {
            // Nothing of the view that went stands: the content is drawn as it is, outside any
            // layer, so a settled view paints exactly as it would on its own.
            self.current.draw_child(scene, transform, text, theme, &env);
            return;
        };
        // One share each, summing to one: the page going out at what the fade has left, the page
        // coming at what it has come. A cross-fade mixes the two pages, so it may only be linear
        // over complementary shares.
        let outgoing = self.previous_opacity * (1.0 - incoming);
        if outgoing <= 0.0 && incoming <= 0.0 {
            return;
        }
        // Both pages stand in one layer, the page going out at its share of the fade and the page
        // coming added to it at the rest: the mix is the linear one of the two pages, so two
        // translucent grounds under each other make the colour between them, never a third tint
        // where the two overlap.
        let clip = rect.inflate(2.0, 2.0);
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &clip,
        );
        if outgoing > 0.0
            && let Some(previous) = self.previous.as_mut()
        {
            tilo_ui_base::widget::in_opacity_layer(scene, transform, rect, outgoing, |scene| {
                previous.draw_child(scene, transform, text, theme, &env);
            });
        }
        if incoming > 0.0 {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::new(
                    vello::peniko::Mix::Normal,
                    vello::peniko::Compose::PlusLighter,
                ),
                incoming,
                transform,
                &clip,
            );
            self.current.draw_child(scene, transform, text, theme, &env);
            scene.pop_layer();
        }
        scene.pop_layer();
    }

    /// The person acts on the view being shown: the one fading out is on its way out and takes
    /// nothing. It is laid out last, over the one it replaces.
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let Some(current) = ctx
            .taffy
            .children(ctx.node_id)
            .ok()
            .and_then(|children| children.last().copied())
        else {
            return;
        };
        let path = ctx.path;
        self.current
            .handle_event(event, &mut ctx.reborrow(path, current));
    }

    fn wants_pointer_capture(&self) -> bool {
        self.current.wants_pointer_capture()
    }

    fn touch_claim(&self, point: Point) -> tilo_ui_base::interaction::TouchClaim {
        self.current.touch_claim(point)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.current.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.current.as_mut()]
    }

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        if let Some(previous) = self.previous.as_deref() {
            visitor(previous);
        }
        visitor(self.current.as_ref());
        true
    }

    fn needs_animation_frame(&self) -> bool {
        // One frame after the old view's last lets it go: until it is dropped it is in the tree.
        self.pending
            || self.previous.is_some()
            || self.fade.is_moving(frame_now())
            || self.current.needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.pending
            || self.fade.is_moving(frame_now())
            || self.current.needs_redraw()
            || self.needs_animation_frame()
    }

    fn has_pending_visual_work(&self) -> bool {
        self.previous.is_some() || self.current.has_pending_visual_work()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        self.current.take_structure_dirty() | own
    }

    fn name(&self) -> &'static str {
        "Swapped"
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        self.name.as_ref().map(|name| {
            tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Group)
                .with_optional_name(Some(name.clone()))
        })
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}
