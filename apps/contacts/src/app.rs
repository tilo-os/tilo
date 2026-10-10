//! Root widget for the Contacts app.
//!
//! The window is one [`NavShell`]: the address book's groups are the
//! sidebar, the people in the chosen group are the overview list, and the
//! selected person (or the form editing them) is the content. The headers
//! carry the commands: search, more and new above the list; favorite, edit
//! and delete above the person.
//!
//! The book is the [`Contacts`] record; the in-memory [`ContactStore`] is
//! the working mirror the window reads. Every change to the book is one of
//! its verbs or field edits, submitted through the window's dispatcher: the
//! same door Ctrl+K and agents take.

use crate::resource::Contacts;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use ui::framework::{Dispatcher, Jobs};
use ui::tilo_resource::Invoke;
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use automerge::ObjId;
use taffy::{Dimension, NodeId, Position, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use ui::LocalState;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    Button, ButtonRole, ContextMenu, ControlSize, Dropdown, Element, MenuItem, NavItem, NavList,
    NavPane, NavSection, NavShell, TaffyTreeExt, Widget,
};

use crate::form::EditForm;
use crate::model::{Contact, ContactId, ContactStore, Labeled, SortKey, group_names_equal};
use crate::vcard::VCardVersion;
use crate::view::{Bridge, ContactList, EmptyPane, ListEntry, Request, Shared, queue};

mod dialogs;
mod effects;
mod files;

use dialogs::{DialogKey, ExportDraft};

/// Private presentation token; it is never written to the shared document.
const FAVORITES_FILTER: &str = "\u{0}contacts:favorites";

/// How long an action toast ("Saved", "Contact created", …) stays on screen
/// before it fades away on its own. The host loop is kept awake (via
/// [`Widget::needs_animation_frame`]) only while a toast is up, so the frame
/// where this elapses clears it and the loop returns to idle.
const TOAST_LIFETIME: Duration = Duration::from_millis(2600);

/// The sidebar destinations that are not groups.
const ALL_DESTINATION: &str = "all";
const FAVORITES_DESTINATION: &str = "favorites";
const GROUP_DESTINATION: &str = "group:";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    View,
    Edit,
}

#[derive(Debug, Clone)]
struct ImportConflict {
    token: uuid::Uuid,
    existing: ContactId,
    incoming: Contact,
}

struct ParsedImport {
    contacts: Vec<Contact>,
    errors: usize,
}

/// Private, review-only state for a potential duplicate. It is rebuilt from
/// the current collaborative contacts whenever opened; no tentative choices
/// leak into the shared document until the explicit Merge button invokes
/// the book's merge.
struct DuplicateReview {
    survivor: Contact,
    source: Contact,
    reasons: Vec<crate::actions::DuplicateReason>,
    /// Each conflicting field and whose value it keeps: `"current"` (the
    /// survivor's) or `"other"` (the merged-away contact's).
    choices: Vec<(crate::actions::MergeField, LocalState<String>)>,
}

/// One command of a contact's action menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ContextAction {
    Edit,
    Favorite,
    Export,
    Delete,
}

impl ContextAction {
    /// The menu label; the favorite row says what choosing it does.
    fn label(self, is_favorite: bool) -> &'static str {
        match self {
            ContextAction::Edit => "Edit",
            ContextAction::Favorite if is_favorite => "Remove from favorites",
            ContextAction::Favorite => "Add to favorites",
            ContextAction::Export => "Export contact",
            ContextAction::Delete => "Delete contact",
        }
    }
}

/// One command of the overview's More menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MoreAction {
    Import,
    Export,
    ReviewDuplicate,
}

/// Which menu is open, and the rows it shows in order (`None` for a
/// separator).
enum OpenMenu {
    Contact(ContactId, Vec<Option<ContextAction>>),
}

/// A menu waiting for its anchor: the row or the button it opens from is
/// placed by the next paint.
#[derive(Clone, Copy, PartialEq, Debug)]
enum PendingMenu {
    Contact(ContactId),
}

/// What the content column shows.
#[derive(Clone, PartialEq, Debug)]
enum ContentKey {
    FirstRun,
    Nothing,
    Contact(Box<Contact>),
    Edit { epoch: u64, calendar: bool },
}

/// What the content column's header holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HeaderKey {
    Empty,
    View { favorite: bool },
    Edit,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent frame bookkeeping (list, structure, layout and paint staleness) and the seeding flags; none is a state machine"
)]
pub struct ContactsApp {
    store: ContactStore,
    selected: Option<ContactId>,
    mode: Mode,
    form: Option<EditForm>,
    form_epoch: u64,
    sort: SortKey,
    group_filter: Option<String>,
    toast: Option<(String, Instant)>,
    duplicate_review: Option<DuplicateReview>,
    duplicate_candidate: Option<crate::actions::DuplicateCandidate>,
    /// The export whose format is being chosen.
    export: Option<ExportDraft>,
    /// When switching to another contact would lose unsaved edits, the
    /// target id parks here while "Save changes?" is asked.
    pending_switch: Option<ContactId>,

    // ---- the window ----
    rect: Rect,
    shell: NavShell,
    shared: Shared,
    /// The sidebar's selection: a destination id.
    destination: LocalState<Option<String>>,
    /// The list's order, as the sort control shows it.
    sort_choice: LocalState<String>,
    /// Whether the sort control stands under the overview's header: only
    /// while the list has rows to order.
    sort_shown: bool,
    overview_controls_shown: Option<(bool, Option<FileWork>)>,
    /// The More menu's rows and whether each is enabled, as last handed to
    /// its button.
    more_shown: Vec<(MoreAction, bool)>,
    /// The More command chosen in the menu, run after the event.
    more_pick: std::sync::Arc<std::sync::Mutex<Option<MoreAction>>>,
    /// The sidebar rows last handed to the shell.
    sidebar_shown: Vec<(String, String, u32)>,
    content_shown: Option<ContentKey>,
    header_shown: Option<HeaderKey>,
    dialog: Option<(DialogKey, Element)>,
    menu: ContextMenu,
    open_menu: Option<OpenMenu>,
    pending_menu: Option<PendingMenu>,
    /// The open menu stays on the row or button it opened from while the
    /// window moves under it.
    menu_follows: Option<PendingMenu>,
    /// A screen to bring to the front of a narrow window once laid out.
    show_pane: Option<NavPane>,
    /// The last website opened from a contact.
    opened_url: Option<String>,
    /// Where the person's page is scrolled to, kept while the same person
    /// shows.
    detail_scroll: (Option<ContactId>, std::rc::Rc<std::cell::Cell<(f32, f32)>>),
    last_mouse: Point,
    list_stale: bool,
    structure_dirty: bool,
    layout_dirty: bool,
    dirty: bool,
    seeded: bool,
    scenario: Option<String>,
    file_work: Rc<Cell<Option<FileWork>>>,
    opened_cards: Option<String>,
    /// Pending untrusted UID collisions. These never enter the document until
    /// the user explicitly chooses a safe resolution.
    import_conflicts: Vec<ImportConflict>,
    pending_conflict_resolution: Option<(String, bool)>,

    dispatcher: Option<Dispatcher<Contacts>>,
    /// The book as it was presented last.
    pub(crate) presented: Option<Rc<Contacts>>,
    pub(crate) jobs: Option<Jobs<Contacts>>,
    pub(crate) account_editor: Option<crate::account_editor::Editor>,
    account_pane: Option<Element>,
    pub(crate) identity_directory:
        Option<ui::framework::PersonRecord<tilo_contact_directory::ContactDirectory>>,
    unconfirmed_save: Option<EditForm>,
}

/// The overview header's commands: search, more, and new contact.
fn overview_actions(shared: &Shared, more: Vec<MenuItem>) -> Vec<Element> {
    vec![
        Box::new(ui::search::search_button(
            ui::search::SearchRequest::this_app(),
        )),
        Box::new(
            Button::icon_only(MORE_BUTTON, Icon::MoreHorizontal, "More actions").with_menu(more),
        ),
        Box::new(
            Button::icon_only("contacts.new", Icon::Plus, "New contact")
                .on_view(queue(shared, Request::NewContact)),
        ),
    ]
}

