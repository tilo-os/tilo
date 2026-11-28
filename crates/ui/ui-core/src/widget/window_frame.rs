//! A window on a shell, as a described component: what a shell draws for a place on a
//! floating shell (`shell.md` §4, `frame_classic`) and for a strip of places (`strips_view`),
//! which is the same thing.
//!
//! Its ground is the window level of the design language ([`SurfaceLevel::Window`]): the app's
//! content colour, the window edge and its soft shadow at the theme's window radius, with what it
//! holds clipped to that shape; the window in front stands higher than those behind it, as a
//! shell shows which window has the keyboard.
//!
//! A window has one bar, never two stacked, and the bar says what state the window is in:
//!
//! - several places: the bar is their tabs, the window's content the place in front;
//! - one place whose view has a header row: the frame offers its window bar to that row
//!   ([`WindowHeader::set_window_bar`](crate::widget::WindowHeader)), so the app's title and
//!   actions and the frame's controls read as one bar;
//! - one place with no header row of its own, in front: the frame's own bar names it;
//! - one such place behind: the bar is the decorations alone, as a shell draws a window nobody
//!   is typing into.
//!
//! The window's own controls stand in a pill of the floating material at the trailing end of the
//! bar in every state, and the app's own parts stay the app's. A view says which window is in
//! front and what its controls are; the theme decides everything the frame is made of.

use taffy::{NodeId, TaffyTree};
use vello::kurbo::{Point, Rect, RoundedRect, Vec2};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::widget::arrange::{Align, At, Fit, Layers, Part, Stack};
use crate::widget::primitives::{SurfaceLevel, paint_surface};
use crate::widget::tabs::{TAB_BAR_HEIGHT, TabBar, TabStrip};
use crate::widget::window_controls::WindowControls;
use crate::widget::window_header::header_row::HeaderRowMetrics;
use crate::{
    widget::IconSize, widget::IconWidget, widget::Text, widget::WindowBar, widget::WindowBarPart,
};
use tilo_ui_base::icons::Icon;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::DrawChild;
use tilo_ui_base::widget::Length;
use tilo_ui_base::{
    component::Component, component::Described, component::Event, component::PartViews,
};
use tilo_ui_base::{tokens::Rhythm, tokens::SpacingStep, tokens::TextRole};
use tilo_ui_base::{widget::Decorator, widget::Element, widget::Spacer, widget::Widget};

/// A window on a shell: the window surface with its edge and shadow, holding the
/// window's content and its controls, clipped to its radius.
#[derive(Component)]
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
    /// The glyph the frame's own bar shows before the window's name, by its icon name (the app's
    /// own, `w.icon`): a window whose content has no header row still says what it shows.
    pub glyph: Option<String>,
    /// Whether it is the window in front, the one the keyboard goes to.
    pub focused: bool,
    /// The shell lists this window's actions where the person reaches them (the shell
    /// bar's window menu): its header's actions that do not fit give way with no API of
    /// their own. Without it what does not fit goes behind the header's API.
    pub lists_actions: bool,
    /// The frame's controls (close, and what else the shell offers on the window), at the
    /// trailing end of the window's bar.
    #[component(part)]
    pub controls: Option<Element>,
    /// The places the window shows, as its tabs: the bar is theirs while there are any, and
    /// the content is the tab in front. Empty for a window of one place, whose bar is the
    /// place's own header or the frame's.
    #[component(part)]
    pub tabs: Option<Element>,
    /// The resource the window's bar carries when a person picks it up, by its published
    /// address: a place on the canvas is dragged onto a composer, or back onto the canvas, by
    /// its own bar ([`ui.DragSource`](super::drag_source::DragSource), `shell.md` §8.1). Empty
    /// leaves the bar no source, as a foreign window's bar has nothing to carry.
    pub carries: Option<String>,
    /// The name a scroll view around this window keeps in view (`scroll(..., keep:)`): the front
    /// strip of a row, which comes into view as the person moves between windows. Unset, the
    /// window asks for nothing.
    #[component(data)]
    pub keep: Option<String>,
    /// Whether the window has just closed: its frame is drawn where it stood and goes back into the
    /// control it was asked for from, taking nothing meanwhile. The shell keeps a closed window in
    /// its layout reading for its departure and draws it here.
    pub leaving: bool,
    /// The control the person pressed for it, in the shell's pixels as `x, y`: the dock icon or the
    /// search row a window comes out of and goes back into. Empty for a window a shortcut or an
    /// agent asked for, which lands and grows where it stands.
    pub control: Vec<i64>,
    /// Whether the window's bar takes a press now.
    pub enabled: bool,
    /// Why it cannot be pressed now, where the name does not say (an action whose requirement
    /// does not hold): spoken as its description beside the name.
    pub description: Option<String>,
    /// Whether the window's trailing edge resizes it: a strip's window, whose row shares its
    /// width out among the windows beside it. A window on a canvas or a lone floating window
    /// takes its size from the shell's own gesture there.
    pub resizable: bool,
    /// The least width the edge may leave the window, in logical pixels: the room its row keeps
    /// every window it shares out (a strip's `44rem`). Unset leaves the toolkit's own least.
    pub narrowest: Option<f64>,
    /// Whether a window stands before it in its row, so its leading edge is a border a person can
    /// carry too. The border between two windows is the previous window's trailing edge, and the
    /// drag resizes the one whose width it changes.
    pub leads: bool,
    /// Whether its leading corners meet the display's edge, where the row starts there: a corner
    /// at the screen is square, since a rounded one would bite a hole out of the screen.
    pub square_leading: bool,
    /// Whether its trailing corners meet the display's edge, where the row ends there.
    pub square_trailing: bool,
    /// A press on the window's bar, where nothing on it took the press: the person reaching for
    /// the window, which the shell answers by bringing it forward with the keyboard in it.
    #[component(event(press))]
    pub press: Event,
    /// An edge was carried and let go: the width it is taking, in logical pixels, the width it had
    /// when it was grabbed, which edge it was (`0` leading, `1` trailing), and whether Shift was
    /// held (a trade of width with the window across the border). The shell turns that into the
    /// shares its row gives its windows.
    #[component(event(value = Vec<i64>))]
    pub resize: Event,
    /// The window's content, filling the window.
    #[component(part(default))]
    pub body: Option<Element>,
}

/// A window takes a press on its bar until something says otherwise.
impl Default for WindowFrameProps {
    fn default() -> Self {
        Self {
            key: String::new(),
            name: None,
            glyph: None,
            focused: false,
            lists_actions: false,
            controls: None,
            tabs: None,
            carries: None,
            keep: None,
            leaving: false,
            control: Vec::new(),
            enabled: true,
            description: None,
            resizable: false,
            narrowest: None,
            leads: false,
            square_leading: false,
            square_trailing: false,
            press: Event::None,
            resize: Event::None,
            body: None,
        }
    }
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
    /// The part a host is changing: 0 for the controls, 1 for the tabs, 2 for the content.
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
    /// The glyph the frame's own bar stands before the name: the app's own.
    glyph: Option<Icon>,
    /// The glyph the bar last built.
    shown_glyph: Option<Icon>,
    /// The header row's measures in the theme of the last layout.
    metrics: Option<HeaderRowMetrics>,
    rect: Rect,
    /// The window's bar this layout: where the tabs, the name and the controls stand.
    bar: Rect,
    /// The pill around the controls, over the bar's ground or over the app's header row.
    pill: Rect,
    /// The window arriving where it stands, or out of the control the person pressed for it, and
    /// gliding when its place changes (`motion::Placed`: `WindowMotion::OPEN` and `MOVE`, landing at
    /// once for a still theme and reduced motion). Drawn as a component of the shell's do: a window a
    /// view builds again each time the layout changes still moves rather than jumping.
    placed: tilo_ui_base::motion::Placed,
    /// The resource the bar carries when it is picked up, by its published address: what a
    /// press on the bar's empty area publishes on the drag bus once it travels past the
    /// threshold, so a place on the canvas reaches a composer or the canvas itself by its own
    /// bar. `None` for a window whose bar carries nothing (a foreign window's).
    carries: Option<String>,
    /// A press on the bar that may become a drag.
    lift: Option<Lift>,
    /// The name the scroll view around it keeps in view, as the view set it.
    keep: Option<String>,
    /// Whether the window has just closed: its frame goes back into its control and takes nothing.
    leaving: bool,
    /// What a press on the bar does.
    press: tilo_ui_base::widget::Press,
    /// Whether the bar takes a press now, and why it cannot when it does not.
    enabled: bool,
    description: Option<String>,
    /// Where the pointer last was, in the frame's logical coordinates.
    cursor: Point,
    /// The pointer rests on the bar.
    on_bar: bool,
    /// Whether the window's trailing edge resizes it, and what a resize reports.
    resizable: bool,
    /// The least width a carried edge leaves it, in logical pixels.
    narrowest: Option<f64>,
    /// Whether a window stands before it, so its leading edge is a border too.
    leads: bool,
    /// Square corners where the window meets the display's edges.
    square_leading: bool,
    square_trailing: bool,
    resize: Event,
    /// A press on the trailing edge being carried.
    grip: Option<Grip>,
    /// The edge light and the size chip the resize shows.
    feedback: crate::widget::ResizeFeedback,
    /// The motion the theme allows, sampled by the latest layout: the pace the chip fades at.
    motion: tilo_ui_base::tokens::EffectiveMotion,
    /// The edge's light or chip changed since the last draw: the frame repaints where it stands.
    resize_dirty: bool,
    /// Where the window stands for the shell's control hints ([`crate::control_hints`]).
    anchor: crate::control_hints::anchors::WindowAnchor,
    /// The window's resizing edge, for the hint that teaches it beside the window.
    hint_edges: crate::control_hints::anchors::EdgesAnchor,
}

/// A press on a window's edge, carried: which edge, the width the window had when it was grabbed,
/// where the pointer pressed and whether Shift was held, so every move says the width the edge is
/// taking and letting go says what the row's shares follow.
#[derive(Clone, Copy, Debug)]
struct Grip {
    side: crate::widget::EdgeSide,
    press_x: f64,
    width: f64,
    height: f64,
    /// Whether the drag trades width with the window across the border instead of widening this one.
    zero_sum: bool,
    /// The width the pointer has carried the edge to, which the chip says.
    taking: f64,
}

