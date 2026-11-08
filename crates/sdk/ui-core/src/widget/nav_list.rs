//! `NavList` — the one navigation list.
//!
//! Every sidebar in Tilo lists its destinations with a `NavList`: mailboxes,
//! channels, playlists, settings categories, places. An app says what the
//! destinations are ([`NavSection`], [`NavItem`]) and which one is chosen;
//! the list decides how they look. There is no colour, size, padding or
//! radius to pass.
//!
//! The list scrolls its rows on the [`VirtualList`] engine: arrow keys move
//! the selection, Left/Right collapse and expand, Enter activates,
//! F2 renames an editable label, Delete reports [`NavList::on_delete`], the
//! Menu key, a right click and a long press ask for the row's menu
//! ([`NavList::on_context_menu`]), rows publish their semantics, and
//! dragging an item onto a drop target moves it. Rows paint through
//! [`crate::chrome::paint_nav_row`], so a painted app that calls that
//! painter draws the same rows.
//!
//! Inside a [`super::NavShell`] the list is the sidebar's content: choosing a
//! destination in a narrow window closes the drawer (or pushes the content
//! screen) without the app doing anything, and the shell's filter field
//! narrows the sections that are [`NavSection::filterable`].

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use vello::kurbo::{Point, Rect};
use vello::peniko::Color;

use crate::LocalState;
use crate::chrome::{NavCount, nav_row_height};
use crate::icons::Icon;
use crate::local_state::LocalBinding;
use crate::reorder::{DropPlacement, DropTarget, ReorderDrop};
use crate::widget::{
    AppModel, FilterCount, Filterable, ListModel, ModelPath, RowStatus, Theme, Tone, VirtualList,
};
use model::{NavModel, NavRowData, NavRowKind, NavState, NavStatus};
use row::{NavPaint, NavRow};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// A group of destinations under a sentence-case title.
#[derive(Clone, Debug, PartialEq)]
pub struct NavSection {
    pub id: String,
    /// Sentence case ("Mailboxes", "On this device"). An empty title draws
    /// no header: the items stand at the top level.
    pub title: String,
    /// The person can fold the section away.
    pub collapsible: bool,
    /// The section starts folded.
    pub collapsed: bool,
    /// A trailing action on the header (usually "New …" with
    /// [`Icon::Plus`]); reported through [`NavList::on_section_action`].
    pub trailing_action: Option<NavAction>,
    /// The one quiet line the section shows while it has no items.
    pub empty_caption: Option<String>,
    /// Rows standing in, while it has no items, for items still being read.
    pub pending: u8,
    /// The window's filter narrows this section's items.
    pub filterable: bool,
    /// The title can be renamed in place ([`NavList::on_rename_action`]).
    pub editable_title: bool,
    /// The header has a menu ([`NavList::on_context_menu`]).
    pub menu: bool,
    pub items: Vec<NavItem>,
}

impl NavSection {
    #[must_use]
    pub fn new(id: impl Into<String>, title: impl Into<String>, items: Vec<NavItem>) -> Self {
        Self::new_owned(id.into(), title.into(), items)
    }

    /// [`Self::new`] once its text is owned: one copy in the toolkit
    /// rather than one in every app that calls the generic constructor.
    fn new_owned(id: String, title: String, items: Vec<NavItem>) -> Self {
        Self {
            id,
            title,
            collapsible: false,
            collapsed: false,
            trailing_action: None,
            empty_caption: None,
            pending: 0,
            filterable: false,
            editable_title: false,
            menu: false,
            items,
        }
    }

    /// Items with no header above them.
    #[must_use]
    pub fn untitled(id: impl Into<String>, items: Vec<NavItem>) -> Self {
        Self::new(id, "", items)
    }

    #[must_use]
    pub fn collapsible(mut self) -> Self {
        self.collapsible = true;
        self
    }

    /// Fold the section when it first appears (it stays foldable).
    #[must_use]
    pub fn collapsed(mut self) -> Self {
        self.collapsible = true;
        self.collapsed = true;
        self
    }

    #[must_use]
    pub fn trailing_action(mut self, action: NavAction) -> Self {
        self.trailing_action = Some(action);
        self
    }

    /// What the section says while it holds nothing ("No drafts"): one
    /// sentence, in tertiary ink, where its first row would be.
    #[must_use]
    pub fn empty_caption(mut self, caption: impl Into<String>) -> Self {
        self.empty_caption = Some(caption.into());
        self
    }

    /// While the section has no items because they are still being read,
    /// `rows` rows stand in for them: the shape of a row (its icon and its
    /// label), in the skeleton tone, so nothing moves when the items arrive.
    /// They are not destinations: nothing selects them, the arrow keys pass
    /// over them, and assistive technology hears "Loading".
    #[must_use]
    pub fn pending(mut self, rows: u8) -> Self {
        self.pending = rows;
        self
    }

    /// Let the window's search narrow this section's items: the window
    /// bar's field, or a shell's own [`super::NavShell::with_filter`] field.
    /// The section draws no field itself. Sections that do not opt in (a
    /// list of connections above the tables being filtered) stay as they
    /// are.
    #[must_use]
    pub fn filterable(mut self) -> Self {
        self.filterable = true;
        self
    }

    /// The person can rename the section in place: F2 on its header, or
    /// [`NavFeed::begin_rename`] from its menu.
    #[must_use]
    pub fn editable_title(mut self) -> Self {
        self.editable_title = true;
        self
    }

    /// The header asks for its menu as an item's row does: a right click, a
    /// long press, the Menu key ([`NavList::on_context_menu`], with the
    /// section's id).
    #[must_use]
    pub fn with_menu(mut self) -> Self {
        self.menu = true;
        self
    }
}

/// A command offered on a row or a section header: an icon, and the name it
/// is announced and tooltipped by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavAction {
    pub id: String,
    pub icon: Icon,
    /// Sentence case, naming what happens ("New playlist", "More").
    pub label: String,
    /// Pressing it asks for the row's menu ([`NavList::on_context_menu`])
    /// instead of reporting the action.
    pub opens_menu: bool,
}

impl NavAction {
    #[must_use]
    pub fn new(id: impl Into<String>, icon: Icon, label: impl Into<String>) -> Self {
        Self::new_owned(id.into(), icon, label.into())
    }

    /// [`Self::new`] once its text is owned: one copy in the toolkit
    /// rather than one in every app that calls the generic constructor.
    fn new_owned(id: String, icon: Icon, label: String) -> Self {
        Self {
            id,
            icon,
            label,
            opens_menu: false,
        }
    }

    /// The row's "More" action: pressing it opens the row's menu under the
    /// action, the same menu a right click opens.
    #[must_use]
    pub fn opens_context_menu(mut self) -> Self {
        self.opens_menu = true;
        self
    }
}

/// Something a [`NavItem`] is also found by: a thing it holds, by its name
/// and the other words for it, or words for the destination itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NavSearchTerm {
    /// The name shown under the row's label when this term found it.
    pub name: Option<String>,
    /// Other words that find it; a word matches a query at a word start.
    pub words: Vec<String>,
}

impl NavSearchTerm {
    /// A thing the destination holds, found by `name`.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            words: Vec::new(),
        }
    }

    /// Words that find the destination itself; the row shows nothing more.
    #[must_use]
    pub fn words(words: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            name: None,
            words: words.into_iter().map(Into::into).collect(),
        }
    }

    /// Also found by `words`.
    #[must_use]
    pub fn also(mut self, words: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.words.extend(words.into_iter().map(Into::into));
        self
    }

    /// Whether `query` (folded) finds this term: at the start of a word of
    /// its name or of one of its words, so "light" finds "Night light" and
    /// "light mode" but not "flight mode".
    fn matches(&self, query: &str) -> bool {
        self.name
            .iter()
            .chain(&self.words)
            .any(|text| matches_word_start(text, query))
    }
}

/// Whether `query` occurs in `text` (both folded) where a word starts.
fn matches_word_start(text: &str, query: &str) -> bool {
    let text = crate::search::normalize_search_text(text);
    let mut before: Option<char> = None;
    text.char_indices().any(|(at, here)| {
        let starts_word = before.is_none_or(|before| !before.is_alphanumeric());
        before = Some(here);
        starts_word && text.get(at..).is_some_and(|rest| rest.starts_with(query))
    })
}

/// One destination. Every part but the label is optional.
#[derive(Clone, Debug, PartialEq)]
pub struct NavItem {
    /// Stable across rebuilds; selection, expansion and focus follow it.
    pub id: String,
    pub label: String,
    pub icon: Option<Icon>,
    pub count: Option<NavCount>,
    /// Show the list's item actions ([`NavList::item_actions`]) on this row
    /// on hover and focus.
    pub actions: bool,
    /// Nested destinations, indented one step.
    pub children: Vec<NavItem>,
    /// Starts expanded (only meaningful with children).
    pub expanded: bool,
    /// Opens even where the person folded it (the place the app just
    /// revealed).
    pub ensure_expanded: bool,
    /// Other items may be dragged into this one ([`NavList::on_move`]).
    pub drop_target: bool,
    /// The label can be renamed in place ([`NavList::on_rename_action`]).
    pub editable_label: bool,
    pub secondary: Option<String>,
    pub meta: Option<String>,
    pub status: Option<NavItemStatus>,
    pub status_label: Option<(String, Tone)>,
    pub badge: Option<String>,
    pub swatch: Option<Color>,
    /// A picture in the icon's place (an album's cover).
    pub art: Option<NavArt>,
    pub checked: Option<bool>,
    pub level: Option<f32>,
    pub presence: Vec<String>,
    /// The person the row stands for, as their avatar in the icon's place.
    pub person: Option<NavPerson>,
    /// Something new waits behind the row (an unread conversation).
    pub unread: bool,
    /// The key assistive technology and tests address the row by; the
    /// default is `nav:item:{id}`.
    pub semantic_key: Option<String>,
    /// What else the row is found by when the list is filtered: the things
    /// the destination holds ("Tap to click" on a touchpad page) and the
    /// words people use for them. A row found by a term that has a name
    /// shows that name under its label.
    pub search_terms: Vec<NavSearchTerm>,
}

/// A picture in a row's icon place ([`NavItem::art`]).
#[derive(Clone, Debug, PartialEq)]
pub struct NavArt {
    /// What the picture is of; its initials stand in while there is none.
    pub name: String,
    pub image: Option<vello::peniko::ImageData>,
}

