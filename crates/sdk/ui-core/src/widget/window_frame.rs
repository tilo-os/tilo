//! A window standing on a desktop, as a described component: what a shell draws for a place on a
//! floating desk (`shell.md` §4, `frame_classic`) and for a strip of places (`strips_view`),
//! which is the same thing.
//!
//! Its ground is the window level of the design language ([`SurfaceLevel::Window`]): the app's
//! content colour, the window edge and its soft shadow at the theme's window radius, with what it
//! holds clipped to that shape; the window in front stands higher than those behind it, as a
//! desktop shows which window has the keyboard.
//!
//! A window has one bar, never two stacked, and the bar says what state the window is in:
//!
//! - several places: the bar is their tabs, the window's content the place in front;
//! - one place whose view has a header row: the frame offers its window bar to that row
//!   ([`WindowHeader::set_window_bar`](crate::widget::WindowHeader)), so the app's title and
//!   actions and the frame's controls read as one bar;
//! - one place with no header row of its own, in front: the frame's own bar names it;
//! - one such place behind: the bar is the decorations alone, as a desktop draws a window nobody
//!   is typing into.
//!
//! The window's own controls stand in a pill of the floating material at the trailing end of the
//! bar in every state, and the app's own slots stay the app's. A view says which window is in
//! front and what its controls are; the theme decides everything the frame is made of.

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::chrome::HeaderRowMetrics;
use crate::component::{Component, Described, SlotViews};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::{Rhythm, SpacingStep, TextRole};
use crate::widget::DrawChild;
use crate::widget::arrange::{Align, At, Fit, Layers, Length, Slot, Stack};
use crate::widget::primitives::{SurfaceLevel, paint_surface};
use crate::widget::tabs::{TAB_BAR_HEIGHT, TabStrip};
use crate::widget::window_controls::WindowControls;
use crate::widget::{Decorator, Element, Spacer, Text, Widget, WindowBar};

/// A window standing on a desktop: the window surface with its edge and shadow, holding the
/// window's content and its controls, clipped to its radius.
#[derive(Component, Default)]
#[component(name = "WindowFrame", role = "Group", label = "name")]
pub struct WindowFrameProps {
    /// The window's key where it stands: what finds the frame by id, as the row it arrived in
    /// pans to show it.
    #[component(key)]
    pub key: String,
    /// The window's name: what assistive technology reads, and the title of the frame's own
    /// bar when the content has no header row to hold the controls.
    #[component(data)]
    pub name: Option<String>,
    /// Whether it is the window in front, the one the keyboard goes to.
    pub focused: bool,
    /// The shell lists this window's actions where the person reaches them (the desktop
    /// bar's window menu): its header's actions that do not fit give way with no door of
    /// their own. Without it what does not fit goes behind the header's door.
    pub lists_actions: bool,
    /// The frame's controls (close, and what else the shell offers on the window), at the
    /// trailing end of the window's bar.
    #[component(slot)]
    pub controls: Option<Element>,
    /// The places the window shows, as its tabs: the bar is theirs while there are any, and
    /// the content is the tab in front. Empty for a window of one place, whose bar is the
    /// place's own header or the frame's.
    #[component(slot)]
    pub tabs: Option<Element>,
    /// The resource the window's bar carries when a person picks it up, by its published
    /// address: a place on the canvas is dragged onto a composer, or back onto the canvas, by
    /// its own bar ([`ui.DragSource`](super::drag_source::DragSource), `shell.md` §8.1). Empty
    /// leaves the bar no source, as a foreign window's bar has nothing to carry.
    pub carries: Option<String>,
    /// The window's content, filling the window.
    #[component(slot(default))]
    pub body: Option<Element>,
}

/// What the frame's own bar shows this layout: the window's tabs, the place it shows, the
/// window's decorations alone, or nothing at all because the app's header holds the bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum BarShape {
    /// The content's header row holds the window's bar.
    #[default]
    Merged,
    /// The tabs of the window's places.
    Tabs,
    /// The place the window shows, in front, with no header row to name it.
    Title,
    /// The window's decorations alone: a window nobody is typing into.
    Bare,
}

/// [`WindowFrameProps`] built: layers on the window surface, the content (under the frame's
/// own bar while it needs one) and the controls over its bar's trailing end.
pub struct WindowFrame {
    /// Its key where it stands, as the surface laying it out names it.
    key: String,
    name: Option<String>,
    focused: bool,
    lists_actions: bool,
    layers: Layers,
    /// The slot a host is changing: 0 for the controls, 1 for the tabs, 2 for the content.
    editing: usize,
    /// The window shows tabs: its bar is theirs, and the content's header row takes no bar.
    tabbed: bool,
    /// The content's header row holds the window's bar, so the frame shows no bar of its own.
    merged: bool,
    /// The bar the frame draws of its own, this layout.
    shape: BarShape,
    /// The name the frame's own bar shows, and whether the window was in front: pressing the
    /// bar again is nothing, so the name is built again only when it says something else.
    title: Option<(String, bool)>,
    /// The name the bar last built: the window's name elided to one line in the room the bar
    /// has, and nothing while the bar has no room for a name at all.
    shown: Option<(String, bool)>,
    /// How wide the controls were laid out, the room the content's header row keeps for
    /// them.
    controls_width: f64,
    /// The controls' width changed since the header row was told: lay out again.
    relayout: bool,
    /// The header row's measures in the theme of the last layout.
    metrics: Option<HeaderRowMetrics>,
    rect: Rect,
    /// The window's bar this layout: where the tabs, the name and the controls stand.
    bar: Rect,
    /// The pill around the controls, over the bar's ground or over the app's header row.
    pill: Rect,
    /// The window arriving where it stands and gliding when its place changes
    /// (`motion::Placed`: `WindowMotion::OPEN` and `MOVE`, landing at once for a still theme and
    /// reduced motion). Drawn as a component of the shell's do: a window a view builds again
    /// each time the arrangement changes still moves rather than jumping.
    placed: crate::motion::Placed,
    /// The resource the bar carries when it is picked up, by its published address: what a
    /// press on the bar's empty area publishes on the drag bus once it travels past the
    /// threshold, so a place on the canvas reaches a composer or the canvas itself by its own
    /// bar. `None` for a window whose bar carries nothing (a foreign window's).
    carries: Option<String>,
    /// A press on the bar that may become a drag.
    lift: Option<Lift>,
    /// Where the pointer last was, in the frame's logical coordinates.
    cursor: Point,
}

