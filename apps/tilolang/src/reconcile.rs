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

use tilolang_tilo::runtime::{Binding, UiNode, Value};
use ui::LocalState;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::arrange::{
    Align, At, Collapse, Distribute, Fit, Grid, Layers, Length, Navigation, Page, Row, Side, Slot,
    Split, Stack,
};
use ui::widget::{
    Avatar, Button, ButtonRole, Card, Checkbox, Container, ControlSize, Divider, Dropdown, Element,
    EmptyState, Field, FormSection, ListRow, ProgressBar, Property, PropertyList, Scrollable,
    Section, Segment, Spacer, Text, TextInput, Toggle, ToggleGroup, labeled_check,
};

use crate::window::pane::Pane;

/// What a person did to the view.
#[derive(Clone, Debug, PartialEq)]
pub enum UiEvent {
    /// The button `key` was activated.
    Press(String),
    /// The input `key` now holds `text`.
    Edit(String, String),
    /// The checkbox `key` is now `checked`.
    Toggle(String, bool),
    /// The suggestion in the invocation card `key` was allowed or declined.
    Resolve(String, bool),
    /// The node `key`'s event `event` happened: a case of a `choice` was picked.
    Fire(String, String),
}

/// Where controls report what the person did.
pub type EventSink = Rc<RefCell<Vec<UiEvent>>>;

/// The name a bound field is announced by.
pub type FieldNames = Rc<dyn Fn(&Binding) -> String>;

/// The cases of an enum (by id) with what a control shows for each.
pub type Cases = Rc<dyn Fn(&str) -> Vec<(String, String)>>;

/// What the room calls a participant (by id).
pub type People = Rc<dyn Fn(&str) -> String>;

/// Builds and reconciles the widgets of a view tree.
pub struct Reconciler {
    sink: EventSink,
    names: FieldNames,
    texts: HashMap<String, LocalState<String>>,
    flags: HashMap<String, LocalState<bool>>,
    /// The handle on the split that collapses to navigate, the last one built.
    navigation: Option<Navigation>,
    cases: Cases,
    people: People,
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
        Value::Principal(who) => who.0.clone(),
        Value::Opt(Some(inner)) => display(inner),
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
    fit.width.min = length(node, "min");
    fit.width.max = length(node, "max");
    if let Some(ratio) = number(node, "aspect") {
        fit = fit.aspect(ratio);
    }
    fit
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

/// Container widgets, whatever their layout, as the keyed reconciler reaches their children.
trait SlotHost {
    fn take(&mut self) -> Vec<Slot>;
    fn put(&mut self, slots: Vec<Slot>);
}

impl<L: ui::widget::arrange::Layout + 'static> SlotHost for ui::widget::arrange::Arranged<L> {
    fn take(&mut self) -> Vec<Slot> {
        self.take_slots()
    }

    fn put(&mut self, slots: Vec<Slot>) {
        self.set_slots(slots);
    }
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
    pub fn new(sink: EventSink, names: FieldNames) -> Self {
        Self {
            sink,
            names,
            texts: HashMap::new(),
            flags: HashMap::new(),
            navigation: None,
            cases: Rc::new(|_| Vec::new()),
            people: Rc::new(str::to_owned),
        }
    }