/// The overview header's More button, which opens the [`MoreAction`] menu.
const MORE_BUTTON: &str = "contacts.more";

/// The list's orders: the control's value, its label, and the key.
const SORTS: &[(&str, &str, SortKey)] = &[
    ("name", "Sort by last name", SortKey::Name),
    ("organization", "Sort by organization", SortKey::Org),
];

fn sort_value(sort: SortKey) -> &'static str {
    SORTS
        .iter()
        .find(|(_, _, key)| *key == sort)
        .map_or("name", |(value, ..)| value)
}

/// The list's order, which the shell stands under the overview's header.
fn sort_control(shared: &Shared, sort_choice: &LocalState<String>) -> Element {
    let labels: Vec<String> = SORTS
        .iter()
        .map(|(_, label, _)| (*label).to_owned())
        .collect();
    let chosen = std::rc::Rc::clone(shared);
    let mut sort = Dropdown::from_state(labels, sort_choice)
        .with_id("contacts.sort")
        .with_accessible_name("Sort contacts")
        .on_change(move |value| {
            if let Some((.., key)) = SORTS.iter().find(|(v, ..)| *v == value) {
                chosen.borrow_mut().push(Request::Sort(*key));
            }
        });
    sort.set_value_options(
        SORTS
            .iter()
            .map(|(value, label, _)| ((*value).to_owned(), (*label).to_owned()))
            .collect(),
    );
    Box::new(
        ui::widget::Row::new()
            .with_id("contacts.sort-row")
            .with_padding_rhythm(None, Some(ui::tokens::Rhythm::Related))
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_align_items(taffy::AlignItems::Center)
            .push(Box::new(sort.control_size(ControlSize::Regular))),
    )
}

/// The first widget in `root`'s tree with `id`, depth first, to change.
fn find_mut<'w>(root: &'w mut dyn Widget, id: &str) -> Option<&'w mut dyn Widget> {
    if root.test_id() == Some(id) {
        return Some(root);
    }
    root.children_mut()
        .into_iter()
        .find_map(|child| find_mut(child, id))
}

impl ContactsApp {
    /// Submit `invoke` through the door, after the event: a destructive
    /// verb is confirmed by the host there, and a refusal reaches
    /// [`Self::announce_refusal`].
    fn try_perform_action(
        &self,
        _doc: &mut ui::widget::Edits<'_>,
        invoke: Invoke<Contacts>,
    ) -> Result<(), ui::tilo_actions::ActionError> {
        self.submit(invoke)
            .map_err(ui::tilo_actions::ActionError::Refused)
    }
    fn reconcile_refused_action(
        &mut self,
        _doc: &ui::widget::Edits<'_>,
        operation: &str,
        error: &ui::tilo_actions::ActionError,
    ) {
        // Nothing changed: show the book as it stands again, in place of
        // what the window assumed.
        if let Some(book) = self.presented.clone() {
            self.present_book(&book);
        }
        self.show_toast(format!("Couldn't {operation}: {error}"));
    }

    pub(crate) fn import_conflict_rows(&self) -> Vec<ui::framework::LiveRow> {
        self.import_conflicts
            .iter()
            .map(|conflict| {
                crate::actions::import_conflict_row(
                    &conflict.token.to_string(),
                    &conflict.incoming.display_name(),
                )
            })
            .collect()
    }

    /// Show the contact a search hit or an agent's `open` names; a contact
    /// that is gone is refused.
    pub(crate) fn opener(
        &self,
    ) -> impl FnMut(&ui::tilo_resource::Id) -> Result<(), ui::tilo_resource::Refusal> + 'static
    {
        let shared = Rc::clone(&self.shared);
        move |id| {
            let contact = id
                .as_str()
                .parse::<ContactId>()
                .ok()
                .filter(|contact| shared.borrow().known.contains(contact))
                .ok_or_else(|| ui::tilo_resource::Refusal::new("no such contact"))?;
            shared.borrow_mut().push(Request::Reveal(contact));
            Ok(())
        }
    }

    pub(crate) fn attach_dispatcher(&mut self, dispatcher: Dispatcher<Contacts>) {
        self.dispatcher = Some(dispatcher);
    }

    pub(crate) fn present_book(&mut self, state: &Contacts) {
        // A refused save re-presents the book too; keep its form for the
        // announcement that follows.
        if !self
            .dispatcher
            .as_ref()
            .is_some_and(|dispatcher| dispatcher.refused().with(Option::is_some))
        {
            self.unconfirmed_save = None;
        }
        self.store.contacts.clone_from(&state.contacts);
        self.shared.borrow_mut().known = state.contacts.iter().map(|contact| contact.id).collect();
        self.duplicate_candidate = crate::actions::duplicate_candidates(&self.store.contacts, 1)
            .into_iter()
            .next();
        if !self.seeded && !self.store.contacts.is_empty() {
            self.seeded = true;
            if self.selected.is_none() {
                self.selected = self.visible_ids().first().copied();
            }
        }
        // A merge the person confirmed has landed: its review is done.
        if let Some(review) = &self.duplicate_review
            && self.store.get(review.source.id).is_none()
        {
            self.selected = Some(review.survivor.id);
            self.duplicate_review = None;
        }
        if let Some(sel) = self.selected
            && self.store.get(sel).is_none()
        {
            self.selected = self.visible_ids().first().copied();
        }
        if let Some(form) = &mut self.form {
            if let Some(canonical) = self.store.get(form.draft.id) {
                if form.reconcile(canonical) {
                    self.form_epoch = self.form_epoch.wrapping_add(1);
                }
            } else if !form.is_new {
                if form.has_local_edits() {
                    self.show_toast(
                        "This contact no longer exists. Your unsaved edits are retained.",
                    );
                } else {
                    self.cancel_edit();
                }
            }
        }
        crate::priority_contacts::publish_if_changed(&self.store);
        self.layout_dirty = true;
        self.list_stale = true;
        self.dirty = true;
    }

    fn announce_refusal(&mut self) {
        let Some(dispatcher) = &self.dispatcher else {
            return;
        };
        let refused = dispatcher.refused();
        let Some(refusal) = refused.with(Clone::clone) else {
            return;
        };
        refused.set(None);
        let Some(form) = self.unconfirmed_save.take() else {
            self.show_toast(format!(
                "Couldn't change the address book: {}",
                refusal.reason
            ));
            return;
        };
        // The draft is not a durable authority: reopen it, rebased on the
        // record as it now stands, so the person's text is not lost.
        let base = self
            .store
            .get(form.draft.id)
            .cloned()
            .unwrap_or_else(|| form.base.clone());
        self.form = Some(EditForm::with_base(base, form.draft, form.is_new));
        self.mode = Mode::Edit;
        self.layout_dirty = true;
        self.list_stale = true;
        self.show_toast(format!("Couldn't save contact: {}", refusal.reason));
    }

    fn submit(&self, invoke: Invoke<Contacts>) -> Result<(), String> {
        let Some(dispatcher) = &self.dispatcher else {
            return Err("the address book is not open".into());
        };
        if dispatcher.read_only() {
            return Err("you can only view this address book".into());
        }
        dispatcher.submit(invoke);
        Ok(())
    }

    /// The live app. Never fabricates sample content.
    ///
    /// Nothing here touches the disk: the address book lives in the shared
    /// Automerge document, which the launcher opens through `ui::persist`
    /// and writes back on autosave.
    pub fn new() -> Self {
        Self::configured()
    }

    pub(crate) fn with_accounts(
        mut self,
        editor: crate::account_editor::Editor,
        pane: Element,
    ) -> Self {
        self.shell = self
            .shell
            .inspector_header(
                crate::account_editor::CATALOG.t("contacts-accounts"),
                Vec::new(),
            )
            .with_inspector_hidden()
            .with_inspector_state(&editor.visible);
        self.account_editor = Some(editor);
        self.account_pane = Some(pane);
        self
    }