/// A press on the bar that may become a drag of the window's own bar.
#[derive(Clone, Copy, Debug)]
struct Lift {
    /// Where the press began, in the frame's logical coordinates.
    start: Point,
    /// The press travelled past the threshold: the drag is live on the bus.
    active: bool,
}

/// The layer holding the content, and the one holding the controls.
const CONTENT: usize = 0;
const CONTROLS: usize = 1;
/// In the content's column: the frame's own bar, then the content.
const OWN_BAR: usize = 0;
const BODY: usize = 1;
/// The room the frame's own bar keeps for the window's name, logical pixels: below it a name
/// would be an ellipsis and nothing more, so the bar shows the decorations alone. Six steps of
/// the spacing scale, so it follows the theme's own.
fn name_room(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s16 + theme.tokens.spacing.s8)
}

/// In the frame's own bar: the window's tabs, its name, then the room the controls keep.
const TABS: usize = 0;
const NAME: usize = 1;
const AFTER: usize = 2;

fn empty() -> Element {
    Box::new(Spacer::token(SpacingStep::S0))
}

/// How the tabs stand in the frame's own bar: the bar of a window of several places is
/// theirs, so they take its room; a window of one leaves that slot empty.
fn tabs_fit(tabbed: bool) -> Fit {
    if tabbed { Fit::fill(1.0) } else { Fit::hug() }
}

/// How the room after the tabs stands in the bar: what the tabs leave, which a window of one
/// gives to its name.
fn after_fit(tabbed: bool) -> Fit {
    if tabbed { Fit::hug() } else { Fit::fill(1.0) }
}

/// Whether `widget` holds the window's tabs: a view names the tabs slot on every window, so
/// what the slot holds says whether this window has tabs, never the slot's mere presence.
fn holds_tabs(widget: &dyn Widget) -> bool {
    let tabs = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<TabStrip>())
        .is_some();
    tabs || widget.children().iter().any(|child| holds_tabs(*child))
}

fn slot(element: Element, fit: Fit) -> Slot {
    let mut slot = Slot::new(element);
    slot.fit = fit;
    slot
}

/// The fit of a bar: at least one bar tall, hugging what it holds across. A window narrow
/// enough that its name wraps stands a taller bar rather than a title past it.
fn bar_fit(shown: bool) -> Fit {
    let mut fit = Fit::hug();
    if shown {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a tab bar measure fits f32"
        )]
        let height = Length::Dp(TAB_BAR_HEIGHT as f32);
        fit.height.min = Some(height);
    }
    fit
}

impl Described for WindowFrame {
    type Props = WindowFrameProps;

    fn from_props(_key: &str, props: WindowFrameProps) -> Self {
        let WindowFrameProps {
            key,
            name,
            focused,
            lists_actions,
            controls,
            tabs,
            carries,
            body,
        } = props;
        let tabbed = tabs.as_deref().is_some_and(holds_tabs);
        let own = Stack::row()
            .align(Align::Center)
            .push_slot(slot(tabs.unwrap_or_else(empty), tabs_fit(tabbed)))
            .push_slot(slot(empty(), Fit::hug()))
            .push_slot(slot(empty(), after_fit(tabbed)));
        let column = Stack::stack()
            .fill_width()
            .fill_height()
            .gap(Rhythm::None)
            .push_slot(slot(Box::new(own), bar_fit(tabbed)))
            .push_slot(slot(body.unwrap_or_else(empty), Fit::fill(1.0)));
        let controls = Stack::row()
            .align(Align::Center)
            .push(controls.unwrap_or_else(empty));
        let layers = Layers::layers()
            .push_slot(slot(Box::new(column), Fit::fill(1.0)))
            .push_at(Box::new(controls), At::TopTrailing);
        Self {
            key,
            name,
            focused,
            lists_actions,
            layers,
            editing: 2,
            tabbed,
            merged: !tabbed,
            shape: if tabbed {
                BarShape::Tabs
            } else {
                BarShape::Merged
            },
            title: None,
            shown: None,
            controls_width: 0.0,
            relayout: false,
            metrics: None,
            rect: Rect::ZERO,
            bar: Rect::ZERO,
            pill: Rect::ZERO,
            placed: if crate::motion::window_has_shown() {
                crate::motion::Placed::closed()
            } else {
                crate::motion::Placed::opened()
            },
            carries,
            lift: None,
            cursor: Point::ZERO,
        }
    }