    /// Read enum cases and participants' names through `cases` and `people`.
    #[must_use]
    pub fn with_lookups(mut self, cases: Cases, people: People) -> Self {
        self.cases = cases;
        self.people = people;
        self
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
            "heading" => self.text(node, Some(heading_role(node))),
            "text" => self.text(node, None),
            "section" | "card" | "form" | "field" => self.holder(node),
            "facts" => Self::facts(node),
            "toggle" => self.toggle(node),
            "choice" => self.choice(node),
            "avatar" => self.avatar(node),
            "list" => self.rows(node),
            "item" => self.stack(node, Rhythm::Tight),
            "button" => self.button(node),
            "list_row" => self.list_row(node),
            "checkbox" => self.checkbox(node),
            "input" => self.input(node),
            "image" => self.image(node),
            "embed" => self.embed(node),
            "invocation" => self.invocation_card(node),
            "error" => self.error_block(node),
            _ => self.text_of(node, "", TextRole::Caption),
        }
    }

    fn slots(&mut self, node: &UiNode) -> Vec<Slot> {
        node.children
            .iter()
            .map(|child| self.slot(node, child, None))
            .collect()
    }

    /// `child` as a slot of its container, with the fit its own arguments ask for. `reuse` is
    /// the widget it keeps from the previous tree.
    fn slot(&mut self, parent: &UiNode, child: &UiNode, reuse: Option<Element>) -> Slot {
        let element = reuse.unwrap_or_else(|| self.build(child));
        let mut slot = Slot::new(element);
        slot.fit = fit_of(child);
        // Beside siblings in a row, a container takes the room they leave rather than hugging
        // its content, so what it holds lays out in the width the row offers.
        if parent.kind == "row"
            && (is_container(&child.kind) || child.kind == "embed")
            && number(child, "fill").is_none()
        {
            slot.fit = Fit::fill(1.0);
            slot.fit.width.min = length(child, "min");
            slot.fit.width.max = length(child, "max");
        }
        if child.kind == "spacer" {
            slot.fit = Fit::fill(1.0);
        }
        if let Some(Value::Int(span)) = prop(child, "span") {
            slot.span = u16::try_from(*span).unwrap_or(1).max(1);
        }
        slot.at = position(child);
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

    fn text(&mut self, node: &UiNode, role: Option<TextRole>) -> Element {
        if role.is_none() && token(node, "role") == Some("numeric") {
            let value = prop(node, "value").map(display).unwrap_or_default();
            return self.numeric_text(node, &value);
        }
        let role =
            role.unwrap_or_else(
                || match token(node, "role").or_else(|| token(node, "style")) {
                    Some("display") => TextRole::Display,
                    Some("title") => TextRole::Title,
                    Some("heading") => TextRole::Heading,
                    Some("caption") => TextRole::Caption,
                    Some("label") => TextRole::Label,
                    Some("code") => TextRole::Code,
                    _ => TextRole::Body,
                },
            );
        let value = prop(node, "value").map(display).unwrap_or_default();
        self.text_of(node, &value, role)
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

    fn button(&self, node: &UiNode) -> Element {
        let label = prop(node, "label").map(display).unwrap_or_default();
        let sink = Rc::clone(&self.sink);
        let key = node.key.clone();
        let role = match token(node, "role").or_else(|| token(node, "style")) {
            Some("primary") => ButtonRole::Primary,
            Some("destructive") => ButtonRole::Destructive,
            Some("plain") => ButtonRole::Plain,
            _ => ButtonRole::Secondary,
        };
        let size = match token(node, "size") {
            Some("large") => ControlSize::Large,
            Some("small") => ControlSize::Small,
            _ => ControlSize::Regular,
        };
        let enabled = prop(node, "enabled").is_none_or(|_| flag(node, "enabled"))
            && node.events.contains_key("press");
        let mut button = Button::new(&node.key, &label);
        if let Some(icon) =
            token(node, "icon").and_then(|name| ui::icons::Icon::from_name(&name.replace('_', "-")))
        {
            button = button.with_icon(icon);
        }
        Box::new(
            button
                .role(role)
                .control_size(size)
                .with_enabled(enabled)
                .on_view(move || sink.borrow_mut().push(UiEvent::Press(key.clone()))),
        )
    }

    /// A list row: the title with its secondary line, hover lift and selection of the design
    /// system's row.
    fn list_row(&self, node: &UiNode) -> Element {
        let title = prop(node, "title").map(display).unwrap_or_default();
        let sink = Rc::clone(&self.sink);
        let key = node.key.clone();
        let mut row = ListRow::new(&node.key, title).selected(flag(node, "selected"));
        if let Some(secondary) = prop(node, "secondary").map(display)
            && !secondary.is_empty()
        {
            row = row.secondary(secondary);
        }
        let shown = |name: &str| {
            prop(node, name)
                .map(display)
                .filter(|text| !text.is_empty())
        };
        if let Some(meta) = shown("meta") {
            row = row.meta(meta);
        }
        if let Some(value) = shown("value") {
            row = row.value(value);
        }
        if let Some(badge) = shown("badge") {
            row = row.badge(badge);
        }
        if let Some(icon) = icon_prop(node, "mark") {
            row = row.mark(icon);
        }
        if let Some(icon) = icon_prop(node, "icon") {
            row = row.icon(icon);
        }
        if let Some(Value::Principal(who)) = prop(node, "avatar") {
            row = row.avatar((self.people)(&who.0), who.0.clone());
        }
        if prop(node, "checked").is_some() {
            row = row.checked(flag(node, "checked"));
        }
        if node.binding.is_some() {
            let state = self
                .flags
                .get(&node.key)
                .cloned()
                .unwrap_or_else(|| LocalState::new(false));
            let toggles = Rc::clone(&self.sink);
            let key = node.key.clone();
            row = row.checkbox(state.get(), move |checked| {
                toggles
                    .borrow_mut()
                    .push(UiEvent::Toggle(key.clone(), checked));
            });
        }
        if node.events.contains_key("press") {
            row = row.on_view(move || sink.borrow_mut().push(UiEvent::Press(key.clone())));
        }
        Box::new(row)
    }

    fn input(&mut self, node: &UiNode) -> Element {
        let state = self
            .texts
            .entry(node.key.clone())
            .or_insert_with(|| LocalState::new(String::new()))
            .clone();
        let sink = Rc::clone(&self.sink);
        let key = node.key.clone();
        let placeholder = prop(node, "placeholder").map(display).unwrap_or_default();
        let name = node
            .binding
            .as_ref()
            .map_or_else(|| placeholder.clone(), |binding| (self.names)(binding));
        let mut input = TextInput::from_state(&state)
            .on_change(move |text| {
                sink.borrow_mut()
                    .push(UiEvent::Edit(key.clone(), text.to_owned()));
            })
            .with_id(&node.key)
            .with_accessible_name(&name)
            .with_placeholder(&placeholder)
            .disabled(prop(node, "enabled").is_some() && !flag(node, "enabled"));
        // `lines: n` asks for room for n lines of text: the toolkit's wrapping field.
        if let Some(lines) = number(node, "lines") {
            input = input
                .multiline(true)
                .with_min_lines(lines.max(1.0) as usize);
        }
        // In a document, the title and the body are edited in place, as running text.
        match token(node, "field_role") {
            Some("title") => {
                input = input
                    .with_text_role(TextRole::LargeTitle)
                    .with_plain_chrome();
            }
            Some("body") => {
                input = input.multiline(true).with_plain_chrome();
            }
            _ => {}
        }
        Box::new(input)
    }

    fn checkbox(&mut self, node: &UiNode) -> Element {
        let state = self
            .flags
            .entry(node.key.clone())
            .or_insert_with(|| LocalState::new(false))
            .clone();
        let sink = Rc::clone(&self.sink);
        let key = node.key.clone();
        let label = prop(node, "label").map(display).unwrap_or_default();
        let control = Checkbox::from_state(&state)
            .on_change(move |checked| {
                sink.borrow_mut()
                    .push(UiEvent::Toggle(key.clone(), checked));
            })
            .with_id(&node.key)
            .with_enabled(prop(node, "enabled").is_none() || flag(node, "enabled"));
        Box::new(labeled_check(label, control))
    }

    /// A block an agent or a person wrote, rendered inline: its result in a card, so it reads as
    /// somebody's message rather than as part of the app.
    fn embed(&mut self, node: &UiNode) -> Element {
        let inner = self.stack(node, Rhythm::Related);
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
        let name = prop(node, "action").map(display).unwrap_or_default();
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
        if let Some(app) = prop(node, "app").map(display) {
            card = card.push_slot(self.fixed_text(
                &format!("{}~app", node.key),
                &app,
                TextRole::Caption,
            ));
        }
        if let Some(result) = prop(node, "result").map(display) {
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
                            sink.borrow_mut()
                                .push(UiEvent::Resolve(key.clone(), approve));
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
            let line = prop(child, "value").map(display).unwrap_or_default();
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

    /// A text that shows a number: tabular figures, so a changing count does not jitter.
    fn numeric_text(&mut self, node: &UiNode, value: &str) -> Element {
        let state = self
            .texts
            .entry(node.key.clone())
            .or_insert_with(|| LocalState::new(value.to_owned()))
            .clone();
        state.set(value.to_owned());
        Box::new(
            Text::from_state(&state, |shown: &String| shown.clone())
                .with_role(TextRole::Title)
                .numeric()
                .with_id(node.key.clone()),
        )
    }

    /// A section, card, form or field: the native widget, with each part of its content in a
    /// [`Pane`] the reconciler reaches again, so content that runs again keeps its widgets.
    fn holder(&mut self, node: &UiNode) -> Element {
        let mut panes: Vec<Element> = holder_parts(node)
            .into_iter()
            .map(|part| Box::new(Pane::new(self.build(part))) as Element)
            .collect();
        match node.kind.as_str() {
            "section" => {
                let title = prop(node, "title").map(display).unwrap_or_default();
                if title.is_empty() {
                    Box::new(Section::new(panes))
                } else {
                    Box::new(Section::titled(title, panes))
                }
            }
            "card" => {
                let body = Stack::stack()
                    .fill_width()
                    .gap(Rhythm::Related)
                    .with_id(node.key.clone());
                let body = panes
                    .into_iter()
                    .fold(body, |body, pane| body.push_slot(Slot::new(pane)));
                Box::new(Card::new(Box::new(body)))
            }
            _ => {
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
                        let label = prop(child, "label").map(display).unwrap_or_default();
                        section = section.field(Field::custom(label, pane));
                    } else {
                        section = section.content(pane);
                    }
                }
                Box::new(section)
            }
        }
    }

    fn update_holder(&mut self, mut old: Element, old_node: &UiNode, new_node: &UiNode) -> Element {
        if holder_shape(old_node) != holder_shape(new_node) {
            return self.build(new_node);
        }
        let old_parts = holder_parts(old_node);
        let new_parts = holder_parts(new_node);
        let mut panes = Vec::new();
        collect_panes(old.as_mut(), &mut panes);
        if panes.len() != new_parts.len() {
            return self.build(new_node);
        }
        for ((pane, before), after) in panes.into_iter().zip(old_parts).zip(new_parts) {
            let inner = pane.take();
            let updated = self.update(inner, before, after);
            pane.set(updated);
        }
        old
    }

    /// Facts: the native property list, a label and a value per fact.
    fn facts(node: &UiNode) -> Element {
        let properties = node
            .children
            .iter()
            .filter(|child| child.kind == "fact")
            .map(|fact| {
                Property::new(
                    prop(fact, "label").map(display).unwrap_or_default(),
                    prop(fact, "value").map(display).unwrap_or_default(),
                )
            })
            .collect();
        Box::new(PropertyList::new(properties))
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
        let label = prop(node, "label").map(display).unwrap_or_default();
        let control = Toggle::from_state(&state).on_change(move |checked| {
            sink.borrow_mut()
                .push(UiEvent::Toggle(key.clone(), checked));
        });
        Box::new(FormSection::untitled().field(Field::new(label, control)))
    }

    /// A choice of an enum's cases: segments for a few, a menu for more.
    fn choice(&self, node: &UiNode) -> Element {
        let cases = token(node, "enum")
            .map(|id| (self.cases)(id))
            .unwrap_or_default();
        let selected = match prop(node, "selected") {
            Some(Value::Text(case)) => Some(case.clone()),
            Some(Value::Enum(value)) => Some(value.variant.clone()),
            _ => None,
        };
        let at = selected
            .as_ref()
            .and_then(|case| cases.iter().position(|(name, _)| name == case));
        let sink = Rc::clone(&self.sink);
        let key = node.key.clone();
        let names: Vec<String> = cases.iter().map(|(name, _)| name.clone()).collect();
        if cases.len() <= 4 {
            let segments = cases
                .iter()
                .map(|(_, label)| Segment::new(label.clone()))
                .collect();
            Box::new(
                ToggleGroup::new(segments, at.unwrap_or(usize::MAX)).on_change(move |index| {
                    if let Some(case) = names.get(index) {
                        sink.borrow_mut()
                            .push(UiEvent::Fire(key.clone(), format!("pick.{case}")));
                    }
                }),
            )
        } else {
            let labels: Vec<String> = cases.iter().map(|(_, label)| label.clone()).collect();
            let state = LocalState::new(
                at.and_then(|index| labels.get(index).cloned())
                    .unwrap_or_default(),
            );
            let chosen = labels.clone();
            Box::new(
                Dropdown::from_state(labels, &state).on_change(move |label| {
                    if let Some(case) = chosen
                        .iter()
                        .position(|shown| shown == label)
                        .and_then(|index| names.get(index))
                    {
                        sink.borrow_mut()
                            .push(UiEvent::Fire(key.clone(), format!("pick.{case}")));
                    }
                }),
            )
        }
    }

    fn avatar(&self, node: &UiNode) -> Element {
        let (name, identity) = match prop(node, "who") {
            Some(Value::Principal(who)) => ((self.people)(&who.0), who.0.clone()),
            Some(other) => (display(other), display(other)),
            None => (String::new(), String::new()),
        };
        Box::new(Avatar::person(name, identity))
    }

    /// A list inside content: its rows, as a section of the page.
    fn rows(&mut self, node: &UiNode) -> Element {
        let rows: Vec<Element> = node
            .children
            .iter()
            .map(|item| {
                let row = find(item, "list_row").unwrap_or(item);
                if row.kind == "list_row" {
                    self.list_row(row)
                } else {
                    self.build(row)
                }
            })
            .collect();
        if rows.is_empty() {
            let empty = prop(node, "empty").map(display).unwrap_or_default();
            return Box::new(EmptyState::new(empty).inline());
        }
        Box::new(Section::new(rows))
    }

    /// Bring `old`, the widget built for `old_node`, up to date with `new_node`.
    pub fn update(&mut self, mut old: Element, old_node: &UiNode, new_node: &UiNode) -> Element {
        if old_node == new_node {
            return old;
        }
        if old_node.kind != new_node.kind || old_node.key != new_node.key {
            return self.build(new_node);
        }
        match new_node.kind.as_str() {
            "text" | "heading" | "image" => {
                if same_shell_without_value(old_node, new_node) {
                    let value = prop(new_node, "value").map(display).unwrap_or_default();
                    self.set_text(&new_node.key, &value);
                    return old;
                }
                self.build(new_node)
            }
            "section" | "card" | "form" | "field" => self.update_holder(old, old_node, new_node),
            "input" | "checkbox" => {
                if same_shell(old_node, new_node) {
                    old
                } else {
                    self.build(new_node)
                }
            }
            "scroll" if same_shell(old_node, new_node) => {
                if let Some(scroll) = old
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<Scrollable>())
                    && let Some(content) = scroll.child_mut()
                {
                    let inner = std::mem::replace(content, Box::new(Spacer::fixed(0.0)));
                    *content = self.update_children(inner, old_node, new_node);
                    return old;
                }
                self.build(new_node)
            }
            "embed" if same_shell(old_node, new_node) => {
                if let Some(card) = old
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<Container>())
                    && let Some(inner) = card.take_child()
                {
                    let updated = self.update_children(inner, old_node, new_node);
                    card.set_child(updated);
                    return old;
                }
                self.build(new_node)
            }
            kind if is_container(kind) && same_shell(old_node, new_node) => {
                self.update_children(old, old_node, new_node)
            }
            kind if is_container(kind) => self.rebuild_shell(old, old_node, new_node),
            _ => self.build(new_node),
        }
    }

    /// Keep `container`, reconciling its children against the new node's.
    fn update_children(
        &mut self,
        mut container: Element,
        old_node: &UiNode,
        new_node: &UiNode,
    ) -> Element {
        let Some(old_slots) = with_host(&mut container, |host| host.take()) else {
            return self.build(new_node);
        };
        let slots = self.reconcile_slots(old_slots, old_node, new_node);
        if with_host(&mut container, |host| host.put(slots)).is_none() {
            return self.build(new_node);
        }
        container
    }

    /// A container whose own arguments changed: a new shell around the children that keep their
    /// widgets.
    fn rebuild_shell(&mut self, mut old: Element, old_node: &UiNode, new_node: &UiNode) -> Element {
        let Some(old_slots) = with_host(&mut old, |host| host.take()) else {
            return self.build(new_node);
        };
        let slots = self.reconcile_slots(old_slots, old_node, new_node);
        let mut fresh = self.build(&UiNode {
            children: Vec::new(),
            ..new_node.clone()
        });
        if with_host(&mut fresh, |host| host.put(slots)).is_none() {
            return self.build(new_node);
        }
        fresh
    }

    fn reconcile_slots(
        &mut self,
        old_slots: Vec<Slot>,
        old_node: &UiNode,
        new_node: &UiNode,
    ) -> Vec<Slot> {
        let mut kept: HashMap<&str, (Element, &UiNode)> = old_slots
            .into_iter()
            .zip(&old_node.children)
            .map(|(slot, node)| (node.key.as_str(), (slot.into_element(), node)))
            .collect();
        new_node
            .children
            .iter()
            .map(|child| {
                let reuse = kept
                    .remove(child.key.as_str())
                    .map(|(element, old_child)| self.update(element, old_child, child));
                self.slot(new_node, child, reuse)
            })
            .collect()
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

fn icon_prop(node: &UiNode, name: &str) -> Option<ui::icons::Icon> {
    let name = match prop(node, name)? {
        Value::Text(name) => name.clone(),
        Value::Opt(Some(inner)) => match inner.as_ref() {
            Value::Text(name) => name.clone(),
            _ => return None,
        },
        _ => return None,
    };
    ui::icons::Icon::from_name(&name.replace('_', "-"))
}

fn same_shell_without_value(a: &UiNode, b: &UiNode) -> bool {
    let strip = |node: &UiNode| {
        let mut props = node.props.clone();
        props.remove("value");
        props
    };
    a.kind == b.kind && a.key == b.key && strip(a) == strip(b)
}

fn heading_role(node: &UiNode) -> TextRole {
    match prop(node, "level") {
        Some(Value::Int(1)) => TextRole::Title,
        Some(Value::Int(3)) => TextRole::Subheading,
        _ => TextRole::Heading,
    }
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
        let mut node = node("text", key);
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
        Reconciler::new(Rc::default(), Rc::new(|_| String::new()))
    }

    /// The identity of each child widget of a container.
    fn identities(container: &Element) -> Vec<usize> {
        container
            .children()
            .iter()
            .map(|child| std::ptr::from_ref::<dyn ui::widget::Widget>(*child).cast::<()>() as usize)
            .collect()
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
        let [a, b, c] = identities(&built)[..] else {
            panic!("three children");
        };
        // A new child at the front, `b` gone, `c` moved before `a`.
        let new = stack("s", vec![text("n", "n"), text("c", "c"), text("a", "a")]);
        let updated = reconciler.update(built, &old, &new);
        let after = identities(&updated);
        assert_eq!(after.len(), 3);
        assert_eq!(&after[1..], &[c, a], "kept widgets follow their keys");
        assert!(
            ![a, b, c].contains(&after[0]),
            "the new child is a new widget"
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
}
