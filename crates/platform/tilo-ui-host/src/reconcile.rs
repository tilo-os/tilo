//! The runtime's view tree as retained widgets, reconciled by key.
//!
//! A view evaluates to a tree of [`UiNode`]s with stable keys. The first tree is built into
//! widgets of the shared layout primitives (`Stack`, `Row`, `Grid`, `Split`, `Page`, `Layers`)
//! and the toolkit's controls. When the view runs again, the new tree is reconciled against the
//! old one: a node with the same key and kind keeps its widget (and with it focus, scroll
//! position and text state), a child that is new is built, and one that is gone is dropped. Text,
//! input and checkbox values live in shared state, so a changed value touches no widget.
//!
//! Nothing here draws or measures. Layout belongs to the primitives, and every length is in `rem`
//! or a rhythm token: a view never names a pixel.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::{UiNode, Value};
use ui::LocalState;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::arrange::{
    Align, At, Collapse, Distribute, Fit, Grid, Layers, Length, Navigation, Page, Row, Side, Slot,
    Split, Stack,
};
use ui::widget::{
    Avatar, AvatarSize, Button, ButtonRole, Checkbox, Container, ControlSize, Divider, Dropdown,
    Element, EmptyState, Field, FormSection, Scrollable, Section, Segment, Spacer, Text, Toggle,
    ToggleGroup, labeled_check,
};

use crate::adapter::{Adapter, NodeSource};
use crate::pane::Pane;
use ui::presence::PathSegment;

/// What a person did to the view.
#[derive(Clone, Debug, PartialEq)]
pub enum UiEvent {
    /// The button `key` was activated.
    Press(String),
    /// The input `key` now holds `text`.
    Edit(String, String),
    /// The rich-text input `key` now holds `rich`.
    EditRich(String, tilo_render_protocol::patch::rich::RichText),
    /// The checkbox `key` is now `checked`.
    Toggle(String, bool),
    /// The suggestion in the invocation card `key` was allowed or declined.
    Resolve(String, bool),
    /// The node `key`'s event `event` happened (a case of a `choice` was picked, a row of a
    /// list selected), carrying a value when the event carries one.
    Fire(String, String, Option<Value>),
    /// A row of a resource was activated: show the resource.
    Open(crate::ResourceRef),
    /// A reference in rich text was activated: open what it refers to (the `mention` mark's
    /// value).
    OpenReference(String),
}

/// Where controls report what the person did.
pub type EventSink = Rc<Reports>;

/// What the person did, in the order they did it: what the window's own controls report here,
/// and what the described components report through the view's adapter, which is taken in
/// before each report of the controls' so that neither overtakes the other.
#[derive(Default)]
pub struct Reports {
    events: RefCell<Vec<UiEvent>>,
    adapter: RefCell<Option<Adapter>>,
}

impl Reports {
    /// Report `event`, after what the components reported before it.
    pub fn push(&self, event: UiEvent) {
        self.take_in();
        self.events.borrow_mut().push(event);
    }

    /// Everything reported since the last call, oldest first.
    pub fn take(&self) -> Vec<UiEvent> {
        self.take_in();
        std::mem::take(&mut *self.events.borrow_mut())
    }

    /// Whether nothing was reported.
    pub fn is_empty(&self) -> bool {
        self.take_in();
        self.events.borrow().is_empty()
    }

    /// The components of the view `adapter` builds report here too.
    fn follow(&self, adapter: &Adapter) {
        *self.adapter.borrow_mut() = Some(adapter.clone());
    }

    fn take_in(&self) {
        let reported = match self.adapter.borrow().as_ref() {
            Some(adapter) => adapter.take_events(),
            None => return,
        };
        self.events.borrow_mut().extend(reported);
    }
}

/// A case of an enum, as a control shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Case {
    /// The variant's name, what the runtime is told.
    pub name: String,
    pub label: String,
    pub icon: Option<ui::icons::Icon>,
}

/// Builds and reconciles the widgets of a view tree.
pub struct Reconciler {
    sink: EventSink,
    texts: HashMap<String, LocalState<String>>,
    flags: HashMap<String, LocalState<bool>>,
    counts: HashMap<String, LocalState<i64>>,
    numbers: HashMap<String, LocalState<f64>>,
    riches: HashMap<String, LocalState<tilo_render_protocol::patch::rich::RichText>>,
    /// The handle on the split that collapses to navigate, the last one built.
    navigation: Option<Navigation>,
    /// What the components it builds report, shared with every reconciler of the same view.
    adapter: Adapter,
    /// Where the rows of the virtual lists it builds come from.
    rows: Option<crate::adapter::RowsOf>,
    /// Each component of the view that did not build or take its props, and why: shown as a
    /// caption in its place, and read by whoever checks the view (layout-check, tests), so a
    /// component that does not build is never only a log line. Shared with every reconciler of
    /// the same view.
    broken: Rc<RefCell<Vec<String>>>,
    /// The fragments that fill because the filled fragment they are the lone child of does,
    /// by key, each with the weight it passes on: a view call's fragment holds the fragment
    /// the view's body renders in, which holds the view's root, so a call's `fill` reaches
    /// that root through each fragment between ([`fragment_fill`]).
    filling: HashMap<String, f32>,
}

fn prop<'a>(node: &'a UiNode, name: &str) -> Option<&'a Value> {
    node.props.get(name)
}

/// How a value reads on screen.
#[must_use]
pub fn display(value: &Value) -> String {
    let formats = ui::format::Formats::current();
    match value {
        Value::Text(text) | Value::Error(text) => text.clone(),
        Value::Int(number) => {
            let digits = formats.count(number.unsigned_abs());
            if *number < 0 {
                format!("-{digits}")
            } else {
                digits
            }
        }
        Value::Float(number) => formats.decimal(*number, 2),
        Value::Bool(flag) => flag.to_string(),
        // A case by the name the runtime holds it by: a control that offers the cases finds
        // its own among them by the same name.
        Value::Enum(case) => case.clone(),
        Value::Principal(who) => who.id.clone(),
        Value::Opt(Some(inner)) => display(inner),
        // As the person reading reads it: each reference as its chip reads for them.
        Value::Rich(rich) => ui::widget::rich_doc::plain_text(rich),
        _ => String::new(),
    }
}

/// The flag `name` of `node`: a bool as the runtime lowers one (`Int`), or a `Bool`.
pub(crate) fn flag(node: &UiNode, name: &str) -> bool {
    match prop(node, name) {
        Some(Value::Int(number)) => *number != 0,
        Some(Value::Float(weight)) => *weight != 0.0,
        Some(Value::Bool(flag)) => *flag,
        _ => false,
    }
}

fn token<'a>(node: &'a UiNode, name: &str) -> Option<&'a str> {
    match prop(node, name) {
        Some(Value::Text(text)) => Some(text),
        _ => None,
    }
}