    /// The hermetic `--shots` / test app: seeds the varied sample address book
    /// and never touches the user's disk, so a headless frame is populated and
    /// tests are deterministic.
    pub fn sample() -> Self {
        Self::configured()
    }

    fn configured() -> Self {
        let store = ContactStore::new();
        let shared: Shared = std::rc::Rc::new(std::cell::RefCell::new(Bridge::default()));
        let destination = LocalState::new(Some(ALL_DESTINATION.to_owned()));
        let sort_choice = LocalState::new(sort_value(SortKey::Name).to_owned());
        let mut shell = NavShell::new()
            .with_id("contacts.shell")
            .sidebar(Box::new(Self::sidebar_list(&[], &destination)))
            .overview(Box::new(ContactList::new(std::rc::Rc::clone(&shared))))
            .content(Box::new(EmptyPane::new(
                std::rc::Rc::clone(&shared),
                "No contact selected",
                None,
            )))
            .overview_header("All contacts", overview_actions(&shared, Vec::new()));
        shell.show(NavPane::Overview);
        Self {
            store,
            selected: None,
            mode: Mode::View,
            form: None,
            form_epoch: 0,
            sort: SortKey::Name,
            group_filter: None,
            toast: None,
            duplicate_review: None,
            duplicate_candidate: None,
            export: None,
            pending_switch: None,
            rect: Rect::ZERO,
            shell,
            shared,
            destination,
            sort_choice,
            sort_shown: false,
            overview_controls_shown: None,
            more_shown: Vec::new(),
            more_pick: std::sync::Arc::default(),
            sidebar_shown: Vec::new(),
            content_shown: None,
            header_shown: None,
            dialog: None,
            menu: ContextMenu::new(),
            open_menu: None,
            pending_menu: None,
            menu_follows: None,
            show_pane: None,
            opened_url: None,
            detail_scroll: (None, std::rc::Rc::default()),
            last_mouse: Point::ZERO,
            list_stale: true,
            structure_dirty: false,
            layout_dirty: true,
            dirty: true,
            seeded: false,
            scenario: None,
            file_work: Rc::default(),
            opened_cards: None,
            dispatcher: None,
            presented: None,
            jobs: None,
            account_editor: None,
            account_pane: None,
            identity_directory: None,
            unconfirmed_save: None,
            import_conflicts: Vec::new(),
            pending_conflict_resolution: None,
        }
    }

    /// Stage a capture scenario: it is applied once the sample book is
    /// seeded, on the first `sync`.
    pub fn stage_scenario(&mut self, name: Option<&str>) {
        self.scenario = name.map(str::to_owned);
    }

    /// Bring the selected person to the front of a narrow window.
    pub fn shots_show_detail(&mut self) {
        self.show_pane = Some(NavPane::Content);
    }

    /// Stage a hosted capture's scenario once its sample book has been
    /// presented: open on the book's first, richly filled record, then pose
    /// `scenario`.
    pub(crate) fn stage_capture(&mut self, scenario: Option<&str>) {
        self.selected = self.store.contacts.first().map(|contact| contact.id);
        self.list_stale = true;
        match scenario {
            Some("detail") => self.shots_show_detail(),
            Some(name) => self.stage(name),
            None => {}
        }
        self.dirty = true;
    }

