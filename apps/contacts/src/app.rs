//! Root widget for the Contacts app.
//!
//! The window is one [`NavShell`]: the address book's groups are the
//! sidebar, the people in the chosen group are the overview list, and the
//! selected person (or the form editing them) is the content. The headers
//! carry the commands: search, more and new above the list; favorite, edit
//! and delete above the person.
//!
//! State lives in the Automerge document via [`ContactsDoc`]; the in-memory
//! [`ContactStore`] is the working mirror the window reads. Every change to
//! the book is a [`crate::actions::ContactsAction`] dispatched through one
//! action sink, the same route Ctrl+K and agents take.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::mpsc::Receiver;
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ChangeHash, ObjId};
use taffy::{Dimension, NodeId, Position, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use ui::LocalState;
use ui::icons::Icon;
use ui::portal::{MultiChannel as ImportChannel, PortalChannel as ExportChannel};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    Button, ButtonRole, ContextMenu, Element, MenuItem, NavItem, NavList, NavPane, NavSection,
    NavShell, TaffyTreeExt, Toolbar, Widget,
};

use crate::doc::ContactsDoc;
use crate::form::EditForm;
use crate::model::{
    Contact, ContactId, ContactStore, Labeled, SortKey, group_names_equal, normalize_text,
    sample_contacts,
};
use crate::vcard::VCardVersion;
use crate::view::{Bridge, ContactList, EmptyPane, ListEntry, Request, Shared, queue};

mod dialogs;
mod files;
mod operations;

use dialogs::{DialogKey, ExportDraft};

/// Private presentation token; it is never written to the shared document.
const FAVORITES_FILTER: &str = "\u{0}contacts:favorites";

/// How long an action toast ("Saved", "Contact deleted", …) stays on screen
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
    generation: u64,
    contacts: Vec<Contact>,
    errors: usize,
}

/// Compact stable-id lookup and presentation order over the authoritative
/// in-memory CRDT projection. It intentionally contains ids/positions and
/// group spelling only; it is not a second contact-resource tree.
#[derive(Default)]
struct ContactsResourceIndex {
    contacts: HashMap<ContactId, usize>,
    ordered_contacts: BTreeSet<ContactOrder>,
    groups: HashMap<String, String>,
    ordered_groups: BTreeSet<String>,
    group_variants: BTreeMap<String, BTreeMap<String, usize>>,
}

/// The display sort key for a contact resource.  Keeping it as a tree rather
/// than a sorted vector makes a normal one-contact edit O(log n), leaving the
/// bounded palette snapshot to iterate only its first page.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct ContactOrder {
    label: String,
    id: ContactId,
}

impl ContactOrder {
    fn for_contact(contact: &Contact) -> Self {
        Self {
            label: normalize_text(&contact.display_name()),
            id: contact.id,
        }
    }
}

/// A successful local Contacts action has already updated the authoritative
/// document and is immediately mirrored by the UI call site.  Remembering
/// just its old record lets `sync` update retained derived state without
/// reparsing/reindexing a whole address book.
struct LocalContactChange {
    id: ContactId,
    before: Option<Contact>,
}

fn update_resource_groups(
    index: &mut ContactsResourceIndex,
    contact: &Contact,
    add: bool,
    affected: &mut BTreeSet<String>,
) {
    for entry in &contact.groups {
        let name = entry.value.trim();
        if name.is_empty() {
            continue;
        }
        let key = normalize_text(name);
        affected.insert(key.clone());
        if add {
            *index
                .group_variants
                .entry(key)
                .or_default()
                .entry(name.to_string())
                .or_default() += 1;
            continue;
        }
        let remove_key = {
            let Some(variants) = index.group_variants.get_mut(&key) else {
                continue;
            };
            let Some(count) = variants.get_mut(name) else {
                continue;
            };
            *count = count.saturating_sub(1);
            if *count == 0 {
                variants.remove(name);
            }
            variants.is_empty()
        };
        if remove_key {
            index.group_variants.remove(&key);
        }
    }
}

fn refresh_resource_group_labels(index: &mut ContactsResourceIndex, affected: BTreeSet<String>) {
    for key in affected {
        let Some(variants) = index.group_variants.get(&key) else {
            index.groups.remove(&key);
            index.ordered_groups.remove(&key);
            continue;
        };
        let Some(label) = variants.keys().next().cloned() else {
            continue;
        };
        index.groups.insert(key.clone(), label);
        index.ordered_groups.insert(key);
    }
}

/// Private, review-only state for a potential duplicate. It is rebuilt from
/// the current collaborative contacts whenever opened; no tentative choices
/// leak into the shared document until the explicit Merge button emits the
/// typed `ContactsAction::MergeContacts` envelope.
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
    Sort(SortKey),
}

/// Which menu is open, and the rows it shows in order (`None` for a
/// separator).
enum OpenMenu {
    Contact(ContactId, Vec<Option<ContextAction>>),
    More(Vec<Option<MoreAction>>),
}