/// The cases a `choice` node shows, as its app read them from the enum it chooses from: each a
/// name, a label and an icon.
fn cases_of(node: &UiNode) -> Vec<Case> {
    let Some(Value::List(cases)) = prop(node, "cases") else {
        return Vec::new();
    };
    cases
        .iter()
        .filter_map(|case| match case {
            Value::List(parts) => match parts.as_slice() {
                [Value::Text(name), Value::Text(label), icon] => Some(Case {
                    name: name.clone(),
                    label: label.clone(),
                    icon: match icon {
                        Value::Opt(Some(icon)) => match &**icon {
                            Value::Text(icon) => ui::icons::Icon::from_name(icon),
                            _ => None,
                        },
                        _ => None,
                    },
                }),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "layout weights and sizes are small values far inside f32"
)]
fn number(node: &UiNode, name: &str) -> Option<f32> {
    match prop(node, name) {
        Some(Value::Float(number)) => Some(*number as f32),
        Some(Value::Int(number)) => Some(*number as f32),
        _ => None,
    }
}

/// A length argument: a unit-tagged float prop (`min.rem`, `max.dp`).
fn length(node: &UiNode, name: &str) -> Option<Length> {
    number(node, &format!("{name}.rem"))
        .map(Length::Rem)
        .or_else(|| number(node, &format!("{name}.dp")).map(Length::Dp))
}

fn rhythm(node: &UiNode, name: &str, default: Rhythm) -> Rhythm {
    match token(node, name) {
        Some("none") => Rhythm::None,
        Some("tight") => Rhythm::Tight,
        Some("related") => Rhythm::Related,
        Some("sibling") => Rhythm::Sibling,
        Some("section") => Rhythm::Section,
        Some("group") => Rhythm::Group,
        _ => default,
    }
}

fn align(node: &UiNode, default: Align) -> Align {
    match token(node, "align") {
        Some("start") => Align::Start,
        Some("center") => Align::Center,
        Some("end") => Align::End,
        Some("stretch") => Align::Stretch,
        _ => default,
    }
}

fn position(node: &UiNode) -> Option<At> {
    Some(match token(node, "at")? {
        "top_leading" => At::TopLeading,
        "top" => At::Top,
        "top_trailing" => At::TopTrailing,
        "leading" => At::Leading,
        "center" => At::Center,
        "trailing" => At::Trailing,
        "bottom_leading" => At::BottomLeading,
        "bottom" => At::Bottom,
        "bottom_trailing" => At::BottomTrailing,
        _ => return None,
    })
}

/// How many leads a node reserves before its own content (`lead: 1`): a document's body stands
/// one lead in, under the text its heading's control leads to.
fn lead(node: &UiNode) -> i16 {
    match prop(node, "lead") {
        Some(Value::Int(count)) => i16::try_from(*count).unwrap_or(0),
        _ => 0,
    }
}

/// The tree as it is drawn: a `page` inside another page, or inside a block's view, is a plain
/// stack. A page is the outermost layout of a window (gutter, readable measure); a view shown
/// within another view's layout takes the width that layout offers and adds no second gutter.
#[must_use]
pub fn normalize(node: &UiNode) -> UiNode {
    let mut out = page_once(node);
    crate::attach::hoist_in(&mut out);
    out
}

/// The tree with a page inside another page, or inside a block's view, a plain stack.
fn page_once(node: &UiNode) -> UiNode {
    fn walk(node: &UiNode, inside: bool) -> UiNode {
        let nested = inside || node.kind == "page" || node.kind == "embed";
        let mut out = node.clone();
        if inside && node.kind == "page" {
            "stack".clone_into(&mut out.kind);
            out.props.clear();
        }
        out.children = node
            .children
            .iter()
            .map(|child| walk(child, nested))
            .collect();
        out
    }
    walk(node, false)
}

/// How a child is sized in its container: `fill`, `min:`, `max:` bound its inline size.
pub(crate) fn fit_of(node: &UiNode) -> Fit {
    let mut fit = number(node, "fill").map_or_else(Fit::hug, Fit::fill);
    // A grid's `min` is its columns' least width, not its own: it takes the width it is given
    // and holds as many such columns as fit, one where none does.
    if node.kind != "grid" {
        fit.width.min = length(node, "min");
    }
    fit.width.max = length(node, "max");
    if let Some(ratio) = number(node, "aspect") {
        fit = fit.aspect(ratio);
    }
    fit
}

/// How `child` is sized in its container `parent`: the fit its own arguments ask for, the
/// columns it spans, where it sits among layers.
#[cfg(test)]
fn placement(parent: &UiNode, child: &UiNode) -> Placement {
    placement_in(parent, None, child)
}

/// The weight `child` fills its container `parent` with because `parent` is a fragment that
/// fills, by its own `fill` or by `inherited` (the weight the fragment it is the lone child
/// of passed on), and `child` is all it holds and asks nothing of its own: a call of a view
/// stands for what the view draws.
fn fragment_fill(parent: &UiNode, inherited: Option<f32>, child: &UiNode) -> Option<f32> {
    if parent.kind != "fragment" || parent.children.len() != 1 || number(child, "fill").is_some() {
        return None;
    }
    number(parent, "fill").or(inherited)
}

/// [`placement`] of `child` in `parent`, which fills with `inherited` when the fragment it is
/// the lone child of passed that on ([`fragment_fill`]).
fn placement_in(parent: &UiNode, inherited: Option<f32>, child: &UiNode) -> Placement {
    let mut fit = fit_of(child);
    // Beside siblings in a row, a container takes the room they leave rather than hugging
    // its content, so what it holds lays out in the width the row offers. A row that wraps
    // flows its items at their own width instead, as chips and tags do; and a row with a
    // spacer has said where its room goes, so its containers hold what they hold and the
    // spacer takes the rest (a bar's leading and trailing parts, each a group of its own). A
    // row whose group must give way when it is short of room (a card's title truncating
    // beside its menu) leaves the spacer out: the group then takes the room itself.
    if parent.kind == "row"
        && !flag(parent, "wrap")
        && !parent
            .children
            .iter()
            .any(|sibling| sibling.kind == "spacer")
        && (is_container(&child.kind) || child.kind == "embed")
        && number(child, "fill").is_none()
    {
        fit = Fit::fill(1.0);
        fit.width.min = length(child, "min");
        fit.width.max = length(child, "max");
    }
    // A call of a view stands for what the view draws: `show(r, fill)` or a filled call of
    // a view asks the view itself to fill, so the lone root it draws (a window's NavShell, a
    // list) takes the fragment's room instead of hugging its content. The view's body is a
    // fragment of its own inside the call's, and a view whose root is a call of another view
    // is one more: each passes the fill on.
    if let Some(weight) = fragment_fill(parent, inherited, child) {
        let min = fit.width.min;
        let max = fit.width.max;
        fit = Fit::fill(weight);
        fit.width.min = min;
        fit.width.max = max;
    }
    if child.kind == "spacer" {
        fit = Fit::fill(1.0);
    }
    let span = match prop(child, "span") {
        Some(Value::Int(span)) => u16::try_from(*span).unwrap_or(1).max(1),
        _ => 1,
    };
    Placement {
        fit,
        span,
        at: position(child),
    }
}

/// The shell of a node: everything of it but its children. Two nodes with equal shells keep
/// their container widget; only the children are reconciled.
fn same_shell(a: &UiNode, b: &UiNode) -> bool {
    a.kind == b.kind
        && a.key == b.key
        && a.props == b.props
        && a.binding == b.binding
        && a.events.keys().eq(b.events.keys())
}

fn is_container(kind: &str) -> bool {
    matches!(
        kind,
        "stack"
            | "column"
            | "fragment"
            | "row"
            | "grid"
            | "split"
            | "page"
            | "layers"
            | "primary"
            | "secondary"
            // A component whose own layout is a row or a column of its slots.
            | "ui.DragRun"
    )
}

/// Container widgets, whatever their layout, as the keyed reconciler reaches their children:
/// each change through the container's slot operations, which describe it to the window.
trait SlotHost {
    fn child_mut(&mut self, index: usize) -> Option<&mut Element>;
    fn replace(&mut self, index: usize, element: Element);
    fn insert(&mut self, index: usize, slot: Slot);
    fn remove(&mut self, index: usize);
    fn move_slot(&mut self, from: usize, to: usize);
    fn set_placement(&mut self, index: usize, placement: Placement);
    /// Reserve `lead` leads before what the container holds.
    fn set_lead(&mut self, lead: i16);
}

impl<L: ui::widget::arrange::Layout + 'static> SlotHost for ui::widget::arrange::Arranged<L> {
    fn child_mut(&mut self, index: usize) -> Option<&mut Element> {
        ui::widget::arrange::Arranged::child_mut(self, index)
    }

    fn replace(&mut self, index: usize, element: Element) {
        ui::widget::arrange::Arranged::replace(self, index, element);
    }

    fn insert(&mut self, index: usize, slot: Slot) {
        ui::widget::arrange::Arranged::insert(self, index, slot);
    }

    fn remove(&mut self, index: usize) {
        ui::widget::arrange::Arranged::remove(self, index);
    }

    fn move_slot(&mut self, from: usize, to: usize) {
        ui::widget::arrange::Arranged::move_slot(self, from, to);
    }

    fn set_placement(&mut self, index: usize, placement: Placement) {
        let Placement { fit, span, at } = placement;
        ui::widget::arrange::Arranged::set_placement(self, index, fit, span, at);
    }

    fn set_lead(&mut self, lead: i16) {
        ui::widget::arrange::Arranged::set_lead(self, lead);
    }
}

/// How a child is sized and placed in its container.
#[derive(Clone, Copy)]
struct Placement {
    fit: Fit,
    span: u16,
    at: Option<At>,
}

fn with_host<R>(element: &mut Element, run: impl FnOnce(&mut dyn SlotHost) -> R) -> Option<R> {
    use ui::widget::arrange::Arranged;
    use ui::widget::arrange::{FlexLayout, GridLayout, LayersLayout, PageLayout, SplitLayout};
    let any = element.as_any_mut()?;
    if any.is::<Arranged<FlexLayout>>() {
        return any
            .downcast_mut::<Arranged<FlexLayout>>()
            .map(|host| run(host));
    }
    if any.is::<Arranged<GridLayout>>() {
        return any
            .downcast_mut::<Arranged<GridLayout>>()
            .map(|host| run(host));
    }
    if any.is::<Arranged<SplitLayout>>() {
        return any
            .downcast_mut::<Arranged<SplitLayout>>()
            .map(|host| run(host));
    }
    if any.is::<Arranged<PageLayout>>() {
        return any
            .downcast_mut::<Arranged<PageLayout>>()
            .map(|host| run(host));
    }
    if any.is::<Arranged<LayersLayout>>() {
        return any
            .downcast_mut::<Arranged<LayersLayout>>()
            .map(|host| run(host));
    }
    None
}

impl Reconciler {
    #[must_use]
    pub fn new(sink: EventSink) -> Self {
        let adapter = Adapter::default();
        sink.follow(&adapter);
        Self {
            sink,
            texts: HashMap::new(),
            flags: HashMap::new(),
            counts: HashMap::new(),
            numbers: HashMap::new(),
            riches: HashMap::new(),
            navigation: None,
            adapter,
            rows: None,
            broken: Rc::default(),
            filling: HashMap::new(),
        }
    }

    /// A reconciler of the same view as `other`: its widgets report to the same sink and
    /// adapter.
    #[must_use]
    pub fn sharing(other: &Self) -> Self {
        Self {
            sink: Rc::clone(&other.sink),
            texts: HashMap::new(),
            flags: HashMap::new(),
            counts: HashMap::new(),
            numbers: HashMap::new(),
            riches: HashMap::new(),
            navigation: None,
            adapter: other.adapter.clone(),
            rows: other.rows.clone(),
            broken: Rc::clone(&other.broken),
            filling: HashMap::new(),
        }
    }

    /// Each component of the view that did not build or take its props, and why, as of now.
    #[must_use]
    pub fn broken(&self) -> Vec<String> {
        self.broken.borrow().clone()
    }

    fn note_broken(&self, node: &UiNode, error: &ui::component::Error) {
        tracing::warn!(%error, key = %node.key, "a component of the view did not build");
        self.broken
            .borrow_mut()
            .push(format!("{} ({}): {error}", node.kind, node.key));
    }

    /// Read the rows of the virtual lists it builds from `rows`.
    #[must_use]
    pub fn with_rows(mut self, rows: crate::adapter::RowsOf) -> Self {
        self.rows = Some(rows);
        self
    }

    /// The rows of the virtual list `key`, as the host renders them.
    #[must_use]
    pub fn rows_of(&self, key: &str) -> Option<ui::component::SharedRows> {
        self.rows.as_ref().and_then(|rows| rows(key))
    }

    /// What the components of the view report.
    #[must_use]
    pub fn adapter(&self) -> &Adapter {
        &self.adapter
    }

    /// Have the components it builds report to `adapter`, the view's.
    pub fn report_to(&mut self, adapter: Adapter) {
        self.adapter = adapter;
    }

    /// The text the control `key` shows, kept in step with what it is bound to.
    pub fn text_state(&mut self, key: &str) -> LocalState<String> {
        self.texts
            .entry(key.to_owned())
            .or_insert_with(|| LocalState::new(String::new()))
            .clone()
    }

    /// The flag the control `key` shows, kept in step with what it is bound to.
    pub fn flag_state(&mut self, key: &str) -> LocalState<bool> {
        self.flags
            .entry(key.to_owned())
            .or_insert_with(|| LocalState::new(false))
            .clone()
    }

    /// The whole number the control `key` shows, kept in step with what it is bound to.
    pub fn count_state(&mut self, key: &str) -> LocalState<i64> {
        self.counts
            .entry(key.to_owned())
            .or_insert_with(|| LocalState::new(0))
            .clone()
    }

    /// The number the control `key` shows, kept in step with what it is bound to.
    pub fn number_state(&mut self, key: &str) -> LocalState<f64> {
        self.numbers
            .entry(key.to_owned())
            .or_insert_with(|| LocalState::new(0.0))
            .clone()
    }

    /// Show `number` in the control `key`, reporting whether that changed what it shows.
    pub fn set_count(&self, key: &str, number: i64) -> bool {
        if let Some(state) = self.counts.get(key)
            && state.get() != number
        {
            state.set(number);
            return true;
        }
        false
    }

    /// Show `number` in the control `key`, reporting whether that changed what it shows.
    pub fn set_number(&self, key: &str, number: f64) -> bool {
        if let Some(state) = self.numbers.get(key)
            && state.get().total_cmp(&number).is_ne()
        {
            state.set(number);
            return true;
        }
        false
    }

    /// The rich text the rich input `key` shows, kept in step with what it is bound to.
    pub fn rich_state(
        &mut self,
        key: &str,
    ) -> LocalState<tilo_render_protocol::patch::rich::RichText> {
        self.riches
            .entry(key.to_owned())
            .or_insert_with(|| {
                LocalState::new(tilo_render_protocol::patch::rich::RichText::default())
            })
            .clone()
    }

    /// Show `rich` in the rich input `key`, reporting whether that changed what it shows.
    pub fn set_rich(&self, key: &str, rich: &tilo_render_protocol::patch::rich::RichText) -> bool {
        if let Some(state) = self.riches.get(key)
            && state.with(|shown| shown != rich)
        {
            state.set(rich.clone());
            return true;
        }
        false
    }

    /// The handle on the split of the view that collapses to one pane at a time, when it has one.
    #[must_use]
    pub fn navigation(&self) -> Option<Navigation> {
        self.navigation.clone()
    }

    /// Forget the split of the last view built, before building another.
    pub fn clear_navigation(&mut self) {
        self.navigation = None;
    }

    /// Show `text` in the input or text node `key`, whatever widget shows it, reporting whether
    /// that changed what it shows.
    pub fn set_text(&self, key: &str, text: &str) -> bool {
        if let Some(state) = self.texts.get(key)
            && state.with(|shown| shown != text)
        {
            state.set(text.to_owned());
            return true;
        }
        false
    }

    /// Show `checked` in the checkbox `key`, reporting whether that changed what it shows.
    pub fn set_flag(&self, key: &str, checked: bool) -> bool {
        if let Some(state) = self.flags.get(key)
            && state.get() != checked
        {
            state.set(checked);
            return true;
        }
        false
    }

    /// The widgets of the tree under `node`.
    pub fn build(&mut self, node: &UiNode) -> Element {
        let mut element = self.build_kind(node);
        // A container's lead is room of its own, over the padding it keeps: what it holds
        // stands that far in from its leading edge, under the text a heading's control leads
        // to.
        let lead = lead(node);
        if lead > 0 {
            with_host(&mut element, |host| host.set_lead(lead));
        }
        element
    }

    /// The widget of `node` itself, before the lead its arguments reserve.
    fn build_kind(&mut self, node: &UiNode) -> Element {
        if crate::adapter::component_name(&node.kind).is_some() {
            return self.component(node);
        }
        match node.kind.as_str() {
            "stack" | "column" | "fragment" | "primary" | "secondary" => {
                let default = if node.kind == "fragment" {
                    Rhythm::Tight
                } else {
                    Rhythm::Sibling
                };
                self.stack(node, default)
            }
            "row" => self.row(node),
            "grid" => self.grid(node),
            "split" => self.split(node),
            "page" => self.page(node),
            "layers" => self.layers(node),
            "scroll" => self.scroll(node),
            // A spacer takes the room its row or column leaves (`placement`).
            "spacer" => Box::new(Spacer::new().with_id(node.key.clone())),
            "divider" => Box::new(Divider::horizontal()),
            "form" | "field" => self.holder(node),
            "toggle" => self.toggle(node),
            "choice" => self.choice(node),
            "list" => self.rows(node),
            "item" => self.stack(node, Rhythm::Tight),
            "checkbox" => self.checkbox(node),
            "date" => self.date(node),
            "image" => self.image(node),
            "embed" => self.embed(node),
            "invocation" => self.invocation_card(node),
            "error" => self.error_block(node),
            _ => self.text_of(node, "", TextRole::Caption),
        }
    }

    /// A node of a described component, built by the one adapter from its description.
    fn component(&mut self, node: &UiNode) -> Element {
        let Some(description) = crate::adapter::description_of(node) else {
            self.note_broken(node, &ui::component::Error::unknown(&node.kind));
            return self.text_of(node, "", TextRole::Caption);
        };
        let built = {
            let mut source = NodeSource::new(node, description, self);
            ui::component::build(&description.name, &mut source)
        };
        built.unwrap_or_else(|error| {
            self.note_broken(node, &error);
            self.text_of(node, "", TextRole::Caption)
        })
    }

    /// Bring `widget`, built for the component node `old_node`, to `new_node`: its props in
    /// place where the widget takes them, and the views in its slots reconciled where they
    /// stand. `Some` is the widget built anew when it could not.
    fn update_component(
        &mut self,
        widget: &mut Element,
        old_node: &UiNode,
        new_node: &UiNode,
        description: &'static ui::component::Description,
    ) -> Option<Element> {
        if !crate::adapter::same_data(old_node, new_node, description) {
            let taken = {
                let mut source = NodeSource::in_place(new_node, description, self);
                ui::component::update(&description.name, widget, &mut source)
            };
            match taken {
                Ok(true) => {}
                Ok(false) => return Some(self.build(new_node)),
                Err(error) => {
                    self.note_broken(new_node, &error);
                    return Some(self.build(new_node));
                }
            }
        }
        // Each slot of the widget takes its views by key, in place: those gone removed, those
        // kept moved and reconciled where they stand, those new built.
        for slot in description
            .slots
            .iter()
            .filter(|slot| slot.accepts == ui::component::Accepts::View)
        {
            let old = crate::adapter::slot_nodes(old_node, description, &slot.name);
            let new = crate::adapter::slot_nodes(new_node, description, &slot.name);
            if old == new {
                continue;
            }
            let Some(views) = ui::component::slot_views(&description.name, widget, &slot.name)
            else {
                return Some(self.build(new_node));
            };
            self.reconcile_views(views, &old, &new);
        }
        // A slot of components whose own views changed (a run's items, each holding a view of
        // its own): the part's views change where they stand, so the widget keeps what they
        // hold rather than being built again around them.
        for slot in description
            .slots
            .iter()
            .filter(|slot| slot.accepts.reads_components())
        {
            let old = crate::adapter::slot_parts(old_node, description, &slot.name);
            let new = crate::adapter::slot_parts(new_node, description, &slot.name);
            for child in &new {
                let Some(part) = crate::adapter::description_of(child) else {
                    continue;
                };
                let Some(before) = old
                    .iter()
                    .find(|old| old.key == child.key && old.kind == child.kind)
                else {
                    continue;
                };
                for of in part
                    .slots
                    .iter()
                    .filter(|of| of.accepts == ui::component::Accepts::View)
                {
                    let old = crate::adapter::slot_nodes(before, part, &of.name);
                    let new = crate::adapter::slot_nodes(child, part, &of.name);
                    if old == new {
                        continue;
                    }
                    let Some(views) = ui::component::part_views(
                        &description.name,
                        widget,
                        &slot.name,
                        &child.key,
                        &of.name,
                    ) else {
                        return Some(self.build(new_node));
                    };
                    self.reconcile_views(views, &old, &new);
                }
            }
        }
        None
    }

    /// Bring the views of one slot, built for `old`, to `new` by key: those gone removed, those
    /// kept moved to their place and reconciled in it, those new built and inserted.
    fn reconcile_views(
        &mut self,
        views: &mut dyn ui::component::SlotViews,
        old: &[&UiNode],
        new: &[&UiNode],
    ) {
        let mut held: Vec<&UiNode> = old.to_vec();
        for index in (0..held.len()).rev() {
            let gone = held
                .get(index)
                .is_some_and(|old| !new.iter().any(|child| child.key == old.key));
            if gone {
                views.remove(index);
                held.remove(index);
            }
        }
        for (index, &child) in new.iter().enumerate() {
            let Some(at) = held.iter().position(|old| old.key == child.key) else {
                let view = self.slot_view(child);
                views.insert(index, view);
                held.insert(index, child);
                continue;
            };
            if at != index {
                views.move_view(at, index);
                let moved = held.remove(at);
                held.insert(index, moved);
            }
            let Some(&before) = held.get(index) else {
                continue;
            };
            let Some(view) = views.view_mut(index) else {
                continue;
            };
            // The view carries the fit its node asks for, so a view that comes to say `fill`
            // asks for the room the component gives it from the next build on (the slot keeps
            // the fit it was placed with, as a rule a view's own arguments do not change under
            // its key). A [`Fitted`] asking for nothing sizes as a bare view does.
            if let Some(fitted) = view
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<ui::widget::Fitted>())
            {
                fitted.set_fit(fit_of(child));
                let replaced = match fitted
                    .content_mut()
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<Pane>())
                {
                    Some(pane) => {
                        if let Some(replaced) = self.update_in(pane.content_mut(), before, child) {
                            pane.replace(replaced);
                        }
                        None
                    }
                    None => self.update_in(fitted.content_mut(), before, child),
                };
                if let Some(replaced) = replaced {
                    *fitted.content_mut() = replaced;
                }
                continue;
            }
            let replaced = match view.as_any_mut().and_then(|any| any.downcast_mut::<Pane>()) {
                Some(pane) => {
                    if let Some(replaced) = self.update_in(pane.content_mut(), before, child) {
                        pane.replace(replaced);
                    }
                    None
                }
                None => self.update_in(view, before, child),
            };
            if let Some(replaced) = replaced {
                views.replace(index, replaced);
            }
        }
    }

    /// `node`, built, as the view a described component's slot holds: a component as its
    /// widget, a view of another kind in a pane the reconciler reaches again, each in a
    /// [`ui::widget::Fitted`] carrying the fit its node wrote, so a view that says `fill` fills
    /// the slot the component puts it in ([`ui::widget::arrange::Slot::new`] takes the fit).
    pub(crate) fn slot_view(&mut self, node: &UiNode) -> Element {
        let built = self.build(node);
        let pane: Element = if crate::adapter::description_of(node).is_some() {
            built
        } else {
            Box::new(Pane::new(built))
        };
        let fit = fit_of(node);
        if fit == Fit::hug() {
            return pane;
        }
        Box::new(ui::widget::Fitted::new(pane, fit))
    }

    fn slots(&mut self, node: &UiNode) -> Vec<Slot> {
        placed(node)
            .into_iter()
            .map(|child| self.slot(node, &child))
            .collect()
    }

    /// How `child` is sized in its container `parent` ([`placement_in`]), and, for a fragment
    /// that fills because `parent` passes its fill on, the weight it passes on in turn: placed
    /// before it is built, so what it holds is placed knowing it.
    fn place(&mut self, parent: &UiNode, placed: &Placed<'_>) -> Placement {
        let child = placed.node;
        let through = &placed.through;
        let inherited = self.filling.get(&parent.key).copied();
        let mut placement = placement_in(parent, inherited, child);
        // A fragment draws nothing of its own, so what it asks for is the child standing under
        // it: a `for`'s fill reaches each row it places in a row or a grid, and the narrowest
        // width it names floors each of them.
        let mut passed = None;
        let mut aspect = None;
        for fragment in through {
            passed = number(fragment, "fill").or(passed);
            if prop(child, "min").is_none()
                && let Some(min) = length(fragment, "min")
            {
                placement.fit.width.min = Some(min);
            }
            if prop(child, "max").is_none()
                && let Some(max) = length(fragment, "max")
            {
                placement.fit.width.max = Some(max);
            }
            if prop(child, "aspect").is_none() && aspect.is_none() {
                aspect = number(fragment, "aspect");
            }
        }
        // The child's own arguments win; else it takes the weight the fragments pass on.
        if let Some(weight) = passed.filter(|_| number(child, "fill").is_none()) {
            let (min, max) = (placement.fit.width.min, placement.fit.width.max);
            placement.fit = Fit::fill(weight);
            placement.fit.width.min = min;
            placement.fit.width.max = max;
        }
        if let Some(ratio) = aspect {
            placement.fit = placement.fit.aspect(ratio);
        }
        match fragment_fill(parent, inherited, child) {
            Some(weight) if child.kind == "fragment" => {
                self.filling.insert(child.key.clone(), weight);
            }
            _ => {
                self.filling.remove(&child.key);
            }
        }
        placement
    }

    /// `child`, built, as a slot of its container `parent`.
    fn slot(&mut self, parent: &UiNode, placed: &Placed<'_>) -> Slot {
        let child = placed.node;
        let Placement { fit, span, at } = self.place(parent, placed);
        let mut slot = Slot::new(self.build(child));
        slot.fit = fit;
        slot.span = span;
        slot.at = at;
        slot
    }

    fn stack(&mut self, node: &UiNode, default: Rhythm) -> Element {
        let mut stack = Stack::stack()
            .fill_width()
            .gap(rhythm(node, "gap", default))
            .padding(rhythm(node, "padding", Rhythm::None))
            .align(align(node, Align::Stretch))
            .with_id(node.key.clone());
        for slot in self.slots(node) {
            stack = stack.push_slot(slot);
        }
        Box::new(stack)
    }

    fn row(&mut self, node: &UiNode) -> Element {
        let mut row = Row::row()
            .fill_width()
            .gap(rhythm(node, "gap", Rhythm::Related))
            .padding(rhythm(node, "padding", Rhythm::None))
            .align(align(node, Align::Center))
            .distribute(match token(node, "distribute") {
                Some("center") => Distribute::Center,
                Some("end") => Distribute::End,
                Some("between") => Distribute::Between,
                _ => Distribute::Start,
            })
            .with_id(node.key.clone());
        if flag(node, "wrap") {
            row = row.wrap();
        }
        for slot in self.slots(node) {
            row = row.push_slot(slot);
        }
        Box::new(row)
    }

    fn grid(&mut self, node: &UiNode) -> Element {
        let mut grid = match (number(node, "columns"), length(node, "min")) {
            (Some(columns), _) => Grid::columns(columns as u16),
            (None, min) => Grid::auto(min.unwrap_or(Length::Rem(11.0))),
        }
        .gap(rhythm(node, "gap", Rhythm::Sibling))
        .with_id(node.key.clone());
        // A dashboard's cards list different things: each is as tall as what it lists.
        if holds_cards(node) {
            grid = grid.hugging();
        }
        for slot in self.slots(node) {
            grid = grid.push_slot(slot);
        }
        Box::new(grid)
    }

    fn split(&mut self, node: &UiNode) -> Element {
        let minimum = |kind: &str, default: f32| {
            node.children
                .iter()
                .find(|child| child.kind == kind)
                .and_then(|pane| length(pane, "min"))
                .unwrap_or(Length::Rem(default))
        };
        let mut split = Split::new(minimum("primary", 16.0), minimum("secondary", 20.0))
            .side(match token(node, "side") {
                Some("trailing") => Side::Trailing,
                _ => Side::Leading,
            })
            .collapse(match token(node, "collapse") {
                Some("drawer") => Collapse::Drawer,
                Some("navigate") => Collapse::Navigate,
                _ => Collapse::Stack,
            })
            .with_id(node.key.clone());
        if let Some(ratio) = number(node, "ratio") {
            split = split.ratio(ratio);
        }
        if token(node, "collapse") == Some("navigate") {
            self.navigation = Some(split.navigation());
        }
        for child in &node.children {
            let mut slot = Slot::new(self.build(child));
            slot.fit = Fit::hug();
            split = split.push_slot(slot);
        }
        Box::new(split)
    }

    fn page(&mut self, node: &UiNode) -> Element {
        let mut page = if flag(node, "full") {
            Page::full()
        } else {
            Page::new()
        };
        // A page that fills takes the height its pane gives it: a canvas's content (a board's
        // lanes) stands on the pane's floor, and what it holds scrolls inside it.
        if flag(node, "fill") {
            page = page.filling();
        }
        let mut page = page.with_id(node.key.clone());
        for slot in self.slots(node) {
            page = page.push_slot(slot);
        }
        Box::new(page)
    }

    fn layers(&mut self, node: &UiNode) -> Element {
        let mut layers = Layers::layers().with_id(node.key.clone());
        for slot in self.slots(node) {
            layers = layers.push_slot(slot);
        }
        Box::new(layers)
    }

    fn scroll(&mut self, node: &UiNode) -> Element {
        let mut inner = Stack::stack()
            .fill_width()
            .gap(Rhythm::Sibling)
            .with_id(format!("{}~content", node.key));
        for slot in self.slots(node) {
            inner = inner.push_slot(slot);
        }
        // A surface that fills its pane gives what it holds the pane's height, so its content
        // stands at the pane's size and pans inside it rather than growing to its own: the desk
        // a person pans, and a row of windows kept to their own widths whose bars and apps fill
        // the room between the desk's edges.
        let filling = flag(node, "fill") || token(node, "axis") == Some("both");
        if filling {
            inner = inner.fill_height();
        }
        let across = token(node, "axis");
        let scroll = if filling {
            let axis = match across {
                Some("horizontal") => ui::widget::ScrollAxis::Horizontal,
                Some("both") => ui::widget::ScrollAxis::Both,
                _ => ui::widget::ScrollAxis::Vertical,
            };
            Scrollable::new(axis).fill_parent_height()
        } else if across == Some("horizontal") {
            Scrollable::new_horizontal()
        } else {
            Scrollable::new_vertical().fit_content_height()
        };
        // A row that shares the desk's width by weight fills it when its
        // children fit, and scrolls once their least widths need more.
        let scroll = if filling && across == Some("horizontal") {
            scroll.with_content_min_viewport_width()
        } else {
            scroll
        };
        Box::new(scroll.with_child(Box::new(inner)))
    }

    fn text_of(&mut self, node: &UiNode, value: &str, role: TextRole) -> Element {
        let state = self
            .texts
            .entry(node.key.clone())
            .or_insert_with(|| LocalState::new(value.to_owned()))
            .clone();
        state.set(value.to_owned());
        Box::new(
            Text::from_state(&state, |shown: &String| shown.clone())
                .with_role(role)
                .with_id(node.key.clone()),
        )
    }

    fn date(&mut self, node: &UiNode) -> Element {
        let state = self
            .texts
            .entry(node.key.clone())
            .or_insert_with(|| LocalState::new(String::new()))
            .clone();
        Box::new(crate::date::DateControl::new(
            &node.key,
            &state,
            &self.sink,
            flag(node, "optional"),
        ))
    }

    fn checkbox(&mut self, node: &UiNode) -> Element {
        let state = self
            .flags
            .entry(node.key.clone())
            .or_insert_with(|| LocalState::new(false))
            .clone();
        let sink = Rc::clone(&self.sink);
        let key = node.key.clone();
        let label = crate::date::shown(node, "label").unwrap_or_default();
        let control = Checkbox::from_state(&state)
            .on_change(move |checked| {
                sink.push(UiEvent::Toggle(key.clone(), checked));
            })
            .with_id(&node.key)
            .with_enabled(prop(node, "enabled").is_none() || flag(node, "enabled"));
        Box::new(labeled_check(label, control))
    }

    /// A block an agent or a person wrote, rendered inline: its result in a card, so it reads as
    /// somebody's message rather than as part of the app.
    fn embed(&mut self, node: &UiNode) -> Element {
        let inner = self.stack(node, Rhythm::Related);
        if !frames(node) {
            // A view made of objects (an agent's answer of cards) stands as they do: a card
            // never stands in another, so the boundary draws nothing of its own.
            return Box::new(Container::new().with_no_padding().with_child(inner));
        }
        // A view boundary: the card takes the width its column offers, up to the readable measure,
        // and the embedded view lays itself out in what it was given.
        Box::new(
            Container::new()
                .with_background()
                .readable()
                .with_child(inner),
        )
    }

    /// What became of an action block when it was posted, drawn from the invocation record: a
    /// proposal waiting on a person is its own block ([`Self::suggestion`]).
    fn invocation_card(&mut self, node: &UiNode) -> Element {
        if flag(node, "proposed") {
            return self.suggestion(node);
        }
        let state = token(node, "state").unwrap_or("pending");
        let name = crate::date::shown(node, "label").unwrap_or_default();
        let mut card = Stack::stack()
            .gap(Rhythm::Related)
            .with_id(node.key.clone());
        let status = crate::i18n::t(&format!("tilolang-invocation-{state}"));
        let header = Row::row()
            .fill_width()
            .gap(Rhythm::Related)
            .distribute(Distribute::Between)
            .push_slot(self.fixed_text(&format!("{}~name", node.key), &name, TextRole::Label))
            .push_slot(self.fixed_text(&format!("{}~state", node.key), &status, TextRole::Caption));
        card = card.push_slot(Slot::new(Box::new(header)));
        if let Some(result) = crate::date::shown(node, "result") {
            card = card.push_slot(self.fixed_text(
                &format!("{}~result", node.key),
                &result,
                TextRole::Body,
            ));
        }
        for slot in self.slots(node) {
            card = card.push_slot(slot);
        }
        if state == "suggested" && flag(node, "can_resolve") {
            // An ask the authority raised answers as a confirmation.
            let choices = Row::row()
                .gap(Rhythm::Related)
                .push_slot(self.answer(
                    node,
                    "allow",
                    &crate::i18n::t("tilolang-ask-confirm"),
                    true,
                    false,
                ))
                .push_slot(self.answer(
                    node,
                    "decline",
                    &crate::i18n::t("tilolang-ask-decline"),
                    false,
                    false,
                ));
            card = card.push_slot(Slot::new(Box::new(choices)));
        }
        Box::new(card)
    }

    /// A proposal waiting on the person: one calm block on the inset surface — who proposes it,
    /// what it would do, and the answer to it — so it is noticed where it stands among what the
    /// view draws. What it would do is the action's own proposal view when it declares one, and
    /// otherwise the words it would say, each value as its type shows it (a day as the day fact).
    fn suggestion(&mut self, node: &UiNode) -> Element {
        let (who, identity) = match prop(node, "author") {
            Some(Value::Principal(who)) => (who.name.clone(), who.id.clone()),
            Some(other) => {
                let shown = display(other);
                (shown.clone(), shown)
            }
            None => (String::new(), String::new()),
        };
        let says = crate::i18n::word(
            "tilolang-suggestion-says",
            &[("name".to_owned(), who.clone())],
            "{name} suggests:",
        );
        let mut sentence = Row::row()
            .gap(Rhythm::Tight)
            .wrap()
            .with_id(format!("{}~says", node.key))
            .push(Box::new(
                Avatar::person(who, identity).size(AvatarSize::Small),
            ))
            .push_slot(self.fixed_text(&format!("{}~by", node.key), &says, TextRole::Body));
        let choices = Row::row()
            .gap(Rhythm::Related)
            .push_slot(self.answer(
                node,
                "allow",
                &crate::i18n::t("tilolang-suggestion-accept"),
                true,
                true,
            ))
            .push_slot(self.answer(
                node,
                "decline",
                &crate::i18n::t("tilolang-suggestion-dismiss"),
                false,
                true,
            ));
        let mut rows: Vec<Element> = Vec::new();
        if flag(node, "view") {
            // The action shows what it would do itself: the sentence says who proposes it, the
            // view stands under it, and the answer beneath.
            rows.push(Box::new(sentence));
            for child in &node.children {
                rows.push(self.build(child));
            }
            rows.push(Box::new(choices));
        } else {
            // The words the action would say stand in the sentence, after who proposes it.
            for child in &node.children {
                sentence = sentence.push(self.build(child));
            }
            // The answer stands at the end of the sentence's line when it fits there, and below
            // it when the room is too narrow.
            rows.push(Box::new(
                Row::row()
                    .fill_width()
                    .wrap()
                    .gap(Rhythm::Related)
                    .distribute(Distribute::Between)
                    .push(Box::new(sentence))
                    .push(Box::new(choices)),
            ));
        }
        let mut block = Stack::stack()
            .fill_width()
            .gap(Rhythm::Tight)
            .with_id(node.key.clone());
        for row in rows {
            block = block.push(row);
        }
        Box::new(
            Container::new()
                .with_background()
                .with_inset(Rhythm::Related)
                .with_child(Box::new(block)),
        )
    }

    /// The button answering the invocation card `node`: allowing it or declining it. The answer
    /// to a suggestion is compact, so it stands at the end of the sentence's own line.
    fn answer(
        &self,
        node: &UiNode,
        suffix: &str,
        label: &str,
        approve: bool,
        compact: bool,
    ) -> Slot {
        let sink = Rc::clone(&self.sink);
        let key = node.key.clone();
        let role = if approve {
            ButtonRole::Primary
        } else if compact {
            ButtonRole::Plain
        } else {
            ButtonRole::Secondary
        };
        let size = if compact {
            ControlSize::Small
        } else {
            ControlSize::Regular
        };
        Slot::new(Box::new(
            Button::new(&format!("{}~{suffix}", node.key), label)
                .role(role)
                .control_size(size)
                .on_view(move || {
                    sink.push(UiEvent::Resolve(key.clone(), approve));
                }),
        ))
    }

    /// A block that did not compile or failed: the diagnostics, as text.
    fn error_block(&mut self, node: &UiNode) -> Element {
        let mut block = Stack::stack()
            .gap(Rhythm::Tight)
            .with_id(node.key.clone())
            .push_slot(self.fixed_text(
                &format!("{}~title", node.key),
                &crate::i18n::t("tilolang-embed-error"),
                TextRole::Label,
            ));
        for child in &node.children {
            let line = crate::date::shown(child, "value").unwrap_or_default();
            block = block.push_slot(self.fixed_text(&child.key, &line, TextRole::Code));
        }
        Box::new(block)
    }

    /// A text slot of host chrome or of a record, with its own state.
    fn fixed_text(&mut self, key: &str, value: &str, role: TextRole) -> Slot {
        let state = self
            .texts
            .entry(key.to_owned())
            .or_insert_with(|| LocalState::new(value.to_owned()))
            .clone();
        state.set(value.to_owned());
        Slot::new(Box::new(
            Text::from_state(&state, |shown: &String| shown.clone())
                .with_role(role)
                .with_id(key),
        ))
    }

    fn image(&mut self, node: &UiNode) -> Element {
        let alt = prop(node, "alt").or_else(|| prop(node, "src")).map(display);
        self.text_of(node, &alt.unwrap_or_default(), TextRole::Caption)
    }

    /// A form or a field: the native widget, with each part of its content in a [`Pane`] the
    /// reconciler reaches again, so content that runs again keeps its widgets.
    fn holder(&mut self, node: &UiNode) -> Element {
        let mut section = FormSection::untitled();
        if node.kind == "field" {
            return Box::new(section.field(self.field_of(node)));
        }
        for child in &node.children {
            if child.kind == "field" {
                section = section.field(self.field_of(child));
            } else {
                section = section.content(self.pane(child));
            }
        }
        Box::new(section)
    }

    /// The field `node`: its label and every control its block holds, one under the next. A
    /// day and the time of that day stand as one field's two controls.
    fn field_of(&mut self, node: &UiNode) -> Field {
        let mut panes: Vec<Element> = node.children.iter().map(|child| self.pane(child)).collect();
        let content = match panes.len() {
            0 => Box::new(Spacer::token(ui::tokens::SpacingStep::S0)) as Element,
            1 => panes.remove(0),
            _ => {
                let mut stack = Stack::stack();
                for pane in panes {
                    stack = stack.push(pane);
                }
                Box::new(stack)
            }
        };
        let label = crate::date::shown(node, "label").unwrap_or_default();
        if label.is_empty() {
            // The form's title says what its one field asks: no label stands beside it.
            return Field::unlabeled(content);
        }
        Field::custom(label, content)
    }

    /// `node` built, in the pane the holder reaches it again by.
    fn pane(&mut self, node: &UiNode) -> Element {
        Box::new(Pane::new(self.build(node)))
    }

    /// A holder whose shape is the same keeps its widget; each part is reconciled in its pane.
    fn update_holder(
        &mut self,
        widget: &mut Element,
        old_node: &UiNode,
        new_node: &UiNode,
    ) -> Option<Element> {
        if holder_shape(old_node) != holder_shape(new_node) {
            return Some(self.build(new_node));
        }
        let old_parts = holder_parts(old_node);
        let new_parts = holder_parts(new_node);
        let mut panes = Vec::new();
        collect_panes(widget.as_mut(), &mut panes);
        if panes.len() != new_parts.len() {
            return Some(self.build(new_node));
        }
        for ((pane, before), after) in panes.into_iter().zip(old_parts).zip(new_parts) {
            if let Some(replaced) = self.update_in(pane.content_mut(), before, after) {
                pane.replace(replaced);
            }
        }
        None
    }

    /// A switch bound to a `Bool`, with its label: a row of a form.
    fn toggle(&mut self, node: &UiNode) -> Element {
        let state = self
            .flags
            .entry(node.key.clone())
            .or_insert_with(|| LocalState::new(false))
            .clone();
        let sink = Rc::clone(&self.sink);
        let key = node.key.clone();
        let label = crate::date::shown(node, "label").unwrap_or_default();
        let control = Toggle::from_state(&state).on_change(move |checked| {
            sink.push(UiEvent::Toggle(key.clone(), checked));
        });
        Box::new(FormSection::untitled().field(Field::new(label, control)))
    }

    /// A choice of an enum's cases: segments for a few, a menu for more.
    fn choice(&self, node: &UiNode) -> Element {
        let cases = cases_of(node);
        let selected = match prop(node, "selected") {
            Some(Value::Text(case)) => Some(case.clone()),
            Some(Value::Enum(variant)) => Some(variant.clone()),
            _ => None,
        };
        let at = selected
            .as_ref()
            .and_then(|case| cases.iter().position(|shown| shown.name == *case));
        let sink = Rc::clone(&self.sink);
        let key = node.key.clone();
        let names: Vec<String> = cases.iter().map(|case| case.name.clone()).collect();
        // A form's quick choices stand one under the next, each the width of the field and read
        // as a row: the label at its leading edge, and what the choice stands for at its
        // trailing edge where the choice says its cases hold one ("Tomorrow morning …
        // Sat 09:00"). A label is never squeezed or elided, and every choice is one press.
        if flag(node, "list") {
            let mut list = Stack::stack().fill_width().gap(Rhythm::Tight);
            for (index, case) in cases.iter().enumerate() {
                let sink = Rc::clone(&self.sink);
                let key = node.key.clone();
                let name = case.name.clone();
                let mut button = Button::new(&format!("{}~{index}", node.key), &case.label)
                    .fill_width()
                    .toggled(at == Some(index));
                if let Some(detail) = crate::date::shown_case(node, &case.name) {
                    button = button.with_detail(detail);
                }
                list = list.push(Box::new(button.on_click(move || {
                    sink.push(UiEvent::Fire(key.clone(), format!("pick.{name}"), None));
                })));
            }
            return Box::new(list);
        }
        // A few cases are segments; more, or people and resources (`menu`), a menu.
        if cases.len() <= 4 && !flag(node, "menu") {
            let segments = cases
                .iter()
                .map(|case| Segment::new(case.label.clone()))
                .collect();
            Box::new(
                ToggleGroup::new(segments, at.unwrap_or(usize::MAX)).on_change(move |index| {
                    if let Some(case) = names.get(index) {
                        sink.push(UiEvent::Fire(key.clone(), format!("pick.{case}"), None));
                    }
                }),
            )
        } else {
            let labels: Vec<String> = cases.iter().map(|case| case.label.clone()).collect();
            let state = LocalState::new(
                at.and_then(|index| labels.get(index).cloned())
                    .unwrap_or_default(),
            );
            let chosen = labels.clone();
            let placeholder = crate::date::shown(node, "placeholder").unwrap_or_default();
            Box::new(
                Dropdown::from_state(labels, &state)
                    .with_placeholder(&placeholder)
                    .on_change(move |label| {
                        if let Some(case) = chosen
                            .iter()
                            .position(|shown| shown == label)
                            .and_then(|index| names.get(index))
                        {
                            sink.push(UiEvent::Fire(key.clone(), format!("pick.{case}"), None));
                        }
                    }),
            )
        }
    }

    /// A list inside content: its rows (a card's preview, which has only those it shows), as a
    /// section of the page.
    fn rows(&mut self, node: &UiNode) -> Element {
        let row_kind = crate::adapter::kind("ListRow");
        let rows: Vec<Element> = node
            .children
            .iter()
            .map(|item| {
                let row = find(item, &row_kind).unwrap_or(item);
                self.build(row)
            })
            .collect();
        if rows.is_empty() {
            let empty = crate::date::shown(node, "empty").unwrap_or_default();
            return Box::new(EmptyState::new(empty).inline());
        }
        Box::new(Section::new(rows))
    }

    /// Bring `old`, the widget built for `old_node`, up to date with `new_node`: the same
    /// widget, changed in place, or the one built in its stead.
    pub fn update(&mut self, mut old: Element, old_node: &UiNode, new_node: &UiNode) -> Element {
        self.update_in(&mut old, old_node, new_node).unwrap_or(old)
    }

    /// Bring `widget`, built for `old_node`, up to date with `new_node` in place: a changed
    /// value sets the widget that shows it, which draws itself again, and a container's
    /// children are inserted, removed, moved or replaced by key through its slot operations.
    /// `Some` is a widget built for `new_node` that its container shows instead (through its
    /// own `replace`): `widget` could not take the change.
    pub fn update_in(
        &mut self,
        widget: &mut Element,
        old_node: &UiNode,
        new_node: &UiNode,
    ) -> Option<Element> {
        if old_node == new_node {
            return None;
        }
        if old_node.kind != new_node.kind || old_node.key != new_node.key {
            return Some(self.build(new_node));
        }
        if let Some(description) = crate::adapter::description_of(new_node) {
            return self.update_component(widget, old_node, new_node, description);
        }
        match new_node.kind.as_str() {
            "image" => {
                if same_shell_but(old_node, new_node, "value") {
                    let value = crate::date::shown_brief(new_node, "value").unwrap_or_default();
                    self.set_text(&new_node.key, &value);
                    return None;
                }
                Some(self.build(new_node))
            }
            "form" | "field" => self.update_holder(widget, old_node, new_node),
            "checkbox" | "date" => (!same_shell(old_node, new_node)).then(|| self.build(new_node)),
            "scroll" if same_shell(old_node, new_node) => {
                let Some(scroll) = widget
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<Scrollable>())
                else {
                    return Some(self.build(new_node));
                };
                let kept = scroll
                    .child_mut()
                    .is_some_and(|content| self.reconcile_children(content, old_node, new_node));
                // A surface the person pans (a desk of strips) shows what just arrived on it: a
                // card put on a canvas, or the window of a place just placed, comes into view.
                if kept {
                    let canvas = token(new_node, "axis") == Some("both");
                    let arrived = if canvas {
                        arrived_card(old_node, new_node)
                    } else {
                        None
                    }
                    .or_else(|| arrived_window(old_node, new_node));
                    if let Some(arrived) = arrived {
                        scroll.reveal_on_layout(arrived);
                    }
                }
                (!kept).then(|| self.build(new_node))
            }
            "embed" if same_shell(old_node, new_node) && frames(old_node) == frames(new_node) => {
                let Some(card) = widget
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<Container>())
                else {
                    return Some(self.build(new_node));
                };
                let kept = card
                    .child_mut()
                    .is_some_and(|inner| self.reconcile_children(inner, old_node, new_node));
                (!kept).then(|| self.build(new_node))
            }
            kind if is_container(kind) && same_shell(old_node, new_node) => {
                let kept = self.reconcile_children(widget, old_node, new_node);
                (!kept).then(|| self.build(new_node))
            }
            kind if is_container(kind) => Some(self.rebuild_shell(widget, old_node, new_node)),
            _ => Some(self.build(new_node)),
        }
    }

    /// Reconcile the children of `container`, built for `old_node`, with `new_node`'s by key,
    /// through its slot operations: whether it is a container that could.
    fn reconcile_children(
        &mut self,
        container: &mut Element,
        old_node: &UiNode,
        new_node: &UiNode,
    ) -> bool {
        with_host(container, |host| {
            self.reconcile_slots(host, old_node, new_node);
        })
        .is_some()
    }

    /// A container whose own arguments changed: a new shell around the children, which keep
    /// their widgets.
    fn rebuild_shell(
        &mut self,
        old: &mut Element,
        old_node: &UiNode,
        new_node: &UiNode,
    ) -> Element {
        let mut fresh = self.build(&UiNode {
            children: Vec::new(),
            ..new_node.clone()
        });
        let moved = with_host(old, |old_host| {
            with_host(&mut fresh, |host| {
                // The old children, in order, into the new shell; then reconciled there.
                for (index, child) in placed(old_node).into_iter().enumerate() {
                    let widget = old_host
                        .child_mut(index)
                        .map(|held| std::mem::replace(held, Box::new(Spacer::fixed(0.0))));
                    let Some(widget) = widget else {
                        break;
                    };
                    let mut slot = Slot::new(widget);
                    let Placement { fit, span, at } = self.place(old_node, &child);
                    slot.fit = fit;
                    slot.span = span;
                    slot.at = at;
                    host.insert(index, slot);
                }
            })
        })
        .flatten();
        if moved.is_none() {
            return self.build(new_node);
        }
        let old_shell = UiNode {
            props: new_node.props.clone(),
            ..old_node.clone()
        };
        if self.reconcile_children(&mut fresh, &old_shell, new_node) {
            fresh
        } else {
            self.build(new_node)
        }
    }

    /// Bring the slots of `host`, built for `old_node`'s children, to `new_node`'s by key:
    /// those gone removed, those kept moved to their place and reconciled in it, those new
    /// built and inserted.
    fn reconcile_slots(&mut self, host: &mut dyn SlotHost, old_node: &UiNode, new_node: &UiNode) {
        let mut held = placed(old_node);
        let placing = placed(new_node);
        for index in (0..held.len()).rev() {
            let gone = held
                .get(index)
                .is_some_and(|old| !placing.iter().any(|child| child.node.key == old.node.key));
            if gone {
                host.remove(index);
                held.remove(index);
            }
        }
        for (index, child) in placing.into_iter().enumerate() {
            let node = child.node;
            let Some(at) = held.iter().position(|old| old.node.key == node.key) else {
                let slot = self.slot(new_node, &child);
                host.insert(index, slot);
                held.insert(index, child);
                continue;
            };
            if at != index {
                host.move_slot(at, index);
                let moved = held.remove(at);
                held.insert(index, moved);
            }
            let Some(old_child) = held.get(index).map(|old| old.node) else {
                continue;
            };
            let placement = self.place(new_node, &child);
            let replaced = host
                .child_mut(index)
                .and_then(|widget| self.update_in(widget, old_child, node));
            if let Some(replaced) = replaced {
                host.replace(index, replaced);
            }
            host.set_placement(index, placement);
        }
    }
}