    fn stage(&mut self, scenario: &str) {
        match scenario {
            "new" => {
                self.begin_new_contact();
                self.show_pane = Some(NavPane::Content);
            }
            "edit" | "calendar" => {
                if let Some(contact) = self.selected.and_then(|id| self.store.get(id)).cloned() {
                    self.begin_edit(contact, false);
                    if scenario == "calendar"
                        && let Some(form) = &mut self.form
                    {
                        form.calendar_open = true;
                        // The birthday sits near the end of the form.
                        form.scroll.set((0.0, f32::MAX));
                    }
                }
                self.show_pane = Some(NavPane::Content);
            }
            "export" => self.open_export_dialog(),
            "unsaved" => {
                if let Some(contact) = self.selected.and_then(|id| self.store.get(id)).cloned() {
                    self.begin_edit(contact, false);
                    if let Some(form) = &self.form {
                        let title = format!("{} and Poet", form.value(crate::form::Slot::Title));
                        form.set_value(crate::form::Slot::Title, &title);
                    }
                    self.pending_switch = self
                        .visible_ids()
                        .into_iter()
                        .find(|id| Some(*id) != self.selected);
                }
            }
            "groups" => {
                if let Some(group) = self.store.all_groups().into_iter().next() {
                    self.set_group_filter(Some(group));
                }
            }
            "favorites" => self.set_group_filter(Some(FAVORITES_FILTER.into())),
            "context" => self.pending_menu = self.selected.map(PendingMenu::Contact),
            "more" => {
                self.publish_more();
                if let Some(button) = self.more_button() {
                    button.open(true);
                }
            }
            "import-conflict" => {
                if let Some(existing) = self.selected.and_then(|id| self.store.get(id)).cloned() {
                    let mut incoming = existing.clone();
                    incoming.notes = "Met at the Royal Society; prefers letters to calls.".into();
                    self.import_conflicts = vec![ImportConflict {
                        token: uuid::Uuid::new_v4(),
                        existing: existing.id,
                        incoming,
                    }];
                }
            }
            "duplicate-review" => self.stage_duplicate_review(),
            "empty" => {
                self.store = ContactStore::new();
                self.selected = None;
                self.mode = Mode::View;
                self.form = None;
                self.list_stale = true;
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Show the canonical duplicate pair staged through the resource's actions.
    fn stage_duplicate_review(&mut self) {
        let Some(candidate) = self.duplicate_candidate.clone() else {
            return;
        };
        let _ = self.open_duplicate_review(candidate.survivor_id, candidate.source_id);
    }

    /// Ids currently visible after the group filter, in sort order.
    fn visible_ids(&self) -> Vec<ContactId> {
        if self.group_filter.as_deref() == Some(FAVORITES_FILTER) {
            let mut contacts = self
                .store
                .contacts
                .iter()
                .filter(|contact| contact.favorite)
                .collect::<Vec<_>>();
            contacts.sort_by(|left, right| crate::model::sort_cmp(left, right, self.sort));
            return contacts.into_iter().map(|contact| contact.id).collect();
        }
        self.store
            .filtered_sorted("", self.group_filter.as_deref(), self.sort)
    }

    /// The group the list shows, by name, or `None` for every contact.
    fn destination_label(&self) -> Option<String> {
        match self.group_filter.as_deref() {
            None => None,
            Some(FAVORITES_FILTER) => Some("Favorites".to_owned()),
            Some(group) => Some(group.to_owned()),
        }
    }

    fn destination_id(&self) -> String {
        match self.group_filter.as_deref() {
            None => ALL_DESTINATION.to_owned(),
            Some(FAVORITES_FILTER) => FAVORITES_DESTINATION.to_owned(),
            Some(group) => format!("{GROUP_DESTINATION}{group}"),
        }
    }

    fn show_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), ui::time::now()));
        self.dirty = true;
    }

    /// `true` when a toast is showing and has outlived [`TOAST_LIFETIME`].
    fn toast_expired(&self) -> bool {
        self.toast
            .as_ref()
            .is_some_and(|(_, shown)| shown.elapsed() >= TOAST_LIFETIME)
    }

    fn set_group_filter(&mut self, group: Option<String>) {
        self.group_filter = group;
        if let Some(selected) = self.selected
            && !self.visible_ids().contains(&selected)
        {
            self.selected = self.visible_ids().first().copied();
        }
        let id = self.destination_id();
        if self
            .destination
            .with(|current| current.as_deref() != Some(id.as_str()))
        {
            self.destination.set(Some(id));
        }
        self.list_stale = true;
        self.dirty = true;
    }

    /// Follow the sidebar: the person chose a destination.
    fn poll_destination(&mut self) {
        let Some(chosen) = self.destination.with(Clone::clone) else {
            return;
        };
        if chosen == self.destination_id() {
            return;
        }
        let group = if chosen == FAVORITES_DESTINATION {
            Some(FAVORITES_FILTER.to_owned())
        } else {
            chosen.strip_prefix(GROUP_DESTINATION).map(str::to_owned)
        };
        self.set_group_filter(group);
    }

    fn begin_edit(&mut self, draft: Contact, is_new: bool) {
        self.form = Some(EditForm::new(draft, is_new));
        self.form_epoch = self.form_epoch.wrapping_add(1);
        self.mode = Mode::Edit;
        self.menu.close();
        self.open_menu = None;
        self.reveal_content();
        self.dirty = true;
    }

    /// Enter the same blank-contact flow from every affordance (header,
    /// first-run state, keyboard and assistive technology).
    fn begin_new_contact(&mut self) {
        let mut contact = Contact::new();
        contact.phones.push(Labeled::new("mobile", ""));
        contact.emails.push(Labeled::new("home", ""));
        self.begin_edit(contact, true);
    }

    fn cancel_edit(&mut self) {
        self.form = None;
        self.mode = Mode::View;
        self.dirty = true;
    }

    fn save_edit(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if let Some(mut form) = self.form.take() {
            form.collect();
            let c = form.draft.clone();
            let id = c.id;
            let saved = if form.is_new {
                self.try_perform_action(doc, Contacts::add(&c))
            } else {
                match Contacts::save(&form.base, &c) {
                    Some(invoke) => self.try_perform_action(doc, invoke),
                    None if self.store.get(id).is_none() => Err(
                        ui::tilo_actions::ActionError::Refused("no such contact".into()),
                    ),
                    None => Ok(()),
                }
            };
            match saved {
                Ok(()) => {
                    self.store.upsert(c);
                    self.selected = Some(id);
                    self.show_toast(if form.is_new {
                        "Contact created"
                    } else {
                        "Saved"
                    });
                    self.mode = Mode::View;
                    self.shared.borrow_mut().reveal = Some(id);
                    self.unconfirmed_save = Some(form);
                }
                Err(error) => {
                    // The draft is intentionally not a durable authority. Put
                    // the document back in charge, then preserve the user's
                    // unsaved text in an editor rebased on the latest record.
                    let base = self
                        .store
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| form.base.clone());
                    self.form = Some(EditForm::with_base(base, form.draft, form.is_new));
                    self.form_epoch = self.form_epoch.wrapping_add(1);
                    self.mode = Mode::Edit;
                    self.show_toast(format!("Couldn't save contact: {error}"));
                }
            }
        }
        self.list_stale = true;
        self.dirty = true;
    }

    /// Toggle the favourite flag through the sole published action route.
    /// Every header, semantic, keyboard, and menu affordance calls this so a
    /// stale local contact cannot mutate the projection after a refusal.
    fn toggle_favorite(&mut self, doc: &mut ui::widget::Edits<'_>, id: ContactId) -> bool {
        let Some(favorite) = self.store.get(id).map(|contact| !contact.favorite) else {
            return false;
        };
        let edit = Invoke::edit(&Contacts::favorite(id, favorite))
            .map_err(|error| ui::tilo_actions::ActionError::Refused(error.to_string()));
        match edit.and_then(|edit| self.try_perform_action(doc, edit)) {
            Ok(()) => {
                if let Some(contact) = self.store.get_mut(id) {
                    contact.favorite = favorite;
                }
                if let Some(form) = &mut self.form
                    && form.draft.id == id
                {
                    form.draft.favorite = favorite;
                    form.base.favorite = favorite;
                }
                self.list_stale = true;
                self.dirty = true;
                true
            }
            Err(error) => {
                self.reconcile_refused_action(doc, "update favorite", &error);
                false
            }
        }
    }

    fn resolve_import_conflict(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        token: &str,
        import_as_new: bool,
    ) -> Result<(), String> {
        let token =
            uuid::Uuid::parse_str(token).map_err(|_| "invalid import conflict id".to_string())?;
        let index = self
            .import_conflicts
            .iter()
            .position(|conflict| conflict.token == token)
            .ok_or_else(|| "import conflict no longer exists".to_string())?;
        let Some(conflict) = self.import_conflicts.get(index).cloned() else {
            return Err("import conflict no longer exists".into());
        };
        if import_as_new {
            let mut contact = conflict.incoming;
            contact.id = uuid::Uuid::new_v4();
            self.try_perform_action(doc, Contacts::add(&contact))
                .map_err(|error| error.to_string())?;
            self.store.upsert(contact);
            self.show_toast("Imported as a new contact");
        } else {
            self.show_toast("Kept the existing contact");
        }
        self.import_conflicts.remove(index);
        self.list_stale = true;
        self.dirty = true;
        Ok(())
    }

    /// Whether an export is being set up or written.
    pub(crate) fn export_is_busy(&self) -> bool {
        self.export.is_some() || self.portal_in_flight()
    }

    /// Whether the open edit form holds changes not yet written to the
    /// store: for a new contact, any content at all; otherwise any
    /// difference from the stored record.
    fn form_is_dirty(&mut self) -> bool {
        let Some(form) = &mut self.form else {
            return false;
        };
        if form.is_new {
            return form.has_content();
        }
        form.collect();
        self.store
            .get(form.draft.id)
            .is_none_or(|stored| *stored != form.draft)
    }

    /// Switch the selected contact to `id`. If an edit form is open with
    /// unsaved changes, park the target in `pending_switch` and ask
    /// "Save changes?" instead of switching directly.
    fn request_select(&mut self, id: ContactId) {
        if self.mode == Mode::Edit {
            let same = self.form.as_ref().is_some_and(|form| form.draft.id == id);
            if same {
                return;
            }
            if self.form_is_dirty() {
                self.pending_switch = Some(id);
                self.dirty = true;
                return;
            }
        }
        self.commit_select(id);
    }

    /// Open `id` from *outside* the list — the global search or an agent. The
    /// target may be hidden by the current group, so "open" reveals it:
    /// when it is not in the visible set, show every contact first.
    fn reveal_contact(&mut self, id: ContactId) {
        if !self.visible_ids().contains(&id) {
            self.set_group_filter(None);
        }
        self.commit_select(id);
    }

    /// Reveal a durable membership group through its canonical view action.
    /// This is presentation-only: opening a group must not fabricate a second
    /// mutable index or history entry. A narrow window shows the list.
    fn reveal_group(&mut self, requested: &str) -> Result<(), String> {
        let group = self
            .store
            .all_groups()
            .into_iter()
            .find(|group| group_names_equal(group, requested))
            .ok_or_else(|| "group no longer exists".to_string())?;
        self.set_group_filter(Some(group));
        self.show_pane = Some(NavPane::Overview);
        self.dirty = true;
        Ok(())
    }

    /// Unconditionally switch selection to `id` (leaving any edit form).
    fn commit_select(&mut self, id: ContactId) {
        self.form = None;
        self.mode = Mode::View;
        self.selected = Some(id);
        self.shared.borrow_mut().reveal = Some(id);
        self.reveal_content();
        self.dirty = true;
    }

    /// Bring the content to the front of a narrow window.
    fn reveal_content(&mut self) {
        if self.rect.width() > 0.0 {
            self.shell.show(NavPane::Content);
        } else {
            self.show_pane = Some(NavPane::Content);
        }
    }

    /// Delete the contact `id`: the host asks the person to confirm, and the
    /// contact leaves the list once the book no longer holds it.
    fn delete_contact(&mut self, doc: &mut ui::widget::Edits<'_>, id: ContactId) {
        if let Err(error) = self.try_perform_action(doc, Contacts::delete(id)) {
            self.reconcile_refused_action(doc, "delete contact", &error);
        }
        self.dirty = true;
    }

    /// Move the list selection by `delta` rows within the visible set.
    fn move_selection(&mut self, delta: isize) {
        let ids = self.visible_ids();
        if ids.is_empty() {
            return;
        }
        let cur = self
            .selected
            .and_then(|s| ids.iter().position(|x| *x == s))
            .map_or(-1, |p| p as isize);
        let next = (cur + delta).clamp(0, ids.len() as isize - 1) as usize;
        let Some(&id) = ids.get(next) else {
            return;
        };
        self.selected = Some(id);
        self.shared.borrow_mut().reveal = Some(id);
        self.dirty = true;
    }

    fn execute_context_action(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        target: ContactId,
        action: ContextAction,
    ) {
        match action {
            ContextAction::Edit => {
                if let Some(c) = self.store.get(target).cloned() {
                    self.selected = Some(target);
                    self.begin_edit(c, false);
                }
            }
            ContextAction::Favorite => {
                self.toggle_favorite(doc, target);
            }
            ContextAction::Export => {
                if let Err(error) = self.invoke_contact_export_operation(doc, target) {
                    self.show_toast(format!("Couldn't export contact: {error}"));
                }
            }
            ContextAction::Delete => {
                self.selected = Some(target);
                self.delete_contact(doc, target);
            }
        }
        self.dirty = true;
    }

    /// The More menu's commands and whether each can run now.
    fn more_rows(&self) -> Vec<(MoreAction, bool)> {
        let importing = self.portal_in_flight();
        let mut rows = vec![
            (MoreAction::Import, !importing),
            (
                MoreAction::Export,
                !self.store.is_empty() && !self.export_is_busy(),
            ),
        ];
        if self.duplicate_candidate.is_some() {
            rows.push((MoreAction::ReviewDuplicate, true));
        }
        rows
    }

    /// Hand the More button its rows when what they offer changed.
    fn publish_more(&mut self) {
        let rows = self.more_rows();
        if rows == self.more_shown {
            return;
        }
        let items = rows
            .iter()
            .map(|&(action, enabled)| {
                let label = match action {
                    MoreAction::Import if !enabled => "Importing…",
                    MoreAction::Import => "Import contacts",
                    MoreAction::Export => "Export contacts",
                    MoreAction::ReviewDuplicate => "Review possible duplicate",
                };
                let pick = std::sync::Arc::clone(&self.more_pick);
                MenuItem::action(label, move || {
                    if let Ok(mut pick) = pick.lock() {
                        *pick = Some(action);
                    }
                })
                .with_enabled(enabled)
            })
            .collect();
        if let Some(button) = self.more_button() {
            button.set_items(items);
            self.more_shown = rows;
        }
    }

    /// The overview header's More button, inside the shell.
    fn more_button(&mut self) -> Option<&mut ui::widget::MenuButton> {
        find_mut(&mut self.shell, MORE_BUTTON)?
            .as_any_mut()?
            .downcast_mut::<ui::widget::MenuButton>()
    }

    fn execute_more_action(&mut self, doc: &ui::widget::Edits<'_>, action: MoreAction) {
        match action {
            MoreAction::Import => self.request_import(),
            MoreAction::Export => self.open_export_dialog(),
            MoreAction::ReviewDuplicate => match self.invoke_duplicate_review_operation(doc) {
                Ok(_) => {}
                Err(error) => self.show_toast(format!("Couldn't open the review: {error}")),
            },
        }
        self.dirty = true;
    }
}