/// The person a row stands for ([`NavItem::person`]).
#[derive(Clone, Debug, PartialEq)]
pub struct NavPerson {
    /// Who they are; their initials stand in for a picture.
    pub identity: String,
    /// Their colour in the workspace, ringing the avatar; `None` takes the colour their
    /// identity carries ([`AvatarFace::new`]), which every window paints the same.
    pub colour: Option<Color>,
    /// They are speaking now.
    pub speaking: bool,
    /// The name their initials come from, when the label says more than
    /// their name ("Dana (via Discord)"); else the label.
    pub name: Option<String>,
    /// Their picture; their initials stand in while there is none.
    pub image: Option<vello::peniko::ImageData>,
}

/// A row's status dot, as [`NavItem::status`] keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavItemStatus(NavStatus);

impl NavItemStatus {
    /// The status as the row paints it.
    #[must_use]
    pub fn row_status(&self) -> RowStatus<'_> {
        self.0.row_status()
    }
}

impl NavItem {
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new_owned(id.into(), label.into())
    }

    /// [`Self::new`] once its text is owned: one copy in the toolkit
    /// rather than one in every app that calls the generic constructor.
    fn new_owned(id: String, label: String) -> Self {
        Self {
            id,
            label,
            icon: None,
            count: None,
            actions: true,
            children: Vec::new(),
            expanded: false,
            ensure_expanded: false,
            drop_target: false,
            editable_label: false,
            secondary: None,
            meta: None,
            status: None,
            status_label: None,
            badge: None,
            swatch: None,
            art: None,
            person: None,
            unread: false,
            checked: None,
            level: None,
            presence: Vec::new(),
            semantic_key: None,
            search_terms: Vec::new(),
        }
    }

    /// Also found, when the list is filtered, by `terms`: what the
    /// destination holds and the words people use for it.
    #[must_use]
    pub fn search_terms(mut self, terms: impl IntoIterator<Item = NavSearchTerm>) -> Self {
        self.search_terms = terms.into_iter().collect();
        self
    }

    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// A quiet count (items in a folder). Zero shows nothing: a count means
    /// something wants attention, and nothing does.
    #[must_use]
    pub fn count(mut self, count: u32) -> Self {
        self.count = (count > 0).then_some(NavCount::Neutral(count));
        self
    }

    /// A count that wants attention (unread, mentions): the list's one
    /// filled badge, in the person's colour. Zero shows nothing.
    #[must_use]
    pub fn live_count(mut self, count: u32) -> Self {
        self.count = (count > 0).then_some(NavCount::Live(count));
        self
    }

    #[must_use]
    pub fn children(mut self, children: Vec<NavItem>) -> Self {
        self.children = children;
        self
    }

    #[must_use]
    pub fn expanded(mut self) -> Self {
        self.expanded = true;
        self
    }

    /// Open this item whenever the sections are handed over, even where the
    /// person folded it: the place the app is revealing (the database a
    /// connection just opened).
    #[must_use]
    pub fn ensure_expanded(mut self) -> Self {
        self.expanded = true;
        self.ensure_expanded = true;
        self
    }

    #[must_use]
    pub fn drop_target(mut self) -> Self {
        self.drop_target = true;
        self
    }

    #[must_use]
    pub fn editable_label(mut self) -> Self {
        self.editable_label = true;
        self
    }

    /// This row offers none of the list's item actions.
    #[must_use]
    pub fn without_actions(mut self) -> Self {
        self.actions = false;
        self
    }

    /// One `Caption` line under the label (a host and port, a schedule).
    /// It elides; the row grows to hold it.
    #[must_use]
    pub fn secondary(mut self, text: impl Into<String>) -> Self {
        self.secondary = Some(text.into());
        self
    }

    /// A tertiary figure at the trailing edge (a size).
    #[must_use]
    pub fn meta(mut self, text: impl Into<String>) -> Self {
        self.meta = Some(text.into());
        self
    }

    /// A status dot at the trailing edge: a drive's health, a connection's
    /// state, an account that needs attention, a person's presence.
    #[must_use]
    pub fn status(mut self, status: RowStatus<'_>) -> Self {
        self.status = Some(NavItemStatus(NavStatus::of(status)));
        self
    }

    /// A quiet tinted badge naming the row's state ("Active").
    #[must_use]
    pub fn status_label(mut self, label: impl Into<String>, tone: Tone) -> Self {
        self.status_label = Some((label.into(), tone));
        self
    }

    /// A short leading code that classifies the row (`GET`, `POST`). The
    /// codes of one section share a column, so their labels line up.
    #[must_use]
    pub fn badge(mut self, code: impl Into<String>) -> Self {
        self.badge = Some(code.into());
        self
    }

    /// The content colour the row stands for (a calendar, a project), in
    /// the icon's place.
    #[must_use]
    pub fn swatch(mut self, color: Color) -> Self {
        self.swatch = Some(color);
        self
    }

    /// A small rounded picture in the icon's place: what the destination
    /// holds (an album's cover), or the initials of `name` while it has
    /// none. The label lines up with the icons of the rows around it.
    #[must_use]
    pub fn art(mut self, name: impl Into<String>, image: Option<vello::peniko::ImageData>) -> Self {
        self.art = Some(NavArt {
            name: name.into(),
            image,
        });
        self
    }

    /// A checkbox in the leading slot (a calendar shown or hidden). The
    /// whole row toggles it and reports through [`NavList::on_check_action`]; a
    /// checkable row is not a destination and is never selected.
    #[must_use]
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    /// A usage bar under the label, `fraction` of it filled (a backup
    /// destination's space).
    #[must_use]
    pub fn level(mut self, fraction: f32) -> Self {
        self.level = Some(fraction.clamp(0.0, 1.0));
        self
    }

    /// Someone is on this place: a mark in their presence colour. Call once
    /// per person.
    #[must_use]
    pub fn presence(mut self, identity: impl Into<String>) -> Self {
        self.presence.push(identity.into());
        self
    }

    /// The row stands for a person (someone in a call): their avatar, in
    /// `colour`, in the icon's place, ringed while `speaking`. With no
    /// colour, the avatar takes the colour their identity carries, as every
    /// face of them does.
    #[must_use]
    pub fn person(
        mut self,
        identity: impl Into<String>,
        colour: Option<Color>,
        speaking: bool,
    ) -> Self {
        self.person = Some(NavPerson {
            identity: identity.into(),
            colour,
            speaking,
            name: None,
            image: None,
        });
        self
    }

    /// The face of the person the row stands for ([`Self::person`]): the
    /// name their initials come from and their picture.
    #[must_use]
    pub fn person_face(
        mut self,
        name: impl Into<String>,
        image: Option<vello::peniko::ImageData>,
    ) -> Self {
        if let Some(person) = &mut self.person {
            person.name = Some(name.into());
            person.image = image;
        }
        self
    }

    /// Something new waits behind the row: its label stands out at rest.
    #[must_use]
    pub fn unread(mut self, unread: bool) -> Self {
        self.unread = unread;
        self
    }

    /// The key assistive technology, agents and tests address the row by.
    #[must_use]
    pub fn semantic_key(mut self, key: impl Into<String>) -> Self {
        self.semantic_key = Some(key.into());
        self
    }
}

/// Where a dragged item lands (see [`NavList::on_move`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavDrop {
    /// Into a drop target, as its last child.
    Into(String),
    /// Just before this item.
    Before(String),
    /// Just after this item.
    After(String),
}

fn count_items(items: &[NavItem]) -> usize {
    items
        .iter()
        .map(|item| 1usize.saturating_add(count_items(&item.children)))
        .sum()
}

type Callback<A> = Option<Box<dyn FnMut(A)>>;

/// A navigation list's selection, published as its view action `select`
/// ([`NavList::on_view`]).
struct NavView {
    choice: crate::widget::view_choice::ViewChoice,
    handler: Box<dyn FnMut(String)>,
}

/// Every destination of `sections`, nested ones included: (id, label).
fn destinations(sections: &[NavSection]) -> Vec<(String, String)> {
    all_items(sections)
        .into_iter()
        .map(|item| (item.id.clone(), item.label.clone()))
        .collect()
}

/// The id the app gave the item or section a row key names
/// (`item:{id}` → `id`).
fn app_id(key: &str) -> &str {
    key.strip_prefix("item:")
        .or_else(|| key.strip_prefix("section:"))
        .unwrap_or(key)
}

/// The row key a row control's semantic target names: `section-action:{key}`,
/// `row-action:{index}:{key}`, `check:{key}`.
fn control_row(target: &str) -> Option<&str> {
    target
        .strip_prefix("section-action:")
        .or_else(|| target.strip_prefix("check:"))
        .or_else(|| {
            target
                .strip_prefix("row-action:")
                .and_then(|rest| rest.split_once(':'))
                .map(|(_, key)| key)
        })
}

/// What the list's callbacks report into, shared with the closures the
/// list engine holds.
#[derive(Default)]
struct NavShared {
    /// Row keys of the checkable rows.
    checkable: HashSet<String>,
    item_actions: Vec<String>,
    /// Section row key → the id its trailing action runs.
    section_actions: HashMap<String, String>,
    view: Option<NavView>,
    selection_actions: HashMap<String, crate::widget::ActionEmitter<()>>,
    /// The places whose selection the view bound to an action, as it last said.
    component_places: HashSet<String>,
    /// Section row key → the action a section's add runs when the view bound it.
    section_add_actions: HashMap<String, crate::widget::ActionEmitter<()>>,
    /// The sections whose add the view bound to an action, as it last said.
    component_adds: HashSet<String>,
    selection_emitted: bool,
    /// What the list's gestures run as published actions ([`NavList::item_action`]
    /// and the rest of `bound.rs`).
    bound: bound::NavBindings,
    on_action: Option<Box<dyn FnMut(&str, &str)>>,
    on_section_action: Option<Box<dyn FnMut(&str, &str)>>,
    /// What a [`NavFeed`] hears of renames and checks.
    on_rename: Option<Box<dyn FnMut(&str, String)>>,
    on_check: Option<Box<dyn FnMut(&str, bool)>>,
    on_move: Option<Box<dyn FnMut(&str, NavDrop)>>,
    on_expand: Option<Box<dyn FnMut(&str, bool)>>,
    on_delete: Callback<String>,
    on_menu: Option<Box<dyn FnMut(&str, Rect, Point)>>,
    /// A row's menu the list engine was asked for: the row's key, its box
    /// and where the menu opens. The list answers it with the row's lift.
    asked_menu: Option<(String, Rect, Point)>,
    binding: Option<LocalBinding<Option<String>>>,
    /// The chosen destination, including quiet app-driven reconciliation.
    selected: Option<String>,
    /// The person chose a destination since the shell last asked.
    navigated: bool,
}