/// The nodes a holder shows each in a pane: a section's or card's children, a form's fields'
/// controls (or its other content), a field's controls.
fn holder_parts(node: &UiNode) -> Vec<&UiNode> {
    match node.kind.as_str() {
        "form" => node
            .children
            .iter()
            .flat_map(|child| {
                if child.kind == "field" {
                    child.children.iter().collect::<Vec<_>>()
                } else {
                    vec![child]
                }
            })
            .collect(),
        _ => node.children.iter().collect(),
    }
}

/// What a holder's widget is built from besides its parts' contents: its own props, and the
/// key, kind and props of each part (a field's label).
/// One part of a holder as its shape compares: its key, its kind and its props.
type PartShape = (String, String, Vec<(String, Value)>);

/// Where a bound control is for presence: the resource and the field it edits, so collaborators
/// at the same field see each other's carets. A control bound to view state has none.
pub(crate) fn presence_path(node: &UiNode) -> Vec<PathSegment> {
    node.binding
        .as_ref()
        .and_then(|binding| binding.field())
        .map(|(target, field)| {
            vec![
                PathSegment::Field("tilolang".to_owned()),
                PathSegment::Field(target.to_owned()),
                PathSegment::Field(field.to_owned()),
            ]
        })
        .unwrap_or_default()
}