// =====================================================================
// The window: sidebar, list, content, headers, dialogs and menus
// =====================================================================

impl ContactsApp {
    fn sidebar_list(
        rows: &[(String, String, u32)],
        destination: &LocalState<Option<String>>,
    ) -> NavList {
        let mut book = Vec::new();
        let mut groups = Vec::new();
        for (id, label, count) in rows {
            let item = NavItem::new(id.clone(), label.clone()).count(*count);
            match id.as_str() {
                ALL_DESTINATION => book.push(item.icon(Icon::Users)),
                FAVORITES_DESTINATION => book.push(item.icon(Icon::Star)),
                _ => groups.push(item),
            }
        }
        if book.is_empty() {
            book.push(NavItem::new(ALL_DESTINATION, "All contacts").icon(Icon::Users));
        }
        let mut sections = vec![NavSection::untitled("book", book)];
        if !groups.is_empty() {
            sections.push(NavSection::new("groups", "Groups", groups));
        }
        NavList::new(sections)
            .with_id("contacts.groups")
            .with_accessible_name("Groups")
            .bind_selection(destination)
    }

    /// The sidebar's rows for the book as it is now.
    fn sidebar_rows(&self) -> Vec<(String, String, u32)> {
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let favorites = self.store.contacts.iter().filter(|c| c.favorite).count();
        let mut rows = vec![
            (
                ALL_DESTINATION.to_owned(),
                "All contacts".to_owned(),
                count(self.store.len()),
            ),
            (
                FAVORITES_DESTINATION.to_owned(),
                "Favorites".to_owned(),
                count(favorites),
            ),
        ];
        for group in self.store.all_groups() {
            let members = self
                .store
                .contacts
                .iter()
                .filter(|contact| {
                    contact
                        .groups
                        .iter()
                        .any(|entry| group_names_equal(&entry.value, &group))
                })
                .count();
            rows.push((format!("{GROUP_DESTINATION}{group}"), group, count(members)));
        }
        rows
    }

    fn content_key(&self) -> ContentKey {
        if self.mode == Mode::Edit
            && let Some(form) = &self.form
        {
            return ContentKey::Edit {
                epoch: self.form_epoch,
                calendar: form.calendar_open,
            };
        }
        if self.store.is_empty() {
            return ContentKey::FirstRun;
        }
        match self.selected.and_then(|id| self.store.get(id)) {
            Some(contact) => ContentKey::Contact(Box::new(contact.clone())),
            None => ContentKey::Nothing,
        }
    }

    fn header_key(&self) -> HeaderKey {
        if self.mode == Mode::Edit && self.form.is_some() {
            return HeaderKey::Edit;
        }
        match self.selected.and_then(|id| self.store.get(id)) {
            Some(contact) if !self.store.is_empty() => HeaderKey::View {
                favorite: contact.favorite,
            },
            _ => HeaderKey::Empty,
        }
    }

    /// The content header's title and commands for `key`.
    fn content_header(&self, key: HeaderKey) -> (&'static str, Vec<Element>) {
        let shared = &self.shared;
        match key {
            HeaderKey::Empty => ("", Vec::new()),
            HeaderKey::View { favorite } => {
                let commands: Vec<Element> = vec![
                    Box::new(
                        Button::icon_only("contacts.favorite", Icon::Star, "Favorite")
                            .toggled(favorite)
                            .on_view(queue(shared, Request::ToggleFavorite)),
                    ),
                    Box::new(
                        Button::new("contacts.edit", "Edit")
                            .on_view(queue(shared, Request::EditSelected)),
                    ),
                    Box::new(
                        Button::icon_only("contacts.delete", Icon::Trash2, "Delete contact")
                            .on_view(queue(shared, Request::DeleteSelected)),
                    ),
                ];
                ("", commands)
            }
            HeaderKey::Edit => (
                "",
                vec![
                    Box::new(
                        Button::new("contacts.form.cancel", "Cancel")
                            .on_view(queue(shared, Request::Cancel)),
                    ),
                    Box::new(
                        Button::new("contacts.form.save", "Save")
                            .role(ButtonRole::Primary)
                            .on_view(queue(shared, Request::Save)),
                    ),
                ],
            ),
        }
    }