/// How deep the band along a window's edge is where a press carries it: the same grip every
/// window edge in the shell offers (`tokens::Layout::resize_grip`).
fn edge_grip() -> f64 {
    f64::from(tilo_ui_base::tokens::Layout::BASE.resize_grip)
}

/// The least width an edge drag leaves a window: enough for its bar and its controls.
const LEAST_WIDTH: f64 = 160.0;

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

/// In the frame's own bar: the window's tabs, its name, then the room the controls keep.
const TABS: usize = 0;
const NAME: usize = 1;
const AFTER: usize = 2;

fn empty() -> Element {
    Box::new(Spacer::token(SpacingStep::S0))
}

/// The control a shell's reading gives a window, as `x, y` in its pixels: the dock icon or the
/// search row it was asked for from, or nothing when the reading names none.
fn control_of(control: &[i64]) -> Option<Point> {
    match control {
        [x, y] => {
            #[allow(
                clippy::cast_precision_loss,
                reason = "a point on a screen, far inside the integers f64 holds exactly"
            )]
            let (x, y) = (*x as f64, *y as f64);
            Some(Point::new(x, y))
        }
        _ => None,
    }
}

/// How the tabs stand in the frame's own bar: the bar of a window of several places is
/// theirs, so they take its width; a window of one leaves that part empty.
fn tabs_fit(tabbed: bool) -> Fit {
    if tabbed { Fit::fill(1.0) } else { Fit::hug() }
}

/// How the name stands in the bar: a window of one place gives the room the tabs leave and the
/// controls do not keep to its name, which elides itself in it; a window of several shows its
/// tabs and no name.
fn name_fit(tabbed: bool) -> Fit {
    if tabbed { Fit::hug() } else { Fit::fill(1.0) }
}

/// The room the window's controls keep at the bar's trailing end, in logical pixels: up to the
/// three a shell puts there (the window's menu, minimize and close, or the like), each a shell
/// button the bar's own side, in the pill around them, and the bar's inset from the window's
/// edge. The frame keeps it before it lays out, so its bar and the header row an app draws
/// reserve it in the one solve, never from a width measured after one.
fn controls_room(m: &HeaderRowMetrics) -> f64 {
    use crate::widget::window_controls::{LEAD_GAP, PILL_PAD};
    const MOST: f64 = 3.0;
    MOST * m.chip + (MOST - 1.0) * m.gap + 2.0 * PILL_PAD + m.inset + LEAD_GAP
}

/// Whether `widget` holds the window's tabs: a view names the tabs part on every window, so
/// what the part holds says whether this window has tabs, never the part's mere presence.
fn holds_tabs(widget: &dyn Widget) -> bool {
    let tabs = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<TabStrip>())
        .is_some();
    tabs || widget.children().iter().any(|child| holds_tabs(*child))
}

/// The bar's tab strip under `widget`, handed to `ask`: a window of one tab is its title, so the
/// frame around it takes the whole window from a press the strip would otherwise keep.
fn with_tabs_bar<R>(widget: &mut dyn Widget, ask: &mut dyn FnMut(&mut TabBar) -> R) -> Option<R> {
    if let Some(strip) = widget
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<TabStrip>())
    {
        return Some(ask(TabStrip::bar_mut(strip)));
    }
    for child in widget.children_mut() {
        if let Some(asked) = with_tabs_bar(child, ask) {
            return Some(asked);
        }
    }
    None
}

fn part(element: Element, fit: Fit) -> Part {
    let mut part = Part::new(element);
    part.fit = fit;
    part
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
            glyph,
            focused,
            lists_actions,
            controls,
            tabs,
            carries,
            keep,
            leaving,
            control,
            enabled,
            description,
            resizable,
            narrowest,
            leads,
            square_leading,
            square_trailing,
            press,
            resize,
            body,
        } = props;
        let tabbed = tabs.as_deref().is_some_and(holds_tabs);
        // The bar's parts stand edge to edge: the name's own gutter is the one inset before it,
        // so it starts on the content's edge rather than a gap past it. The room after the name
        // is the controls', which the layers draw there and the row keeps for them.
        let own = Stack::row()
            .align(Align::Center)
            .gap(Rhythm::None)
            .push_part(part(tabs.unwrap_or_else(empty), tabs_fit(tabbed)))
            .push_part(part(empty(), name_fit(tabbed)))
            .push_part(part(empty(), Fit::hug()));
        let column = Stack::stack()
            .fill_width()
            .fill_height()
            .gap(Rhythm::None)
            .push_part(part(Box::new(own), bar_fit(tabbed)))
            .push_part(part(body.unwrap_or_else(empty), Fit::fill(1.0)));
        let controls = Stack::row()
            .align(Align::Center)
            .push(controls.unwrap_or_else(empty));
        let layers = Layers::layers()
            .push_part(part(Box::new(column), Fit::fill(1.0)))
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
            glyph: glyph.as_deref().and_then(Icon::from_name),
            shown_glyph: None,
            metrics: None,
            rect: Rect::ZERO,
            bar: Rect::ZERO,
            pill: Rect::ZERO,
            placed: if leaving {
                // A window the shell keeps only as it leaves: it starts whole where it stood and
                // goes from there, back into the control it was asked for from.
                tilo_ui_base::motion::Placed::leaving().from_control(control_of(&control))
            } else if tilo_ui_base::motion::window_has_shown() {
                tilo_ui_base::motion::Placed::closed()
                    .from_control(control_of(&control))
                    // A window the person pressed for comes out of the control they pressed — the
                    // dock icon or the search row. A window a shortcut or an agent asked for lands
                    // where it stands, and one in the session's first frame rests.
                    .arriving_out_of_pointer()
            } else {
                tilo_ui_base::motion::Placed::opened()
            },
            carries,
            lift: None,
            keep,
            leaving,
            press: tilo_ui_base::widget::Press::of(press),
            enabled,
            description,
            cursor: Point::ZERO,
            on_bar: false,
            resizable,
            narrowest,
            leads,
            square_leading,
            square_trailing,
            resize,
            grip: None,
            feedback: crate::widget::ResizeFeedback::new(),
            motion: tilo_ui_base::tokens::EffectiveMotion::default(),
            resize_dirty: false,
            anchor: crate::control_hints::anchors::WindowAnchor::new(),
            hint_edges: crate::control_hints::anchors::EdgesAnchor::new(),
        }
    }

    fn set_props(&mut self, props: WindowFrameProps) -> bool {
        // The props an update in place reads carry no part views: whether the bar is the window's
        // tabs is told by the part the view gave, which is reconciled where it stands. The shape
        // follows it there (`take_structure_dirty`), so a frame that holds a carried tab or a
        // window being dragged is never built again under the person's hand.
        if props.tabs.is_some() && props.tabs.as_deref().is_some_and(holds_tabs) != self.tabbed {
            return false;
        }
        let glyph = props.glyph.as_deref().and_then(Icon::from_name);
        if self.name != props.name || self.glyph != glyph {
            self.title = None;
        }
        self.key = props.key;
        self.name = props.name;
        self.glyph = glyph;
        self.focused = props.focused;
        self.lists_actions = props.lists_actions;
        self.carries = props.carries;
        self.keep = props.keep;
        self.leaving = props.leaving;
        self.placed.set_control(control_of(&props.control));
        self.enabled = props.enabled;
        self.description = props.description;
        self.resizable = props.resizable;
        self.narrowest = props.narrowest;
        self.leads = props.leads;
        self.square_leading = props.square_leading;
        self.square_trailing = props.square_trailing;
        self.resize = props.resize;
        self.press = tilo_ui_base::widget::Press::of(props.press);
        true
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        self.editing = match part {
            "controls" => 0,
            "tabs" => 1,
            "body" => 2,
            _ => return None,
        };
        Some(self)
    }
}

/// The view of the part being changed: the controls, the tabs, or the content.
impl PartViews for WindowFrame {
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

    /// The row of the frame's own bar: the window's tabs, its name, and the width the controls
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

    /// The header row the window's content draws, as the frame laid it out last.
    pub fn content_header_row(&self) -> Option<&dyn crate::widget::WindowHeader> {
        let column = *self.layers.children().first()?;
        crate::widget::window_header::root_header_row(column)
    }

    /// Where the content's header row stands while it holds the window's bar; `None` while the
    /// frame draws a bar of its own.
    pub fn held_bar(&self) -> Option<WindowBarPart> {
        if !self.merged {
            return None;
        }
        let column = *self.layers.children().first()?;
        crate::widget::window_header::content_bar(column)
    }

    /// The window's bar as the frame laid it out last: the content's header row while it holds
    /// the bar, the frame's own bar otherwise.
    pub fn bar_rect(&self) -> Rect {
        self.bar
    }

    /// The bar the frame draws of its own above the content, as laid out last: empty while the
    /// content's header row holds the bar.
    pub fn own_bar_rect(&self) -> Rect {
        self.layers
            .children()
            .first()
            .and_then(|column| column.children().get(OWN_BAR).copied())
            .map_or(Rect::ZERO, Widget::get_rect)
    }

    /// Put `view` in the part being changed.
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
                    row.set_placement(NAME, name_fit(tabbed), 1, None);
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
            controls_width: controls_room(m),
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
    fn build_bar(&mut self, shape: BarShape, m: &HeaderRowMetrics) {
        self.shape = shape;
        let bar = shape != BarShape::Merged;
        if let Some(column) = self.column() {
            column.set_placement(OWN_BAR, bar_fit(bar), 1, None);
        }
        let title = match shape {
            BarShape::Title => Some((self.name.clone().unwrap_or_default(), self.focused)),
            _ => None,
        };
        if title == self.title && self.glyph == self.shown_glyph {
            return;
        }
        self.title = title;
        self.shown_glyph = self.glyph;
        let element = self.name_element(m);
        if let Some(own) = self.own_bar_row() {
            let _ = own.replace(NAME, element);
        }
    }