/// The card that arrived under `new_node` since `old_node`, by its key: the last of `new_node`'s
/// outermost cards (a place on a desk, not what its content holds) `old_node` did not hold. A
/// card's body is laid out under its key, which is what the scroll holding it finds it by
/// ([`Scrollable::reveal_on_layout`]).
fn arrived_card(old_node: &UiNode, new_node: &UiNode) -> Option<String> {
    fn cards<'a>(node: &'a UiNode, out: &mut Vec<&'a str>) {
        if node.kind == crate::adapter::kind("Card") {
            out.push(&node.key);
            return;
        }
        for child in &node.children {
            cards(child, out);
        }
    }
    let (mut before, mut now) = (Vec::new(), Vec::new());
    cards(old_node, &mut before);
    cards(new_node, &mut now);
    now.into_iter()
        .rev()
        .find(|key| !before.contains(key))
        .map(str::to_owned)
}

/// The window that arrived under `new_node` since `old_node`, by its key: the frames a row of a
/// desk holds, the last of them new. A row pans to the window just placed in it, as the desk's
/// own row does, so the place that just came onto the desk stands where the person can see it.
fn arrived_window(old_node: &UiNode, new_node: &UiNode) -> Option<String> {
    fn frames<'a>(node: &'a UiNode, out: &mut Vec<&'a str>) {
        if node.kind == crate::adapter::kind("WindowFrame") {
            out.push(&node.key);
            return;
        }
        for child in &node.children {
            frames(child, out);
        }
    }
    let (mut before, mut now) = (Vec::new(), Vec::new());
    frames(old_node, &mut before);
    frames(new_node, &mut now);
    now.into_iter()
        .rev()
        .find(|key| !before.contains(key))
        .map(str::to_owned)
}