    /// Hand the list its rows and the sidebar its groups.
    fn publish_list(&mut self) {
        self.list_stale = false;
        let rows: Vec<ListEntry> = self
            .visible_ids()
            .into_iter()
            .filter_map(|id| self.store.get(id))
            .map(|contact| ListEntry {
                id: contact.id,
                name: contact.display_name(),
                secondary: contact.subtitle(),
                favorite: contact.favorite,
            })
            .collect();
        let empty_title = match self.group_filter.as_deref() {
            Some(FAVORITES_FILTER) => Some("No favorites yet".to_owned()),
            Some(_) => Some("No one is in this group".to_owned()),
            None => None,
        };
        let sortable = !rows.is_empty();
        self.sort_shown = sortable;
        {
            let mut bridge = self.shared.borrow_mut();
            if bridge.rows != rows || bridge.empty_title != empty_title {
                bridge.rows = rows;
                bridge.empty_title = empty_title;
                bridge.revision = bridge.revision.wrapping_add(1);
            }
        }
        let sidebar = self.sidebar_rows();
        if sidebar != self.sidebar_shown {
            // A group that emptied out is no destination any more.
            if let Some(group) = self.group_filter.clone()
                && group != FAVORITES_FILTER
                && !sidebar
                    .iter()
                    .any(|(id, ..)| *id == format!("{GROUP_DESTINATION}{group}"))
            {
                self.set_group_filter(None);
                return self.publish_list();
            }
            self.shell.set_sidebar(Some(Box::new(Self::sidebar_list(
                &sidebar,
                &self.destination,
            ))));
            self.sidebar_shown = sidebar;
            self.structure_dirty = true;
        }
        self.shell.set_overview_title(
            self.destination_label()
                .unwrap_or_else(|| "All contacts".to_owned()),
        );
    }

    /// The shared list controls and a truthful, live announcement of the
    /// file operation held by the same guard that prevents duplicate work.
    fn publish_overview_controls(&mut self) {
        let key = (self.sort_shown, self.file_work.get());
        if self.overview_controls_shown == Some(key) {
            return;
        }
        let mut controls = ui::widget::Column::new()
            .with_no_padding()
            .with_intrinsic_height();
        if self.sort_shown {
            controls = controls.push(sort_control(&self.shared, &self.sort_choice));
        }
        if let Some(work) = key.1 {
            let label = match work {
                FileWork::Import => "Importing contacts",
                FileWork::Export => "Exporting contacts",
            };
            controls = controls.push(Box::new(
                ui::widget::ProgressBar::indeterminate().with_label(label),
            ));
        }
        self.shell.set_overview_controls(
            (self.sort_shown || key.1.is_some()).then(|| Box::new(controls) as Element),
        );
        self.overview_controls_shown = Some(key);
        self.structure_dirty = true;
    }

    /// Hand the shell and the panes what the window shows now. Cheap when
    /// nothing changed: each part is rebuilt only when what it shows did.
    fn publish(&mut self) {
        if self.list_stale {
            self.publish_list();
        }
        self.publish_overview_controls();
        self.publish_more();
        if self.selected.is_some_and(|id| self.store.get(id).is_none()) {
            self.selected = self.visible_ids().first().copied();
        }
        {
            // A contact being created is no one in the list yet.
            let creating = self.mode == Mode::Edit && self.form.as_ref().is_some_and(|f| f.is_new);
            let shown = if creating { None } else { self.selected };
            let mut bridge = self.shared.borrow_mut();
            if bridge.selected != shown {
                bridge.selected = shown;
                bridge.revision = bridge.revision.wrapping_add(1);
            }
        }

        let content = self.content_key();
        if self.content_shown.as_ref() != Some(&content) {
            let element: Element = match &content {
                ContentKey::FirstRun => Box::new(EmptyPane::new(
                    std::rc::Rc::clone(&self.shared),
                    "No contacts yet",
                    Some(("New contact", Request::NewContact)),
                )),
                ContentKey::Nothing => Box::new(EmptyPane::new(
                    std::rc::Rc::clone(&self.shared),
                    "No contact selected",
                    None,
                )),
                ContentKey::Contact(contact) => {
                    if self.detail_scroll.0 != Some(contact.id) {
                        self.detail_scroll = (Some(contact.id), std::rc::Rc::default());
                    }
                    crate::detail::contact_page(contact, &self.shared, &self.detail_scroll.1)
                }
                ContentKey::Edit { .. } => match &self.form {
                    Some(form) => form.element(&self.shared),
                    None => Box::new(ui::widget::Spacer::new()),
                },
            };
            self.shell.set_content(element);
            self.content_shown = Some(content);
            self.structure_dirty = true;
        }

        let header = self.header_key();
        if self.header_shown != Some(header) {
            let (title, actions) = self.content_header(header);
            self.shell.set_content_header(title, actions);
            self.header_shown = Some(header);
            self.structure_dirty = true;
        }

        let wanted = self.wanted_dialog();
        if self.dialog.as_ref().map(|(key, _)| key) != wanted.as_ref() {
            self.dialog = wanted.and_then(|key| {
                let element = self.build_dialog(&key)?;
                Some((key, element))
            });
            self.structure_dirty = true;
        }
        if self.structure_dirty {
            self.layout_dirty = true;
            self.dirty = true;
        }
    }

    /// Open a contact's actions at `anchor`.
    fn open_menu_at(&mut self, menu: PendingMenu, anchor: Rect) {
        let mut built = ContextMenu::new();
        let open = match menu {
            PendingMenu::Contact(id) => {
                let Some(contact) = self.store.get(id) else {
                    return;
                };
                let favorite = contact.favorite;
                let rows = vec![
                    Some(ContextAction::Edit),
                    Some(ContextAction::Favorite),
                    Some(ContextAction::Export),
                    None,
                    Some(ContextAction::Delete),
                ];
                for row in &rows {
                    built.push(match row {
                        Some(ContextAction::Delete) => {
                            MenuItem::destructive_selection(ContextAction::Delete.label(favorite))
                        }
                        Some(action) => MenuItem::selection(action.label(favorite)),
                        None => MenuItem::separator(),
                    });
                }
                if let Some((_, row)) = self
                    .shared
                    .borrow()
                    .painted
                    .iter()
                    .find(|(row, _)| *row == id)
                {
                    built.anchor_row(*row);
                }
                OpenMenu::Contact(id, rows)
            }
        };
        built.open_anchored(anchor, self.rect);
        self.menu = built;
        self.open_menu = Some(open);
        self.menu_follows = None;
        self.dirty = true;
    }

    /// The anchor a waiting menu opens from, once the window is painted.
    fn menu_anchor(&self, menu: PendingMenu) -> Option<Rect> {
        match menu {
            PendingMenu::Contact(id) => self
                .shared
                .borrow()
                .painted
                .iter()
                .find(|(row, _)| *row == id)
                .map(|(_, row)| {
                    // Under the row (above it near the bottom of the
                    // window), starting where its name does, so the row
                    // stays readable while its actions are open.
                    let x = row.x0 + row.height();
                    Rect::new(x, row.y0, x, row.y1)
                }),
        }
    }

    fn run_menu_choice(&mut self, doc: &mut ui::widget::Edits<'_>, index: usize) {
        match self.open_menu.take() {
            Some(OpenMenu::Contact(id, rows)) => {
                if let Some(Some(action)) = rows.get(index) {
                    self.execute_context_action(doc, id, *action);
                }
            }
            None => {}
        }
    }

    /// Turn what the panes and buttons asked for into view changes and
    /// document actions.
    fn apply_requests(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let picked = self.more_pick.lock().ok().and_then(|mut pick| pick.take());
        if let Some(action) = picked {
            self.execute_more_action(doc, action);
        }
        loop {
            let requests = self.shared.borrow_mut().take_requests();
            if requests.is_empty() {
                break;
            }
            for request in requests {
                self.apply_request(doc, request);
            }
        }
    }