    fn set_props(&mut self, props: WindowFrameProps) -> bool {
        if props.tabs.as_deref().is_some_and(holds_tabs) != self.tabbed {
            return false;
        }
        if self.name != props.name {
            self.title = None;
        }
        self.key = props.key;
        self.name = props.name;
        self.focused = props.focused;
        self.lists_actions = props.lists_actions;
        self.carries = props.carries;
        true
    }

    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        self.editing = match slot {
            "controls" => 0,
            "tabs" => 1,
            "body" => 2,
            _ => return None,
        };
        Some(self)
    }
}

/// The view of the slot being changed: the controls, the tabs, or the content.
impl SlotViews for WindowFrame {
    fn len(&self) -> usize {
        1
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        if index != 0 {
            return None;
        }
        match self.editing {
            0 => self.controls_row()?.child_mut(0),
            1 => self.own_bar_row()?.child_mut(TABS),
            _ => self.column()?.child_mut(BODY),
        }
    }

    fn insert(&mut self, _index: usize, view: Element) {
        self.put(view);
    }

    fn remove(&mut self, _index: usize) {
        self.put(empty());
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, _index: usize, view: Element) {
        self.put(view);
    }
}

impl WindowFrame {
    fn stack_at(&mut self, index: usize) -> Option<&mut Stack> {
        self.layers
            .child_mut(index)?
            .as_any_mut()?
            .downcast_mut::<Stack>()
    }

    /// The column of the frame's own bar over the content.
    fn column(&mut self) -> Option<&mut Stack> {
        self.stack_at(CONTENT)
    }

    /// The row the controls stand in.
    fn controls_row(&mut self) -> Option<&mut Stack> {
        self.stack_at(CONTROLS)
    }

    /// The row of the frame's own bar: the window's tabs, its name, and the room the controls
    /// keep.
    fn own_bar_row(&mut self) -> Option<&mut Stack> {
        self.column()?
            .child_mut(OWN_BAR)?
            .as_any_mut()?
            .downcast_mut::<Stack>()
    }

    /// The content the window shows.
    fn body_mut(&mut self) -> Option<&mut Element> {
        self.column()?.child_mut(BODY)
    }

    /// Put `view` in the slot being changed.
    fn put(&mut self, view: Element) {
        match self.editing {
            0 => {
                if let Some(row) = self.controls_row() {
                    let _ = row.replace(0, view);
                }
            }
            1 => {
                if let Some(row) = self.own_bar_row() {
                    let _ = row.replace(TABS, view);
                }
                self.tabbed = self
                    .own_bar_row()
                    .and_then(|row| row.children().get(TABS).copied())
                    .is_some_and(holds_tabs);
                let tabbed = self.tabbed;
                if let Some(row) = self.own_bar_row() {
                    row.set_placement(TABS, tabs_fit(tabbed), 1, None);
                    row.set_placement(AFTER, after_fit(tabbed), 1, None);
                }
            }
            _ => {
                if let Some(column) = self.column() {
                    let _ = column.replace(BODY, view);
                }
                self.merged = false;
            }
        }
    }

    /// Offer the content's header row the window's bar — or take it away while the window's
    /// own tabs are the bar — and show the frame's own bar when nothing takes it.
    fn offer_bar(&mut self, m: &HeaderRowMetrics) {
        if self.tabbed {
            if let Some(body) = self.body_mut() {
                crate::widget::window_header::offer_window_bar(body.as_mut(), None);
            }
            self.merged = false;
            return;
        }
        let bar = WindowBar {
            controls_width: self.controls_width + m.inset,
            lists_actions: self.lists_actions,
        };
        self.merged = self.body_mut().is_some_and(|body| {
            crate::widget::window_header::offer_window_bar(body.as_mut(), Some(bar))
        });
    }

    /// The bar the frame draws of its own, this layout.
    fn shape(&self) -> BarShape {
        if self.tabbed {
            BarShape::Tabs
        } else if self.merged {
            BarShape::Merged
        } else if self.focused {
            BarShape::Title
        } else {
            BarShape::Bare
        }
    }

    /// Build the frame's own bar for `shape`: one bar tall, nothing in it while the app's
    /// header holds the bar, else its name — and never a second bar of the app's under it.
    fn build_bar(&mut self, shape: BarShape, m: &HeaderRowMetrics, theme: &Theme, room: f64) {
        self.shape = shape;
        let bar = shape != BarShape::Merged;
        if let Some(column) = self.column() {
            column.set_placement(OWN_BAR, bar_fit(bar), 1, None);
        }
        let title = match shape {
            BarShape::Title => Some((self.name.clone().unwrap_or_default(), self.focused)),
            _ => None,
        };
        let shown = title
            .clone()
            .map(|(name, focused)| (self.elided(&name, m, theme, room), focused));
        if title == self.title && shown == self.shown {
            return;
        }
        self.title = title;
        self.shown = shown;
        let element = self.name_element(m);
        if let Some(own) = self.own_bar_row() {
            let _ = own.replace(NAME, element);
        }
    }