/// Whether the embedded view `node` is framed as one object: unless it is made of objects
/// itself, its cards standing at its top or in its top layout.
fn frames(node: &UiNode) -> bool {
    !node
        .children
        .iter()
        .any(|child| child.kind == "ui.Card" || holds_cards(child))
}

/// Whether `node` holds cards, directly or through the fragments a `match` or `if` leaves.
/// The children `node` places in its slots. What a `for` or an `if` places in a grid or a row
/// arrives as a fragment; there each of its children is a cell or an item of its own, where a
/// fragment built as a stack would pile them into one. Elsewhere a fragment is a slot of its
/// own, its children stacked tight.
fn placed(node: &UiNode) -> Vec<Placed<'_>> {
    // A row and a grid place every child a loop's fragment holds, as slots of their own, so a
    // row's items share the row's spans one by one. Every other container keeps a fragment as
    // a slot of its own: a view call is a box standing where the view put it, and what the
    // view asks for is asked of that box.
    fn flatten<'a>(node: &'a UiNode, through: &mut Vec<&'a UiNode>, out: &mut Vec<Placed<'a>>) {
        for child in &node.children {
            if child.kind == "fragment" {
                through.push(child);
                flatten(child, through, out);
                through.pop();
            } else {
                out.push(Placed {
                    node: child,
                    through: through.clone(),
                });
            }
        }
    }
    let mut out = Vec::new();
    if matches!(node.kind.as_str(), "grid" | "row") {
        flatten(node, &mut Vec::new(), &mut out);
    } else {
        out.extend(node.children.iter().map(|child| Placed {
            node: child,
            through: Vec::new(),
        }));
    }
    out
}

