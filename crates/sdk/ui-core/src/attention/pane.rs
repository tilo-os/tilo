//! The Needs you surface: the waiting stack as rows a person can act on.
//!
//! One row per item, ordered by what it costs to leave: the app's icon and
//! name (never the icon alone), what happened, what it is about and why,
//! since when, and one button naming the verb. Up and Down move the choice,
//! Enter acts, Escape defers ([`Presses::key`]); the count of what is
//! deferred stays under the stack, because a quiet system must still say
//! what it holds back.
//!
//! A row without an action cannot be built ([`super::Action`] is required),
//! so the surface never shows a line a person can only read.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::Icon;
use crate::tokens::{Rhythm, TextRole};
use crate::widget::{
    Button, Column, ControlSize, Decorator, Element, EmptyState, ListRow, Row, Scrollable, Section,
    SectionHeader, Text, Widget,
};

use super::record::{Item, ItemId, deferred, items, revision};

/// What the person pressed on the stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// Act on this item: the app performs the verb it waits on.
    Act(ItemId),
    /// Put it out of the way, still counted.
    Defer(ItemId),
}

/// The stack's state: what its rows wrote, which row the keyboard is on, and
/// the revision it last drew.
#[derive(Clone, Default)]
pub struct Presses {
    pressed: Rc<RefCell<Vec<Press>>>,
    drawn: Rc<Cell<Option<u64>>>,
    chosen: Rc<Cell<Option<ItemId>>>,
}

impl std::fmt::Debug for Presses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Presses")
            .field("pending", &self.pressed.borrow().len())
            .field("chosen", &self.chosen.get())
            .finish_non_exhaustive()
    }
}

impl Presses {
    /// A stack with nothing pressed and no row chosen.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the stack moved since it was last drawn.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.drawn.get() != Some(revision())
    }

    /// Note that the stack was built against the registry as it stands.
    pub fn drawn(&self) {
        self.drawn.set(Some(revision()));
    }

    /// What was pressed since the last call.
    #[must_use]
    pub fn take(&self) -> Vec<Press> {
        std::mem::take(&mut *self.pressed.borrow_mut())
    }

    /// The row the keyboard is on, when the stack has the keyboard.
    #[must_use]
    pub fn chosen(&self) -> Option<ItemId> {
        self.chosen.get()
    }

    /// Move the choice to `id`.
    pub fn choose(&self, id: Option<ItemId>) {
        self.chosen.set(id);
    }

    /// The keys the stack answers, as its owner hands them over: Up and Down
    /// move the choice, Home and End go to the ends of the stack, Enter acts
    /// on the chosen row, Escape puts it aside. `true` when the key was the
    /// stack's.
    pub fn key(&self, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }
        let stack = items();
        let at = self
            .chosen
            .get()
            .and_then(|chosen| stack.iter().position(|item| item.id() == chosen));
        match &event.logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                let next = at.map_or(0, |index| index.saturating_add(1));
                self.chosen
                    .set(stack.get(next).or(stack.last()).map(Item::id));
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                let next = at.map_or(stack.len().saturating_sub(1), |index| {
                    index.saturating_sub(1)
                });
                self.chosen.set(stack.get(next).map(Item::id));
                true
            }
            Key::Named(NamedKey::Home) => {
                self.chosen.set(stack.first().map(Item::id));
                true
            }
            Key::Named(NamedKey::End) => {
                self.chosen.set(stack.last().map(Item::id));
                true
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(id) = self.chosen.get() {
                    self.press(Press::Act(id));
                }
                true
            }
            Key::Named(NamedKey::Escape) => {
                if let Some(id) = self.chosen.get() {
                    self.press(Press::Defer(id));
                }
                true
            }
            _ => false,
        }
    }

    fn press(&self, press: Press) {
        self.pressed.borrow_mut().push(press);
    }
}

/// The stack: what is waiting, and what is deferred.
#[must_use]
pub fn pane(presses: &Presses) -> Column {
    presses.drawn();
    let stack = items();
    if stack.is_empty() {
        return Column::new().with_no_padding().push(empty());
    }
    // One section per urgency, in the order the stack is sorted by: what
    // costs most to leave is first.
    let mut sections: Vec<(super::Urgency, Vec<Element>)> = Vec::new();
    for item in &stack {
        match sections.last_mut() {
            Some((urgency, rows)) if *urgency == item.urgency() => rows.push(row(item, presses)),
            _ => sections.push((item.urgency(), vec![row(item, presses)])),
        }
    }
    let mut parts: Vec<Element> = sections
        .into_iter()
        .map(|(urgency, rows)| {
            Box::new(Section::new(rows).with_header(SectionHeader::group(
                crate::i18n::widget_format(urgency.key(), &[]),
            ))) as Element
        })
        .collect();
    let held = deferred();
    if held > 0 {
        parts.push(Box::new(
            Text::new(&crate::i18n::widget_format(
                "attention-deferred",
                &[("count", crate::i18n::ArgValue::from(held as f64))],
            ))
            .with_role(TextRole::Caption)
            .dimmed(),
        ));
    }
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Group);
    for part in parts {
        column = column.push(part);
    }
    Column::new().with_no_padding().push(Box::new(
        Scrollable::new_vertical().with_child(Box::new(column)),
    ))
}