    /// The window's name, elided to one line of the room `room` the bar has for it, as a tab's
    /// title is: a name too long for the bar ends in an ellipsis, never in a second line taller
    /// than the bar. Below the room a name needs at all the bar shows no name: the decorations
    /// alone say whose window it is.
    fn elided(&self, name: &str, m: &HeaderRowMetrics, theme: &Theme, room: f64) -> String {
        let budget = room - m.gutter;
        if room < name_room(theme) || budget <= 0.0 {
            return String::new();
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a window's bar room fits f32"
        )]
        let budget = budget as f32;
        let style = theme.text(TextRole::Label);
        crate::text_render::with_layout_renderer(theme, |text| {
            text.elide_to_width_style(name, budget, &style)
        })
    }

    /// The name the frame's own bar shows: nothing while the bar has no room for it, so a window
    /// too narrow for its name shows its decorations alone instead of a name folded into a column.
    fn name_element(&self, m: &HeaderRowMetrics) -> Element {
        let Some((name, focused)) = self.shown.clone().filter(|(name, _)| !name.is_empty()) else {
            return empty();
        };
        let mut title = Text::new(&name).with_role(TextRole::Label);
        if !focused {
            title = title.dimmed();
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a header row measure fits f32"
        )]
        let gutter = Spacer::fixed(m.gutter as f32);
        let row = Stack::row()
            .align(Align::Center)
            .push(Box::new(gutter))
            .push(Box::new(title))
            .push_slot(slot(empty(), Fit::fill(1.0)));
        Box::new(row)
    }

    /// The bar's row: the content's header row when it holds the bar, else the frame's own.
    fn bar_row(&self, m: &HeaderRowMetrics) -> Rect {
        if self.merged {
            return self
                .layers
                .children()
                .first()
                .and_then(|column| crate::widget::window_header::window_bar_slot(*column))
                .map_or_else(
                    || {
                        Rect::new(
                            self.rect.x0,
                            self.rect.y0,
                            self.rect.x1,
                            self.rect.y0 + m.height,
                        )
                    },
                    |slot| slot.row,
                );
        }
        self.layers
            .children()
            .first()
            .and_then(|column| column.children().get(OWN_BAR).copied())
            .map_or(Rect::ZERO, Widget::get_rect)
    }

    /// How far the controls move from the frame's top trailing corner, where the layers put
    /// them, to stand on the bar's row, `inset` in from its trailing end, as the header row's
    /// own buttons stand from theirs. Both ends are laid out on device pixels, so the move is
    /// too.
    fn controls_offset(&self, m: &HeaderRowMetrics, bar: Rect) -> Vec2 {
        let Some(controls) = self.layers.children().get(CONTROLS).map(|c| c.get_rect()) else {
            return Vec2::ZERO;
        };
        Vec2::new(
            bar.x1.min(self.rect.x1) - m.inset - controls.x1,
            bar.y0 - controls.y0,
        )
    }

    /// The pointer moved while the bar is held: past the threshold the bar's resource is in
    /// flight on the drag bus, with the window's name as its label.
    fn carry(&mut self, point: Point) {
        let (Some(carries), Some(lift)) = (self.carries.as_deref(), self.lift.as_mut()) else {
            return;
        };
        if !lift.active && lift.start.distance(point) > crate::dnd::DRAG_START_THRESHOLD_PX {
            lift.active = true;
        }
        if !lift.active {
            return;
        }
        let title = self.name.clone().unwrap_or_default();
        crate::widget::drag_bus::publish(crate::widget::drag_bus::InFlight {
            source: self.key.clone(),
            payload: crate::dnd::DragPayload::Custom {
                kind: crate::widget::drag_source::RESOURCE_PAYLOAD.to_owned(),
                blob: carries.as_bytes().to_vec(),
            },
            label: title,
            pointer: point,
            runs: vec![(self.key.clone(), self.rect)],
        });
    }

    /// The bar was let go: `Some(true)` when the press travelled and the drag ended here (the
    /// content sees no click), `Some(false)` when it never travelled.
    fn let_go(&mut self) -> Option<bool> {
        let lift = self.lift.take()?;
        if lift.active {
            let Some(carries) = self.carries.clone() else {
                return Some(true);
            };
            crate::widget::drag_bus::end(Some(crate::widget::drag_bus::Landed {
                source: self.key.clone(),
                payload: crate::dnd::DragPayload::Custom {
                    kind: crate::widget::drag_source::RESOURCE_PAYLOAD.to_owned(),
                    blob: carries.into_bytes(),
                },
                pointer: self.cursor,
            }));
        } else {
            crate::widget::drag_bus::end(None);
        }
        Some(lift.active)
    }
}

impl Decorator for WindowFrame {
    type Inner = Layers;

    fn inner(&self) -> &Layers {
        &self.layers
    }

    fn inner_mut(&mut self) -> &mut Layers {
        &mut self.layers
    }