/// A menu waiting for its anchor: the row or the button it opens from is
/// placed by the next paint.
#[derive(Clone, Copy, PartialEq, Debug)]
enum PendingMenu {
    Contact(ContactId),
    More,
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
    resource_index: ContactsResourceIndex,
    /// Retained complete Ctrl+K catalogue. A provider clone is O(1), so opening
    /// search never reindexes or clones the address book.
    search_provider: crate::search_provider::ContactSearchProvider,
    search_heads: Vec<ChangeHash>,
    #[cfg(test)]
    resource_snapshot_materialized: std::cell::Cell<usize>,
    #[cfg(test)]
    resource_exact_materialized: std::cell::Cell<usize>,
    #[cfg(test)]
    resource_index_rebuilds: std::cell::Cell<usize>,
    #[cfg(test)]
    local_change_index_lookups: std::cell::Cell<usize>,
    selected: Option<ContactId>,
    mode: Mode,
    form: Option<EditForm>,
    /// Bumped whenever the form's field states are replaced, so the content
    /// rebuilds its widgets over the new states.
    form_epoch: u64,
    sort: SortKey,
    group_filter: Option<String>,

    toast: Option<(String, Instant)>,
    confirm_delete: Option<ContactId>,
    duplicate_review: Option<DuplicateReview>,
    /// The first bounded duplicate candidate, recomputed only when the
    /// authoritative document generation changes, so a very large book is
    /// not rescanned on every frame to decide whether review is offered.
    duplicate_candidate: Option<crate::actions::DuplicateCandidate>,
    duplicate_candidate_heads: Vec<ChangeHash>,
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
    /// When `true`, a first `sync` onto an empty book seeds the varied sample
    /// address book. Only the `--shots` and test constructors set this — the
    /// live app never fabricates content.
    seed_samples: bool,
    /// A capture scenario staged once the book is seeded.
    scenario: Option<String>,

    /// In-flight import file-chooser: the portal worker's receiver, drained
    /// once per frame. `Some` ⇒ the host's multi-select Open dialog is up.
    import_rx: Option<ImportChannel>,
    import_parse_rx: Option<Receiver<ParsedImport>>,
    import_generation: u64,
    /// In-flight export Save dialog: the portal worker's receiver.
    export_rx: Option<ExportChannel>,
    /// The contacts + vCard version parked while the export Save dialog is up,
    /// written to the chosen destination when the portal returns a path.
    pending_export: Option<(Vec<ContactId>, VCardVersion)>,
    resource_invocation: Option<ui::tilo_actions::InvocationId>,
    completed_resource_invocation: Option<ui::tilo_actions::InvocationUpdate>,
    /// Pending untrusted UID collisions. These never enter the document until
    /// the user explicitly chooses a safe resolution.
    import_conflicts: Vec<ImportConflict>,
    pending_conflict_resolution: Option<(String, bool)>,

    /// Framework-owned action dispatch: every on-screen affordance routes its
    /// [`ContactsAction`](crate::actions::ContactsAction) through here so it
    /// brackets exactly one undo boundary, encodes under
    /// [`CONTACTS_ACTION_TYPE`](crate::actions::CONTACTS_ACTION_TYPE), and replays
    /// byte-identically on peers — the same path Ctrl+K takes.
    actions: crate::resource::ActionSink,

    /// The document heads the in-memory [`ContactStore`] was last derived from.
    /// When the document moves past this out of band — a Ctrl+K palette action,
    /// or a peer's change merged in — the next `sync` re-reads the store.
    model_heads: Vec<ChangeHash>,
    /// Heads produced by local actions whose one-contact store mutations have
    /// already been applied by their UI call sites.  `sync` can safely take
    /// the incremental retained-index path only for this exact generation;
    /// any unseen peer/bulk change falls back to authoritative reconciliation.
    local_projection_heads: Option<Vec<ChangeHash>>,
    pending_local_changes: Vec<LocalContactChange>,
}

/// The overview header's commands: search, more, and new contact.
fn overview_actions(shared: &Shared) -> Vec<Element> {
    vec![
        Box::new(ui::search::search_button(
            ui::search::SearchRequest::this_app(),
        )),
        Box::new(
            Button::icon_only("contacts.more", Icon::MoreHorizontal, "More actions")
                .on_view(queue(shared, Request::More)),
        ),
        Box::new(
            Button::icon_only("contacts.new", Icon::Plus, "New contact")
                .on_view(queue(shared, Request::NewContact)),
        ),
    ]
}

/// The first widget in `root`'s tree with `id`, depth first.
fn find_rect(root: &dyn Widget, id: &str) -> Option<Rect> {
    if root.test_id() == Some(id) {
        return Some(root.get_rect());
    }
    let mut found = None;
    root.for_each_child(&mut |child| {
        if found.is_none() {
            found = find_rect(child, id);
        }
    });
    found
}

impl ContactsApp {
    /// The live app. Never fabricates sample content.
    ///
    /// Nothing here touches the disk: the address book lives in the shared
    /// Automerge document, which the launcher opens through `ui::persist`
    /// and writes back on autosave.
    pub fn new() -> Self {
        Self::configured(false)
    }

    /// The hermetic `--shots` / test app: seeds the varied sample address book
    /// and never touches the user's disk, so a headless frame is populated and
    /// tests are deterministic.
    pub fn sample() -> Self {
        Self::configured(true)
    }