    fn apply_request(&mut self, doc: &mut ui::widget::Edits<'_>, request: Request) {
        match request {
            Request::Select(id) => self.request_select(id),
            Request::Reveal(id) => {
                if self.store.get(id).is_some() {
                    self.reveal_contact(id);
                }
            }
            Request::Edit(id) => {
                if let Some(contact) = self.store.get(id).cloned() {
                    self.selected = Some(id);
                    self.begin_edit(contact, false);
                }
            }
            Request::EditField(id, slot) => {
                if let Some(contact) = self.store.get(id).cloned() {
                    self.selected = Some(id);
                    self.begin_edit(contact, false);
                    if let Some(form) = &mut self.form {
                        form.focus = Some(slot);
                    }
                }
            }
            Request::ContextMenu(id, point) => {
                if self.mode == Mode::Edit && self.form.as_ref().is_some_and(|f| f.draft.id != id) {
                    self.request_select(id);
                    return;
                }
                self.selected = Some(id);
                if let Some(point) = point {
                    self.open_menu_at(
                        PendingMenu::Contact(id),
                        Rect::new(point.x, point.y, point.x, point.y),
                    );
                } else {
                    self.shared.borrow_mut().reveal = Some(id);
                    self.pending_menu = Some(PendingMenu::Contact(id));
                }
            }
            Request::Sort(sort) => {
                if self.sort != sort {
                    self.sort = sort;
                    self.list_stale = true;
                }
                if self.sort_choice.with(|value| value != sort_value(sort)) {
                    self.sort_choice.set(sort_value(sort).to_owned());
                }
            }
            Request::NewContact => self.begin_new_contact(),
            Request::ToggleFavorite => {
                if let Some(id) = self.selected {
                    self.toggle_favorite(doc, id);
                }
            }
            Request::EditSelected => {
                if let Some(contact) = self.selected.and_then(|id| self.store.get(id)).cloned() {
                    self.begin_edit(contact, false);
                }
            }
            Request::DeleteSelected => {
                if let Some(id) = self.selected {
                    self.delete_contact(doc, id);
                }
            }
            Request::Save => self.save_edit(doc),
            Request::FocusField(slot) => self.focus_form_field(slot),
            Request::AdvanceField(slot) => self.advance_form_field(doc, slot),
            Request::Cancel => self.cancel_edit(),
            Request::Add(kind) => {
                if let Some(form) = &mut self.form {
                    form.add(kind);
                    self.form_epoch = self.form_epoch.wrapping_add(1);
                }
            }
            Request::Remove(target) => {
                if let Some(form) = &mut self.form {
                    form.remove(target);
                    self.form_epoch = self.form_epoch.wrapping_add(1);
                }
            }
            Request::ToggleCalendar => {
                if let Some(form) = &mut self.form {
                    form.calendar_open = !form.calendar_open;
                }
            }
            Request::PickDate(iso) => {
                if let Some(form) = &mut self.form {
                    form.set_value(crate::form::Slot::Birthday, &iso);
                    form.collect();
                    form.calendar_open = false;
                }
            }
            Request::OpenWebsite(url) => {
                ui::widget::open_external_url(&url);
                self.opened_url = Some(url);
            }
            Request::Dialog(choice) => self.answer_dialog(doc, choice),
        }
        self.dirty = true;
    }

    /// The shared field submitted Enter; the last input submits the same
    /// Save action as the header, while an earlier input advances locally.
    fn advance_form_field(&mut self, doc: &mut ui::widget::Edits<'_>, slot: crate::form::Slot) {
        let next = self.form.as_ref().and_then(|form| {
            let index = form
                .fields
                .iter()
                .position(|(candidate, _)| *candidate == slot)?;
            form.fields.get(index + 1).map(|(next, _)| *next)
        });
        if let Some(next) = next {
            self.focus_form_field(next);
        } else if self
            .form
            .as_ref()
            .is_some_and(|form| form.fields.last().is_some_and(|(last, _)| *last == slot))
        {
            self.save_edit(doc);
        }
    }

    /// Rebuild the retained form with the requested shared input focused.
    /// Its local bindings and scroll offset survive; the host admits the new
    /// focus owner through its ordinary composition pass.
    fn focus_form_field(&mut self, slot: crate::form::Slot) {
        if let Some(form) = &mut self.form
            && form.fields.iter().any(|(candidate, _)| *candidate == slot)
        {
            form.focus = Some(slot);
            self.form_epoch = self.form_epoch.wrapping_add(1);
            self.dirty = true;
        }
    }

    /// Keys the window answers when no control took them: moving through
    /// the list, opening, deleting, and leaving the form.
    fn handle_key(&mut self, key: &KeyEvent, mods: ModifiersState) -> bool {
        if key.state != ElementState::Pressed {
            return false;
        }
        if matches!(key.logical_key, Key::Named(NamedKey::Escape)) && self.mode == Mode::Edit {
            self.cancel_edit();
            return true;
        }
        if self.mode == Mode::Edit && matches!(key.logical_key, Key::Named(NamedKey::Enter)) {
            self.shared
                .borrow_mut()
                .push(Request::FocusField(crate::form::Slot::First));
            return true;
        }
        if self.mode != Mode::View {
            return false;
        }
        let context_chord = matches!(key.logical_key, Key::Named(NamedKey::ContextMenu))
            || (mods.shift_key() && matches!(key.logical_key, Key::Named(NamedKey::F10)));
        if context_chord {
            if let Some(id) = self.selected {
                self.shared.borrow_mut().reveal = Some(id);
                self.pending_menu = Some(PendingMenu::Contact(id));
                self.dirty = true;
            }
            return true;
        }
        if mods.control_key() || mods.super_key() || mods.alt_key() {
            return false;
        }
        match &key.logical_key {
            Key::Named(NamedKey::ArrowDown) => self.move_selection(1),
            Key::Named(NamedKey::ArrowUp) => self.move_selection(-1),
            Key::Named(NamedKey::Enter) if self.selected.is_some() => self.reveal_content(),
            Key::Named(NamedKey::Delete | NamedKey::Backspace) if self.selected.is_some() => {
                self.shared.borrow_mut().push(Request::DeleteSelected);
            }
            Key::Character(c) if c.eq_ignore_ascii_case("n") => self.begin_new_contact(),
            Key::Character(c) if c.eq_ignore_ascii_case("e") => {
                let Some(contact) = self.selected.and_then(|id| self.store.get(id)).cloned() else {
                    return false;
                };
                self.begin_edit(contact, false);
            }
            _ => return false,
        }
        self.dirty = true;
        true
    }
}

/// The node of the app's child at `index` (the shell first, then an open
/// dialog).
fn child_node(taffy: &TaffyTree<()>, node: NodeId, index: usize) -> Option<NodeId> {
    taffy.children(node).ok()?.get(index).copied()
}