    /// The window's bar is the handle a person picks the window up by: a press on the bar's
    /// empty area that travels carries `carries` on the drag bus, as a
    /// [`DragSource`](super::drag_source::DragSource) does, so a place on the canvas reaches a
    /// composer or the canvas itself by its own bar (`shell.md` §8.1, "Dropping on the
    /// composer"). The tabs, the controls and the app's own header buttons take their presses
    /// first: only what the content did not take is the bar's.
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.carries.is_none() {
            self.layers.handle_event(event, ctx);
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = ctx.logical(position);
                self.cursor = point;
                if self.lift.is_some() {
                    self.carry(point);
                    if self.lift.is_some_and(|lift| lift.active) {
                        *ctx.handled = true;
                        return;
                    }
                }
                self.layers.handle_event(event, ctx);
            }
            WindowEvent::CursorLeft { .. } => {
                if self.lift.take().is_some() {
                    crate::widget::drag_bus::end(None);
                }
                self.layers.handle_event(event, ctx);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.layers.handle_event(event, ctx);
                if *ctx.handled || !self.bar.contains(self.cursor) {
                    return;
                }
                self.lift = Some(Lift {
                    start: self.cursor,
                    active: false,
                });
                *ctx.focus_target = Some(ctx.node_id);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.lift.is_some() => {
                if self.let_go() == Some(true) {
                    *ctx.handled = true;
                    return;
                }
                self.layers.handle_event(event, ctx);
            }
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ElementState::Pressed
                    && key.logical_key == Key::Named(NamedKey::Escape)
                    && self.lift.is_some() =>
            {
                self.lift = None;
                crate::widget::drag_bus::end(None);
                *ctx.handled = true;
            }
            _ => self.layers.handle_event(event, ctx),
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.lift.is_some()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let m = HeaderRowMetrics::resolve(theme);
        self.offer_bar(&m);
        let room = self.rect.width()
            - self.controls_width
            - crate::widget::window_controls::EDGE_INSET
            - crate::widget::window_controls::LEAD_GAP;
        self.build_bar(self.shape(), &m, theme, room);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a header row measure fits f32"
        )]
        let height = Length::Dp(if self.merged {
            m.height as f32
        } else {
            TAB_BAR_HEIGHT as f32
        });
        let mut fit = Fit::hug();
        fit.height.min = Some(height);
        fit.height.max = Some(height);
        self.layers
            .set_placement(CONTROLS, fit, 1, Some(At::TopTrailing));
        // // The tabs and the name have the room the controls leave, never more: a window narrow // enough
        // that its bar cannot hold them both squeezes them, and the strip shows the // titles it can,
        // rather than the two covering each other.
        if room > 0.0 {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a window's bar room fits f32"
            )]
            let room = Length::Dp(room as f32);
            let tabbed = self.tabbed;
            if let Some(row) = self.own_bar_row() {
                let mut tabs = tabs_fit(tabbed);
                tabs.width.max = Some(room);
                let mut name = Fit::hug();
                name.width.max = Some(room);
                row.set_placement(TABS, tabs, 1, None);
                row.set_placement(NAME, name, 1, None);
            }
        }
        self.metrics = Some(m);
        self.relayout = false;
        self.layers.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.layers.apply_layout(taffy, node, origin);
        self.rect = self.layers.get_rect();
        if let Some(m) = self.metrics {
            self.bar = self.bar_row(&m);
            let by = self.controls_offset(&m, self.bar);
            if by != Vec2::ZERO {
                self.layers.offset_child(taffy, CONTROLS, by);
            }
            let controls = self
                .layers
                .children()
                .get(CONTROLS)
                .map_or(Rect::ZERO, |c| c.get_rect());
            let pill = WindowControls::around(self.bar, controls);
            if pill != self.pill {
                self.relayout = true;
            }
            self.pill = pill;
        }
        let width = self
            .layers
            .children()
            .get(CONTROLS)
            .map_or(0.0, |c| c.get_rect().width());
        if (width - self.controls_width).abs() > 0.5 {
            self.controls_width = width;
            self.relayout = true;
        }
    }

    fn needs_redraw(&self) -> bool {
        self.relayout || (self.layers.needs_redraw() || self.layers.needs_animation_frame())
    }

    /// Whether the window is arriving, going or moving: what asks for frames while it does,
    /// and what its own content asks for.
    fn needs_animation_frame(&self) -> bool {
        self.placed.is_moving(crate::motion::frame_now()) || self.layers.needs_animation_frame()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.layers.collect_damage(regions);
        let now = crate::motion::frame_now();
        let drawn = self.placed.rect(now);
        if drawn != self.rect {
            regions.push(drawn.union(self.rect));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let rect = self.rect;
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let now = crate::motion::frame_now();
        self.placed.follow(
            rect,
            true,
            theme.motion(),
            crate::motion::input_source(),
            now,
        );
        let pose = self.placed.pose(rect, now);
        let bounds = rect.union(pose.drawn_rect(rect));
        pose.paint(scene, transform, bounds, |scene, transform| {
            self.paint_frame(scene, transform, text, theme, presence, rect);
        });
    }

    /// What the window holds is cut at its edge: a list scrolled past its top is not seen over
    /// the window behind it, and a press there does not reach it.
    fn child_clip_rect(&self) -> Option<Rect> {
        (self.rect.width() > 0.0).then_some(self.rect)
    }

    fn take_structure_dirty(&mut self) -> bool {
        let relayout = std::mem::take(&mut self.relayout);
        self.layers.take_structure_dirty() || relayout
    }

    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        Widget::take_structure_change(&mut self.layers)
    }

    fn name(&self) -> &'static str {
        "WindowFrame"
    }

    /// The window is found by the key it stands under, so a surface that pans can bring the
    /// one that just arrived into view.
    fn test_id(&self) -> Option<&str> {
        (!self.key.is_empty()).then_some(self.key.as_str())
    }

    /// The frame is a group named for the window, and what it holds is read inside it.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.name.as_ref().map(|name| {
            crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                .with_optional_name(Some(name.clone()))
        })
    }
}