    fn configured(seed_samples: bool) -> Self {
        let store = ContactStore::new();
        let shared: Shared = std::rc::Rc::new(std::cell::RefCell::new(Bridge::default()));
        let destination = LocalState::new(Some(ALL_DESTINATION.to_owned()));
        let shell = NavShell::new()
            .with_id("contacts.shell")
            .sidebar(Box::new(Self::sidebar_list(&[], &destination)))
            .overview(Box::new(ContactList::new(std::rc::Rc::clone(&shared))))
            .content(Box::new(EmptyPane::new(
                std::rc::Rc::clone(&shared),
                "No contact selected",
                None,
            )))
            .overview_header("All contacts", overview_actions(&shared));
        Self {
            seed_samples,
            search_provider: crate::search_provider::ContactSearchProvider::new(&store),
            store,
            resource_index: ContactsResourceIndex::default(),
            search_heads: Vec::new(),
            #[cfg(test)]
            resource_snapshot_materialized: std::cell::Cell::new(0),
            #[cfg(test)]
            resource_exact_materialized: std::cell::Cell::new(0),
            #[cfg(test)]
            resource_index_rebuilds: std::cell::Cell::new(0),
            #[cfg(test)]
            local_change_index_lookups: std::cell::Cell::new(0),
            selected: None,
            mode: Mode::View,
            form: None,
            form_epoch: 0,
            sort: SortKey::Name,
            group_filter: None,
            toast: None,
            confirm_delete: None,
            duplicate_review: None,
            duplicate_candidate: None,
            duplicate_candidate_heads: Vec::new(),
            export: None,
            pending_switch: None,
            rect: Rect::ZERO,
            shell,
            shared,
            destination,
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
            import_rx: None,
            import_parse_rx: None,
            import_generation: 0,
            export_rx: None,
            pending_export: None,
            resource_invocation: None,
            completed_resource_invocation: None,
            import_conflicts: Vec::new(),
            pending_conflict_resolution: None,
            actions: crate::resource::ActionSink::default(),
            model_heads: Vec::new(),
            local_projection_heads: None,
            pending_local_changes: Vec::new(),
        }
    }

    /// Stage a capture scenario: it is applied once the sample book is
    /// seeded, on the first `sync`.
    pub fn stage_scenario(&mut self, name: &str) {
        self.scenario = Some(name.to_owned());
    }