/// A child a container places, and the fragments it stands through, outermost first: a fragment
/// draws nothing of its own, so the sizing and placement it asks for are the child's.
struct Placed<'a> {
    node: &'a UiNode,
    through: Vec<&'a UiNode>,
}

fn holds_cards(node: &UiNode) -> bool {
    node.children.iter().any(|child| match child.kind.as_str() {
        "ui.Card" => true,
        "fragment" => holds_cards(child),
        _ => false,
    })
}

fn holder_shape(node: &UiNode) -> Vec<PartShape> {
    let entry = |node: &UiNode| {
        (
            node.key.clone(),
            node.kind.clone(),
            node.props
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect::<Vec<_>>(),
        )
    };
    // A part in its pane changes in place: its key and kind are all the shape needs of it.
    let shallow = |node: &UiNode| (node.key.clone(), node.kind.clone(), Vec::new());
    let mut shape = vec![entry(node)];
    match node.kind.as_str() {
        "field" => shape.extend(node.children.iter().map(shallow)),
        "form" => {
            for child in &node.children {
                shape.push(entry(child));
                if child.kind == "field" {
                    shape.extend(child.children.iter().map(shallow));
                }
            }
        }
        _ => shape.extend(node.children.iter().map(entry)),
    }
    shape
}

/// Every [`Pane`] under `widget`, in order, not looking inside a pane.
fn collect_panes<'a>(widget: &'a mut dyn ui::widget::Widget, out: &mut Vec<&'a mut Pane>) {
    if widget.as_any().is_some_and(<dyn std::any::Any>::is::<Pane>) {
        if let Some(pane) = widget
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<Pane>())
        {
            out.push(pane);
        }
        return;
    }
    for child in widget.children_mut() {
        collect_panes(child, out);
    }
}