    /// The name the frame's own bar shows, eliding itself to the one line the width layout gives
    /// it: a name too long for the bar ends in an ellipsis, never in a second line taller than
    /// the bar or under the controls, whose room the row keeps.
    fn name_element(&self, m: &HeaderRowMetrics) -> Element {
        let glyph = self
            .shown_glyph
            .map(|icon| Box::new(IconWidget::new(icon).sized(IconSize::Small)) as Element);
        let name = self.title.clone().filter(|(name, _)| !name.is_empty());
        if glyph.is_none() && name.is_none() {
            return empty();
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a header row measure fits f32"
        )]
        let gutter = Spacer::fixed(m.gutter as f32);
        let mut row = Stack::row()
            .align(Align::Center)
            .gap(Rhythm::None)
            .push(Box::new(gutter));
        if let Some(glyph) = glyph {
            row = row.push(glyph);
        }
        if let Some((name, focused)) = name {
            let mut title = Text::new(&name)
                .with_role(TextRole::Label)
                .with_truncate(true);
            if !focused {
                title = title.dimmed();
            }
            row = row.push_part(part(Box::new(title), Fit::fill(1.0)));
        }
        Box::new(row)
    }

    /// The bar's row: the content's header row when it holds the bar, else the frame's own.
    fn bar_row(&self, m: &HeaderRowMetrics) -> Rect {
        if self.merged {
            return self
                .layers
                .children()
                .first()
                .and_then(|column| crate::widget::window_header::window_bar_part(*column))
                .map_or_else(
                    || {
                        Rect::new(
                            self.rect.x0,
                            self.rect.y0,
                            self.rect.x1,
                            self.rect.y0 + m.height,
                        )
                    },
                    |part| part.row,
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
        if !lift.active && lift.start.distance(point) > tilo_ui_base::dnd::DRAG_START_THRESHOLD_PX {
            lift.active = true;
        }
        if !lift.active {
            return;
        }
        let title = self.name.clone().unwrap_or_default();
        crate::widget::drag_bus::carry(
            self.key.clone(),
            carries.as_bytes().to_vec(),
            title,
            point,
            self.rect,
        );
    }

    /// The pointer came to rest on the bar (`on`) or left it: resting there is where a window is
    /// taken to move it.
    fn rest_on_bar(&mut self, on: bool) {
        if on && !self.on_bar {
            crate::control_hints::performed::note(
                crate::control_hints::performed::Performed::HoveredWindowBar,
            );
        }
        self.on_bar = on;
    }

    /// The bar was let go: `Some(true)` when the press travelled and the drag ended here (the
    /// content sees no click), `Some(false)` when it never travelled.
    fn let_go(&mut self) -> Option<bool> {
        let lift = self.lift.take()?;
        if lift.active {
            // The person carried the window by its bar to where it was let go.
            crate::control_hints::performed::note(
                crate::control_hints::performed::Performed::MovedWindow,
            );
            let Some(carries) = self.carries.clone() else {
                return Some(true);
            };
            crate::widget::drag_bus::end(Some(crate::widget::drag_bus::Landed {
                source: self.key.clone(),
                payload: tilo_ui_base::dnd::DragPayload::Custom {
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

    /// The edge of the window a press at `p` would carry, while the window is one a row shares its
    /// width out among: its trailing edge, whose drag widens it, and its leading edge when a window
    /// stands before it, whose drag widens the window across the border.
    fn edge_at(&self, p: Point) -> Option<crate::widget::EdgeSide> {
        if !self.resizable
            || self.leaving
            || self.rect.width() <= 2.0 * edge_grip()
            || self.pill.contains(p)
            || p.y < self.rect.y0
            || p.y > self.rect.y1
        {
            return None;
        }
        let leading = self.leads && p.x >= self.rect.x0 && p.x <= self.rect.x0 + edge_grip();
        let trailing = p.x <= self.rect.x1 && p.x >= self.rect.x1 - edge_grip();
        if leading {
            return Some(crate::widget::EdgeSide::Leading);
        }
        trailing.then_some(crate::widget::EdgeSide::Trailing)
    }

    /// The width the carried edge is taking at `x`: the pointer's travel from the press added to
    /// the width it had for a trailing edge, taken from it for a leading one, never below the
    /// least the window may be.
    fn carried_width(&self, x: f64) -> f64 {
        let Some(grip) = self.grip else {
            return self.rect.width();
        };
        // A window the room already holds narrower than the least keeps what it has: its edge
        // never jumps wider as the drag starts.
        let least = self.narrowest.unwrap_or(LEAST_WIDTH).min(grip.width);
        let travel = x - grip.press_x;
        let wanted = match grip.side {
            crate::widget::EdgeSide::Trailing => grip.width + travel,
            _ => grip.width - travel,
        };
        wanted.max(least)
    }

    /// The pointer moved while the edge is carried: the chip says the size it is taking.
    fn carry_edge(&mut self, point: Point) {
        let taking = self.carried_width(point.x);
        if let Some(grip) = &mut self.grip {
            grip.taking = taking;
            self.feedback.carrying((taking, grip.height));
        }
    }

    /// The carried edge was let go: the width it is taking goes to whoever shares the row out,
    /// as one typed change. Let go where it was grabbed, it reports nothing.
    fn release_edge(&mut self, now: crate::time::Instant) {
        let Some(grip) = self.grip.take() else {
            return;
        };
        self.feedback.carrying((grip.taking, grip.height));
        self.feedback.end(now);
        if (grip.taking - grip.width).abs() >= 0.5 {
            crate::control_hints::performed::note(
                crate::control_hints::performed::Performed::ResizedWindow,
            );
            let (taking, width) = (grip.taking.round() as i64, grip.width.round() as i64);
            let side = match grip.side {
                crate::widget::EdgeSide::Leading => 0,
                _ => 1,
            };
            let least = self.narrowest.unwrap_or(LEAST_WIDTH).round() as i64;
            self.resize
                .fire(Some(tilo_ui_base::component::Value::List(vec![
                    tilo_ui_base::component::Value::Int(taking),
                    tilo_ui_base::component::Value::Int(width),
                    tilo_ui_base::component::Value::Int(side),
                    tilo_ui_base::component::Value::Int(i64::from(u8::from(grip.zero_sum))),
                    tilo_ui_base::component::Value::Int(least),
                ])));
        }
    }

    /// The edge's own pointer input: hover, press, carry and let go, and Escape to leave it
    /// where it was. Returns whether the edge took the event.
    fn handle_edge(
        &mut self,
        event: &WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) -> bool {
        if !self.resizable || self.leaving {
            return false;
        }
        let now = tilo_ui_base::motion::frame_now();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = ctx.logical(position);
                self.cursor = point;
                if self.grip.is_some() {
                    self.carry_edge(point);
                    *ctx.handled = true;
                    return true;
                }
                let side = self.edge_at(point);
                if side.is_some() || self.feedback.lit(now).is_some() {
                    self.resize_dirty = true;
                }
                self.feedback.hover(side, now);
                false
            }
            WindowEvent::CursorLeft { .. } => {
                if self.feedback.lit(now).is_some() {
                    self.resize_dirty = true;
                }
                self.feedback.hover(None, now);
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.grip.is_none() => {
                let Some(side) = self.edge_at(self.cursor) else {
                    return false;
                };
                self.grip = Some(Grip {
                    side,
                    press_x: self.cursor.x,
                    width: self.rect.width(),
                    height: self.rect.height(),
                    zero_sum: ctx.modifiers.state().shift_key(),
                    taking: self.rect.width(),
                });
                self.feedback
                    .begin(side, (self.rect.width(), self.rect.height()));
                self.lift = None;
                crate::widget::drag_bus::end(None);
                *ctx.focus_target = Some(ctx.node_id);
                *ctx.handled = true;
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.grip.is_some() => {
                self.release_edge(now);
                *ctx.handled = true;
                true
            }
            WindowEvent::KeyboardInput { event: key, .. }
                if self.grip.is_some()
                    && key.state == ElementState::Pressed
                    && key.logical_key == Key::Named(NamedKey::Escape) =>
            {
                self.grip = None;
                self.feedback.cancel();
                *ctx.handled = true;
                true
            }
            WindowEvent::Focused(false) if self.grip.is_some() => {
                self.grip = None;
                self.feedback.cancel();
                false
            }
            _ => false,
        }
    }

    /// Whether the bar's parts say the window shows tabs, compared with the shape it was built
    /// with: the view may take its tabs away or give it some (the style's `strips_tabs`), and a
    /// bar built the other way is built again around the part it now holds. True when the shape
    /// changed, so the host rebuilds the frame.
    fn notice_bar_shape(&mut self) -> bool {
        let tabbed = self
            .own_bar_row()
            .and_then(|row| row.child_mut(TABS))
            .is_some_and(|tabs| holds_tabs(tabs.as_ref()));
        let changed = tabbed != self.tabbed;
        if changed {
            self.tabbed = tabbed;
        }
        changed
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        // A window on its way out takes nothing: its place is gone from the layout, so a press on
        // its bar or in its content would reach an action that cannot run.
        if self.leaving {
            return;
        }
        // Its edge comes first: a press there carries the width the window takes, and reaches
        // neither its bar nor its content.
        if self.handle_edge(event, ctx) {
            return;
        }
        if self.carries.is_none() {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.cursor = ctx.logical(position);
            }
            self.layers.handle_event(event, ctx);
            if !*ctx.handled
                && matches!(
                    event,
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        button: MouseButton::Left,
                        ..
                    }
                )
                && self.enabled
                && self.bar.contains(self.cursor)
            {
                self.press.run();
            }
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = ctx.logical(position);
                self.cursor = point;
                self.rest_on_bar(self.lift.is_none() && self.bar.contains(point));
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
                self.rest_on_bar(false);
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
                if !self.bar.contains(self.cursor) {
                    return;
                }
                if !self.enabled {
                    return;
                }
                if *ctx.handled {
                    // The bar's tabs took the press. A window of one tab shows it as the bar's
                    // title, so the bar is the window's handle: the pill's own press gives way,
                    // and the whole window is taken from there.
                    let lifts = with_tabs_bar(&mut self.layers, &mut |tabs| {
                        if !(tabs.pressed_a_tab() && tabs.lifts_the_window()) {
                            return false;
                        }
                        tabs.cancel_press_for_carry();
                        true
                    });
                    if lifts != Some(true) {
                        return;
                    }
                }
                self.lift = Some(Lift {
                    start: self.cursor,
                    active: false,
                });
                *ctx.focus_target = Some(ctx.node_id);
                *ctx.handled = true;
                self.press.run();
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
        // A frame holding the pointer itself is not the only one that can: a tab carried out of
        // the window's bar, and a drag the window's content latched, keep the pointer past the
        // frame's edge only when the frame hands the capture down.
        self.lift.is_some()
            || self.grip.is_some()
            || self
                .layers
                .children()
                .iter()
                .any(|child| child.wants_pointer_capture())
    }

    /// Over a window's trailing edge the pointer shows the width can be dragged; over its bar
    /// or its content, whatever is there says.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if let Some(side) = self
            .grip
            .map(|grip| grip.side)
            .or_else(|| self.edge_at(point))
        {
            return Some(match side {
                crate::widget::EdgeSide::Trailing => winit::window::CursorIcon::EResize,
                _ => winit::window::CursorIcon::WResize,
            });
        }
        self.layers.cursor_shape(point)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.motion = theme.motion();
        let m = HeaderRowMetrics::resolve(theme);
        self.offer_bar(&m);
        self.build_bar(self.shape(), &m);
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
        // The bar's row keeps the room the controls stand in, so the name ends where they begin
        // in the same solve that places both, never from a width measured after one.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a window's bar area fits f32"
        )]
        let room = Length::Dp(controls_room(&m) as f32);
        let mut kept = Fit::hug();
        kept.width.min = Some(room);
        kept.width.max = Some(room);
        let tabbed = self.tabbed;
        if let Some(row) = self.own_bar_row() {
            row.set_placement(TABS, tabs_fit(tabbed), 1, None);
            row.set_placement(NAME, name_fit(tabbed), 1, None);
            row.set_placement(AFTER, kept, 1, None);
        }
        // The bar the frame builds here, and the places its parts take, are this build's: a
        // window built into a view asks its host for no second build of the tree around it.
        if self.metrics.is_none() {
            let _ = Widget::take_structure_change(&mut self.layers);
        }
        self.metrics = Some(m);
        self.layers.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.layers.apply_layout(taffy, node, origin);
        self.rect = self.layers.get_rect();
        let Some(m) = self.metrics else {
            return;
        };
        self.bar = self.bar_row(&m);
        let by = self.controls_offset(&m, self.bar);
        if by != Vec2::ZERO {
            self.layers.offset_child(taffy, CONTROLS, by);
        }
        // Where the controls stand is drawn and pressed, never laid out again: the pill hugs the
        // controls the view gave and the row the app's header keeps for them.
        let controls = self
            .layers
            .children()
            .get(CONTROLS)
            .map_or(Rect::ZERO, |c| c.get_rect());
        self.pill = WindowControls::around(self.bar, controls);
    }

    fn needs_redraw(&self) -> bool {
        self.layers.needs_redraw()
            || self.layers.needs_animation_frame()
            || self.resize_dirty
            || self.grip.is_some()
            || self
                .feedback
                .lit(tilo_ui_base::motion::frame_now())
                .is_some()
            || self
                .feedback
                .needs_frame(tilo_ui_base::motion::frame_now(), self.motion.fast())
    }

    /// Whether the window is arriving, going or moving: what asks for frames while it does,
    /// and what its own content asks for, and the edge light coming up or the size chip fading.
    fn needs_animation_frame(&self) -> bool {
        self.placed.is_moving(tilo_ui_base::motion::frame_now())
            || self.layers.needs_animation_frame()
            || self
                .feedback
                .needs_frame(tilo_ui_base::motion::frame_now(), self.motion.fast())
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.layers.collect_damage(regions);
        let now = tilo_ui_base::motion::frame_now();
        let drawn = self.placed.rect(now);
        if drawn != self.rect {
            regions.push(drawn.union(self.rect));
        }
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        let rect = self.rect;
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let now = tilo_ui_base::motion::frame_now();
        self.placed.follow(
            rect,
            !self.leaving,
            theme.motion(),
            tilo_ui_base::motion::input_source(),
            now,
        );
        let pose = self.placed.pose(rect, now);
        let bounds = rect.union(pose.drawn_rect(rect));
        pose.paint(scene, transform, bounds, |scene, transform| {
            self.paint_frame(
                &mut tilo_ui_base::draw::DrawCtx::new(scene, transform, text, theme, env),
                rect,
            );
        });
        // The edge light and the size the window is taking, over the window itself
        // (`docs/design-language.md`, "Window edge resize"): nothing at rest, quiet ink when
        // the pointer rests on an edge, primary ink while it is carried.
        let fade = theme.motion().fast();
        if let Some((side, lit)) = self.feedback.lit(now) {
            let alpha = self.feedback.light_alpha(now, fade);
            crate::widget::paint_edge(scene, transform, theme, rect, side, lit, alpha);
        }
        if let Some((size, side, alpha)) = self.feedback.chip(now, fade) {
            crate::widget::paint_size_chip(scene, transform, text, theme, rect, side, size, alpha);
        }
        // Where the window stands for the shell's control hints, and while a hint may show,
        // where it shows text and controls: what a hint never covers. Content too much to read
        // counts whole.
        let ink = crate::control_hints::anchors::wants_ink()
            .then(|| crate::control_hints::ink::of(&*self).unwrap_or_else(|| vec![rect]));
        self.anchor
            .state(self.focused, self.on_bar, self.carries.is_some());
        self.anchor.drawn(transform, rect, self.bar, ink);
        // The edge a person resizes this window by, so the hint that teaches it stands beside
        // that edge rather than beside a canvas's places.
        let edges: Vec<vello::kurbo::Line> = if self.resizable && !self.resize.is_none() {
            vec![crate::widget::edge_feedback::edge_line(
                rect,
                crate::widget::EdgeSide::Trailing,
            )]
        } else {
            Vec::new()
        };
        self.hint_edges.drawn(transform, edges);
    }

    /// What the window holds is cut at its edge: a list scrolled past its top is not seen over
    /// the window behind it, and a press there does not reach it.
    fn child_clip_rect(&self) -> Option<Rect> {
        (self.rect.width() > 0.0).then_some(self.rect)
    }

    /// The frame was drawn: the edge light and chip it painted are its last frame's.
    fn mark_drawn(&mut self) {
        self.resize_dirty = false;
        tilo_ui_base::widget::Widget::mark_drawn(&mut self.layers);
    }

    fn take_structure_dirty(&mut self) -> bool {
        // The bar's shape follows the part its view gives it: a window whose tabs came or went is
        // built again, one frame after the part changed.
        let reshaped = self.notice_bar_shape();
        reshaped | self.layers.take_structure_dirty()
    }

    fn take_structure_change(&mut self) -> tilo_ui_base::widget::StructureChange {
        if self.notice_bar_shape() {
            // The bar's shape is the frame's own layout, so a host that hears of it builds the
            // frame again rather than remounting its children.
            return tilo_ui_base::widget::StructureChange::Rebuild;
        }
        // The bar built again (its name, its app's glyph) is a change of the frame's own, which
        // a host learns here as it learns its children's; the room the bar keeps is layout's,
        // never a structure change.
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

    fn kept_in_view(&self) -> Option<&str> {
        self.keep.as_deref()
    }

    /// A window in a row the person carries settles on its leading edge: the row's scroll comes
    /// to rest with a window flush at its edge, never between two or over the gap before one.
    fn snap_points(&self, axis: tilo_ui_base::widget::ScrollAxis, out: &mut Vec<f32>) {
        if matches!(axis, tilo_ui_base::widget::ScrollAxis::Horizontal) && self.rect.width() > 0.0 {
            out.push(0.0);
        }
    }

    /// The frame is a group named for the window, and what it holds is read inside it.
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        self.name.as_ref().map(|name| {
            let node = tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Group)
                .with_optional_name(Some(name.clone()));
            // Why it cannot be pressed now, if it says: read beside its name, never instead.
            let node = match &self.description {
                Some(why) => node.with_description(why.clone()),
                None => node,
            };
            // The size the window is taking while its edge is carried: a status a screen reader
            // reads as the person drags, from the same words the chip shows.
            let chip = self
                .feedback
                .chip(tilo_ui_base::motion::frame_now(), self.motion.fast())
                .map(|(size, _, _)| {
                    let words = format!("{} by {}", size.0.round() as i64, size.1.round() as i64);
                    tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::StaticText)
                        .with_optional_name(Some(words))
                });
            // The window's bar, named for the window as its frame is: where a press takes the
            // window and where a gesture that moves the strip aims, whatever its tabs report.
            if self.bar.area() > 0.0 {
                let mut children = vec![
                    tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Group)
                        .with_optional_name(Some(name.clone()))
                        .with_bounds(self.bar),
                ];
                children.extend(chip);
                return node.with_children(children);
            }
            match chip {
                Some(chip) => node.with_children(vec![chip]),
                None => node,
            }
        })
    }
}