impl WindowFrame {
    /// The window itself, drawn where `rect` says: what [`Decorator::draw`] draws once the
    /// window is posed where its arrival or move has reached.
    fn paint_frame(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        rect: Rect,
    ) {
        let radius = SurfaceLevel::Window.radius(theme).min(rect.height() / 2.0);
        if self.focused {
            crate::surface::floating_shadow(scene, transform, rect, radius, 1.0, theme);
        }
        let on = paint_surface(scene, transform, rect, SurfaceLevel::Window, theme);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &RoundedRect::from_rect(rect, radius),
        );
        // // The content under the window's own pill under the window's own controls: the pill // stands
        // over the app's header row where the app's row holds the bar, and over the // frame's bar
        // otherwise, so the window's controls are the frame's wherever they are.
        let pill = self.pill;
        {
            let mut children = self.layers.children_mut();
            if let Some(content) = children.get_mut(CONTENT) {
                content.draw_child(scene, transform, text, &on, presence);
            }
        }
        crate::widget::window_controls::paint_pill(scene, transform, pill, theme);
        {
            let mut children = self.layers.children_mut();
            if let Some(controls) = children.get_mut(CONTROLS) {
                controls.draw_child(scene, transform, text, &on, presence);
            }
        }
        scene.pop_layer();
        if self.focused
            && crate::surface::border(theme).is_none()
            && theme.finish(crate::tokens::SurfaceRole::Window).hairline()
        {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                theme.tokens.colors.window_edge_focused(),
                None,
                &RoundedRect::from_rect(rect.inflate(0.5, 0.5), radius + 0.5),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{Described, Event, ResourceId};
    use crate::widget::{TabItemProps, TabStrip, TabStripProps};

    /// A window's tabs, as the shell's strip hands them to its frame: two places, the first in
    /// front.
    fn two_places() -> Box<TabStrip> {
        let tab = |key: &str, title: &str| TabItemProps {
            key: key.to_owned(),
            item: Some(ResourceId(format!("@notes.Note:{key}"))),
            title: Some(title.to_owned()),
            icon: None,
            attention: false,
            modified: false,
            busy: false,
            presence: Vec::new(),
        };
        let tabs = vec![tab("a", "Sorting"), tab("b", "Second")];
        Box::new(TabStrip::from_props(
            "frame",
            TabStripProps {
                key: "frame".to_owned(),
                active: tabs.first().and_then(|t| t.item.clone()),
                focused: true,
                activate: Event::None,
                close: Event::None,
                reorder: Event::None,
                detach: Event::None,
                tabs,
            },
        ))
    }

    /// The window drawn on its ground at `rect`, rendered: what a person sees around its edge.
    fn rendered(theme: &Theme, focused: bool, rect: Rect) -> Vec<u8> {
        let mut frame = WindowFrame::from_props(
            "w",
            WindowFrameProps {
                name: Some("A window".to_owned()),
                focused,
                ..WindowFrameProps::default()
            },
        );
        frame.rect = rect;
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            theme.tokens.colors.surfaces.base,
            None,
            &Rect::new(0.0, 0.0, 240.0, 240.0),
        );
        let mut text = crate::font_catalog::fixture_renderer();
        Widget::draw(
            &mut frame,
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            theme,
            &PresenceCtx::empty(),
        );
        crate::offscreen::OffscreenRenderer::new(240, 240)
            .render(&scene, vello::peniko::Color::BLACK)
    }

    /// How far the pixel at (`x`, `y`) stands from the ground, in relative luminance.
    fn from_ground(rgba: &[u8], theme: &Theme, x: u32, y: u32) -> f64 {
        let (r, g, b, _) = crate::offscreen::OffscreenRenderer::sample(rgba, 240, x, y);
        let seen = vello::peniko::Color::from_rgb8(r, g, b);
        (crate::a11y::relative_luminance(seen)
            - crate::a11y::relative_luminance(theme.tokens.colors.surfaces.base))
        .abs()
    }

    #[test]
    fn the_window_in_front_stands_apart_from_the_ground_by_its_edge_and_its_shadow() {
        let rect = Rect::new(40.0, 40.0, 200.0, 160.0);
        for (theme, dark) in [(Theme::dark(), true), (Theme::light(), false)] {
            let front = rendered(&theme, true, rect);
            let behind = rendered(&theme, false, rect);
            // // The pixel row just outside the top edge, away from the corners: the edge.
            let edge = |rgba: &[u8]| from_ground(rgba, &theme, 120, 39);
            assert!(
                edge(&front) > edge(&behind) + crate::tokens::colors::HAIRLINE_MIN_DELTA,
                "the window in front has the heavier edge: {} over {}",
                edge(&front),
                edge(&behind)
            );
            assert!(
                edge(&behind) >= crate::tokens::colors::HAIRLINE_MIN_DELTA / 2.0,
                "a window behind still has an edge on the ground: {}",
                edge(&behind)
            );
            let shadow = |rgba: &[u8]| from_ground(rgba, &theme, 120, 172);
            let lighter = if dark {
                shadow(&front) >= shadow(&behind)
            } else {
                shadow(&front) > shadow(&behind)
            };
            assert!(
                lighter,
                "a window behind casts the lighter shadow: {} under {}",
                shadow(&behind),
                shadow(&front)
            );
        }
    }