impl Default for ContactsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for ContactsApp {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if let Some(editor) = &self.account_editor {
            editor.sync_visibility();
            // Accounts is a contact's inspector, first entered through that
            // resource's action. Keep browsing chrome free of an unused toggle.
            if editor.visible.get()
                && let Some(pane) = self.account_pane.take()
            {
                self.shell.set_inspector(Some(pane));
            }
        }
        self.import_opened_cards();
        self.announce_refusal();
        self.poll_destination();
        if let Some(scenario) = self.scenario.take() {
            self.stage(&scenario);
        }
        self.publish();
        self.shell.sync(state, path);
        if let Some((_, dialog)) = &mut self.dialog {
            dialog.sync(state, path);
        }
    }
    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ObjId) {
        if let Some((token, import_as_new)) = self.pending_conflict_resolution.take()
            && let Err(reason) = self.resolve_import_conflict(edits, &token, import_as_new)
        {
            self.show_toast(reason);
        }
        self.shell.settle(edits, path);
        if let Some((_, dialog)) = &mut self.dialog {
            dialog.settle(edits, path);
        }
        self.apply_requests(edits);
        self.poll_destination();
        self.publish();
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let mut changed = self.shell.update(state, path);
        if let Some((_, dialog)) = &mut self.dialog {
            changed |= dialog.update(state, path);
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_dirty = false;
        let shell = self.shell.build_layout(taffy, theme, node_map);
        let mut children = vec![shell];
        if let Some((_, dialog)) = &mut self.dialog {
            let node = dialog.build_layout(taffy, theme, node_map);
            if let Ok(style) = taffy.style(node) {
                let mut style = style.clone();
                style.position = Position::Absolute;
                style.inset = taffy::Rect {
                    left: taffy::LengthPercentageAuto::length(0.0),
                    right: taffy::LengthPercentageAuto::length(0.0),
                    top: taffy::LengthPercentageAuto::length(0.0),
                    bottom: taffy::LengthPercentageAuto::length(0.0),
                };
                let _ = taffy.set_style(node, style);
            }
            children.push(node);
        }
        let style = Style {
            display: taffy::Display::Flex,
            flex_grow: 1.0,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let child_origin = Point::new(self.rect.x0, self.rect.y0);
        if let Some(shell) = child_node(taffy, node, 0) {
            self.shell.apply_layout(taffy, shell, child_origin);
        }
        if let Some((_, dialog)) = &mut self.dialog
            && let Some(dialog_node) = child_node(taffy, node, 1)
        {
            dialog.apply_layout(taffy, dialog_node, child_origin);
        }
        if let Some(pane) = self.show_pane.take() {
            self.shell.show(pane);
            self.shell.snap();
            self.layout_dirty = true;
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.shell.draw(scene, transform, tr, theme, presence);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        self.shell
            .draw_with_presence(scene, transform, tr, theme, presence, permissions);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.shell
            .draw_overlay(scene, transform, tr, theme, presence);
        if let Some(menu) = self.pending_menu
            && let Some(anchor) = self.menu_anchor(menu)
        {
            self.pending_menu = None;
            self.open_menu_at(menu, anchor);
            self.menu_follows = Some(menu);
            // A menu opened from the keyboard starts on its first command.
            self.menu.focus_first_action();
        }
        if self.menu.is_open()
            && let Some(menu) = self.menu_follows
            && let Some(anchor) = self.menu_anchor(menu)
        {
            self.menu.reanchor(anchor, self.rect);
            if let PendingMenu::Contact(id) = menu
                && let Some((_, row)) = self
                    .shared
                    .borrow()
                    .painted
                    .iter()
                    .find(|(row, _)| *row == id)
            {
                self.menu.anchor_row(*row);
            }
        }
        if self.menu.is_open() {
            self.menu.layout_within(tr, theme);
            self.menu
                .draw_overlay(scene, transform, tr, theme, presence);
        }
        if let Some((_, dialog)) = &mut self.dialog {
            dialog.draw_overlay(scene, transform, tr, theme, presence);
        }
        if self.toast_expired() {
            self.toast = None;
        }
        if let Some((msg, _)) = &self.toast {
            paint_transient(
                scene,
                transform,
                tr,
                theme,
                self.rect,
                ToastAnchor::BottomCenter,
                &ToastCard::new(msg, ToastKind::Info),
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        let input = matches!(
            event,
            WindowEvent::MouseInput { .. }
                | WindowEvent::KeyboardInput { .. }
                | WindowEvent::MouseWheel { .. }
        );

        // An open menu owns input until it closes.
        if self.menu.is_open() {
            self.menu.handle_event(event, ctx);
            // Any other press outside the menu dismisses it, like a click.
            if !*ctx.handled
                && matches!(
                    event,
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        ..
                    }
                )
            {
                self.menu.close();
            }
            let choice = self.menu.take_selection();
            let doc = &mut ctx.state;
            if let Some(index) = choice {
                self.run_menu_choice(doc, index);
            }
            if !self.menu.is_open() {
                self.open_menu = None;
                self.dirty = true;
            }
            if input {
                *ctx.handled = true;
                self.apply_requests(doc);
                self.publish();
                return;
            }
        }

        // An open dialog is modal.
        if self.dialog.is_some() {
            if let Some(node) = child_node(ctx.taffy, ctx.node_id, 1)
                && let Some((_, dialog)) = &mut self.dialog
            {
                dialog.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            }
            if input {
                *ctx.handled = true;
            }
            let doc = &mut ctx.state;
            self.apply_requests(doc);
            self.publish();
            return;
        }

        // Ctrl+S saves the form wherever the focus is in it.
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
            && self.mode == Mode::Edit
            && (ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key())
            && matches!(&key.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("s"))
        {
            let doc = &mut ctx.state;
            self.save_edit(doc);
            self.publish();
            *ctx.handled = true;
            return;
        }

        if let Some(shell) = child_node(ctx.taffy, ctx.node_id, 0) {
            self.shell
                .handle_event(event, &mut ctx.reborrow(ctx.path, shell));
        }

        let mods = ctx.modifiers.state();
        let doc = &mut ctx.state;
        if !*ctx.handled
            && let WindowEvent::KeyboardInput { event: key, .. } = event
            && self.handle_key(key, mods)
        {
            *ctx.handled = true;
        }
        self.poll_destination();
        self.apply_requests(doc);
        self.publish();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = vec![&self.shell];
        if let Some((_, dialog)) = &self.dialog {
            children.push(dialog.as_ref());
        }
        if self.menu.is_open() {
            children.push(&self.menu);
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.shell);
        if let Some((_, dialog)) = &self.dialog {
            visitor(dialog.as_ref());
        }
        if self.menu.is_open() {
            visitor(&self.menu);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = vec![&mut self.shell];
        if let Some((_, dialog)) = &mut self.dialog {
            children.push(dialog.as_mut());
        }
        if self.menu.is_open() {
            children.push(&mut self.menu);
        }
        children
    }

    fn wants_pointer_capture(&self) -> bool {
        self.shell.wants_pointer_capture()
    }

    fn name(&self) -> &'static str {
        "ContactsApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("contacts.app")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn traps_focus(&self) -> bool {
        self.dialog
            .as_ref()
            .is_some_and(|(_, dialog)| dialog.traps_focus())
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.dialog
            .as_ref()
            .and_then(|(_, dialog)| dialog.modal_capture())
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let (message, _) = self.toast.as_ref()?;
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::StaticText)
                .with_name(message.clone())
                .with_hashed_key("contacts.status")
                .with_live(ui::a11y::Politeness::Assertive),
        )
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.toast.is_some()
            || self.portal_in_flight()
            || self.shell.needs_redraw()
            || self.dialog.as_ref().is_some_and(|(_, d)| d.needs_redraw())
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.shell.mark_drawn();
        if let Some((_, dialog)) = &mut self.dialog {
            dialog.mark_drawn();
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.toast.is_some()
            || self.portal_in_flight()
            || self.pending_menu.is_some()
            || self.shell.needs_animation_frame()
            || self
                .dialog
                .as_ref()
                .is_some_and(|(_, d)| d.needs_animation_frame())
    }

    fn needs_idle_sync(&self) -> bool {
        self.portal_in_flight()
            || self.shared.borrow().has_requests()
            || self.shell.needs_idle_sync()
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty
            || self.shell.is_layout_dirty()
            || self
                .dialog
                .as_ref()
                .is_some_and(|(_, d)| d.is_layout_dirty())
    }

    fn layout_needs_measure(&self) -> bool {
        self.layout_dirty
            || self.shell.layout_needs_measure()
            || self
                .dialog
                .as_ref()
                .is_some_and(|(_, d)| d.layout_needs_measure())
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure_dirty);
        dirty |= self.shell.take_structure_dirty();
        if let Some((_, dialog)) = &mut self.dialog {
            dirty |= dialog.take_structure_dirty();
        }
        dirty
    }
}

impl ui::widget::controls::ActionControls for ContactsApp {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        // This capability owns the whole mounted subtree, including modal
        // table/selection/footer controls and the contextual menu.
        self.children_mut()
            .into_iter()
            .find_map(ui::widget::controls::take_control_invocation)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests;

/// The one import or export running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileWork {
    Import,
    Export,
}

/// Holds the running [`FileWork`] until the work ends, however it ends.
struct FileWorkGuard(Rc<Cell<Option<FileWork>>>);

impl Drop for FileWorkGuard {
    fn drop(&mut self) {
        self.0.set(None);
    }
}

/// Size and kind of a file a chooser named.
#[cfg(test)]
fn file_metadata(path: &std::path::Path) -> Result<tilo_files::Meta, String> {
    tilo_files::file_at(path)
        .and_then(|(dir, name)| tilo_files::blocking::metadata(&dir, &name))
        .map_err(|error| error.to_string())
}
