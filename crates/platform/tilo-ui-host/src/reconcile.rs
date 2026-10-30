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
    Button, ButtonRole, Checkbox, Container, Divider, Dropdown, Element, EmptyState, Field,
    FormSection, ProgressBar, Scrollable, Section, Segment, Spacer, Text, Toggle, ToggleGroup,
    labeled_check,
};

use crate::adapter::{Adapter, NodeSource};
use crate::body::BodyEditor;
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
        Value::Principal(who) => who.id.clone(),
        Value::Opt(Some(inner)) => display(inner),
        // As the person reading reads it: each reference as its chip reads for them.
        Value::Rich(rich) => ui::widget::rich_doc::plain_text(rich),
        _ => String::new(),
    }
}

fn flag(node: &UiNode, name: &str) -> bool {
    match prop(node, name) {
        Some(Value::Int(number)) => *number != 0,
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
fn fit_of(node: &UiNode) -> Fit {
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
fn placement(parent: &UiNode, child: &UiNode) -> Placement {
    let mut fit = fit_of(child);
    // Beside siblings in a row, a container takes the room they leave rather than hugging
    // its content, so what it holds lays out in the width the row offers. A row that wraps
    // flows its items at their own width instead, as chips and tags do.
    if parent.kind == "row"
        && !flag(parent, "wrap")
        && (is_container(&child.kind) || child.kind == "embed")
        && number(child, "fill").is_none()
    {
        fit = Fit::fill(1.0);
        fit.width.min = length(child, "min");
        fit.width.max = length(child, "max");
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
            riches: HashMap::new(),
            navigation: None,
            adapter,
            rows: None,
            broken: Rc::default(),
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
            riches: HashMap::new(),
            navigation: None,
            adapter: other.adapter.clone(),
            rows: other.rows.clone(),
            broken: Rc::clone(&other.broken),
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

    /// Show `rich` in the rich input `key`.
    pub fn set_rich(&self, key: &str, rich: &tilo_render_protocol::patch::rich::RichText) {
        if let Some(state) = self.riches.get(key)
            && state.with(|shown| shown != rich)
        {
            state.set(rich.clone());
        }
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

    /// Show `text` in the input or text node `key`, whatever widget shows it.
    pub fn set_text(&self, key: &str, text: &str) {
        if let Some(state) = self.texts.get(key)
            && state.with(|shown| shown != text)
        {
            state.set(text.to_owned());
        }
    }

    /// Show `checked` in the checkbox `key`.
    pub fn set_flag(&self, key: &str, checked: bool) {
        if let Some(state) = self.flags.get(key)
            && state.get() != checked
        {
            state.set(checked);
        }
    }

    /// The widgets of the tree under `node`.
    pub fn build(&mut self, node: &UiNode) -> Element {
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
            "spacer" => Box::new(Spacer::fixed(0.0).with_id(node.key.clone())),
            "divider" => Box::new(Divider::horizontal()),
            "progress" => Box::new(ProgressBar::new(number(node, "value").unwrap_or(0.0))),
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
        // In a document, a body is the block editor native Notes edits a note in.
        if is_body(node) {
            let state = self.text_state(&node.key);
            let placeholder = crate::date::shown(node, "placeholder").unwrap_or_default();
            return Box::new(BodyEditor::new(
                &node.key,
                &state,
                &self.sink,
                presence_path(node),
                &placeholder,
            ));
        }
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
        if is_body(new_node) {
            return (!same_shell(old_node, new_node)).then(|| self.build(new_node));
        }
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
                let built = self.build(child);
                let view = if crate::adapter::description_of(child).is_some() {
                    built
                } else {
                    Box::new(Pane::new(built)) as Element
                };
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

    fn slots(&mut self, node: &UiNode) -> Vec<Slot> {
        node.children
            .iter()
            .map(|child| self.slot(node, child))
            .collect()
    }

    /// `child`, built, as a slot of its container `parent`.
    fn slot(&mut self, parent: &UiNode, child: &UiNode) -> Slot {
        let mut slot = Slot::new(self.build(child));
        let Placement { fit, span, at } = placement(parent, child);
        slot.fit = fit;
        slot.span = span;
        slot.at = at;
        slot
    }

    fn stack(&mut self, node: &UiNode, default: Rhythm) -> Element {
        let mut stack = Stack::stack()
            .fill_width()
            .gap(rhythm(node, "gap", default))
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
        }
        .with_id(node.key.clone());
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
        let inner = Stack::stack()
            .fill_width()
            .gap(Rhythm::Sibling)
            .with_id(format!("{}~content", node.key));
        let mut inner = inner;
        for slot in self.slots(node) {
            inner = inner.push_slot(slot);
        }
        let scroll = if token(node, "axis") == Some("horizontal") {
            Scrollable::new_horizontal()
        } else {
            Scrollable::new_vertical().fit_content_height()
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

    /// What became of an action block when it was posted, drawn from the invocation record.
    fn invocation_card(&mut self, node: &UiNode) -> Element {
        let state = token(node, "state").unwrap_or("pending");
        let name = crate::date::shown(node, "action").unwrap_or_default();
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
        if let Some(app) = crate::date::shown(node, "app") {
            card = card.push_slot(self.fixed_text(
                &format!("{}~app", node.key),
                &app,
                TextRole::Caption,
            ));
        }
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
            let answer = |reconciler: &Self, suffix: &str, label: String, approve: bool| {
                let sink = Rc::clone(&reconciler.sink);
                let key = node.key.clone();
                let role = if approve {
                    ButtonRole::Primary
                } else {
                    ButtonRole::Secondary
                };
                Slot::new(Box::new(
                    Button::new(&format!("{}~{suffix}", node.key), &label)
                        .role(role)
                        .on_view(move || {
                            sink.push(UiEvent::Resolve(key.clone(), approve));
                        }),
                ))
            };
            let choices = Row::row()
                .gap(Rhythm::Related)
                .push_slot(answer(
                    self,
                    "allow",
                    crate::i18n::t("tilolang-ask-confirm"),
                    true,
                ))
                .push_slot(answer(
                    self,
                    "decline",
                    crate::i18n::t("tilolang-ask-decline"),
                    false,
                ));
            card = card.push_slot(Slot::new(Box::new(choices)));
        }
        Box::new(card)
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
        let mut panes: Vec<Element> = holder_parts(node)
            .into_iter()
            .map(|part| Box::new(Pane::new(self.build(part))) as Element)
            .collect();
        let mut section = FormSection::untitled();
        let fields: Vec<&UiNode> = if node.kind == "field" {
            vec![node]
        } else {
            node.children.iter().collect()
        };
        panes.reverse();
        for child in fields {
            let Some(pane) = panes.pop() else {
                break;
            };
            if child.kind == "field" {
                let label = crate::date::shown(child, "label").unwrap_or_default();
                section = section.field(Field::custom(label, pane));
            } else {
                section = section.content(pane);
            }
        }
        Box::new(section)
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
        let avatars = node
            .children
            .iter()
            .any(|item| find(item, &row_kind).is_some_and(shows_avatar));
        let rows: Vec<Element> = node
            .children
            .iter()
            .map(|item| {
                let row = find(item, &row_kind).unwrap_or(item);
                if row.kind == row_kind {
                    self.build(&in_avatar_column(row, avatars))
                } else {
                    self.build(row)
                }
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
                for (index, child) in old_node.children.iter().enumerate() {
                    let widget = old_host
                        .child_mut(index)
                        .map(|held| std::mem::replace(held, Box::new(Spacer::fixed(0.0))));
                    let Some(widget) = widget else {
                        break;
                    };
                    let mut slot = Slot::new(widget);
                    let Placement { fit, span, at } = placement(old_node, child);
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
        let mut held: Vec<&UiNode> = old_node.children.iter().collect();
        for index in (0..held.len()).rev() {
            let gone = held
                .get(index)
                .is_some_and(|old| !new_node.children.iter().any(|child| child.key == old.key));
            if gone {
                host.remove(index);
                held.remove(index);
            }
        }
        for (index, child) in new_node.children.iter().enumerate() {
            let Some(at) = held.iter().position(|old| old.key == child.key) else {
                let slot = self.slot(new_node, child);
                host.insert(index, slot);
                held.insert(index, child);
                continue;
            };
            if at != index {
                host.move_slot(at, index);
                let moved = held.remove(at);
                held.insert(index, moved);
            }
            let Some(&old_child) = held.get(index) else {
                continue;
            };
            let replaced = host
                .child_mut(index)
                .and_then(|widget| self.update_in(widget, old_child, child));
            if let Some(replaced) = replaced {
                host.replace(index, replaced);
            }
            host.set_placement(index, placement(new_node, child));
        }
    }
}

/// The nodes a holder shows each in a pane: a section's or card's children, a form's fields'
/// controls (or its other content), a field's control.
fn holder_parts(node: &UiNode) -> Vec<&UiNode> {
    match node.kind.as_str() {
        "field" => node.children.first().into_iter().collect(),
        "form" => node
            .children
            .iter()
            .filter_map(|child| {
                if child.kind == "field" {
                    child.children.first()
                } else {
                    Some(child)
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

/// Whether a list's row node shows an avatar: a participant's, or the item's own glyph.
fn shows_avatar(row: &UiNode) -> bool {
    crate::adapter::description_of(row).is_some_and(|description| {
        !crate::adapter::slot_parts(row, description, "avatar").is_empty()
    })
}

/// The row node as its list shows it: the room an avatar takes is kept while the list shows
/// avatars, and given up while no row of it does.
fn in_avatar_column(row: &UiNode, avatars: bool) -> UiNode {
    let mut row = row.clone();
    if !avatars {
        row.props.remove("avatar_space");
    }
    row
}

/// Whether `node` is a document's body: a text field the window shows as the block editor
/// native Notes edits a note in.
fn is_body(node: &UiNode) -> bool {
    node.kind == crate::adapter::kind("TextInput") && token(node, "field_role") == Some("body")
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
    let mut shape = vec![entry(node)];
    for child in &node.children {
        shape.push(entry(child));
        if node.kind == "form" && child.kind == "field" {
            shape.extend(
                child
                    .children
                    .iter()
                    .map(|control| (control.key.clone(), control.kind.clone(), Vec::new())),
            );
        }
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
}