/// The radius at each corner of a window: square where the corner meets the display's edge, the
/// theme's radius everywhere else. A rounded corner at the screen would bite a hole out of it.
#[must_use]
pub fn corner_radii(
    radius: f64,
    square_leading: bool,
    square_trailing: bool,
) -> vello::kurbo::RoundedRectRadii {
    let (leading, trailing) = (
        if square_leading { 0.0 } else { radius },
        if square_trailing { 0.0 } else { radius },
    );
    vello::kurbo::RoundedRectRadii::new(leading, trailing, trailing, leading)
}

impl WindowFrame {
    /// The window itself, drawn where `rect` says: what [`Decorator::draw`] draws once the
    /// window is posed where its arrival or move has reached.
    fn paint_frame(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>, rect: Rect) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        let radius = SurfaceLevel::Window.radius(theme).min(rect.height() / 2.0);
        let radii = corner_radii(radius, self.square_leading, self.square_trailing);
        if self.focused {
            tilo_ui_base::surface::floating_shadow(scene, transform, rect, radius, 1.0, theme);
        }
        let on = paint_surface(scene, transform, rect, SurfaceLevel::Window, theme);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &RoundedRect::new(rect.x0, rect.y0, rect.x1, rect.y1, radii),
        );
        // // The content under the window's own pill under the window's own controls: the pill // stands
        // over the app's header row where the app's row holds the bar, and over the // frame's bar
        // otherwise, so the window's controls are the frame's wherever they are.
        let pill = self.pill;
        {
            let mut children = self.layers.children_mut();
            if let Some(content) = children.get_mut(CONTENT) {
                content.draw_child(scene, transform, text, &on, &env);
            }
        }
        crate::widget::window_controls::paint_pill(scene, transform, pill, theme);
        {
            let mut children = self.layers.children_mut();
            if let Some(controls) = children.get_mut(CONTROLS) {
                controls.draw_child(scene, transform, text, &on, &env);
            }
        }
        scene.pop_layer();
        // The window in front wears the focused edge whatever the finish is: a shell tells the
        // person which window the keyboard is in, and a quiet border would say nothing.
        if self.focused {
            // The edge says which window the keyboard is in, so it clears the non-text floor on
            // the window's own ground whatever the theme's accent is there: a light theme's
            // accent over its light surface would otherwise be no edge at all.
            let colors = &theme.tokens.colors;
            let edge = colors.ink_on(
                SurfaceLevel::Window.color(theme),
                colors.window_edge_focused(),
                tilo_ui_base::tokens::colors::NON_TEXT_MIN_CONTRAST,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                transform,
                edge,
                None,
                &RoundedRect::new(
                    rect.x0 - 0.5,
                    rect.y0 - 0.5,
                    rect.x1 + 0.5,
                    rect.y1 + 0.5,
                    corner_radii(radius + 0.5, self.square_leading, self.square_trailing),
                ),
            );
        }
    }
}