impl NavShared {
    /// Whether choosing destination `id` runs an action that is unavailable now.
    fn blocked(&self, id: &str) -> bool {
        self.selection_actions
            .get(id)
            .is_some_and(|action| !action.is_available())
    }

    /// The person chose the row of `key`: a click, or the keyboard moving
    /// the selection.
    fn choose(&mut self, key: &str) {
        let Some(id) = key.strip_prefix("item:").map(str::to_owned) else {
            return;
        };
        if self.blocked(&id) {
            return;
        }
        self.navigated = true;
        if self.selected.as_deref() == Some(id.as_str()) {
            // Choosing the chosen destination again still navigates to it.
            if let Some(action) = self.selection_actions.get_mut(&id) {
                let _ = action.emit(());
            } else if let Some(view) = self.view.as_mut() {
                (view.handler)(id.clone());
            }
            self.selection_emitted = false;
            return;
        }
        self.selection_emitted = false;
        self.selected = Some(id.clone());
        if let Some(binding) = self.binding.as_mut() {
            binding.publish(&Some(id.clone()));
        }
        if let Some(action) = self.selection_actions.get_mut(&id) {
            let _ = action.emit(());
            self.selection_emitted = true;
        } else if let Some(view) = self.view.as_mut() {
            // The person's choice is the published view action's own run:
            // no host decides a view or records it, and the list may be off
            // screen once it is made.
            (view.handler)(id);
        }
    }

    /// The person activated the row of `key` (Enter): the chosen
    /// destination's screen opens again.
    fn activate(&mut self, key: &str) {
        let Some(id) = key.strip_prefix("item:").map(str::to_owned) else {
            return;
        };
        if self.blocked(&id) {
            return;
        }
        self.navigated = true;
        if std::mem::take(&mut self.selection_emitted) {
            return;
        }
        if let Some(action) = self.selection_actions.get_mut(&id) {
            let _ = action.emit(());
        } else {
            // A list that answers through its view action or its selection
            // stand opens the place itself: an activation chooses like a
            // press, whether or not the row was already chosen.
            self.choose(key);
        }
    }

    /// Run item action `index` on the row of `key`.
    fn run_item_action(&mut self, key: &str, index: usize) {
        let Some(action) = self.item_actions.get(index).cloned() else {
            return;
        };
        let id = app_id(key).to_owned();
        if let Some(emitter) = self.bound.items.get_mut(&action) {
            bound::emit(emitter, id);
        } else if let Some(callback) = self.on_action.as_mut() {
            callback(&id, &action);
        }
    }

    /// Run the trailing action of the section header `key`.
    fn run_section_action(&mut self, key: &str) -> bool {
        let Some(action) = self.section_actions.get(key).cloned() else {
            return false;
        };
        let section = app_id(key).to_owned();
        if let Some(emitter) = self.bound.sections.get_mut(&action) {
            bound::emit(emitter, section);
        } else if let Some(emitter) = self.section_add_actions.get_mut(key) {
            let _ = emitter.emit(());
        } else if let Some(callback) = self.on_section_action.as_mut() {
            callback(&section, &action);
        }
        true
    }

    /// Ask the host for the menu of the row of `key`, the row marked by
    /// `lift`, at `at`.
    fn report_menu(&mut self, key: &str, lift: Rect, at: Point) {
        if let Some(callback) = self.on_menu.as_mut() {
            callback(app_id(key), lift, at);
        }
    }
}

/// The navigation list. See the module docs.
pub struct NavList {
    list: Box<VirtualList<NavModel>>,
    shared: Rc<RefCell<NavShared>>,
    /// What every row paints from beyond its own data.
    paint: Rc<RefCell<NavPaint>>,
    /// The folds and checks the person made.
    state: NavState,
    /// The sections as the app handed them, before any filter.
    sections: Vec<NavSection>,
    /// The filter the window's field applies to filterable sections.
    filter: String,
    /// The state the filter is kept in ([`Self::bind_filter`]).
    filter_binding: Option<LocalBinding<String>>,
    /// The app state the list follows, read at every sync and update.
    source: Option<NavSource>,
    /// The app declared its places come under headings, even while none
    /// has loaded yet ([`Self::with_headings`]).
    headings: bool,
    /// The feed the list answers "which place is here?" through, for a
    /// drag the app runs over it ([`NavFeed::place_at`]).
    feed: Option<NavFeed>,
    /// The name assistive technology, agents and tests read for the list as
    /// a whole: what it narrows, apart from the app's own title.
    semantic_key: Option<String>,
    /// The actions at the trailing edge of every item that offers them.
    row_actions: Vec<NavAction>,
    /// The key the list was built under: what its sections are addressed by.
    key: String,
    /// A drag staged for a capture, shown once the rows stand placed
    /// ([`Self::stage_drag`]).
    staged_drag: Option<(String, String)>,
}

/// What a followed list reads: the sections and the chosen destination, or
/// `None` while the state cannot be read (it is borrowed elsewhere).
type NavSource = Box<dyn FnMut() -> Option<(Vec<NavSection>, Option<String>)>>;

impl NavList {
    /// Erase the widget type in the SDK, keeping the navigation list's widget
    /// implementation in the shared runtime rather than in each thin app.
    /// The boxed widget still downcasts to `NavList`.
    #[must_use]
    pub fn into_widget(self) -> Box<dyn crate::widget::Widget> {
        Box::new(self)
    }

    #[must_use]
    pub fn new(sections: Vec<NavSection>) -> Self {
        let state = NavState::default();
        let model = model::build(&sections, "", &state);
        let shared = Rc::new(RefCell::new(NavShared::default()));
        let paint = Rc::new(RefCell::new(NavPaint::default()));
        let row_paint = Rc::clone(&paint);
        let (select_shared, activate_shared, rename_shared, delete_shared) = (
            Rc::clone(&shared),
            Rc::clone(&shared),
            Rc::clone(&shared),
            Rc::clone(&shared),
        );
        let list = VirtualList::with_selection_delegate(
            model,
            crate::num::f32_of_f64(nav_row_height(&Theme::dark())),
            move |model: &NavModel, index, _key, build| {
                let row = model.row(index).cloned().unwrap_or(NavRowData {
                    key: String::new(),
                    kind: NavRowKind::Pending { id: String::new() },
                });
                Box::new(NavRow::new(row, Rc::clone(&row_paint), build.selection))
            },
        )
        .filling()
        // A navigation row lays out its own insets and paints its own lift.
        .flush()
        .overlay_scrollbar()
        .selection_follows_focus()
        .with_semantic_key(Clone::clone)
        .on_select(move |key, _| select_shared.borrow_mut().choose(&key))
        .on_activate(move |key| activate_shared.borrow_mut().activate(&key))
        .on_rename(move |key, title| {
            let mut shared = rename_shared.borrow_mut();
            let id = app_id(&key).to_owned();
            if let Some(emitter) = shared.bound.rename.as_mut() {
                bound::emit(emitter, (id, title));
            } else if let Some(callback) = shared.on_rename.as_mut() {
                callback(&id, title);
            }
        })
        .on_delete(move |key| {
            let mut shared = delete_shared.borrow_mut();
            let Some(id) = key.strip_prefix("item:").map(str::to_owned) else {
                return;
            };
            if let Some(emitter) = shared.bound.delete.as_mut() {
                bound::emit(emitter, id);
            } else if let Some(callback) = shared.on_delete.as_mut() {
                callback(id);
            }
        });
        let list = Self {
            list: Box::new(list),
            shared,
            paint,
            state,
            sections,
            filter: String::new(),
            filter_binding: None,
            source: None,
            headings: false,
            feed: None,
            semantic_key: None,
            row_actions: Vec::new(),
            key: String::new(),
            staged_drag: None,
        };
        list.refresh_shared();
        list
    }

    /// Refresh what the callbacks read of the rows: which check, which
    /// header runs which action.
    fn refresh_shared(&self) {
        let model = self.list.model();
        let mut shared = self.shared.borrow_mut();
        shared.checkable = (0..model.len())
            .filter_map(|index| model.row(index))
            .filter(|row| model.is_checkable(&row.key))
            .map(|row| row.key.clone())
            .collect();
        shared.section_actions = self
            .sections
            .iter()
            .filter_map(|section| {
                let action = section.action_id()?;
                Some((format!("section:{}", section.id), action.to_owned()))
            })
            .collect();
    }