/// The first node of `kind` under `node`.
fn find<'a>(node: &'a UiNode, kind: &str) -> Option<&'a UiNode> {
    node.children.iter().find_map(|child| {
        (child.kind == kind)
            .then_some(child)
            .or_else(|| find(child, kind))
    })
}

/// Whether `a` and `b` have the same shell but for their property `name`, which the widget
/// built for `a` takes in place.
fn same_shell_but(a: &UiNode, b: &UiNode, name: &str) -> bool {
    let strip = |node: &UiNode| {
        let mut props = node.props.clone();
        props.remove(name);
        props
    };
    a.kind == b.kind
        && a.key == b.key
        && a.binding == b.binding
        && a.events.keys().eq(b.events.keys())
        && strip(a) == strip(b)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn node(kind: &str, key: &str) -> UiNode {
        UiNode {
            kind: kind.to_owned(),
            key: key.to_owned(),
            props: BTreeMap::new(),
            binding: None,
            events: BTreeMap::new(),
            anchor: None,
            children: Vec::new(),
        }
    }

    fn text(key: &str, value: &str) -> UiNode {
        let mut node = node("ui.Text", key);
        node.props.insert("value".to_owned(), Value::text(value));
        node
    }

    fn stack(key: &str, children: Vec<UiNode>) -> UiNode {
        UiNode {
            children,
            ..node("stack", key)
        }
    }

    fn reconciler() -> Reconciler {
        Reconciler::new(Rc::default())
    }

    /// The identity of each child widget of a container.
    fn identities(container: &Element) -> Vec<usize> {
        container
            .children()
            .iter()
            .map(|child| std::ptr::from_ref::<dyn ui::widget::Widget>(*child).cast::<()>() as usize)
            .collect()
    }

    /// A component that does not build is shown as an empty caption in its place, and said:
    /// whoever checks the view (layout-check, a test) reads why, never only a log line.
    #[test]
    fn a_component_that_does_not_build_is_recorded_with_why() {
        let mut reconciler = reconciler();
        // `ui.Text` requires its value; an unknown component is not described at all.
        let view = stack("s", vec![node("ui.Text", "t"), node("ui.Nothing", "n")]);
        let _ = reconciler.build(&view);
        let broken = reconciler.broken();
        assert_eq!(broken.len(), 2, "both are recorded: {broken:?}");
        assert!(
            broken
                .iter()
                .any(|b| b.contains("ui.Text (t)") && b.contains("value"))
        );
        assert!(broken.iter().any(|b| b.contains("ui.Nothing (n)")));
        // Every reconciler of the same view records into the one list.
        let rows = Reconciler::sharing(&reconciler);
        assert_eq!(rows.broken().len(), 2);
    }

    #[test]
    fn a_changed_value_keeps_the_widget_that_shows_it() {
        let mut reconciler = reconciler();
        let old = stack("s", vec![text("a", "0"), text("b", "x")]);
        let built = reconciler.build(&old);
        let before = identities(&built);
        let new = stack("s", vec![text("a", "1"), text("b", "x")]);
        let updated = reconciler.update(built, &old, &new);
        assert_eq!(identities(&updated), before, "no widget was rebuilt");
    }

    #[test]
    fn children_keep_their_widgets_across_inserts_removals_and_moves() {
        let mut reconciler = reconciler();
        let old = stack("s", vec![text("a", "a"), text("b", "b"), text("c", "c")]);
        let built = reconciler.build(&old);
        let [a, _, c] = identities(&built)[..] else {
            panic!("three children");
        };
        // A new child at the front, `b` gone, `c` moved before `a`.
        let new = stack("s", vec![text("n", "n"), text("c", "c"), text("a", "a")]);
        let updated = reconciler.update(built, &old, &new);
        let after = identities(&updated);
        assert_eq!(after.len(), 3);
        assert_eq!(&after[1..], &[c, a], "kept widgets follow their keys");
        assert_eq!(
            updated.children().first().and_then(|new| new.test_id()),
            Some("n"),
            "the new child is built for its node"
        );
    }

    #[test]
    fn a_container_whose_arguments_changed_keeps_its_children() {
        let mut reconciler = reconciler();
        let old = stack("s", vec![text("a", "a")]);
        let built = reconciler.build(&old);
        let before = identities(&built);
        let mut new = stack("s", vec![text("a", "a")]);
        new.props.insert("gap".to_owned(), Value::text("section"));
        let updated = reconciler.update(built, &old, &new);
        assert_eq!(identities(&updated), before);
    }

    #[test]
    fn a_field_shows_every_control_it_holds() {
        let mut day = node("ui.DateField", "day");
        day.props
            .insert("value".to_owned(), Value::text("2026-06-30"));
        let mut time = node("ui.TimePicker", "time");
        time.props.insert("value".to_owned(), Value::text("09:00"));
        let field = UiNode {
            children: vec![day, time],
            props: [("label".to_owned(), Value::text("Due"))]
                .into_iter()
                .collect(),
            ..node("field", "due")
        };
        assert_eq!(
            holder_parts(&field).len(),
            2,
            "the day and the time of that day are both the field's"
        );
        let mut reconciler = reconciler();
        let mut built = reconciler.build(&field);
        let mut panes = Vec::new();
        collect_panes(built.as_mut(), &mut panes);
        let before: Vec<usize> = panes
            .iter()
            .map(|pane| std::ptr::from_ref::<dyn ui::widget::Widget>(*pane).cast::<()>() as usize)
            .collect();
        assert_eq!(before.len(), 2, "both controls sit in a pane");
        // The time moves: both panes stay, and each reconciles in place.
        let mut new = field.clone();
        new.children[1]
            .props
            .insert("value".to_owned(), Value::text("10:30"));
        let mut updated = reconciler.update(built, &field, &new);
        let mut panes = Vec::new();
        collect_panes(updated.as_mut(), &mut panes);
        let after: Vec<usize> = panes
            .iter()
            .map(|pane| std::ptr::from_ref::<dyn ui::widget::Widget>(*pane).cast::<()>() as usize)
            .collect();
        assert_eq!(after, before, "both keep their pane");
    }

    /// The stack `element` is, as the arrangement that leads what it holds.
    fn arrangement(
        element: &mut Element,
    ) -> &mut ui::widget::arrange::Arranged<ui::widget::arrange::FlexLayout> {
        element
            .as_any_mut()
            .and_then(|any| any.downcast_mut())
            .expect("a stack is an arrangement")
    }

    #[test]
    fn a_container_that_leads_stands_its_content_one_lead_in() {
        let mut reconciler = reconciler();
        let mut led = stack("s", vec![text("a", "a")]);
        led.props.insert("lead".to_owned(), Value::Int(1));
        let mut built = reconciler.build(&led);
        assert_eq!(
            arrangement(&mut built).lead(),
            1,
            "the node's lead is the container's"
        );
        let mut plain = reconciler.build(&stack("p", vec![text("b", "b")]));
        assert_eq!(
            arrangement(&mut plain).lead(),
            0,
            "a container without one keeps its edge"
        );
    }

    #[test]
    fn a_node_of_another_kind_is_built_again() {
        let mut reconciler = reconciler();
        let old = stack("s", vec![text("a", "a")]);
        let built = reconciler.build(&old);
        let before = identities(&built);
        let new = stack("s", vec![node("divider", "a")]);
        let updated = reconciler.update(built, &old, &new);
        assert_ne!(identities(&updated), before);
    }

    #[test]
    fn every_primitive_of_the_language_builds() {
        let mut reconciler = reconciler();
        let mut split = node("split", "sp");
        let mut primary = node("primary", "p");
        primary
            .props
            .insert("min.rem".to_owned(), Value::Float(18.0));
        primary.children.push(text("pt", "primary"));
        let mut secondary = node("secondary", "q");
        secondary
            .props
            .insert("min.rem".to_owned(), Value::Float(22.0));
        secondary.children.push(text("st", "secondary"));
        split.children = vec![primary, secondary];
        let mut grid = node("grid", "g");
        grid.props.insert("min.rem".to_owned(), Value::Float(11.0));
        grid.children = vec![text("g1", "one"), text("g2", "two")];
        let mut page = node("page", "pg");
        page.props.insert("full".to_owned(), Value::Int(1));
        page.children = vec![split, grid];
        let mut row = node("row", "r");
        row.props
            .insert("distribute".to_owned(), Value::text("between"));
        row.children = vec![
            text("rt", "left"),
            node("spacer", "sp1"),
            node("divider", "dv"),
        ];
        let layers = UiNode {
            children: vec![text("l1", "base"), {
                let mut over = text("l2", "over");
                over.props
                    .insert("at".to_owned(), Value::text("bottom_trailing"));
                over
            }],
            ..node("layers", "ly")
        };
        let scroll = UiNode {
            children: vec![page, row, layers],
            ..node("scroll", "sc")
        };
        let built = reconciler.build(&scroll);
        assert!(built.children().len() <= 1);
    }

    #[test]
    fn values_read_the_way_people_read_them() {
        assert_eq!(display(&Value::text("hi")), "hi");
        assert_eq!(display(&Value::Int(2)), "2");
        assert_eq!(display(&Value::Int(-2)), "-2");
        assert_eq!(display(&Value::some(Value::Int(3))), "3");
        assert_eq!(display(&Value::Unit), "");
    }

    #[test]
    fn rich_text_reads_as_its_reader_reads_each_reference() {
        use tilo_render_protocol::patch::rich::{Attr, Block, Embed, InlineView, Mark, RichText};
        const TASK: &str =
            r#"{"app":"so.tilo.lang.board","id":"tl:so.tilo.lang.board:d/f:t1","kind":""}"#;
        // Written while the task was called "Acquisition plan"; the window is told how the
        // reference reads for the person it shows the text to.
        let read_as = |view: InlineView| {
            Value::Rich(RichText {
                blocks: vec![Block {
                    kind: "paragraph".into(),
                    text: "Notes in Acquisition plan".into(),
                    marks: vec![Mark {
                        start: 9,
                        end: 25,
                        name: "mention".into(),
                        value: Attr::Text(TASK.into()),
                    }],
                    attrs: BTreeMap::new(),
                }],
                embeds: vec![Embed {
                    reference: TASK.into(),
                    view,
                }],
            })
        };
        let holder = read_as(InlineView::Resource {
            title: "Merger timeline".into(),
            icon: None,
            detail: None,
        });
        assert_eq!(display(&holder), "Notes in Merger timeline");
        assert_eq!(
            display(&read_as(InlineView::Unreadable)),
            "Notes in Not shared with you"
        );
    }

    /// The rect `built` lays out its keyed node `key` at in a window of `size`.
    fn laid_out(view: &UiNode, size: (u32, u32), key: &str) -> ui::vello::kurbo::Rect {
        let built = reconciler().build(view);
        let harness = ui::headless::FrameHarness::new(ui::State::new(), built, size);
        ui::widget::find_test_id(harness.root_widget(), key)
            .unwrap_or_else(|| panic!("{key} is built"))
            .get_rect()
    }

    /// A `spacer` in a row takes the room its neighbours leave, so what follows it stands at
    /// the row's trailing edge (a bar's status), and a group beside it holds what it holds
    /// rather than taking that room itself.
    #[test]
    fn a_spacer_sends_what_follows_it_to_the_rows_trailing_edge() {
        let mut trailing = stack("end", vec![text("b", "trailing")]);
        trailing.kind = "row".to_owned();
        let row = UiNode {
            children: vec![text("a", "leading"), node("spacer", "gap"), trailing],
            ..node("row", "bar")
        };
        let end = laid_out(&row, (800, 200), "end");
        assert!(
            (end.x1 - 800.0).abs() < 1.0,
            "the trailing group ends at the row's edge: {end:?}"
        );
        assert!(end.width() < 200.0, "it hugs its text: {end:?}");
    }

    /// A filled call of a view (`show(r, fill)`, a filled call of another package's view)
    /// stands for the view: the root the view draws takes the call's whole room, as a window's
    /// shell does, rather than hugging its content.
    #[test]
    fn a_filled_view_calls_root_takes_the_calls_room() {
        let mut call = node("fragment", "call");
        call.props.insert("fill".to_owned(), Value::Int(1));
        call.children = vec![stack("root", vec![text("t", "one line")])];
        let view = stack("window", vec![call]);
        let root = laid_out(&view, (400, 600), "root");
        assert!(
            (root.height() - 600.0).abs() < 1.0,
            "the view's root fills the call: {root:?}"
        );
    }

    /// A view's body renders in a fragment of its own inside its call's, and a view whose root
    /// is a call of another view (a mailbox's view listing through its list's view) puts one
    /// call inside the other: a filled call reaches the root the innermost view draws, as it
    /// is built and once the view draws something else there.
    #[test]
    fn a_filled_calls_fill_reaches_the_root_of_the_view_it_calls_through_each_fragment() {
        let body = |key: &str, child: UiNode| UiNode {
            children: vec![child],
            ..node("fragment", key)
        };
        let call = |key: &str, child: UiNode| body(key, body(&format!("{key}~body"), child));
        let view = |root: UiNode| {
            let mut outer = call("outer", call("inner", root));
            outer.props.insert("fill".to_owned(), Value::Int(1));
            stack("window", vec![outer])
        };
        let first = view(stack("root", vec![text("t", "one line")]));
        let root = laid_out(&first, (400, 600), "root");
        assert!(
            (root.height() - 600.0).abs() < 1.0,
            "the innermost view's root fills the outer call: {root:?}"
        );

        let mut reconciler = reconciler();
        let built = reconciler.build(&view(text("empty", "Nothing here")));
        let next = view(stack("root", vec![text("t", "one line")]));
        let updated = reconciler.update(built, &view(text("empty", "Nothing here")), &next);
        let harness = ui::headless::FrameHarness::new(ui::State::new(), updated, (400, 600));
        let root = ui::widget::find_test_id(harness.root_widget(), "root")
            .expect("the root is built")
            .get_rect();
        assert!(
            (root.height() - 600.0).abs() < 1.0,
            "what the view draws next fills the call too: {root:?}"
        );
    }

    /// A grid's `min` is the least width of its columns, never its own: on a phone narrower
    /// than one column it takes the width it is given and holds one column, rather than
    /// reaching past the window. A stack's `min` stays its own least width.
    #[test]
    fn a_grids_least_column_width_is_not_its_own_least_width() {
        let with_min = |kind: &str| {
            let mut child = node(kind, "c");
            child.props.insert("min.rem".to_owned(), Value::Float(22.0));
            placement(&stack("s", Vec::new()), &child).fit.width.min
        };
        assert_eq!(with_min("grid"), None);
        assert_eq!(with_min("stack"), Some(Length::Rem(22.0)));
    }

    /// What a `for` places in a grid (a fragment per item) fills the grid's cells one by one,
    /// rather than each item piling its cells into one; and an item added later takes cells of
    /// its own while those already shown keep their widgets.
    #[test]
    fn a_grids_cells_from_a_for_are_each_a_cell_and_keep_their_widgets() {
        let item = |key: &str, cells: [&str; 3]| UiNode {
            children: cells
                .iter()
                .map(|cell| text(&format!("{key}{cell}"), cell))
                .collect(),
            ..node("fragment", key)
        };
        let grid = |items: Vec<UiNode>| {
            let mut grid = node("grid", "g");
            grid.props.insert("columns".to_owned(), Value::Int(3));
            grid.children = items;
            grid
        };
        let mut reconciler = reconciler();
        let old = grid(vec![item("a", ["Title", "Old", "New"])]);
        let built = reconciler.build(&old);
        let before = identities(&built);
        assert_eq!(
            before.len(),
            3,
            "each of the item's cells is a cell of the grid"
        );
        let new = grid(vec![
            item("a", ["Title", "Old", "New"]),
            item("b", ["Due", "Mon", "Tue"]),
        ]);
        let updated = reconciler.update(built, &old, &new);
        let after = identities(&updated);
        assert_eq!(
            after.len(),
            6,
            "the added item's cells are cells of their own"
        );
        assert_eq!(
            after[..3],
            before[..],
            "the cells already shown keep their widgets"
        );
    }
}