/// Where the bar of the window frame named `name` stands, in the layout's logical coordinates:
/// the first frame under `root` that reads as that window in front of a person's attention
/// (`WindowFrame::accessibility`), its bar as the frame laid it out last
/// ([`WindowFrame::bar_rect`]).
///
/// A shell that moves a strip under a gesture aims at the bar of the window in front, and finds
/// it here: a walk of the tree the layout already placed, never the accessibility tree, which is
/// built for a reader and costs every widget's name, role and bounds.
#[must_use]
pub fn bar_named(root: &dyn Widget, name: &str) -> Option<Rect> {
    fn walk(widget: &dyn Widget, name: &str) -> Option<Rect> {
        if let Some(frame) = widget
            .as_any()
            .and_then(|any| any.downcast_ref::<WindowFrame>())
            && frame.name.as_deref() == Some(name)
        {
            let bar = frame.bar_rect();
            if bar.width() > 0.0 && bar.height() > 0.0 {
                return Some(bar);
            }
        }
        let mut found = None;
        widget.for_each_child(&mut |child| {
            if found.is_none() {
                found = walk(child, name);
            }
        });
        found
    }
    walk(root, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{TabItemProps, TabStrip, TabStripProps};
    use tilo_ui_base::{component::Described, component::Event, component::ResourceId};
    use vello::Scene;
    use vello::kurbo::Affine;

    tilo_test_env::scenario! {
        fn bar_named_finds_the_bar_the_frame_laid_out;

        phase the_window_is_found_by_the_name_a_person_reads;
        {
            let mut frame = Box::new(WindowFrame::from_props(
                "w",
                WindowFrameProps {
                    key: "w".to_owned(),
                    name: Some("Groceries".to_owned()),
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
            let found = bar_named(frame.as_ref(), "Groceries");
            assert_eq!(found, Some(bar), "the window's own bar, where it stands");
            assert_eq!(bar_named(frame.as_ref(), "Packing list"), None);
        }
    }

    /// A window's tabs, as the shell's strip hands them to its frame: two places, the first in
    /// front.
    fn two_places() -> Box<TabStrip> {
        let tab = |key: &str, title: &str| TabItemProps {
            key: key.to_owned(),
            item: Some(ResourceId(format!("@notes.Note:{key}"))),
            title: Some(title.to_owned()),
            name: None,
            icon: None,
            attention: false,
            modified: false,
            busy: false,
            presence: Vec::new(),
            author: None,
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
                opened_by: Event::None,
                tabs,
            },
        ))
    }

    /// A window's one tab, as the strip of a window of one place hands it to its frame.
    fn one_place() -> Box<TabStrip> {
        let tab = TabItemProps {
            key: "a".to_owned(),
            item: Some(ResourceId("@notes.Note:a".to_owned())),
            title: Some("Groceries — Notes".to_owned()),
            name: None,
            icon: None,
            attention: false,
            modified: false,
            busy: false,
            presence: Vec::new(),
            author: None,
        };
        Box::new(TabStrip::from_props(
            "frame",
            TabStripProps {
                key: "frame".to_owned(),
                active: tab.item.clone(),
                focused: true,
                activate: Event::None,
                close: Event::None,
                reorder: Event::None,
                detach: Event::None,
                opened_by: Event::None,
                tabs: vec![tab],
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
        let mut text = tilo_ui_base::font_catalog::fixture_renderer();
        Widget::draw(
            &mut frame,
            &mut tilo_ui_base::draw::DrawCtx::new(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                theme,
                tilo_ui_base::presence::PresenceCtx::empty().draw_env(),
            ),
        );
        tilo_ui_base::offscreen::OffscreenRenderer::new(240, 240)
            .render(&scene, vello::peniko::Color::BLACK)
    }

    /// How far the pixel at (`x`, `y`) stands from the ground, in relative luminance.
    fn from_ground(rgba: &[u8], theme: &Theme, x: u32, y: u32) -> f64 {
        let (r, g, b, _) = tilo_ui_base::offscreen::OffscreenRenderer::sample(rgba, 240, x, y);
        let seen = vello::peniko::Color::from_rgb8(r, g, b);
        (tilo_ui_base::a11y::relative_luminance(seen)
            - tilo_ui_base::a11y::relative_luminance(theme.tokens.colors.surfaces.base))
        .abs()
    }

    /// A corner at the display's edge is square, and the others keep the theme's radius.
    #[test]
    fn a_corner_at_the_display_s_edge_is_square() {
        let at_the_edge = corner_radii(12.0, true, false);
        assert_eq!(at_the_edge.top_left, 0.0, "the leading corner is square");
        assert_eq!(at_the_edge.bottom_left, 0.0, "both leading corners are");
        assert_eq!(
            at_the_edge.top_right, 12.0,
            "the trailing corner stays round"
        );
        let both = corner_radii(12.0, true, true);
        assert_eq!((both.top_right, both.bottom_right), (0.0, 0.0));
        let inside = corner_radii(12.0, false, false);
        assert_eq!((inside.top_left, inside.bottom_right), (12.0, 12.0));
    }

    /// The window in front wears the accent on its edge, whatever the finish: a shell says which
    /// window the keyboard is in with a colour, never a border so quiet it could be any window.
    #[test]
    fn the_focused_windows_edge_is_the_accent_colour() {
        let rect = Rect::new(40.0, 40.0, 200.0, 160.0);
        for theme in [Theme::dark(), Theme::light()] {
            let front = rendered(&theme, true, rect);
            let (r, g, b, _) =
                tilo_ui_base::offscreen::OffscreenRenderer::sample(&front, 240, 120, 39);
            let accent = theme.tokens.colors.window_edge_focused().to_rgba8();
            let near = |seen: u8, want: u8| (i32::from(seen) - i32::from(want)).abs() <= 12;
            assert!(
                near(r, accent.r) && near(g, accent.g) && near(b, accent.b),
                "the focused edge is the accent: saw {r},{g},{b}, the accent is {}, {}, {}",
                accent.r,
                accent.g,
                accent.b
            );
        }
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
                edge(&front) > edge(&behind) + tilo_ui_base::tokens::colors::HAIRLINE_MIN_DELTA,
                "the window in front has the heavier edge: {} over {}",
                edge(&front),
                edge(&behind)
            );
            assert!(
                edge(&behind) >= tilo_ui_base::tokens::colors::HAIRLINE_MIN_DELTA / 2.0,
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

    /// Lay `frame` out at `rect`, once: the bar's name elides itself in the room layout gives it
    /// and the controls keep their room from that same solve, so the window needs no second one.
    fn laid_out(frame: &mut WindowFrame, rect: Rect) {
        let theme = Theme::dark();
        let mut text = tilo_ui_base::font_catalog::fixture_renderer();
        tilo_ui_base::widget::layout::lay_out_detached(frame, rect, &theme, &mut text)
            .expect("the window lays out");
    }

    /// A window's first layout builds its bar, and every layout after it takes the one it stands
    /// in: the name elides itself in the room the bar gives it and the controls keep their room
    /// from the same solve, so no layout asks its host for another.
    #[test]
    fn a_windows_bar_takes_the_room_it_is_laid_out_in() {
        let mut frame = WindowFrame::from_props(
            "w",
            WindowFrameProps {
                name: Some("A window of one place".to_owned()),
                focused: true,
                controls: Some(Box::new(
                    Stack::row().push(Box::new(Text::new("Close")) as Element),
                )),
                body: Some(Box::new(Text::new("What the window shows.")) as Element),
                ..WindowFrameProps::default()
            },
        );
        let rect = Rect::new(0.0, 0.0, 900.0, 600.0);
        laid_out(&mut frame, rect);
        assert!(
            !Widget::take_structure_change(&mut frame).rebuilds(),
            "a window built into a view asks for no structure around it"
        );
        laid_out(&mut frame, rect);
        assert!(
            !Widget::take_structure_change(&mut frame).rebuilds(),
            "a window laid out where it stands asks for no structure around it"
        );
        assert!(
            !Widget::take_structure_dirty(&mut frame),
            "and for no layout of its own after the one it took"
        );
        assert!(
            !Widget::layout_needs_measure(&frame),
            "its bar was measured in the solve that placed it"
        );
    }

    /// The frame's own bar and its content, laid out.
    fn parts(frame: &WindowFrame) -> (Rect, Rect) {
        let content = frame.layers.children()[CONTENT].children();
        (content[OWN_BAR].get_rect(), content[BODY].get_rect())
    }

    /// `frame` drawn on the 240 square's ground, rendered.
    fn drawn(theme: &Theme, frame: &mut WindowFrame) -> Vec<u8> {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            theme.tokens.colors.surfaces.base,
            None,
            &Rect::new(0.0, 0.0, 240.0, 240.0),
        );
        let mut text = tilo_ui_base::font_catalog::fixture_renderer();
        Widget::draw(
            frame,
            &mut tilo_ui_base::draw::DrawCtx::new(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                theme,
                tilo_ui_base::presence::PresenceCtx::empty().draw_env(),
            ),
        );
        tilo_ui_base::offscreen::OffscreenRenderer::new(240, 240)
            .render(&scene, vello::peniko::Color::BLACK)
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

    /// Who opened a window shows on its tabs where the window stands: their face on the tab they
    /// opened. Pointing at a person's tab names them; an agent's face is its mark, which names
    /// the agent and its step when pointed at and is a stop for the keyboard.
    #[test]
    fn who_opened_a_window_shows_on_its_tabs_where_the_window_stands() {
        use crate::widget::TabAuthorProps;
        use tilo_ui_base::a11y::AccessNode;
        let tab = |key: &str, title: &str, author: TabAuthorProps| TabItemProps {
            key: key.to_owned(),
            item: Some(ResourceId(format!("@notes.Note:{key}"))),
            title: Some(title.to_owned()),
            author: Some(author),
            ..TabItemProps::default()
        };
        let tabs = vec![
            tab(
                "a",
                "Receipts",
                TabAuthorProps {
                    name: Some("Scribe".to_owned()),
                    colour: Some("#2a9d8f".to_owned()),
                    agent: true,
                    run: Some("agent-run:7".to_owned()),
                    step: Some("File the receipts".to_owned()),
                },
            ),
            tab(
                "b",
                "Plan",
                TabAuthorProps {
                    name: Some("Ada".to_owned()),
                    colour: Some("#c85028".to_owned()),
                    ..TabAuthorProps::default()
                },
            ),
            TabItemProps {
                key: "c".to_owned(),
                item: Some(ResourceId("@notes.Note:c".to_owned())),
                title: Some("Diary".to_owned()),
                ..TabItemProps::default()
            },
        ];
        let queue: tilo_ui_base::Intents<tilo_ui_base::component::Fired> =
            tilo_ui_base::Intents::default();
        let strip = TabStrip::from_props(
            "frame",
            TabStripProps {
                key: "frame".to_owned(),
                active: tabs.first().and_then(|t| t.item.clone()),
                focused: true,
                opened_by: Event::View(tilo_ui_base::component::Intent::new(
                    &queue,
                    "frame",
                    "opened_by",
                )),
                tabs,
                ..TabStripProps::default()
            },
        );
        let mut frame = WindowFrame::from_props(
            "w",
            WindowFrameProps {
                name: Some("Receipts".to_owned()),
                focused: true,
                tabs: Some(Box::new(strip)),
                body: Some(Box::new(Spacer::new())),
                ..WindowFrameProps::default()
            },
        );
        let theme = Theme::light();
        // The window as its strip gives it room: a face widens its pill, so the first drawing
        // measures it and the next layout makes its room.
        for _ in 0..2 {
            let mut taffy = TaffyTree::<()>::new();
            let mut nodes = std::collections::HashMap::new();
            let node = Widget::build_layout(&mut frame, &mut taffy, &theme, &mut nodes);
            if let Ok(mut style) = taffy.style(node).cloned() {
                style.size = taffy::Size::from_lengths(900.0_f32, 360.0_f32);
                let _ = taffy.set_style(node, style);
            }
            taffy
                .compute_layout(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(900.0),
                        height: taffy::AvailableSpace::Definite(360.0),
                    },
                )
                .expect("the window solves its layout");
            Widget::apply_layout(&mut frame, &taffy, node, Point::ZERO);
            let _ = drawn(&theme, &mut frame);
        }
        // Each author leads their tab, before its title and the same step from it, person or
        // agent; a window of the person's own wears none.
        let leads = with_tabs_bar(&mut frame, &mut |bar| {
            let titles = bar.painted_labels();
            let ids: Vec<(String, u64)> = bar
                .tabs()
                .iter()
                .map(|tab| (tab.title.clone(), tab.id))
                .collect();
            ids.into_iter()
                .map(|(title, id)| {
                    let at = titles
                        .iter()
                        .find(|label| label.widget == "TabTitle" && label.text == title)
                        .map(|label| label.rect[0]);
                    (title, bar.author_rect(id), at)
                })
                .collect::<Vec<_>>()
        })
        .expect("the frame holds the strip");
        let gaps: Vec<f64> = leads
            .iter()
            .filter_map(|(title, face, at)| {
                let at = at.unwrap_or_else(|| panic!("{title} is drawn"));
                let face = (*face)?;
                assert!(
                    face.x1 < at,
                    "{title}'s author leads its title: {face:?} before x {at}"
                );
                Some(at - face.x1)
            })
            .collect();
        assert_eq!(gaps.len(), 2, "both authors lead their tabs: {leads:?}");
        assert!(
            (gaps[0] - gaps[1]).abs() < 0.5,
            "one step between face and title, person or agent: {gaps:?}"
        );
        assert!(
            leads
                .iter()
                .any(|(title, face, _)| title == "Diary" && face.is_none()),
            "a window of one's own wears no author: {leads:?}"
        );
        let own = frame.layers.children()[CONTENT].children();
        let own = own[OWN_BAR].children();
        let strip = own[TABS];
        let tree = strip.accessibility().expect("the tabs answer a reader");
        let all = |node: &AccessNode| -> Vec<AccessNode> {
            fn walk(node: &AccessNode, out: &mut Vec<AccessNode>) {
                out.push(node.clone());
                for child in &node.children {
                    walk(child, out);
                }
            }
            let mut out = Vec::new();
            walk(node, &mut out);
            out
        };
        let nodes = all(&tree);
        let rect_of = |node: &AccessNode| {
            let at = node.bounds.expect("drawn where it stands");
            Rect::new(at.x0, at.y0, at.x1, at.y1)
        };
        let plan = nodes
            .iter()
            .find(|node| node.name.as_deref() == Some("Plan"))
            .map(rect_of)
            .expect("the tab Ada opened");
        assert_eq!(
            strip
                .tooltip_target_at(plan.center())
                .map(|(_, tip)| tip)
                .as_deref(),
            Some("Ada opened this window"),
            "pointing at her tab names her"
        );
        let mark = nodes
            .iter()
            .find(|node| node.action_target.as_deref() == Some("tab-opened-by"))
            .expect("the agent's face is its mark, a node of the tabs");
        assert_eq!(mark.name.as_deref(), Some("Scribe"));
        assert!(mark.state.focusable, "the keyboard stops on the mark");
        let at = rect_of(mark);
        let receipts = nodes
            .iter()
            .find(|node| node.name.as_deref() == Some("Receipts"))
            .map(rect_of)
            .expect("the tab the agent opened");
        assert!(
            receipts.contains(at.center()) && at.width() > 0.0,
            "the agent's face stands on its tab: {at:?} in {receipts:?}"
        );
        assert_eq!(
            strip
                .tooltip_target_at(at.center())
                .map(|(_, tip)| tip)
                .as_deref(),
            Some("Scribe opened this window\nFile the receipts"),
            "pointing at the mark says which agent and the step it took"
        );
        // Pressing the mark, or reaching it with the keyboard, is the view's event for that tab.
        assert_eq!(
            with_tabs_bar(&mut frame, &mut TabBar::activate_opened_by),
            Some(true)
        );
        let reported = strip_in(&mut frame).map(TabStrip::report);
        assert!(reported.is_some(), "the strip reports what its bar marked");
        let fired = queue.take();
        assert_eq!(fired.len(), 1, "one press, one event: {fired:?}");
        assert_eq!(fired[0].event, "opened_by");
        assert_eq!(
            fired[0].value,
            Some(tilo_ui_base::component::Value::Resource(
                "@notes.Note:a".to_owned()
            ))
        );
    }

    /// The tab strip under `widget`.
    fn strip_in(widget: &mut dyn Widget) -> Option<&mut TabStrip> {
        if widget.as_any_mut().is_some_and(|any| any.is::<TabStrip>()) {
            return widget.as_any_mut().and_then(|any| any.downcast_mut());
        }
        widget.children_mut().into_iter().find_map(strip_in)
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

    /// A window the person pressed for comes out of the control they pressed — the dock icon or the
    /// search row — on its way to where it is laid out, and one a shortcut asked for lands where it
    /// stands.
    #[test]
    fn a_window_the_person_pressed_for_flies_out_of_the_control_they_pressed() {
        let _frame = tilo_ui_base::motion::pin_frame();
        // A window opening in a running session: the shell has drawn a frame before.
        let _shown = tilo_ui_base::motion::laying_out_window(true);
        let control = Point::new(30.0, 700.0);
        tilo_ui_base::motion::note_pointer(control);
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
        let mut frame = WindowFrame::from_props(
            "w",
            WindowFrameProps {
                name: Some("A note".to_owned()),
                focused: true,
                body: Some(Box::new(Spacer::new())),
                ..WindowFrameProps::default()
            },
        );
        let rect = Rect::new(400.0, 40.0, 880.0, 480.0);
        laid_out(&mut frame, rect);
        let now = tilo_ui_base::motion::frame_now();
        frame.placed.follow(
            rect,
            true,
            Theme::dark().motion(),
            tilo_ui_base::motion::input_source(),
            now,
        );
        let mark = frame.placed.pose(rect, now).drawn_rect(rect);
        assert!(
            (mark.center() - control).hypot() < 1.0,
            "the first frame is a mark at the control: {mark:?}"
        );
        assert!(
            mark.width() < rect.width() / 2.0,
            "and much smaller than its frame: {mark:?}"
        );

        // A window a shortcut asked for lands where it stands.
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Keyboard);
        let mut shortcut = WindowFrame::from_props(
            "w",
            WindowFrameProps {
                name: Some("A note".to_owned()),
                focused: true,
                body: Some(Box::new(Spacer::new())),
                ..WindowFrameProps::default()
            },
        );
        laid_out(&mut shortcut, rect);
        shortcut.placed.follow(
            rect,
            true,
            Theme::dark().motion(),
            tilo_ui_base::motion::input_source(),
            now,
        );
        assert_eq!(
            shortcut.placed.pose(rect, now).drawn_rect(rect),
            rect,
            "a shortcut's window shows in its frame at once"
        );
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Action);
    }

    /// A window a shell keeps only as it leaves is drawn whole where it stood and goes back into
    /// the control it was opened from — and it takes nothing on the way, so a press on its bar
    /// reaches no action of a place that has left the layout.
    #[test]
    fn a_window_that_closes_goes_back_into_the_control_it_came_out_of() {
        use taffy::TaffyTree;
        let _frame = tilo_ui_base::motion::pin_frame();
        let _shown = tilo_ui_base::motion::laying_out_window(true);
        // The person closed it with a press: a pointer's departure plays; a shortcut's lands.
        tilo_ui_base::motion::note_source(tilo_ui_base::tokens::MotionSource::Pointer);
        let queue = tilo_ui_base::Intents::default();
        let mut frame = Box::new(WindowFrame::from_props(
            "w",
            WindowFrameProps {
                key: "w".to_owned(),
                name: Some("A note".to_owned()),
                leaving: true,
                control: vec![30, 700],
                press: Event::View(tilo_ui_base::component::Intent::new(&queue, "w", "press")),
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

        // The frame's first draw starts its departure: whole where it stood, on its way.
        let _ = drawn(&theme, &mut frame);
        let now = tilo_ui_base::motion::frame_now();
        let start = frame.placed.pose(rect, now).drawn_rect(rect);
        assert!(
            (start.center() - rect.center()).hypot() < 1.0,
            "it starts whole where it stood: {start:?}"
        );
        assert!(frame.placed.is_moving(now), "and it goes from there");

        let mut doc = automerge::AutoCommit::new();
        let mut send = |frame: &mut WindowFrame, event: &WindowEvent| {
            crate::testing::Dispatch::new()
                .doc(&mut doc)
                .layout(&taffy, node)
                .send(frame, event);
        };
        send(
            frame.as_mut(),
            &tilo_ui_base::synthetic_input::cursor_moved(bar.center().x, bar.center().y),
        );
        send(
            frame.as_mut(),
            &tilo_ui_base::synthetic_input::mouse_press(),
        );
        assert!(
            queue.take().is_empty(),
            "a window on its way out takes no press"
        );

        let mid = now + crate::time::Duration::from_millis(80);
        let _later = tilo_ui_base::motion::pin_frame_at(mid);
        let going = frame.placed.pose(rect, mid).drawn_rect(rect);
        let control = Point::new(30.0, 700.0);
        assert!(
            going.width() < rect.width()
                && (going.center() - control).hypot() < (rect.center() - control).hypot(),
            "it travels back toward the control it came out of: {going:?}"
        );
    }

    /// The window's bar is its handle: a press on it reaches for the window (its `press`, which
    /// the shell answers by bringing it forward), and one on the bar's empty area that travels
    /// publishes the resource the frame carries on the drag bus, and letting go over a drop
    /// target hands it there. A press inside the body is the content's, never the bar's.
    #[test]
    fn a_drag_on_the_window_bar_carries_the_resource_it_holds() {
        use crate::widget::drag_bus;
        use taffy::TaffyTree;
        let queue = tilo_ui_base::Intents::default();
        let mut frame = Box::new(WindowFrame::from_props(
            "w",
            WindowFrameProps {
                key: "w".to_owned(),
                name: Some("Design crit".to_owned()),
                carries: Some("chat.workspace/channels/c_crit".to_owned()),
                press: Event::View(tilo_ui_base::component::Intent::new(&queue, "w", "press")),
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
        send(&tilo_ui_base::synthetic_input::cursor_moved(from_x, from_y));
        send(&tilo_ui_base::synthetic_input::mouse_press());
        assert_eq!(
            queue
                .take()
                .iter()
                .map(|fired| fired.event.as_str())
                .collect::<Vec<_>>(),
            ["press"],
            "a press on the bar reaches for the window"
        );
        for step in 1..=4 {
            let dy = 12.0 * f64::from(step);
            send(&tilo_ui_base::synthetic_input::cursor_moved(
                from_x,
                from_y + dy,
            ));
        }
        let in_flight = drag_bus::current();
        let target = Rect::new(60.0, 200.0, 400.0, 320.0);
        send(&tilo_ui_base::synthetic_input::cursor_moved(200.0, 260.0));
        send(&tilo_ui_base::synthetic_input::mouse_release());
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
        let tilo_ui_base::dnd::DragPayload::Custom { kind, blob } = landed.payload else {
            panic!("the bar carries a resource");
        };
        assert_eq!(kind, crate::widget::drag_source::RESOURCE_PAYLOAD);
        assert_eq!(
            String::from_utf8(blob).expect("an address"),
            "chat.workspace/channels/c_crit",
            "the drop carries the resource the bar holds"
        );

        let (bx, by) = (bar.x0 + bar.width() / 2.0, bar.y1 + 120.0);
        send(&tilo_ui_base::synthetic_input::cursor_moved(bx, by));
        send(&tilo_ui_base::synthetic_input::mouse_press());
        for step in 1..=4 {
            send(&tilo_ui_base::synthetic_input::cursor_moved(
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
        assert!(
            queue.take().is_empty(),
            "a press on the body is the content's, never the bar's"
        );
    }

    /// A window of one tab is its title: a press on that pill reaches for the whole window — its
    /// `press`, which the shell answers by bringing it forward and giving it the keyboard — and a
    /// travel from there carries the resource the window holds, never the pill's own drag.
    #[test]
    fn a_one_tab_bar_lifts_the_whole_window() {
        use crate::widget::drag_bus;
        use taffy::TaffyTree;
        let queue = tilo_ui_base::Intents::default();
        let mut frame = Box::new(WindowFrame::from_props(
            "w",
            WindowFrameProps {
                key: "w".to_owned(),
                name: Some("Groceries — Notes".to_owned()),
                carries: Some("notes.workspace/notes/n_groceries".to_owned()),
                press: Event::View(tilo_ui_base::component::Intent::new(&queue, "w", "press")),
                tabs: Some(one_place()),
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
        let mut send = |event: &WindowEvent| {
            let mut doc = automerge::AutoCommit::new();
            crate::testing::Dispatch::new()
                .doc(&mut doc)
                .layout(&taffy, node)
                .send(frame.as_mut(), event);
        };
        // A point on the pill: the one tab's title stands at the bar's leading edge.
        let (from_x, from_y) = (bar.x0 + 24.0, bar.center().y);
        send(&tilo_ui_base::synthetic_input::cursor_moved(from_x, from_y));
        send(&tilo_ui_base::synthetic_input::mouse_press());
        assert_eq!(
            queue
                .take()
                .iter()
                .map(|fired| fired.event.as_str())
                .collect::<Vec<_>>(),
            ["press"],
            "a press on the window's title reaches for the whole window"
        );
        for step in 1..=4 {
            let dx = 12.0 * f64::from(step);
            send(&tilo_ui_base::synthetic_input::cursor_moved(
                from_x + dx,
                from_y,
            ));
        }
        let in_flight = drag_bus::current();
        let target = Rect::new(60.0, 200.0, 400.0, 320.0);
        send(&tilo_ui_base::synthetic_input::cursor_moved(200.0, 260.0));
        send(&tilo_ui_base::synthetic_input::mouse_release());
        let landed = drag_bus::take("composer", target);
        drag_bus::end(None);
        let in_flight = in_flight.expect("the bar published what the window carries");
        assert_eq!(
            in_flight.source, "w",
            "the drag is the window's, not the pill's"
        );
        assert_eq!(
            in_flight.label, "Groceries — Notes",
            "the ghost says the window's name"
        );
        let landed = landed.expect("the drop landed in the target it was let go over");
        let tilo_ui_base::dnd::DragPayload::Custom { kind, blob } = landed.payload else {
            panic!("the bar carries a resource");
        };
        assert_eq!(kind, crate::widget::drag_source::RESOURCE_PAYLOAD);
        assert_eq!(
            String::from_utf8(blob).expect("an address"),
            "notes.workspace/notes/n_groceries",
            "the drop carries the resource the window shows"
        );
    }

    /// The props an update in place reads carry no part views, so a frame that shows tabs is not
    /// built again by them: a bar built again under the person's hand would let go of the tab or
    /// the window being carried. The shape follows the part the view gives it, never the props.
    #[test]
    fn an_update_in_place_keeps_a_tabbed_frame() {
        let mut frame = WindowFrame::from_props(
            "w",
            WindowFrameProps {
                key: "w".to_owned(),
                name: Some("Groceries — Notes".to_owned()),
                carries: Some("shell:place:1".to_owned()),
                tabs: Some(two_places()),
                body: Some(Box::new(Spacer::new())),
                ..WindowFrameProps::default()
            },
        );
        assert!(frame.tabbed, "fixture: the window shows its tabs");
        let kept = frame.set_props(WindowFrameProps {
            key: "w".to_owned(),
            name: Some("Groceries — Notes".to_owned()),
            carries: Some("shell:place:1".to_owned()),
            focused: true,
            ..WindowFrameProps::default()
        });
        assert!(
            kept,
            "an update in place keeps the frame the person is holding"
        );
        assert!(frame.tabbed, "the bar keeps its tabs");
    }

    /// A window of several tabs keeps its pills for its tabs even while its frame carries what the
    /// window holds: a press on a pill lifts that tab, and the window is carried from the bar's own
    /// room, where no pill stands.
    #[test]
    fn a_press_on_a_pill_of_several_lifts_the_tab() {
        use crate::widget::drag_bus;
        let queue = tilo_ui_base::Intents::default();
        let mut frame = Box::new(WindowFrame::from_props(
            "w",
            WindowFrameProps {
                key: "w".to_owned(),
                name: Some("Groceries — Notes".to_owned()),
                carries: Some("notes.workspace/notes/n_groceries".to_owned()),
                press: Event::View(tilo_ui_base::component::Intent::new(&queue, "w", "press")),
                tabs: Some(two_places()),
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
        fn send(frame: &mut dyn Widget, taffy: &TaffyTree<()>, node: NodeId, event: &WindowEvent) {
            let mut doc = automerge::AutoCommit::new();
            crate::testing::Dispatch::new()
                .doc(&mut doc)
                .layout(taffy, node)
                .send(frame, event);
        }
        let pill = with_tabs_bar(&mut frame.layers, &mut |bar: &mut TabBar| {
            bar.tab_rects().get(1).copied()
        })
        .flatten()
        .expect("the window's second pill is laid out");
        let at = pill.center();
        send(
            frame.as_mut(),
            &taffy,
            node,
            &tilo_ui_base::synthetic_input::cursor_moved(at.x, at.y),
        );
        send(
            frame.as_mut(),
            &taffy,
            node,
            &tilo_ui_base::synthetic_input::mouse_press(),
        );
        assert!(
            queue.take().is_empty(),
            "a press on a pill reaches for the tab, never the window"
        );
        for step in 1..=4 {
            let by = 12.0 * f64::from(step);
            send(
                frame.as_mut(),
                &taffy,
                node,
                &tilo_ui_base::synthetic_input::cursor_moved(at.x + by, at.y + by / 2.0),
            );
        }
        assert!(
            drag_bus::current().is_some(),
            "the tab is carried on the drag bus"
        );
        assert_eq!(
            with_tabs_bar(&mut frame.layers, &mut |bar: &mut TabBar| bar
                .drag_in_flight()),
            Some(true),
            "the pill's own drag is the one in flight"
        );
        send(
            frame.as_mut(),
            &taffy,
            node,
            &tilo_ui_base::synthetic_input::mouse_release(),
        );
        drag_bus::end(None);
    }

    /// A window in a row is resized by carrying its trailing edge: the press takes the edge, the
    /// chip says the size it is taking while it is held, and letting go reports the width the row
    /// shares out — one change, which the shell records as one arranging action.
    #[test]
    fn a_carried_edge_reports_the_width_it_is_taking() {
        use tilo_ui_base::component::{Fired, Intent};
        let queue = tilo_ui_base::Intents::<Fired>::default();
        let mut frame = Box::new(WindowFrame::from_props(
            "w",
            WindowFrameProps {
                key: "w".to_owned(),
                name: Some("A window".to_owned()),
                resizable: true,
                resize: Event::View(Intent::new(&queue, "w", "resize")),
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
        fn send(frame: &mut dyn Widget, taffy: &TaffyTree<()>, node: NodeId, event: &WindowEvent) {
            let mut doc = automerge::AutoCommit::new();
            crate::testing::Dispatch::new()
                .doc(&mut doc)
                .layout(taffy, node)
                .send(frame, event);
        }
        // A point on the trailing edge, below the bar: where a person grabs the window's width.
        let (x, y) = (rect.x1 - 2.0, 300.0);
        send(
            frame.as_mut(),
            &taffy,
            node,
            &tilo_ui_base::synthetic_input::cursor_moved(x, y),
        );
        send(
            frame.as_mut(),
            &taffy,
            node,
            &tilo_ui_base::synthetic_input::mouse_press(),
        );
        assert!(
            queue.take().is_empty(),
            "a press on the edge reaches for the width, never the bar's press"
        );
        for step in 1..=4 {
            send(
                frame.as_mut(),
                &taffy,
                node,
                &tilo_ui_base::synthetic_input::cursor_moved(x - 15.0 * f64::from(step), y),
            );
        }
        let chip = Widget::accessibility(frame.as_mut())
            .and_then(|node| {
                node.children.into_iter().find_map(|child| {
                    (child.role == tilo_ui_base::a11y::Role::StaticText).then_some(child.name?)
                })
            })
            .unwrap_or_default();
        assert_eq!(
            chip, "420 by 360",
            "the chip says the size the edge is taking"
        );
        send(
            frame.as_mut(),
            &taffy,
            node,
            &tilo_ui_base::synthetic_input::mouse_release(),
        );
        let fired = queue.take();
        let resize = fired
            .iter()
            .find(|fired| fired.event == "resize")
            .expect("letting the edge go resizes the window");
        assert_eq!(
            resize.value,
            Some(tilo_ui_base::component::Value::List(vec![
                tilo_ui_base::component::Value::Int(420),
                tilo_ui_base::component::Value::Int(480),
                tilo_ui_base::component::Value::Int(1),
                tilo_ui_base::component::Value::Int(0),
                tilo_ui_base::component::Value::Int(160),
            ])),
            "the width it is taking, the width it had, its trailing edge, no Shift, and the \
             toolkit's own least it kept"
        );
    }

    /// A window with a window before it carries its leading edge too, and Shift makes the drag a
    /// trade with the window across the border: the report says which edge and whether Shift held.
    #[test]
    fn a_leading_edge_and_shift_are_reported() {
        use tilo_ui_base::component::{Fired, Intent};
        let queue = tilo_ui_base::Intents::<Fired>::default();
        let mut frame = Box::new(WindowFrame::from_props(
            "w",
            WindowFrameProps {
                key: "w".to_owned(),
                name: Some("A window".to_owned()),
                resizable: true,
                leads: true,
                resize: Event::View(Intent::new(&queue, "w", "resize")),
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
        fn send(frame: &mut dyn Widget, taffy: &TaffyTree<()>, node: NodeId, event: &WindowEvent) {
            let mut doc = automerge::AutoCommit::new();
            crate::testing::Dispatch::new()
                .modifiers(winit::keyboard::ModifiersState::SHIFT)
                .doc(&mut doc)
                .layout(taffy, node)
                .send(frame, event);
        }
        send(
            frame.as_mut(),
            &taffy,
            node,
            &tilo_ui_base::synthetic_input::cursor_moved(2.0, 300.0),
        );
        send(
            frame.as_mut(),
            &taffy,
            node,
            &tilo_ui_base::synthetic_input::mouse_press(),
        );
        for step in 1..=4 {
            send(
                frame.as_mut(),
                &taffy,
                node,
                &tilo_ui_base::synthetic_input::cursor_moved(2.0 - 15.0 * f64::from(step), 300.0),
            );
        }
        send(
            frame.as_mut(),
            &taffy,
            node,
            &tilo_ui_base::synthetic_input::mouse_release(),
        );
        let fired = queue.take();
        let resize = fired
            .iter()
            .find(|fired| fired.event == "resize")
            .expect("letting the leading edge go resizes the neighbour");
        assert_eq!(
            resize.value,
            Some(tilo_ui_base::component::Value::List(vec![
                tilo_ui_base::component::Value::Int(540),
                tilo_ui_base::component::Value::Int(480),
                tilo_ui_base::component::Value::Int(0),
                tilo_ui_base::component::Value::Int(1),
                tilo_ui_base::component::Value::Int(160),
            ])),
            "the width it is taking, the width it had, its leading edge, Shift held, and the \
             toolkit's own least"
        );
    }

    /// A window the room holds narrower than the least its row keeps still measures from the width
    /// it has: the first pixels of the drag widen it by what the pointer travelled, never by the
    /// jump to that least.
    #[test]
    fn a_window_narrower_than_its_least_never_jumps_wider() {
        use tilo_ui_base::component::{Fired, Intent};
        let queue = tilo_ui_base::Intents::<Fired>::default();
        let mut frame = Box::new(WindowFrame::from_props(
            "w",
            WindowFrameProps {
                key: "w".to_owned(),
                name: Some("A window".to_owned()),
                resizable: true,
                narrowest: Some(704.0),
                resize: Event::View(Intent::new(&queue, "w", "resize")),
                body: Some(Box::new(Spacer::new())),
                ..WindowFrameProps::default()
            },
        ));
        let theme = Theme::dark();
        let rect = Rect::new(0.0, 0.0, 340.0, 360.0);
        let mut taffy = TaffyTree::<()>::new();
        let mut nodes = std::collections::HashMap::new();
        let node = Widget::build_layout(frame.as_mut(), &mut taffy, &theme, &mut nodes);
        if let Ok(mut style) = taffy.style(node).cloned() {
            style.size = taffy::Size::from_lengths(340.0_f32, 360.0_f32);
            let _ = taffy.set_style(node, style);
        }
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(340.0),
                    height: taffy::AvailableSpace::Definite(360.0),
                },
            )
            .expect("the window solves its layout");
        Widget::apply_layout(frame.as_mut(), &taffy, node, rect.origin());
        fn send(frame: &mut dyn Widget, taffy: &TaffyTree<()>, node: NodeId, event: &WindowEvent) {
            let mut doc = automerge::AutoCommit::new();
            crate::testing::Dispatch::new()
                .doc(&mut doc)
                .layout(taffy, node)
                .send(frame, event);
        }
        send(
            frame.as_mut(),
            &taffy,
            node,
            &tilo_ui_base::synthetic_input::cursor_moved(338.0, 300.0),
        );
        send(
            frame.as_mut(),
            &taffy,
            node,
            &tilo_ui_base::synthetic_input::mouse_press(),
        );
        for step in 1..=2 {
            send(
                frame.as_mut(),
                &taffy,
                node,
                &tilo_ui_base::synthetic_input::cursor_moved(338.0 + 10.0 * f64::from(step), 300.0),
            );
        }
        let chip = Widget::accessibility(frame.as_mut())
            .and_then(|node| {
                node.children.into_iter().find_map(|child| {
                    (child.role == tilo_ui_base::a11y::Role::StaticText).then_some(child.name?)
                })
            })
            .unwrap_or_default();
        assert_eq!(
            chip, "360 by 360",
            "the chip measures from the width the window has, not the least it may keep"
        );
        send(
            frame.as_mut(),
            &taffy,
            node,
            &tilo_ui_base::synthetic_input::mouse_release(),
        );
        let fired = queue.take();
        let resize = fired
            .iter()
            .find(|fired| fired.event == "resize")
            .expect("letting the edge go resizes the window");
        assert_eq!(
            resize.value,
            Some(tilo_ui_base::component::Value::List(vec![
                tilo_ui_base::component::Value::Int(360),
                tilo_ui_base::component::Value::Int(340),
                tilo_ui_base::component::Value::Int(1),
                tilo_ui_base::component::Value::Int(0),
                tilo_ui_base::component::Value::Int(704),
            ])),
            "the width it took, the width it had, its trailing edge, no Shift, and the least \
             its row keeps"
        );
    }
}