/// An empty stack says so, and says what it is for.
fn empty() -> Element {
    Box::new(
        EmptyState::new(crate::i18n::widget_format("attention-none", &[]))
            .with_message(crate::i18n::widget_format("attention-none-message", &[]))
            .with_icon(Icon::Check)
            .with_id("attention-empty"),
    )
}

/// One item: who is asking, what about, why, and the one thing to do.
fn row(item: &Item, presses: &Presses) -> Element {
    let id = item.id();
    let detail = item.detail_line();
    let mut row = ListRow::new(format!("attention-row-{}", id.number()), item.title())
        .icon(item.owner().icon)
        .subtitle(item.owner().name.clone())
        .meta(ago(item.since_ms()));
    if !detail.is_empty() {
        row = row.secondary(detail);
    }
    row = row.selected(presses.chosen() == Some(id));
    let actions = Rc::clone(&presses.pressed);
    let act = Button::new(
        &format!("attention-act-{}", id.number()),
        &item.action().label,
    )
    .control_size(ControlSize::Small)
    .on_view(move || actions.borrow_mut().push(Press::Act(id)));
    let actions = Rc::clone(&presses.pressed);
    let defer = Button::icon_only(
        &format!("attention-defer-{}", id.number()),
        Icon::ChevronDown,
        &crate::i18n::widget_format("attention-defer", &[]),
    )
    .control_size(ControlSize::Small)
    .on_view(move || actions.borrow_mut().push(Press::Defer(id)));
    let controls = Row::new()
        .no_wrap()
        .with_rhythm(Rhythm::Related)
        .push(Box::new(act))
        .push(Box::new(defer));
    Box::new(row.trailing(Box::new(controls)))
}

/// How long ago `at_ms` was, in the relative style History uses.
fn ago(at_ms: u64) -> String {
    let secs = crate::time::unix_now_ms().saturating_sub(at_ms) / 1000;
    #[allow(
        clippy::cast_possible_wrap,
        reason = "a moment in the past is far inside i64 seconds"
    )]
    let secs = secs as i64;
    crate::format::relative(secs)
}

/// The count a window's bar shows: nothing while nothing waits, else how
/// many nobody has looked at.
#[must_use]
pub fn pill() -> Option<String> {
    let unseen = super::unseen();
    if unseen == 0 {
        return None;
    }
    Some(crate::i18n::widget_format(
        "attention-pill",
        &[("count", crate::i18n::ArgValue::from(unseen as f64))],
    ))
}

/// What acting on a row asks of the app: the verb, and what it is about.
#[must_use]
pub fn apply(press: Press) -> Option<super::Acted> {
    match press {
        Press::Act(id) => super::take(id),
        Press::Defer(id) => {
            super::defer(id);
            None
        }
    }
}

/// The waiting stack as a widget: it keeps its rows, rebuilds them when the
/// stack moves, and reports what was pressed through its [`Presses`].
///
/// An app declares one where it shows what wants a person, hands it the keys
/// ([`Presses::key`]) while it is open, and answers what was pressed with
/// [`apply`].
pub struct Stack {
    inner: Column,
    presses: Presses,
    restack: bool,
}

impl Stack {
    /// An empty stack that fills itself from the registry.
    #[must_use]
    pub fn new() -> Self {
        let presses = Presses::new();
        let inner = pane(&presses);
        Self {
            inner,
            presses,
            restack: false,
        }
    }

    /// What was pressed, what the keyboard chose, and whether the stack
    /// moved.
    #[must_use]
    pub fn presses(&self) -> &Presses {
        &self.presses
    }

    /// Whether anything waits: an app keeps the surface out of the window
    /// while nothing does.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        items().is_empty()
    }

    fn restack(&mut self) {
        self.inner = pane(&self.presses);
        self.restack = true;
    }
}

impl Default for Stack {
    fn default() -> Self {
        Self::new()
    }
}

impl Decorator for Stack {
    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut Column {
        &mut self.inner
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let inner = Widget::update(&mut self.inner, state, path);
        if self.presses.changed() {
            self.restack();
            return true;
        }
        inner
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.restack) || Widget::take_structure_dirty(&mut self.inner)
    }

    fn name(&self) -> &'static str {
        "NeedsYouStack"
    }
}