    /// Lay `frame` out at `rect`, twice: the host lays a window out again once its controls
    /// have measured.
    fn laid_out(frame: &mut WindowFrame, rect: Rect) {
        let theme = Theme::dark();
        let mut text = crate::font_catalog::fixture_renderer();
        for _ in 0..2 {
            crate::widget::layout::lay_out_detached(frame, rect, &theme, &mut text)
                .expect("the window lays out");
        }
    }

    /// The frame's own bar and its content, laid out.
    fn parts(frame: &WindowFrame) -> (Rect, Rect) {
        let content = frame.layers.children()[CONTENT].children();
        (content[OWN_BAR].get_rect(), content[BODY].get_rect())
    }

    #[test]
    fn the_window_fills_its_frame_under_one_bar() {
        let rect = Rect::new(0.0, 0.0, 480.0, 360.0);
        let mut frame = WindowFrame::from_props(
            "w",
            WindowFrameProps {
                name: Some("A terminal".to_owned()),
                focused: true,
                body: Some(Box::new(Spacer::new())),
                ..WindowFrameProps::default()
            },
        );
        laid_out(&mut frame, rect);
        let (bar, body) = parts(&frame);
        assert!(
            (bar.height() - TAB_BAR_HEIGHT).abs() < 0.5,
            "the window's bar is one bar tall: {bar:?}"
        );
        assert!(
            (body.y0 - bar.y1).abs() < 0.5 && (body.y1 - rect.y1).abs() < 0.5,
            "the window's content fills it under its one bar: {body:?} under {bar:?}"
        );
    }

    #[test]
    fn a_window_of_several_places_is_its_tabs_and_the_place_in_front() {
        let rect = Rect::new(0.0, 0.0, 480.0, 360.0);
        let mut frame = WindowFrame::from_props(
            "w",
            WindowFrameProps {
                name: Some("Two notes".to_owned()),
                focused: true,
                tabs: Some(two_places()),
                body: Some(Box::new(Spacer::new())),
                ..WindowFrameProps::default()
            },
        );
        laid_out(&mut frame, rect);
        assert_eq!(frame.shape, BarShape::Tabs, "the tabs are the window's bar");
        let (bar, body) = parts(&frame);
        assert!(
            (bar.height() - TAB_BAR_HEIGHT).abs() < 0.5,
            "one bar: {bar:?}"
        );
        let own = frame.layers.children()[CONTENT].children();
        let own = own[OWN_BAR].children();
        assert!(
            own[NAME].get_rect().height() < 0.5,
            "the tabs name the window, not the frame: {:?}",
            own[NAME].get_rect()
        );
        assert!(
            (body.y0 - bar.y1).abs() < 0.5 && (body.y1 - rect.y1).abs() < 0.5,
            "the place in front fills the window under the tabs: {body:?} under {bar:?}"
        );
    }

    #[test]
    fn a_window_behind_is_its_decorations_alone() {
        let rect = Rect::new(0.0, 0.0, 480.0, 360.0);
        let mut frame = WindowFrame::from_props(
            "w",
            WindowFrameProps {
                name: Some("A terminal".to_owned()),
                focused: false,
                body: Some(Box::new(Spacer::new())),
                ..WindowFrameProps::default()
            },
        );
        laid_out(&mut frame, rect);
        assert_eq!(frame.shape, BarShape::Bare);
        let own = frame.layers.children()[CONTENT].children();
        let own = own[OWN_BAR].children();
        assert!(
            own[NAME].get_rect().height() < 0.5,
            "a window behind shows no name of its own: {:?}",
            own[NAME].get_rect()
        );
    }

    /// A window's motion: it glides to a new place over several frames, reaches it, then rests
    /// asking for no frames — and a still theme or reduced motion lands it at once.
    #[test]
    fn a_window_glides_to_its_new_place_and_rests_asking_for_no_frames() {
        let _frame = crate::motion::pin_frame();
        let mut frame = WindowFrame::from_props(
            "w",
            WindowFrameProps {
                name: Some("A note".to_owned()),
                focused: true,
                body: Some(Box::new(Spacer::new())),
                ..WindowFrameProps::default()
            },
        );
        let now = crate::motion::frame_now();
        let at = |x: f64| Rect::from_origin_size((x, 40.0), (240.0, 160.0));
        frame.placed.follow(
            at(40.0),
            true,
            Theme::dark().motion(),
            crate::motion::input_source(),
            now,
        );
        assert!(
            !frame.placed.is_moving(now),
            "a window first placed rests where it is laid out"
        );

        frame.placed.follow(
            at(400.0),
            true,
            Theme::dark().motion(),
            crate::tokens::MotionSource::Pointer,
            now,
        );
        let start = frame
            .placed
            .movement()
            .sampled_after(crate::time::Duration::ZERO)
            .0;
        assert!(
            (start.x0 - 40.0).abs() < 3.0,
            "it starts where it stood: {start:?}"
        );
        let mid = frame
            .placed
            .movement()
            .sampled_after(crate::time::Duration::from_millis(80))
            .0;
        assert!(
            mid.x0 > start.x0 + 1.0 && mid.x0 < 400.0 - 1.0,
            "it is on its way: {mid:?}"
        );
        let arrived = frame
            .placed
            .movement()
            .sampled_after(crate::time::Duration::from_secs(3))
            .0;
        assert!((arrived.x0 - 400.0).abs() < 0.5, "it arrives: {arrived:?}");
        assert!(
            !frame
                .placed
                .is_moving(now + crate::time::Duration::from_secs(3))
        );
    }