    /// Bring the selected person to the front of a narrow window.
    pub fn shots_show_detail(&mut self) {
        self.show_pane = Some(NavPane::Content);
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
            "more" => self.pending_menu = Some(PendingMenu::More),
            "confirm" => self.confirm_delete = self.selected,
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

    /// A review of two cards for the same person that disagree on several
    /// fields. Only the hermetic capture store changes; nothing is written
    /// to the document.
    fn stage_duplicate_review(&mut self) {
        let Some(survivor) = self.selected.and_then(|id| self.store.get(id)).cloned() else {
            return;
        };
        // A second card for the selected person, as an old phone's export
        // would bring in: the same email, older details.
        let mut source = survivor.clone();
        source.id = uuid::Uuid::from_u128(survivor.id.as_u128().wrapping_add(0x100));
        source.org = "Babbage & Co.".into();
        source.title = "Translator".into();
        source.notes = "Translated Menabrea's paper on the Analytical Engine.".into();
        source.birthday = None;
        self.store.upsert(source.clone());
        if let Some(candidate) = crate::actions::duplicate_candidate(&survivor, &source) {
            self.selected = Some(survivor.id);
            self.duplicate_review = Some(DuplicateReview::new(survivor, source, candidate.reasons));
            self.list_stale = true;
        }
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

    fn save_edit(&mut self, doc: &mut AutoCommit) {
        if let Some(mut form) = self.form.take() {
            form.collect();
            let c = form.draft.clone();
            let id = c.id;
            let action = if form.is_new {
                crate::actions::create_contact_action(&c)
            } else {
                crate::actions::patch_contact_action(&form.base, &c)
            };
            match self.try_perform_action(doc, &action) {
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
                }
                Err(error) => {
                    // The draft is intentionally not a durable authority. Put
                    // the document back in charge, then preserve the user's
                    // unsaved text in an editor rebased on the latest record.
                    self.store.contacts = ContactsDoc::list_contacts(doc);
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

    /// Attempt the canonical action route shared with the Ctrl+K palette.
    /// Unlike the compatibility dispatch wrapper, this exposes a rejected
    /// operation so local projection updates cannot falsely claim success.
    fn try_perform_action(
        &mut self,
        doc: &mut AutoCommit,
        action: &crate::actions::ContactsAction,
    ) -> Result<(), ui::tilo_actions::ActionError> {
        let heads_before = doc.get_heads();
        let local_change = (heads_before == self.model_heads)
            .then(|| self.local_change_for(action))
            .flatten();
        let outcome = self.actions.try_dispatch_with_context(
            doc,
            crate::actions::CONTACTS_ACTION_TYPE,
            action,
            crate::actions::history_context(action),
        );
        if outcome.is_ok()
            && let Some(change) = local_change
        {
            self.pending_local_changes.push(change);
            self.local_projection_heads = Some(doc.get_heads());
        }
        outcome
    }

    /// Show the address book the framework resource already holds.
    /// An empty projection does not wipe contacts that have not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::ContactsState) {
        if state.contacts.is_empty() {
            return;
        }
        let keep: std::collections::HashSet<_> =
            state.contacts.iter().map(|contact| contact.id).collect();
        let stale_ids: Vec<_> = self
            .store
            .contacts
            .iter()
            .map(|contact| contact.id)
            .filter(|id| !keep.contains(id))
            .collect();
        for id in stale_ids {
            self.store.delete(id);
        }
        for contact in &state.contacts {
            self.store.upsert(contact.clone());
        }
        self.list_stale = true;
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Contacts>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    fn local_change_for(
        &self,
        action: &crate::actions::ContactsAction,
    ) -> Option<LocalContactChange> {
        use crate::actions::ContactsAction;

        let id = match action {
            ContactsAction::CreateContact(data) => data.id.parse().ok()?,
            ContactsAction::PatchContact { id, .. }
            | ContactsAction::DeleteContact { id }
            | ContactsAction::SetFavorite { id, .. } => id.parse().ok()?,
            // A merge changes two projections. Let the next normal document
            // sync reconcile both together rather than risking a one-sided
            // optimistic list update.
            ContactsAction::MergeContacts { .. } => {
                return None;
            }
        };
        let before = self.indexed_contact(id).cloned();
        if matches!(action, ContactsAction::CreateContact(_)) || before.is_some() {
            Some(LocalContactChange { id, before })
        } else {
            // The document knew a row the UI did not: retain the authoritative
            // reconciliation path rather than trusting an incomplete delta.
            None
        }
    }

    /// Restore the local projection from the durable source after a rejected
    /// collaborative operation, and tell both sighted and assistive users what
    /// actually happened.
    fn reconcile_refused_action(
        &mut self,
        doc: &AutoCommit,
        operation: &str,
        error: &ui::tilo_actions::ActionError,
    ) {
        self.store.contacts = ContactsDoc::list_contacts(doc);
        if self.selected.is_some_and(|id| self.store.get(id).is_none()) {
            self.selected = self.visible_ids().first().copied();
        }
        self.show_toast(format!("Couldn't {operation}: {error}"));
        self.list_stale = true;
        self.dirty = true;
    }

    /// Toggle the favourite flag through the sole published action route.
    /// Every header, semantic, keyboard, and menu affordance calls this so a
    /// stale local contact cannot mutate the projection after a refusal.
    fn toggle_favorite(&mut self, doc: &mut AutoCommit, id: ContactId) -> bool {
        let Some(favorite) = self.indexed_contact(id).map(|contact| !contact.favorite) else {
            return false;
        };
        let action = crate::actions::set_favorite_action(id.to_string(), favorite);
        match self.try_perform_action(doc, &action) {
            Ok(()) => {
                if let Some(contact) = self
                    .resource_index
                    .contacts
                    .get(&id)
                    .copied()
                    .and_then(|position| self.store.contacts.get_mut(position))
                    .filter(|contact| contact.id == id)
                {
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
        doc: &mut AutoCommit,
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
            let action = crate::actions::create_contact_action(&contact);
            self.try_perform_action(doc, &action)
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

    /// The last action dispatched, for the `affordance == ActionSpec` covenant
    /// tests.
    #[cfg(test)]
    fn last_dispatched(&self) -> Option<&ui::tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    fn rebuild_resource_index(&mut self) {
        #[cfg(test)]
        self.resource_index_rebuilds
            .set(self.resource_index_rebuilds.get().saturating_add(1));
        let mut index = ContactsResourceIndex::default();
        for (position, contact) in self.store.contacts.iter().enumerate() {
            index.contacts.insert(contact.id, position);
            index
                .ordered_contacts
                .insert(ContactOrder::for_contact(contact));
        }
        let mut all_groups = BTreeSet::new();
        for contact in &self.store.contacts {
            update_resource_groups(&mut index, contact, true, &mut all_groups);
        }
        refresh_resource_group_labels(&mut index, all_groups);
        self.resource_index = index;
    }

    fn apply_local_resource_change(&mut self, before: Option<&Contact>, after: Option<&Contact>) {
        if let Some(before) = before {
            self.resource_index
                .ordered_contacts
                .remove(&ContactOrder::for_contact(before));
            if after.is_none() {
                // `ContactStore::delete` compacts the source vector, so only
                // positions after this one need refreshing; no contact data is
                // cloned or sorted.
                if let Some(position) = self.resource_index.contacts.remove(&before.id)
                    && let Some(contacts) = self.store.contacts.get(position..)
                {
                    for (offset, contact) in contacts.iter().enumerate() {
                        self.resource_index
                            .contacts
                            .insert(contact.id, position + offset);
                    }
                }
            }
        }
        if let Some(after) = after {
            let position = self
                .resource_index
                .contacts
                .get(&after.id)
                .copied()
                .or_else(|| {
                    // New contacts append to the authoritative store. Earlier
                    // pending creates have already populated the map, so its
                    // cardinality is the next append position without a scan.
                    let position = self.resource_index.contacts.len();
                    self.store
                        .contacts
                        .get(position)
                        .filter(|contact| contact.id == after.id)
                        .map(|_| position)
                });
            if let Some(position) = position {
                self.resource_index.contacts.insert(after.id, position);
            }
            self.resource_index
                .ordered_contacts
                .insert(ContactOrder::for_contact(after));
        }
        let mut affected = BTreeSet::new();
        if let Some(before) = before {
            update_resource_groups(&mut self.resource_index, before, false, &mut affected);
        }
        if let Some(after) = after {
            update_resource_groups(&mut self.resource_index, after, true, &mut affected);
        }
        refresh_resource_group_labels(&mut self.resource_index, affected);
    }

    fn indexed_contact(&self, id: ContactId) -> Option<&Contact> {
        #[cfg(test)]
        self.local_change_index_lookups
            .set(self.local_change_index_lookups.get().saturating_add(1));
        let position = *self.resource_index.contacts.get(&id)?;
        self.store
            .contacts
            .get(position)
            .filter(|contact| contact.id == id)
    }

    fn apply_pending_local_changes(&mut self) {
        let changes = std::mem::take(&mut self.pending_local_changes);
        for change in changes {
            let after = self.indexed_contact(change.id).cloned();
            self.apply_local_resource_change(change.before.as_ref(), after.as_ref());
            if let Err(error) = self
                .search_provider
                .apply_contact_change(change.before.as_ref(), after.as_ref())
            {
                self.show_toast(format!("Contacts search update failed: {error}"));
                return;
            }
        }
        self.search_heads.clone_from(&self.model_heads);
    }

    fn refresh_search_provider(&mut self) {
        match self.search_provider.refresh(&self.store) {
            Ok(()) => self.search_heads.clone_from(&self.model_heads),
            Err(error) => self.show_toast(format!("Contacts search update failed: {error}")),
        }
    }

    /// Retain one deterministic, reviewable collision per document
    /// generation. This uses the same bounded detector that publishes
    /// canonical duplicate resources, but moves its O(book) work out of the
    /// hot paint and accessibility paths.
    fn refresh_duplicate_candidate(&mut self) {
        if self.duplicate_candidate_heads == self.model_heads {
            return;
        }
        self.duplicate_candidate = crate::actions::duplicate_candidates(&self.store.contacts, 1)
            .into_iter()
            .next();
        self.duplicate_candidate_heads.clone_from(&self.model_heads);
    }

    /// The contextual Ctrl+K resource page is a bounded snapshot. It never
    /// clones/sorts the entire address book merely because the palette opens.
    pub(crate) fn contacts_resource_ctx(&self) -> crate::actions::ContactsResourceCtx {
        #[cfg(test)]
        self.resource_snapshot_materialized.set(0);
        crate::actions::ContactsResourceCtx {
            contacts: self
                .resource_index
                .ordered_contacts
                .iter()
                .take(crate::actions::MAX_CONTEXT_CONTACT_RESOURCES)
                .filter_map(|order| {
                    let id = order.id;
                    let contact = self
                        .resource_index
                        .contacts
                        .get(&id)
                        .and_then(|position| self.store.contacts.get(*position))
                        .filter(|contact| contact.id == id)
                        .cloned();
                    #[cfg(test)]
                    if contact.is_some() {
                        self.resource_snapshot_materialized
                            .set(self.resource_snapshot_materialized.get().saturating_add(1));
                    }
                    contact
                })
                .collect(),
            groups: self
                .resource_index
                .ordered_groups
                .iter()
                .take(crate::actions::MAX_CONTEXT_GROUP_RESOURCES)
                .filter_map(|key| self.resource_index.groups.get(key).cloned())
                .collect(),
            contact_count: self.store.contacts.len(),
            import_busy: self.import_rx.is_some() || self.import_parse_rx.is_some(),
            export_busy: self.export_is_busy(),
            import_conflicts: self
                .import_conflicts
                .iter()
                .map(|conflict| crate::actions::ImportConflictResource {
                    token: conflict.token.to_string(),
                    incoming_name: conflict.incoming.display_name(),
                    existing_name: self
                        .store
                        .get(conflict.existing)
                        .map_or_else(|| "Deleted contact".into(), Contact::display_name),
                })
                .collect(),
            duplicates: crate::actions::duplicate_candidates(
                &self.store.contacts,
                crate::actions::MAX_CONTEXT_DUPLICATE_RESOURCES,
            )
            .into_iter()
            .filter_map(|candidate| {
                let survivor = self.store.get(candidate.survivor_id)?.clone();
                let source = self.store.get(candidate.source_id)?.clone();
                Some(crate::actions::DuplicateResource {
                    candidate,
                    survivor,
                    source,
                })
            })
            .collect(),
        }
    }

    /// Whether an export is being set up or written.
    pub(crate) fn export_is_busy(&self) -> bool {
        self.export.is_some() || self.export_rx.is_some()
    }

    /// Exact resources read one validated contact/group from the authoritative
    /// store via stable-id indices. A stale position is refused instead of
    /// redirecting an action to whatever shifted into that vector slot.
    pub(crate) fn exact_contacts_resource_ctx(
        &self,
        id: &str,
    ) -> Option<crate::actions::ContactsResourceCtx> {
        #[cfg(test)]
        self.resource_exact_materialized.set(0);
        let mut ctx = crate::actions::ContactsResourceCtx {
            contacts: Vec::new(),
            groups: Vec::new(),
            contact_count: self.store.contacts.len(),
            import_busy: self.import_rx.is_some() || self.import_parse_rx.is_some(),
            export_busy: self.export_is_busy(),
            import_conflicts: Vec::new(),
            duplicates: Vec::new(),
        };
        if id == "contacts.book" {
            return Some(ctx);
        }
        if id == crate::actions::FAVORITES_RESOURCE_ID {
            return Some(ctx);
        }
        if let Some(rest) = id.strip_prefix("contacts.duplicate:") {
            let (survivor, source) = rest.split_once(':')?;
            let survivor_id = survivor.parse::<ContactId>().ok()?;
            let source_id = source.parse::<ContactId>().ok()?;
            let survivor = self.store.get(survivor_id)?.clone();
            let source = self.store.get(source_id)?.clone();
            let candidate = crate::actions::duplicate_candidate(&survivor, &source)?;
            ctx.duplicates.push(crate::actions::DuplicateResource {
                candidate,
                survivor,
                source,
            });
            return Some(ctx);
        }
        if let Some(raw) = id.strip_prefix("contacts.contact:") {
            let contact_id = raw.parse::<ContactId>().ok()?;
            if crate::actions::contact_resource_id(contact_id) != id {
                return None;
            }
            let position = *self.resource_index.contacts.get(&contact_id)?;
            let contact = self
                .store
                .contacts
                .get(position)
                .filter(|contact| contact.id == contact_id)?
                .clone();
            #[cfg(test)]
            self.resource_exact_materialized.set(1);
            ctx.contacts.push(contact);
            return Some(ctx);
        }
        if let Some(raw) = id.strip_prefix("contacts.group:") {
            let group = self.resource_index.groups.get(raw)?.clone();
            ctx.groups.push(group);
            return Some(ctx);
        }
        if let Some(token) = id.strip_prefix("contacts.import-conflict:") {
            let conflict = self
                .import_conflicts
                .iter()
                .find(|conflict| conflict.token.to_string() == token)?;
            ctx.import_conflicts
                .push(crate::actions::ImportConflictResource {
                    token: conflict.token.to_string(),
                    incoming_name: conflict.incoming.display_name(),
                    existing_name: self
                        .store
                        .get(conflict.existing)
                        .map_or_else(|| "Deleted contact".into(), Contact::display_name),
                });
            return Some(ctx);
        }
        None
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

    fn delete_selected(&mut self, doc: &mut AutoCommit, id: ContactId) {
        let action = crate::actions::delete_contact_action(id.to_string());
        match self.try_perform_action(doc, &action) {
            Ok(()) => {
                self.store.delete(id);
                if self.form.as_ref().is_some_and(|form| form.draft.id == id) {
                    self.cancel_edit();
                }
                if self.selected == Some(id) {
                    self.selected = self.visible_ids().first().copied();
                }
                self.show_toast("Contact deleted");
            }
            Err(error) => self.reconcile_refused_action(doc, "delete contact", &error),
        }
        self.confirm_delete = None;
        self.list_stale = true;
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
        doc: &mut AutoCommit,
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
                self.confirm_delete = Some(target);
            }
        }
        self.dirty = true;
    }

    fn execute_more_action(&mut self, doc: &AutoCommit, action: MoreAction) {
        match action {
            MoreAction::Import => self.request_import(),
            MoreAction::Export => self.open_export_dialog(),
            MoreAction::ReviewDuplicate => match self.invoke_duplicate_review_operation(doc) {
                Ok(_) => {}
                Err(error) => self.show_toast(format!("Couldn't open the review: {error}")),
            },
            MoreAction::Sort(sort) => {
                self.sort = sort;
                self.list_stale = true;
            }
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
                let commands = Toolbar::new(ui::widget::toolbar::Orientation::Horizontal)
                    .push_toggle("Favorite", favorite, queue(shared, Request::ToggleFavorite))
                    .push_button("Edit", queue(shared, Request::EditSelected))
                    .push_icon(
                        Icon::Trash2,
                        "Delete contact",
                        queue(shared, Request::DeleteSelected),
                    );
                ("", vec![Box::new(commands)])
            }
            HeaderKey::Edit => (
                "",
                vec![
                    Box::new(
                        Button::new("contacts.form.cancel", "Cancel", None)
                            .on_view(queue(shared, Request::Cancel)),
                    ),
                    Box::new(
                        Button::new("contacts.form.save", "Save", None)
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
            })
            .collect();
        let empty_title = match self.group_filter.as_deref() {
            Some(FAVORITES_FILTER) => Some("No favorites yet".to_owned()),
            Some(_) => Some("No one is in this group".to_owned()),
            None => None,
        };
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

    /// Hand the shell and the panes what the window shows now. Cheap when
    /// nothing changed: each part is rebuilt only when what it shows did.
    fn publish(&mut self) {
        if self.list_stale {
            self.publish_list();
        }
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
            let shell = std::mem::take(&mut self.shell);
            self.shell = shell.content_header(title, actions);
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

    /// Open a menu: a contact's actions at `anchor`, or the More menu under
    /// its button.
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
            PendingMenu::More => {
                let importing = self.import_rx.is_some() || self.import_parse_rx.is_some();
                let mut rows = vec![Some(MoreAction::Import), Some(MoreAction::Export)];
                if self.duplicate_candidate.is_some() {
                    rows.push(Some(MoreAction::ReviewDuplicate));
                }
                rows.extend([
                    None,
                    Some(MoreAction::Sort(SortKey::Name)),
                    Some(MoreAction::Sort(SortKey::Org)),
                ]);
                for row in &rows {
                    built.push(match row {
                        Some(MoreAction::Import) => MenuItem::selection(if importing {
                            "Importing…"
                        } else {
                            "Import contacts"
                        })
                        .with_enabled(!importing),
                        Some(MoreAction::Export) => MenuItem::selection("Export contacts")
                            .with_enabled(!self.store.is_empty() && !self.export_is_busy()),
                        Some(MoreAction::ReviewDuplicate) => {
                            MenuItem::selection("Review possible duplicate")
                        }
                        Some(MoreAction::Sort(key)) => MenuItem::selection(match key {
                            SortKey::Name => "Sort by name",
                            SortKey::Org => "Sort by organization",
                        })
                        .with_checked(self.sort == *key),
                        None => MenuItem::separator(),
                    });
                }
                OpenMenu::More(rows)
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
                    let x = row.x0 + row.height();
                    Rect::new(x, row.center().y, x, row.center().y)
                }),
            PendingMenu::More => find_rect(&self.shell, "contacts.more")
                .filter(|rect| rect.width() > 0.0)
                .map(|rect| Rect::new(rect.x0, rect.y1, rect.x1, rect.y1)),
        }
    }

    fn run_menu_choice(&mut self, doc: &mut AutoCommit, index: usize) {
        match self.open_menu.take() {
            Some(OpenMenu::Contact(id, rows)) => {
                if let Some(Some(action)) = rows.get(index) {
                    self.execute_context_action(doc, id, *action);
                }
            }
            Some(OpenMenu::More(rows)) => {
                if let Some(Some(action)) = rows.get(index) {
                    self.execute_more_action(doc, *action);
                }
            }
            None => {}
        }
    }

    /// Turn what the panes and buttons asked for into view changes and
    /// document actions.
    fn apply_requests(&mut self, doc: &mut AutoCommit) {
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

    fn apply_request(&mut self, doc: &mut AutoCommit, request: Request) {
        match request {
            Request::Select(id) => self.request_select(id),
            Request::Edit(id) => {
                if let Some(contact) = self.store.get(id).cloned() {
                    self.selected = Some(id);
                    self.begin_edit(contact, false);
                }
            }
            Request::ContextMenu(id, point) => {
                if self.mode == Mode::Edit && self.form.as_ref().is_some_and(|f| f.draft.id != id) {
                    self.request_select(id);
                    return;
                }
                self.selected = Some(id);
                self.open_menu_at(
                    PendingMenu::Contact(id),
                    Rect::new(point.x, point.y, point.x, point.y),
                );
            }
            Request::NewContact => self.begin_new_contact(),
            Request::More => match self.menu_anchor(PendingMenu::More) {
                Some(anchor) => {
                    self.open_menu_at(PendingMenu::More, anchor);
                    self.menu_follows = Some(PendingMenu::More);
                }
                None => self.pending_menu = Some(PendingMenu::More),
            },
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
            Request::DeleteSelected => self.confirm_delete = self.selected,
            Request::Save => self.save_edit(doc),
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
                    form.calendar_open = false;
                }
            }
            Request::OpenWebsite(url) => {
                #[cfg(not(test))]
                ui::widget::open_external_url(&url);
                self.opened_url = Some(url);
            }
            Request::Dialog(choice) => self.answer_dialog(doc, choice),
        }
        self.dirty = true;
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
                self.confirm_delete = self.selected;
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
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource,
        dyn ui::widget::controls::ActionControls
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::CONTACTS_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        {
            let (doc, _) = ui::doc::document(state, path);
            self.sync_document(doc);
            self.poll_destination();
            self.apply_requests(doc);
            self.publish();
        }
        self.shell.sync(state, path);
        if let Some((_, dialog)) = &mut self.dialog {
            dialog.sync(state, path);
        }
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
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
        let node = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_grow: 1.0,
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &children,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
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
            self.menu.draw(scene, transform, tr, theme, presence);
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
            let choice = self.menu.take_selection();
            let (doc, _) = ui::doc::document(ctx.state, ctx.path);
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
            let (doc, _) = ui::doc::document(ctx.state, ctx.path);
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
            let (doc, _) = ui::doc::document(ctx.state, ctx.path);
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
        let (doc, _) = ui::doc::document(ctx.state, ctx.path);
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

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let started = if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            Some(started)
        } else {
            self.answer_resource_operation(
                request.kind,
                request.type_id,
                request.payload,
                doc,
                path,
            )?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            })
        };
        self.publish();
        Ok(started)
    }

    fn poll_resource_operation(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<ui::tilo_actions::InvocationUpdate> {
        if cancellation_requested && self.resource_invocation == Some(invocation) {
            self.resource_invocation = None;
            self.import_rx = None;
            self.import_parse_rx = None;
            let exhausted = self.advance_import_generation().is_err();
            self.export_rx = None;
            self.pending_export = None;
            if exhausted {
                self.show_toast("Import cancelled. Restart Contacts to import again.");
            }
            return Some(ui::tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some(
                    if exhausted {
                        "contact file operation cancelled; import generation exhausted"
                    } else {
                        "contact file operation cancelled"
                    }
                    .into(),
                ),
            });
        }
        if self
            .completed_resource_invocation
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            return self.completed_resource_invocation.take();
        }
        None
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

impl ContactsApp {
    /// Fold the document into the working book: seed a hermetic book once,
    /// follow out-of-band changes (the palette, a collaborator), rebase an
    /// open form onto them, and keep search and duplicate review current.
    fn sync_document(&mut self, state: &mut AutoCommit) {
        if let Some((token, import_as_new)) = self.pending_conflict_resolution.take()
            && let Err(error) = self.resolve_import_conflict(state, &token, import_as_new)
        {
            self.show_toast(format!("Couldn't resolve the import: {error}"));
        }
        // Drain any in-flight file-chooser portal request first, so a chosen
        // import/export lands in the book before this frame lays out.
        self.drain_portal(state);
        if !self.seeded {
            let existing = ContactsDoc::list_contacts(state);
            let mut prefer = None;
            if !existing.is_empty() {
                // The host handed us a document already carrying contacts
                // (a restored session or a collaborator's shared book).
                for c in existing {
                    self.store.upsert(c);
                }
            } else if self.seed_samples {
                // Hermetic `--shots` / test path only: populate a varied book
                // and open onto its first, richly-filled record.
                for c in sample_contacts() {
                    if prefer.is_none() {
                        prefer = Some(c.id);
                    }
                    ContactsDoc::upsert_contact(state, &c);
                    self.store.upsert(c);
                }
            }
            if self.selected.is_none() {
                self.selected = prefer.or_else(|| self.visible_ids().first().copied());
            }
            self.seeded = true;
            self.model_heads = state.get_heads();
            self.rebuild_resource_index();
            self.list_stale = true;
            self.dirty = true;
            if let Some(scenario) = self.scenario.take() {
                self.stage(&scenario);
            }
        }

        // Reconcile the in-memory store from the document when it has moved past
        // what the store was last derived from — a Ctrl+K palette action or a
        // merged peer change mutates only the document, so the list would
        // otherwise miss it. An on-screen affordance already updates the store
        // eagerly, so this is a no-op for it beyond re-adopting the same records.
        let heads = state.get_heads();
        if heads != self.model_heads {
            if self.local_projection_heads.as_ref() == Some(&heads) {
                self.model_heads = heads;
                self.local_projection_heads = None;
                self.apply_pending_local_changes();
            } else {
                self.pending_local_changes.clear();
                self.local_projection_heads = None;
                self.store.contacts = ContactsDoc::list_contacts(state);
                self.model_heads = heads;
                self.rebuild_resource_index();
                if let Some(sel) = self.selected
                    && self.store.get(sel).is_none()
                {
                    self.selected = self.visible_ids().first().copied();
                }
            }
            self.list_stale = true;
            self.dirty = true;
            let mut lost = false;
            if let Some(form) = &mut self.form {
                if let Some(canonical) = self.store.get(form.draft.id) {
                    if form.reconcile(canonical) {
                        self.form_epoch = self.form_epoch.wrapping_add(1);
                    }
                } else if !form.is_new {
                    lost = true;
                }
            }
            if lost {
                let keep = self.form.as_mut().is_some_and(EditForm::has_local_edits);
                if keep {
                    self.show_toast("This contact was deleted elsewhere. Your edits are kept.");
                } else {
                    self.cancel_edit();
                }
            }
        }

        crate::priority_contacts::publish_if_changed(&self.store);

        if self.search_heads != self.model_heads {
            self.refresh_search_provider();
        }
        self.refresh_duplicate_candidate();
    }
}

impl ui::search::capability::SearchSource for ContactsApp {
    /// Fold the address book into the unified Ctrl+K palette: a snapshot
    /// [`ContactSearchProvider`](crate::search_provider::ContactSearchProvider)
    /// makes every contact findable there, alongside any actions the app
    /// exports.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_provider.clone())]
    }
}

impl ui::resources::PublishesResources for ContactsApp {
    /// Publish the open book + its contacts so the host's Ctrl+K action palette
    /// scopes to them. When the framework hosts the surface,
    /// [`tilo_resource::publish`] already owns the address-book door; only
    /// live extras (import conflicts, pending merges) remain here.
    fn resources(&self, doc: &AutoCommit) -> Vec<ui::tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }
    /// Resolve contacts outside the contextual palette page. This is the
    /// other half of bounded discovery: a search hit for contact 65,000 still
    /// reaches the exact current resource/action catalogue, while a deleted
    /// contact has no resource for the broker to invoke.
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<ui::tilo_actions::Resource> {
        if self.is_hosted() && id == "contacts.book" {
            return None;
        }
        self.publish_resource(doc, id)
    }
}

impl ui::history_mode::capability::DocumentHistory for ContactsApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
}

impl ui::undo::CommittedActions for ContactsApp {
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its contacts action, so it records one undo boundary for it (see
    /// [`Self::try_perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.take_committed()
    }
}

impl ui::widget::controls::ActionControls for ContactsApp {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        ui::widget::controls::take_control_invocation(&mut self.shell)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests;