    /// Give the list a stable id for tests and inspection.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        let id = id.into();
        self.list = Box::new(self.list.with_id(id.clone()));
        self.key = id;
        self
    }

    /// The name the list is announced by ("Mailboxes", "Settings").
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.list = Box::new(self.list.with_accessible_name(name));
        self
    }

    /// The list's stable identity for assistive technology: its place in the
    /// window, on its own, for what it narrows rather than the app.
    #[must_use]
    pub fn with_semantic_key(mut self, key: impl Into<String>) -> Self {
        let key = key.into();
        self.list = Box::new(self.list.with_id(key.clone()));
        self.semantic_key = Some(key);
        self
    }

    /// Choose `id` and bring its row into view. Does not report through
    /// [`Self::on_view`].
    #[must_use]
    pub fn selected(mut self, id: &str) -> Self {
        self.set_selected(Some(id));
        self
    }

    /// Keep the chosen destination in `state`: the list shows what the state
    /// holds (and brings it into view) and writes the person's choice back.
    #[must_use]
    pub fn bind_selection(mut self, state: &LocalState<Option<String>>) -> Self {
        let (value, binding) = LocalBinding::new(state);
        self.set_selected(value.as_deref());
        self.shared.borrow_mut().binding = Some(binding);
        self
    }

    /// Keep the filter in `state`: the list narrows to what the state holds,
    /// and a filter given to the list (the window's search, through its
    /// [`Filterable`]) is written back, so a view reads what the person
    /// searches for from its own state.
    #[must_use]
    pub fn bind_filter(mut self, state: &LocalState<String>) -> Self {
        let (value, binding) = LocalBinding::new(state);
        self.set_filter(&value);
        self.filter_binding = Some(binding);
        self
    }

    /// Keep the list in step with app state: `source` answers the sections
    /// and the chosen destination, read now and at every sync and update. A
    /// choice the person makes shows until the state answers it; a state that
    /// chooses otherwise wins.
    #[must_use]
    pub fn follow(
        mut self,
        source: impl FnMut() -> Option<(Vec<NavSection>, Option<String>)> + 'static,
    ) -> Self {
        self.source = Some(Box::new(source));
        self.pull();
        self
    }

    /// Read the followed state; whether the list changed.
    fn pull(&mut self) -> bool {
        let Some((sections, selected)) = self.source.as_mut().and_then(|source| source()) else {
            return false;
        };
        let changed = sections != self.sections;
        self.set_sections(sections);
        if self.selected_id() == selected {
            return changed;
        }
        self.set_selected(selected.as_deref());
        true
    }

    /// Called with the item id when a destination is chosen: the list, as
    /// control `id`, publishes choosing a destination as its view action
    /// `select` (`view_choice`), so a person's
    /// click, a screen reader, a script and an agent choose through the same
    /// action. Showing a destination is the window's own state, never an
    /// edit: the action is absent from history.
    #[must_use]
    pub fn on_view(self, id: impl Into<String>, f: impl FnMut(String) + 'static) -> Self {
        let id = id.into();
        self.set_on_view(id.clone(), f);
        self.with_id(id)
    }

    /// [`Self::on_view`] after construction: what choosing a destination does.
    fn set_on_view(&self, id: impl Into<String>, f: impl FnMut(String) + 'static) {
        let id = id.into();
        self.shared.borrow_mut().view = Some(NavView {
            choice: crate::widget::view_choice::ViewChoice::new(id, "Go to"),
            handler: Box::new(f),
        });
    }

    /// The published selection ([`Self::on_view`]), offering the current
    /// destinations.
    fn view_resource(&self) -> Option<tilo_actions::ResourceSpec> {
        let shared = self.shared.borrow();
        let view = shared.view.as_ref()?;
        Some(view.choice.resource(destinations(&self.sections)))
    }

    /// Answer a `select` addressed to this list: show the destination and
    /// report it, whoever chose it.
    fn start_view(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let chosen = match self.shared.borrow().view.as_ref() {
            Some(view) => view.choice.start(op)?,
            None => None,
        };
        let Some(destination) = chosen else {
            return Ok(None);
        };
        if !destinations(&self.sections)
            .iter()
            .any(|(id, _)| *id == destination)
        {
            return Err(format!("no destination `{destination}`"));
        }
        if self.selected_id().as_deref() != Some(destination.as_str()) {
            self.set_selected(Some(&destination));
            if let Some(binding) = self.shared.borrow_mut().binding.as_mut() {
                binding.publish(&Some(destination.clone()));
            }
        }
        let mut shared = self.shared.borrow_mut();
        shared.navigated = true;
        if let Some(view) = shared.view.as_mut() {
            (view.handler)(destination);
        }
        Ok(Some(crate::widget::view_choice::chosen()))
    }

    /// Bind each destination to its published typed action. Bound rows route
    /// selection through host custody; unbound rows choose through
    /// [`Self::on_view`].
    #[must_use]
    pub fn selection_actions(
        self,
        bindings: impl IntoIterator<Item = (String, tilo_actions::ActionBinding<()>)>,
    ) -> Self {
        self.shared.borrow_mut().selection_actions = bindings
            .into_iter()
            .map(|(id, binding)| (id, crate::widget::ActionEmitter::new(binding)))
            .collect();
        self
    }

    /// Actions shown on hover and focus at the trailing edge of every item
    /// that has not opted out ([`NavItem::without_actions`]); reported with
    /// the item id and the action id through `on_action`. An action that
    /// [opens the menu](NavAction::opens_context_menu) asks for the row's
    /// menu instead.
    #[must_use]
    pub fn item_actions(
        mut self,
        actions: Vec<NavAction>,
        on_action: impl FnMut(&str, &str) + 'static,
    ) -> Self {
        self.row_actions = actions;
        self.shared.borrow_mut().on_action = Some(Box::new(on_action));
        self.with_installed_row_actions()
    }

    /// Hand the rows the item actions as they stand.
    fn with_installed_row_actions(self) -> Self {
        self.paint.borrow_mut().actions = self
            .row_actions
            .iter()
            .map(|action| (action.icon, action.label.clone()))
            .collect();
        self.shared.borrow_mut().item_actions = self
            .row_actions
            .iter()
            .map(|action| action.id.clone())
            .collect();
        self
    }

    /// Called with the section id and the action id when a section header's
    /// trailing action is pressed.
    #[must_use]
    pub fn on_section_action(self, f: impl FnMut(&str, &str) + 'static) -> Self {
        self.set_section_action(f);
        self
    }

    /// [`Self::on_section_action`] after construction.
    fn set_section_action(&self, f: impl FnMut(&str, &str) + 'static) {
        self.shared.borrow_mut().on_section_action = Some(Box::new(f));
    }

    /// Bind places' selection and sections' adds to the actions a view bound to them: choosing a
    /// place or pressing a heading's add runs that action through the list's control door, as a
    /// destination bound with [`Self::selection_actions`] does. What the view no longer binds
    /// stops being bound.
    fn bind_actions(
        &self,
        places: &HashMap<String, crate::component::Event>,
        adds: &HashMap<String, crate::component::Event>,
    ) {
        let mut shared = self.shared.borrow_mut();
        for key in std::mem::take(&mut shared.component_places) {
            shared.selection_actions.remove(&key);
        }
        for (key, event) in places {
            if let crate::component::Event::Action(binding) = event {
                shared.selection_actions.insert(
                    key.clone(),
                    crate::widget::ActionEmitter::new(binding.clone()),
                );
                shared.component_places.insert(key.clone());
            }
        }
        for key in std::mem::take(&mut shared.component_adds) {
            shared.section_add_actions.remove(&key);
        }
        for (key, event) in adds {
            if let crate::component::Event::Action(binding) = event {
                shared.section_add_actions.insert(
                    format!("section:{key}"),
                    crate::widget::ActionEmitter::new(binding.clone()),
                );
                shared.component_adds.insert(format!("section:{key}"));
            }
        }
    }

    /// Report renames to `f`: how a [`NavFeed`] hears of them.
    fn on_rename(mut self, f: impl FnMut(&str, String) + 'static) -> Self {
        self.shared.borrow_mut().on_rename = Some(Box::new(f));
        self.list = Box::new(self.list.rename_on_double_click());
        self
    }

    /// Report checks to `f`: how a [`NavFeed`] hears of them.
    fn on_check(self, f: impl FnMut(&str, bool) + 'static) -> Self {
        self.shared.borrow_mut().on_check = Some(Box::new(f));
        self
    }

    /// Called with the item or section id and whether it is now open when
    /// the person folds or unfolds it, so the app can keep what was folded.
    #[must_use]
    pub fn on_expand(self, f: impl FnMut(&str, bool) + 'static) -> Self {
        self.shared.borrow_mut().on_expand = Some(Box::new(f));
        self
    }

    /// Called with the focused item's id when the person presses Delete
    /// (never Backspace, which belongs to whatever editor sits beside the
    /// list).
    #[must_use]
    pub fn on_delete(self, f: impl FnMut(String) + 'static) -> Self {
        self.shared.borrow_mut().on_delete = Some(Box::new(f));
        self
    }

    /// Called when the person asks for an item's menu: a right click or a
    /// long press on its row, the Menu key or Shift+F10 on it, or an item
    /// action that [opens the menu](NavAction::opens_context_menu). It
    /// receives the item id, the row's lift (hand it to
    /// `ContextMenu::anchor_row` so the row stays marked) and where the menu
    /// opens: the pointer, or the bottom-leading corner of the row or the
    /// action.
    #[must_use]
    pub fn on_context_menu(mut self, f: impl FnMut(&str, Rect, Point) + 'static) -> Self {
        self.shared.borrow_mut().on_menu = Some(Box::new(f));
        let shared = Rc::clone(&self.shared);
        self.list = Box::new(self.list.with_row_menu_reporter(move |key, row, at| {
            shared.borrow_mut().asked_menu = Some((key, row, at));
        }));
        self
    }

    /// Let the person drag items: into an item marked
    /// [`NavItem::drop_target`], or before and after items in the same
    /// section. The app moves the item in its own data and hands the list
    /// its new sections ([`Self::set_sections`]).
    #[must_use]
    pub fn on_move(self, f: impl FnMut(&str, NavDrop) + 'static) -> Self {
        self.shared.borrow_mut().on_move = Some(Box::new(f));
        self.with_reordering()
    }

    /// Let items be dragged, reporting each drop to the bound action or the
    /// callback.
    fn with_reordering(mut self) -> Self {
        let shared = Rc::clone(&self.shared);
        self.list = Box::new(self.list.with_reorder(
            |model: &NavModel, source: &String, target: &DropTarget<String>| {
                if !model.is_item(source) || !model.is_item(&target.key) {
                    return false;
                }
                match target.placement {
                    DropPlacement::Inside => model.takes_children(&target.key),
                    DropPlacement::Before | DropPlacement::After => true,
                }
            },
            move |_model: &mut NavModel, dropped: ReorderDrop<String>| {
                let source = app_id(&dropped.source).to_owned();
                let target = app_id(&dropped.target.key).to_owned();
                let drop = match dropped.target.placement {
                    DropPlacement::Inside => NavDrop::Into(target),
                    DropPlacement::Before => NavDrop::Before(target),
                    DropPlacement::After => NavDrop::After(target),
                };
                let mut shared = shared.borrow_mut();
                if let Some(emitter) = shared.bound.moves.as_mut() {
                    bound::emit(emitter, (source, drop));
                } else if let Some(callback) = shared.on_move.as_mut() {
                    callback(&source, drop);
                }
                // The app owns the order; the list changes when it hands
                // back new sections.
                false
            },
        ));
        self
    }

    /// The places come in groups under headings (a contact book's groups,
    /// a mail account's folders), even where none has loaded yet or the
    /// person has made none. A sidebar that can hold headings starts as a
    /// list, so it never turns from rail into list as its groups arrive.
    #[must_use]
    pub fn with_headings(mut self) -> Self {
        self.headings = true;
        self
    }

    /// An icon-over-label compact rail (a workspace switcher, a channel
    /// rail). Section titles give way to space; counts become a mark;
    /// captions wrap to a second line before they elide.
    #[must_use]
    pub fn rail(mut self) -> Self {
        self.set_rail_rows(true);
        self
    }

    /// Paint the rows as a rail's tiles or as list rows; whether that
    /// changed. The rows change shape in place, keeping the selection,
    /// focus and scroll.
    fn set_rail_rows(&mut self, rail: bool) -> bool {
        if self.paint.borrow().rail == rail {
            return false;
        }
        self.paint.borrow_mut().rail = rail;
        self.list.update_model(|_| ());
        true
    }

    /// Replace the destinations, keeping the selection, expansion and focus
    /// of every id that is still there.
    pub fn set_sections(&mut self, sections: Vec<NavSection>) {
        if sections == self.sections {
            return;
        }
        self.sections = sections;
        self.rebuild();
    }

    /// Build the rows from the sections as the filter and the person's
    /// folds show them.
    fn rebuild(&mut self) {
        let selected = self.selected_id();
        let model = model::build(&self.sections, &self.filter, &self.state);
        // Only the rows whose content changed are built anew; the rest keep
        // their widgets, wherever they now stand.
        let changed: Vec<String> = {
            let current = self.list.model();
            (0..model.len())
                .filter_map(|index| model.row(index))
                .filter(|row| current.row_of(&row.key) != Some(*row))
                .map(|row| row.key.clone())
                .collect()
        };
        self.list.update_model_changes(
            &[crate::widget::virtual_list::ListModelChange::RowsChanged { keys: changed }],
            move |current| *current = model,
        );
        self.refresh_shared();
        self.select_quietly(selected.as_deref());
    }

    /// Do what the app asked through its feed: open or fold rows
    /// ([`NavFeed::set_open`]) and open a rename field
    /// ([`NavFeed::begin_rename`]). Whether anything changed.
    fn take_asks(&mut self) -> bool {
        let Some(feed) = self.feed.clone() else {
            return false;
        };
        let mut changed = false;
        for (key, open) in feed.take_openings() {
            if self.is_expanded(&key) != open {
                self.set_expanded(&key, open);
                changed = true;
            }
        }
        let Some(id) = feed.take_rename_request() else {
            return changed;
        };
        let Some(key) = self.row_key(&id) else {
            return changed;
        };
        self.list.reveal(&key);
        self.list.begin_rename(&key) || changed
    }

    /// The key of the row showing item or section `id`, when it shows.
    fn row_key(&self, id: &str) -> Option<String> {
        let model = self.list.model();
        model.row_id(id).and_then(|index| model.key_at(index))
    }

    /// Choose `id` (or nothing) without reporting it, and bring its row
    /// into view (once the list has a viewport, if it has none yet).
    pub fn set_selected(&mut self, id: Option<&str>) {
        if let Some(key) = self.select_quietly(id) {
            self.list.reveal(&key);
        }
    }

    /// Choose the place that names the resource `id`, without reporting it, when the list has
    /// one: the place follows the content the window shows. Answers whether it chose one, and
    /// leaves the selection as it was when no place names `id`.
    pub fn select_naming(&mut self, id: &str) -> bool {
        let Some(item) = self.list.model().item_naming(id).map(str::to_owned) else {
            return false;
        };
        self.set_selected(Some(&item));
        true
    }

    fn select_quietly(&mut self, id: Option<&str>) -> Option<String> {
        let key = id
            .map(|id| format!("item:{id}"))
            .filter(|key| self.list.model().is_item(key));
        match &key {
            Some(key) => {
                self.list.selection_mut().select_only(key.clone());
                self.list.focus_row(key.clone());
            }
            None => self.list.selection_mut().clear(),
        }
        self.shared.borrow_mut().selected = key.as_deref().map(app_id).map(str::to_owned);
        key
    }

    /// The chosen destination.
    #[must_use]
    pub fn selected_id(&self) -> Option<String> {
        let model = self.list.model();
        self.list
            .selection()
            .stored_keys()
            .find(|key| model.is_item(key))
            .map(|key| app_id(key).to_owned())
    }

    /// How many things the chosen destination holds, where the list draws that count beside
    /// it: the number a column header of the same list would otherwise draw a second time.
    #[must_use]
    pub fn selected_count(&self) -> Option<NavCount> {
        let key = self.selected_id()?;
        all_items(&self.sections)
            .into_iter()
            .find(|item| item.id == key)
            .and_then(|item| item.count)
    }

    /// Whether the list is a compact rail of icons, where a count draws as a mark rather than
    /// as its number.
    #[must_use]
    pub fn is_rail(&self) -> bool {
        self.paint.borrow().rail
    }

    /// The person's or the app's fold of item or section `id`, whether or
    /// not its row shows now.
    fn fold_key(&self, id: &str) -> Option<String> {
        if all_items(&self.sections).iter().any(|item| item.id == id) {
            return Some(format!("item:{id}"));
        }
        self.sections
            .iter()
            .any(|section| section.id == id)
            .then(|| format!("section:{id}"))
    }

    /// Open or fold item or section `id` without reporting it.
    pub fn set_expanded(&mut self, id: &str, open: bool) {
        let Some(key) = self.fold_key(id) else {
            return;
        };
        self.state.set_open(&key, open);
        self.rebuild();
    }

    /// Open or fold the row of `key` as the person asked, and report it.
    fn fold(&mut self, key: &str, open: bool) {
        self.state.set_open(key, open);
        self.rebuild();
        if let Some(callback) = self.shared.borrow_mut().on_expand.as_mut() {
            callback(app_id(key), open);
        }
    }

    /// Whether item or section `id` is open.
    #[must_use]
    pub fn is_expanded(&self, id: &str) -> bool {
        if let Some(section) = self.sections.iter().find(|section| section.id == id) {
            return !section.collapsible
                || self
                    .state
                    .open(&format!("section:{id}"), !section.collapsed, false);
        }
        all_items(&self.sections)
            .into_iter()
            .find(|item| item.id == id)
            .is_some_and(|item| {
                // A row with nothing under it yet has nothing to open.
                !item.children.is_empty()
                    && self
                        .state
                        .open(&format!("item:{id}"), item.expanded, item.ensure_expanded)
            })
    }

    /// The painted rect of item or section `id`'s row, edge to edge of the
    /// list, when it is in view.
    #[must_use]
    pub fn row_rect(&self, id: &str) -> Option<Rect> {
        self.list.row_rect(&self.row_key(id)?)
    }

    /// The lift of item `id`'s row: where its hover and selection paint.
    /// Hand it to `ContextMenu::anchor_row` so a menu marks the row.
    #[must_use]
    pub fn row_lift_rect(&self, id: &str) -> Option<Rect> {
        let key = format!("item:{id}");
        let row = self.list.model().row_of(&key)?;
        self.paint
            .borrow()
            .lift_rect(row, self.list.row_rect(&key)?)
    }

    /// Where item action `action` sits on item `id`'s row (whether or not
    /// the row is hovered), to open a menu at it with `open_anchored`.
    #[must_use]
    pub fn item_action_rect(&self, id: &str, action: &str) -> Option<Rect> {
        let index = self
            .shared
            .borrow()
            .item_actions
            .iter()
            .position(|known| known == action)?;
        self.list.row_action_rect(&format!("item:{id}"), index)
    }

    /// Show item `id` being dragged to just before item `before`, for a
    /// capture: the row lifts under a resting pointer and the drop line
    /// shows. Nothing moves unless the person releases it. Needs
    /// [`Self::on_move`].
    pub fn stage_drag(&mut self, id: &str, before: &str) {
        self.staged_drag = Some((format!("item:{id}"), format!("item:{before}")));
        self.apply_staged_drag();
    }

    fn apply_staged_drag(&mut self) {
        if let Some((source, before)) = self.staged_drag.take()
            && !self.list.stage_reorder(&source, &before)
        {
            self.staged_drag = Some((source, before));
        }
    }

    /// Narrow the filterable sections to `query` (an empty query shows
    /// everything). The shell's filter field calls this; an app with its own
    /// field may too. The person's folds come back as the filter clears.
    pub fn set_filter(&mut self, query: &str) {
        if query == self.filter {
            return;
        }
        query.clone_into(&mut self.filter);
        self.rebuild();
        if let Some(binding) = self.filter_binding.as_mut() {
            binding.publish(&self.filter);
        }
    }

    /// The filter the list is narrowed to.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Stable item ids in the current expanded and filtered row order.
    /// Section headers are omitted.
    #[must_use]
    pub fn visible_item_ids(&self) -> Vec<String> {
        let model = self.list.model();
        (0..model.len())
            .filter_map(|index| model.row(index))
            .filter_map(|row| match &row.kind {
                NavRowKind::Item { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect()
    }

    /// Place the list at `rect` for a host that paints its own layout and
    /// positions its children by hand rather than through the layout tree.
    pub fn place_at(&mut self, rect: Rect) {
        let theme = self.paint.borrow().theme.clone();
        // The rows lay out in a tree of the list's own, as the host's would.
        let _ = crate::text_render::with_layout_renderer(theme.text_scale, |text| {
            crate::widget::layout::lay_out_detached(self, rect, &theme, text)
        });
    }

    /// The list's rows engine, for tests reading its state.
    #[cfg(test)]
    pub(crate) fn list(&self) -> &VirtualList<NavModel> {
        &self.list
    }
}

/// The person's hand and keys on the rows' own controls: the list engine
/// selects, opens, renames, drags and asks for menus; the list answers the
/// navigation rows' headers, disclosures, checks and actions.
impl NavList {
    /// Hand the rows what they show of the list's own state: the row
    /// renamed, the row holding the keyboard, a drag.
    fn sync_paint(&self) {
        let mut paint = self.paint.borrow_mut();
        paint.renaming = self.list.renaming().cloned();
        paint.focused = crate::widget::Widget::is_focused(&*self.list)
            .then(|| self.list.focused_key().cloned())
            .flatten();
        paint.dragging = self.list.is_dragging();
    }

    /// The row of `key` and where it stands.
    fn placed(&self, key: &str) -> Option<(NavRowData, Rect)> {
        let key = key.to_owned();
        let row = self.list.model().row_of(&key)?.clone();
        Some((row, self.list.row_rect(&key)?))
    }

    /// Follow the pointer to `point` (or off the list): the row under it
    /// shows its hover and its actions.
    fn hover(&self, point: Option<Point>) {
        let key = point.and_then(|point| self.list.key_at_point(point));
        self.paint.borrow_mut().hovered = key.zip(point);
    }

    /// A press at `point` on one of a row's own controls. Whether it was
    /// one, so the list engine does not select the row.
    fn press_control(&mut self, point: Point) -> bool {
        let Some(key) = self.list.key_at_point(point) else {
            return false;
        };
        let Some((row, rect)) = self.placed(&key) else {
            return false;
        };
        // The keyboard follows the hand to the row it pressed.
        self.list.focus_row(key.clone());
        let (actions, disclosure, section_action) = {
            let paint = self.paint.borrow();
            (
                paint.action_rects(&row, rect),
                paint.disclosure_rect(&row, rect),
                paint.section_action_rect(&row, rect),
            )
        };
        match &row.kind {
            NavRowKind::Section { open, .. } => {
                if section_action.is_some_and(|action| action.contains(point)) {
                    self.shared.borrow_mut().run_section_action(&key);
                } else if disclosure.is_some_and(|band| band.contains(point)) {
                    self.fold(&key, !open);
                }
                // A header is no destination: a press elsewhere on it chooses nothing.
                true
            }
            NavRowKind::Caption { .. } | NavRowKind::Pending { .. } => true,
            NavRowKind::Item { open, spec, .. } => {
                if let Some(index) = actions.iter().position(|action| action.contains(point)) {
                    self.press_item_action(&key, index, actions[index]);
                    return true;
                }
                if disclosure.is_some_and(|chevron| chevron.contains(point)) {
                    self.fold(&key, !open);
                    return true;
                }
                if spec.checked.is_some() {
                    // The whole row is the checkbox's target.
                    self.toggle_check(&key);
                    return true;
                }
                false
            }
        }
    }

    /// Run item action `index` on the row of `key`, standing at `rect`: an
    /// action that opens the menu asks for the row's menu at its corner.
    fn press_item_action(&self, key: &str, index: usize, rect: Rect) {
        if self
            .row_actions
            .get(index)
            .is_some_and(|action| action.opens_menu)
        {
            if let Some((row, placed)) = self.placed(key) {
                let lift = self.paint.borrow().lift_rect(&row, placed);
                if let Some(lift) = lift {
                    self.shared
                        .borrow_mut()
                        .report_menu(key, lift, Point::new(rect.x0, rect.y1));
                }
            }
            return;
        }
        self.shared.borrow_mut().run_item_action(key, index);
    }

    /// Toggle the checkable row of `key`: the row shows the new state at
    /// once, and the app's next sections confirm it.
    fn toggle_check(&mut self, key: &str) -> bool {
        let checked = match self.list.model().row_of(key).map(|row| &row.kind) {
            Some(NavRowKind::Item { spec, .. }) => match spec.checked {
                Some(checked) => checked,
                None => return false,
            },
            _ => return false,
        };
        if self
            .shared
            .borrow()
            .bound
            .check
            .as_ref()
            .is_some_and(|emitter| !emitter.is_available())
        {
            return true;
        }
        self.state.set_checked(key, !checked);
        self.rebuild();
        let mut shared = self.shared.borrow_mut();
        let id = app_id(key).to_owned();
        if let Some(emitter) = shared.bound.check.as_mut() {
            bound::emit(emitter, (id, !checked));
        } else if let Some(callback) = shared.on_check.as_mut() {
            callback(&id, !checked);
        }
        true
    }

    /// Hand the keyboard to the row at `index`: a destination is chosen as
    /// it is reached, a header that folds only takes the keyboard.
    fn focus_stop(&mut self, index: usize) {
        let Some(key) = self.list.model().key_at(index) else {
            return;
        };
        self.list.reveal(&key);
        if self.list.model().is_item(&key) {
            self.list.selection_mut().select_only(key.clone());
            self.shared.borrow_mut().choose(&key);
        }
    }

    /// The row a key on the row of `key` folds or leaves for: the item or
    /// header it stands under.
    fn parent_row(&self, index: usize) -> Option<usize> {
        let model = self.list.model();
        let depth = |row: &NavRowData| match &row.kind {
            NavRowKind::Item { spec, .. } => Some(spec.depth),
            _ => None,
        };
        let own = depth(model.row(index)?)?;
        (0..index).rev().find(|above| {
            model.row(*above).is_some_and(|row| match depth(row) {
                Some(depth) => depth < own,
                None => matches!(
                    row.kind,
                    NavRowKind::Section {
                        collapsible: true,
                        ..
                    }
                ),
            })
        })
    }

    /// A key on the focused row the navigation rows answer themselves:
    /// the arrows pass over captions and headers that do not fold, Left and
    /// Right fold, Space and Enter check a checkable row and fold a header.
    fn key(&mut self, key: &Key, modifiers: winit::keyboard::ModifiersState) -> bool {
        if modifiers.control_key() || modifiers.alt_key() || modifiers.super_key() {
            return false;
        }
        let press = matches!(key, Key::Named(NamedKey::Space | NamedKey::Enter))
            || matches!(key, Key::Character(text) if text.as_str() == " ");
        let control = self.paint.borrow().focused_control.clone();
        if let Some(control) = control {
            if press
                && self.perform_row_action(&control, &crate::a11y::Action::Activate) == Some(true)
            {
                return true;
            }
            // Any other key, or a control that is gone, leaves it for the row.
            self.focus_control(None);
        }
        let focused = self.list.focused_key().cloned();
        let index = focused
            .as_ref()
            .and_then(|key| self.list.model().index_of(key));
        let model = self.list.model();
        let stop = match key {
            Key::Named(NamedKey::ArrowDown) => Some(match index {
                Some(index) => model.stop_from(index, true),
                None => model.first_stop(),
            }),
            Key::Named(NamedKey::ArrowUp) => Some(match index {
                Some(index) => model.stop_from(index, false),
                None => model.first_stop(),
            }),
            Key::Named(NamedKey::Home) => Some(model.first_stop()),
            Key::Named(NamedKey::End) => Some(model.last_stop()),
            _ => None,
        };
        if let Some(stop) = stop {
            if let Some(stop) = stop {
                self.focus_stop(stop);
            }
            return true;
        }
        let (Some(focused), Some(index)) = (focused, index) else {
            return false;
        };
        let branch = model.is_branch(&focused);
        let open = model.is_open(&focused);
        let section = matches!(
            model.row_of(&focused).map(|row| &row.kind),
            Some(NavRowKind::Section { .. })
        );
        match key {
            Key::Named(NamedKey::ArrowRight) if branch && !open => self.fold(&focused, true),
            Key::Named(NamedKey::ArrowLeft) if branch && open => self.fold(&focused, false),
            Key::Named(NamedKey::ArrowLeft) => {
                if let Some(parent) = self.parent_row(index) {
                    self.focus_stop(parent);
                }
            }
            Key::Named(NamedKey::ArrowRight) => {}
            _ if press && model.is_checkable(&focused) => {
                self.toggle_check(&focused);
            }
            _ if press && section => {
                if branch {
                    self.fold(&focused, !open);
                }
            }
            _ => return false,
        }
        true
    }

    /// Hand the keyboard to a row's own control (its semantic target), or
    /// back to the row: the rows involved announce it anew.
    fn focus_control(&mut self, target: Option<&str>) {
        let before = std::mem::replace(
            &mut self.paint.borrow_mut().focused_control,
            target.map(str::to_owned),
        );
        if before.as_deref() != target {
            let rows = [before.as_deref(), target]
                .into_iter()
                .flatten()
                .filter_map(control_row)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            self.list.invalidate_rows(rows);
        }
    }

    /// Answer a row's menu the list engine was asked for, with the row's
    /// lift: at the pointer when the pointer asked, otherwise (the keyboard,
    /// assistive technology) at the bottom-leading corner of the row.
    fn take_menu(&self, by_pointer: bool) {
        let Some((key, row, at)) = self.shared.borrow_mut().asked_menu.take() else {
            return;
        };
        let Some(data) = self.list.model().row_of(&key) else {
            return;
        };
        if !matches!(
            data.kind,
            NavRowKind::Item { .. } | NavRowKind::Section { menu: true, .. }
        ) {
            return;
        }
        let Some(lift) = self.paint.borrow().lift_rect(data, row) else {
            return;
        };
        let at = if by_pointer {
            at
        } else {
            Point::new(lift.x0, lift.y1)
        };
        self.shared.borrow_mut().report_menu(&key, lift, at);
    }

    /// Answer what assistive technology asks of a row's own controls.
    fn perform_row_action(&mut self, target: &str, action: &crate::a11y::Action) -> Option<bool> {
        use crate::a11y::Action;
        if matches!(action, Action::Focus) {
            let Some(key) = control_row(target) else {
                // The row itself takes the keyboard: the list answers.
                self.focus_control(None);
                return None;
            };
            if self.list.model().row_of(key).is_none() {
                return Some(false);
            }
            self.list.focus_row(key.to_owned());
            self.focus_control(Some(target));
            return Some(true);
        }
        if let Some(key) = target.strip_prefix("section-action:") {
            return Some(
                matches!(action, Action::Activate)
                    && self.shared.borrow_mut().run_section_action(key),
            );
        }
        if let Some(rest) = target.strip_prefix("row-action:") {
            let (index, key) = rest.split_once(':')?;
            let index = index.parse::<usize>().ok()?;
            if !matches!(action, Action::Activate) || !self.list.model().shows_row_actions(key) {
                return Some(false);
            }
            let rect = self
                .list
                .row_action_rect(&key.to_owned(), index)
                .unwrap_or_default();
            self.press_item_action(key, index, rect);
            return Some(true);
        }
        if let Some(key) = target.strip_prefix("check:") {
            return Some(
                matches!(action, Action::Activate | Action::ToggleCheck) && self.toggle_check(key),
            );
        }
        let key = target.strip_prefix("key:")?.to_owned();
        let model = self.list.model();
        let (branch, open) = (model.is_branch(&key), model.is_open(&key));
        let (checkable, item) = (model.is_checkable(&key), model.is_item(&key));
        match action {
            Action::Expand | Action::Collapse if branch => {
                let expand = matches!(action, Action::Expand);
                if open != expand {
                    self.fold(&key, expand);
                }
                Some(true)
            }
            Action::ToggleCheck => Some(self.toggle_check(&key)),
            Action::Activate if checkable => Some(self.toggle_check(&key)),
            Action::Activate if !item => {
                if branch {
                    self.fold(&key, !open);
                }
                Some(branch)
            }
            _ => None,
        }
    }
}

impl Filterable for NavList {
    fn set_filter(&mut self, query: &str) {
        Self::set_filter(self, query);
    }

    fn filter_count(&self) -> Option<FilterCount> {
        let filterable: Vec<&NavSection> = self
            .sections
            .iter()
            .filter(|section| section.filterable)
            .collect();
        if filterable.is_empty() {
            return None;
        }
        let total = filterable
            .iter()
            .map(|section| count_items(&section.items))
            .sum();
        let shown = model::filtered(&self.sections, &self.filter)
            .iter()
            .filter(|section| section.filterable)
            .map(|section| count_items(&section.items))
            .sum();
        Some(FilterCount::new(shown, total))
    }
}

/// Lets the shell hosting a list learn that the person just chose a
/// destination, so a narrow window can move on to the content.
pub(crate) trait NavigationSource {
    fn take_navigation(&mut self) -> bool;
}

/// A rail whose width follows its destinations' words
/// ([`crate::chrome::rail_width_fitting`]).
pub(crate) trait RailExtent {
    /// How wide this rail is under `theme`, or `None` when it is no rail.
    fn fitted_rail_width(&self, theme: &crate::Theme) -> Option<f64>;
}

impl RailExtent for NavList {
    fn fitted_rail_width(&self, theme: &crate::Theme) -> Option<f64> {
        if !self.is_rail() {
            return None;
        }
        let items = all_items(&self.sections);
        Some(crate::text_render::with_layout_renderer(
            theme.text_scale,
            |text| {
                crate::chrome::rail_width_fitting(
                    theme,
                    text,
                    items.iter().map(|item| item.label.as_str()),
                )
            },
        ))
    }
}

/// A navigation sidebar whose width decides how it reads: narrow, a rail
/// of tiles (icon over a short caption); wide, a list (icon beside the
/// label). The shell hosting it as its sidebar asks how wide each form
/// needs to be, starts it at the list's width, and switches it as its width
/// crosses the threshold: a window narrow enough for the rail shows one.
pub(crate) trait AdaptiveNav {
    /// The width of the list as a rail: its longest word whole, up to the
    /// rail's cap ([`crate::chrome::sidebar_rail_width_fitting`]).
    fn rail_fit(&self, theme: &crate::Theme) -> f64;
    /// The width at which every label stands whole beside its icon
    /// ([`crate::chrome::nav_list_width_fitting`]): from here on it is a
    /// list.
    fn list_fit(&self, theme: &crate::Theme) -> f64;
    /// Paint the rows as a rail's tiles or as list rows. Returns whether
    /// that changed.
    fn set_rail(&mut self, rail: bool) -> bool;
}

/// Every destination of `sections`, nested ones included, in order.
fn all_items(sections: &[NavSection]) -> Vec<&NavItem> {
    fn walk<'a>(items: &'a [NavItem], out: &mut Vec<&'a NavItem>) {
        for item in items {
            out.push(item);
            walk(&item.children, out);
        }
    }
    let mut out = Vec::new();
    for section in sections {
        walk(&section.items, &mut out);
    }
    out
}

impl AdaptiveNav for NavList {
    fn rail_fit(&self, theme: &crate::Theme) -> f64 {
        let items = all_items(&self.sections);
        crate::text_render::with_layout_renderer(theme.text_scale, |text| {
            crate::chrome::sidebar_rail_width_fitting(
                theme,
                text,
                items.iter().map(|item| item.label.as_str()),
            )
        })
    }

    fn list_fit(&self, theme: &crate::Theme) -> f64 {
        let items = all_items(&self.sections);
        crate::text_render::with_layout_renderer(theme.text_scale, |text| {
            crate::chrome::nav_list_width_fitting(
                theme,
                text,
                items.iter().map(|item| item.label.as_str()),
            )
        })
    }

    fn set_rail(&mut self, rail: bool) -> bool {
        self.set_rail_rows(rail)
    }
}

/// The navigation sidebar `widget` is, when it is one ([`AdaptiveNav`]).
/// Only the sidebar itself counts, never a list nested inside a composite
/// sidebar: a rail can't hold the composite's other controls.
pub(crate) fn adaptive(widget: &dyn crate::widget::Widget) -> Option<&(dyn AdaptiveNav + 'static)> {
    crate::widget::capability::capability::<dyn AdaptiveNav>(widget)
}

/// As [`adaptive`], to switch it.
pub(crate) fn adaptive_mut(
    widget: &mut dyn crate::widget::Widget,
) -> Option<&mut (dyn AdaptiveNav + 'static)> {
    crate::widget::capability::capability_mut::<dyn AdaptiveNav>(widget)
}

/// The width of the first rail in `widget`'s subtree under `theme`
/// ([`RailExtent`]), or `None` where it holds none.
pub(crate) fn fitted_rail_width(
    widget: &dyn crate::widget::Widget,
    theme: &crate::Theme,
) -> Option<f64> {
    if let Some(rail) = crate::widget::capability::capability::<dyn RailExtent>(widget) {
        return rail.fitted_rail_width(theme);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| fitted_rail_width(child, theme))
}

impl NavigationSource for NavList {
    fn take_navigation(&mut self) -> bool {
        std::mem::take(&mut self.shared.borrow_mut().navigated)
    }
}

/// Whether anything in `widget`'s subtree reported a navigation since the
/// last ask (and forget it).
pub(crate) fn take_navigation(widget: &mut dyn crate::widget::Widget) -> bool {
    if let Some(source) = crate::widget::capability::capability_mut::<dyn NavigationSource>(widget)
    {
        return source.take_navigation();
    }
    let mut found = false;
    for child in widget.children_mut() {
        found |= take_navigation(child);
    }
    found
}

/// Whether `node` is a row or a row's button whose action is unavailable.
fn unavailable(node: &crate::a11y::AccessNode, shared: &NavShared) -> bool {
    let Some(target) = node.action_target.as_deref() else {
        return false;
    };
    if let Some(key) = target.strip_prefix("key:") {
        return key
            .strip_prefix("item:")
            .is_some_and(|id| shared.blocked(id))
            || shared
                .bound
                .unavailable_check(shared.checkable.contains(key));
    }
    if let Some(key) = target.strip_prefix("check:") {
        return shared
            .bound
            .unavailable_check(shared.checkable.contains(key));
    }
    shared.bound.unavailable_button(target, shared)
}

/// Whether any row or row button the list publishes is unavailable now.
fn any_unavailable(shared: &NavShared) -> bool {
    shared
        .selection_actions
        .values()
        .any(|action| !action.is_available())
        || shared.bound.any_unavailable()
}

/// Whether [`disable_unavailable`] would change anything under `node`.
fn has_unavailable(node: &crate::a11y::AccessNode, shared: &NavShared) -> bool {
    (!node.state.disabled && unavailable(node, shared))
        || node
            .children
            .iter()
            .any(|child| has_unavailable(child, shared))
}

/// Publish `node` and every control under it whose action is unavailable
/// as disabled.
fn disable_unavailable(node: &mut crate::a11y::AccessNode, shared: &NavShared) {
    node.state.disabled |= unavailable(node, shared);
    for child in &mut node.children {
        disable_unavailable(child, shared);
    }
}

// Not the blanket `Decorator`: apps box a navigation list, and its Widget
// methods (with the list engine they forward to) are compiled once, here
// (`concrete_decorator_widget!` in decorator.rs).
impl crate::widget::decorator::concrete::Decorator for NavList {
    type Inner = VirtualList<NavModel>;

    fn inner(&self) -> &Self::Inner {
        &self.list
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.list
    }

    fn provide<'a>(&'a self, request: &mut crate::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn crate::widget::controls::ActionControls>() {
            request.provide::<dyn crate::widget::controls::ActionControls>(self);
            return;
        }
        if request.wants::<dyn crate::resources::PublishesResources>() {
            request.provide::<dyn crate::resources::PublishesResources>(self);
            return;
        }
        if request.wants::<dyn crate::widget::ResourceOperations>() {
            request.provide::<dyn crate::widget::ResourceOperations>(self);
            return;
        }
        if request.wants::<dyn RailExtent>() {
            request.provide::<dyn RailExtent>(self);
            return;
        }
        if request.wants::<dyn AdaptiveNav>() {
            request.provide::<dyn AdaptiveNav>(self);
            return;
        }
        crate::widget::Widget::provide(&*self.list, request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut crate::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn crate::widget::controls::ActionControls>() {
            request.provide::<dyn crate::widget::controls::ActionControls>(self);
            return;
        }
        if request.wants::<dyn crate::widget::ResourceOperations>() {
            request.provide::<dyn crate::widget::ResourceOperations>(self);
            return;
        }
        if request.wants::<dyn NavigationSource>() {
            request.provide::<dyn NavigationSource>(self);
            return;
        }
        if request.wants::<dyn AdaptiveNav>() {
            request.provide::<dyn AdaptiveNav>(self);
            return;
        }
        if request.wants::<dyn Filterable>()
            && self.sections.iter().any(|section| section.filterable)
        {
            request.provide::<dyn Filterable>(self);
            return;
        }
        crate::widget::Widget::provide_mut(&mut *self.list, request);
    }

    fn build_layout(
        &mut self,
        taffy: &mut crate::taffy::TaffyTree<()>,
        theme: &crate::Theme,
        node_map: &mut HashMap<crate::taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> crate::taffy::NodeId {
        // The rows stand at the theme's heights: a new theme builds them anew.
        if self.paint.borrow().theme != *theme {
            self.paint.borrow_mut().theme = theme.clone();
            self.list.update_model(|_| ());
        }
        let node = crate::widget::Widget::build_layout(&mut *self.list, taffy, theme, node_map);
        // The published owner is NavList, so host lookup and synthetic
        // accessibility routing must identify this same retained wrapper.
        node_map.insert(node, std::ptr::from_mut::<dyn crate::widget::Widget>(self));
        if let Some(feed) = &self.feed {
            feed.publish_node(node);
        }
        node
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let next = self
            .shared
            .borrow_mut()
            .binding
            .as_mut()
            .and_then(LocalBinding::poll);
        if let Some(next) = next {
            self.set_selected(next.as_deref());
        }
        if let Some(filter) = self.filter_binding.as_mut().and_then(LocalBinding::poll) {
            self.set_filter(&filter);
        }
        self.pull();
        self.take_asks();
        crate::widget::Widget::sync(&mut *self.list, state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let pulled = self.pull() | self.take_asks();
        crate::widget::Widget::update(&mut *self.list, state, path) || pulled
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.hover(Some(ctx.logical(position)));
            }
            WindowEvent::CursorLeft { .. } => self.hover(None),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.list.renaming().is_none() => {
                let point = self
                    .paint
                    .borrow()
                    .hovered
                    .as_ref()
                    .map(|(_, point)| *point);
                self.focus_control(None);
                if let Some(point) = point
                    && self.press_control(point)
                {
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && self.list.renaming().is_none()
                    && crate::widget::Widget::is_focused(&*self.list)
                    && self.key(&event.logical_key, ctx.modifiers.state()) =>
            {
                *ctx.handled = true;
                return;
            }
            _ => {}
        }
        crate::widget::Widget::handle_event(&mut *self.list, event, ctx);
        self.take_menu(matches!(event, WindowEvent::MouseInput { .. }));
    }

    /// The list carries out its own structural changes in place: rows a
    /// scroll or a new extent mounts, and rows it lets go of, are mounted
    /// under the list's node. Pass its change on, so a mount settles the
    /// frame instead of rebuilding the window's tree.
    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        crate::widget::Widget::take_structure_change(&mut *self.list)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if let Some(done) = self.perform_row_action(target, action) {
            self.take_menu(false);
            return done;
        }
        let done = crate::widget::Widget::perform_semantic_action(
            &mut *self.list,
            target,
            action,
            state,
            path,
        );
        self.take_menu(false);
        done
    }

    fn apply_layout(
        &mut self,
        taffy: &crate::taffy::TaffyTree<()>,
        node: crate::taffy::NodeId,
        origin: Point,
    ) {
        crate::widget::Widget::apply_layout(&mut *self.list, taffy, node, origin);
        // A drag staged before the rows stood placed shows once they do.
        self.apply_staged_drag();
    }

    fn needs_redraw(&self) -> bool {
        self.sync_paint();
        crate::widget::Widget::needs_redraw(&*self.list)
    }

    fn set_focus(&mut self, focused: bool) {
        if !focused {
            self.focus_control(None);
        }
        crate::widget::Widget::set_focus(&mut *self.list, focused);
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        // A row names its elided label and its actions itself.
        let mut found = None;
        crate::widget::Widget::for_each_child(&*self.list, &mut |row| {
            if found.is_none() {
                found = row.tooltip_target_at(point);
            }
        });
        found
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        crate::widget::decorator::concrete::Decorator::accessibility_shared(self)
            .map(crate::a11y::SharedAccessNode::into_owned)
    }

    /// The list's rows as the engine keeps them between walks, each the
    /// very node of the last walk while nothing it is built from changed: a
    /// keystroke in the pane beside the list builds none of them. A row
    /// whose destination's selection action is unavailable is published
    /// disabled, as a copy of the kept row; availability changes without the
    /// rows changing, so those few are copied on every walk.
    fn accessibility_shared(&self) -> Option<crate::a11y::SharedAccessNode> {
        let mut access = crate::widget::Widget::accessibility_shared(&*self.list)?;
        access.node.role = crate::a11y::Role::Tree;
        if let Some(key) = &self.semantic_key {
            access.node.semantic_key = Some(key.clone());
        }
        let shared = self.shared.borrow();
        if any_unavailable(&shared) {
            disable_unavailable(&mut access.node, &shared);
            for row in &mut access.children {
                if has_unavailable(row, &shared) {
                    disable_unavailable(std::sync::Arc::make_mut(row), &shared);
                }
            }
        }
        Some(access)
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::Theme,
        presence: &crate::PresenceCtx,
    ) {
        let width = crate::widget::Widget::get_rect(&*self.list).width();
        self.sync_paint();
        let rail = {
            let mut paint = self.paint.borrow_mut();
            let resized = (paint.width - width).abs() > f64::EPSILON;
            paint.width = width;
            resized && paint.rail
        };
        if rail {
            // A rail tile's caption wraps in the rail's width.
            self.list.update_model(|_| ());
        }
        crate::widget::Widget::draw(&mut *self.list, scene, transform, text, theme, presence);
        if let Some(feed) = &self.feed {
            let model = self.list.model();
            let rows = (0..model.len())
                .filter_map(|index| model.row(index))
                .filter_map(|row| {
                    let rect = self.list.row_rect(&row.key)?;
                    Some((row.kind.id().to_owned(), rect))
                })
                .collect();
            feed.publish_rows(crate::widget::Widget::get_rect(&*self.list), rows);
        }
    }

    /// Never hidden: answered without building the rows.
    fn accessibility_hidden(&self) -> bool {
        crate::widget::Widget::accessibility_hidden(&*self.list)
    }

    /// The list's own node, without building its rows.
    fn accessibility_root(&self) -> Option<crate::a11y::AccessNode> {
        let mut root = crate::widget::Widget::accessibility_root(&*self.list)?;
        root.role = crate::a11y::Role::Tree;
        if let Some(key) = &self.semantic_key {
            root.semantic_key = Some(key.clone());
        }
        disable_unavailable(&mut root, &self.shared.borrow());
        Some(root)
    }

    fn name(&self) -> &'static str {
        "NavList"
    }
}

mod bound;
mod feed;
mod model;
mod row;
pub use feed::NavFeed;

#[cfg(test)]
mod tests;

impl crate::widget::controls::ActionControls for NavList {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        let mut shared = self.shared.borrow_mut();
        for action in shared.selection_actions.values_mut() {
            let _ = action.install_custody(domain, attachment);
        }
        for action in shared.section_add_actions.values_mut() {
            let _ = action.install_custody(domain, attachment);
        }
        shared.bound.install_custody(domain, attachment);
        if let Some(view) = shared.view.as_mut() {
            view.choice.install_custody(domain, attachment);
        }
        drop(shared);
        crate::widget::controls::install_control_custody(&mut *self.list, domain, attachment);
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        let taken = {
            let mut shared = self.shared.borrow_mut();
            let selection = shared
                .selection_actions
                .values_mut()
                .find_map(crate::widget::ActionEmitter::take);
            selection
                .or_else(|| {
                    shared
                        .section_add_actions
                        .values_mut()
                        .find_map(crate::widget::ActionEmitter::take)
                })
                .or_else(|| shared.bound.take())
                .or_else(|| shared.view.as_mut().and_then(|view| view.choice.take()))
        };
        taken.or_else(|| crate::widget::controls::take_control_invocation(&mut *self.list))
    }

    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let shared = self.shared.borrow();
        let model = self.list.model();
        let mut bindings = crate::widget::controls::control_action_bindings(&*self.list);
        // The list exports only the rows it builds. Keep its control
        // inventory the same set, so a row a screen reader reaches is one a
        // control search finds, including after filtering or scrolling.
        for row in self
            .list
            .mounted_rows()
            .filter_map(|index| model.row(index))
        {
            let key = &row.key;
            for bound in shared.bound.row_controls(key, &shared, model) {
                bindings.retain(|binding| binding.control_id != bound.control_id);
                bindings.push(bound);
            }
            if shared.checkable.contains(key)
                && let Some(check) = shared.bound.check_control(key)
            {
                bindings.retain(|binding| binding.control_id != check.control_id);
                bindings.push(check);
                continue;
            }
            let Some(id) = key.strip_prefix("item:") else {
                continue;
            };
            let action = shared.selection_actions.get(id).map_or_else(
                || {
                    shared
                        .view
                        .as_ref()
                        .map(|view| view.choice.metadata(format!("key:{key}")))
                },
                |action| Some(action.metadata(format!("key:{key}"))),
            );
            if let Some(action) = action {
                bindings.retain(|binding| binding.control_id != action.control_id);
                bindings.push(action);
            }
        }
        for row in self
            .list
            .mounted_rows()
            .filter_map(|index| model.row(index))
        {
            let Some(add) = shared.section_add_actions.get(&row.key) else {
                continue;
            };
            let add = add.metadata(format!("section-action:{}", row.key));
            bindings.retain(|binding| binding.control_id != add.control_id);
            bindings.push(add);
        }
        bindings
    }

    /// Whether `Self::control_action_bindings` holds an action, answered
    /// without building the rows' semantics it also lists: the host asks it
    /// on every frame. The actions are the mounted destinations' selection
    /// actions and whatever the list's own rows bind.
    fn has_action_control_bindings(&self) -> bool {
        let shared = self.shared.borrow();
        let model = self.list.model();
        let rows = || {
            self.list
                .mounted_rows()
                .filter_map(|index| model.row(index))
        };
        let selection = if shared.view.is_some() {
            rows().any(|row| model.is_item(&row.key))
        } else {
            !shared.selection_actions.is_empty()
                && rows().any(|row| {
                    row.key
                        .strip_prefix("item:")
                        .is_some_and(|id| shared.selection_actions.contains_key(id))
                })
        };
        let adds = !shared.section_add_actions.is_empty()
            && rows().any(|row| shared.section_add_actions.contains_key(&row.key));
        let bound = shared.bound.is_bound()
            && rows().any(|row| {
                (shared.checkable.contains(&row.key)
                    && shared.bound.check_control(&row.key).is_some())
                    || !shared
                        .bound
                        .row_controls(&row.key, &shared, model)
                        .is_empty()
            });
        selection
            || adds
            || bound
            || crate::widget::controls::has_action_control_bindings(&*self.list)
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &HashSet<tilo_actions::ActionAddress>,
    ) {
        let mut shared = self.shared.borrow_mut();
        for action in shared.selection_actions.values_mut() {
            action.refresh_availability(allowed);
        }
        for action in shared.section_add_actions.values_mut() {
            action.refresh_availability(allowed);
        }
        shared.bound.refresh_availability(allowed);
        if let Some(view) = shared.view.as_mut() {
            view.choice.refresh_availability(allowed);
        }
        drop(shared);
        crate::widget::controls::refresh_control_action_availability(&mut *self.list, allowed);
    }
}

mod component;
pub use component::{NavEntry, NavItemProps, NavListProps, NavSectionProps};

impl crate::resources::PublishesResources for NavList {
    fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        self.view_resource().into_iter().collect()
    }
}

impl crate::widget::ResourceOperations for NavList {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        if self.shared.borrow().view.is_some() {
            &[crate::widget::view_choice::CHOICE_SELECT]
        } else {
            &[]
        }
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        if let Some(started) = self.start_view(op)? {
            return Ok(Some(started));
        }
        crate::widget::start_in_children(self, op, doc, path)
    }
}