    /// A still theme, and reduced motion, land the window where it is laid out on the frame it
    /// changes.
    #[test]
    fn a_still_theme_and_reduced_motion_land_a_window_at_once() {
        let _frame = crate::motion::pin_frame();
        let still = crate::tokens::EffectiveMotion::new(
            crate::tokens::Motion {
                still: true,
                ..crate::tokens::Motion::default()
            },
            false,
        );
        let reduced = crate::tokens::EffectiveMotion::new(crate::tokens::Motion::default(), true);
        for motion in [still, reduced] {
            let mut frame = WindowFrame::from_props(
                "w",
                WindowFrameProps {
                    name: Some("A note".to_owned()),
                    body: Some(Box::new(Spacer::new())),
                    ..WindowFrameProps::default()
                },
            );
            let at = |x: f64| Rect::from_origin_size((x, 40.0), (240.0, 160.0));
            let now = crate::motion::frame_now();
            frame.placed.follow(
                at(40.0),
                true,
                motion,
                crate::tokens::MotionSource::Pointer,
                now,
            );
            frame.placed.follow(
                at(400.0),
                true,
                motion,
                crate::tokens::MotionSource::Pointer,
                now,
            );
            assert!(
                (frame.placed.rect(now).x0 - 400.0).abs() < 0.5,
                "it lands at its place at once"
            );
            assert!(
                !frame.placed.is_moving(now),
                "and it asks for no frame to get there"
            );
        }
    }

    /// The window's bar is its handle: a press on the bar's empty area that travels publishes
    /// the resource the frame carries on the drag bus, and letting go over a drop target hands
    /// it there. A press inside the body is the content's, never the bar's.
    #[test]
    fn a_drag_on_the_window_bar_carries_the_resource_it_holds() {
        use crate::widget::drag_bus;
        use taffy::TaffyTree;
        let mut frame = Box::new(WindowFrame::from_props(
            "w",
            WindowFrameProps {
                key: "w".to_owned(),
                name: Some("Design crit".to_owned()),
                carries: Some("chat.workspace/channels/c_crit".to_owned()),
                body: Some(Box::new(Spacer::new())),
                ..WindowFrameProps::default()
            },
        ));
        let theme = Theme::dark();
        let rect = Rect::new(0.0, 0.0, 480.0, 360.0);
        let mut taffy = TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        let node = Widget::build_layout(frame.as_mut(), &mut taffy, &theme, &mut nodes);
        if let Ok(mut style) = taffy.style(node).cloned() {
            style.size = taffy::Size::from_lengths(480.0_f32, 360.0_f32);
            let _ = taffy.set_style(node, style);
        }
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(480.0),
                    height: taffy::AvailableSpace::Definite(360.0),
                },
            )
            .expect("the window solves its layout");
        Widget::apply_layout(frame.as_mut(), &taffy, node, rect.origin());
        let bar = frame.bar;
        assert!(bar.height() > 0.0, "the frame draws a bar: {bar:?}");
        let mut send = |event: &WindowEvent| {
            let mut doc = automerge::AutoCommit::new();
            crate::testing::Dispatch::new()
                .doc(&mut doc)
                .layout(&taffy, node)
                .send(frame.as_mut(), event);
        };
        let (from_x, from_y) = (bar.x0 + bar.width() / 2.0, bar.center().y);
        send(&crate::synthetic_input::cursor_moved(from_x, from_y));
        send(&crate::synthetic_input::mouse_press());
        for step in 1..=4 {
            let dy = 12.0 * f64::from(step);
            send(&crate::synthetic_input::cursor_moved(from_x, from_y + dy));
        }
        let in_flight = drag_bus::current();
        let target = Rect::new(60.0, 200.0, 400.0, 320.0);
        send(&crate::synthetic_input::cursor_moved(200.0, 260.0));
        send(&crate::synthetic_input::mouse_release());
        let landed = drag_bus::take("composer", target);
        drag_bus::end(None);
        let in_flight = in_flight.expect("the bar published what it carries");
        assert_eq!(
            in_flight.source, "w",
            "the drag names the frame it came from"
        );
        assert_eq!(
            in_flight.label, "Design crit",
            "the ghost says the window's name"
        );
        let landed = landed.expect("the drop landed in the target it was let go over");
        let crate::dnd::DragPayload::Custom { kind, blob } = landed.payload else {
            panic!("the bar carries a resource");
        };
        assert_eq!(kind, crate::widget::drag_source::RESOURCE_PAYLOAD);
        assert_eq!(
            String::from_utf8(blob).expect("an address"),
            "chat.workspace/channels/c_crit",
            "the drop carries the resource the bar holds"
        );

        let (bx, by) = (bar.x0 + bar.width() / 2.0, bar.y1 + 120.0);
        send(&crate::synthetic_input::cursor_moved(bx, by));
        send(&crate::synthetic_input::mouse_press());
        for step in 1..=4 {
            send(&crate::synthetic_input::cursor_moved(
                bx + 12.0 * f64::from(step),
                by,
            ));
        }
        let body_drag = drag_bus::current();
        drag_bus::end(None);
        assert!(
            body_drag.is_none(),
            "a press on the body is the content's, never the bar's"
        );
    }
}
