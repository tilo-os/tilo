//! Root widget for the Contacts app.
//!
//! A self-laid-out master/detail widget. The left pane is a searchable,
//! sortable, group-filterable list; the right pane shows the selected
//! contact's detail or an editing form. State lives in the Automerge
//! document via [`ContactsDoc`]; the in-memory [`ContactStore`] is the
//! working mirror that the UI reads and writes.
//!
//! Layout is computed manually in [`Widget::apply_layout`] (no taffy
//! child tree) so the dynamic, repeated-field form and the scrollable
//! list can be driven precisely; embedded [`TextInput`]s are positioned
//! with `apply_layout_external` and routed events directly.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ChangeHash, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::icons::Icon;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::tokens::colors::ON_FILL_MIN_CONTRAST;
use ui::widget::{
    Avatar, Button, ButtonFace, ButtonStyle, Checkbox, CheckboxFace, Chip, ChipLead, ChipStyle,
    Link, TextInput, Widget,
};

use crate::date;
use crate::doc::ContactsDoc;
use crate::model::{
    Address, Birthday, Contact, ContactId, ContactStore, Labeled, SortKey, sample_contacts,
};
use crate::portal::{self, ExportOutcome, ImportOutcome};
use crate::responsive::{Pane, PushNav};
use crate::vcard::{self, VCardVersion};

/// Suggested file name for the export Save dialog.
const EXPORT_FILE_NAME: &str = "contacts-export.vcf";

const TOOLBAR_H: f64 = 48.0;
const ROW_H: f64 = 62.0;

/// How long an action toast ("Saved", "Contact deleted", …) stays on screen
/// before it fades away on its own. The host loop is kept awake (via
/// [`Widget::needs_animation_frame`]) only while a toast is up, so the frame
/// where this elapses clears it and the loop returns to idle.
const TOAST_LIFETIME: Duration = Duration::from_millis(2600);
const DIVIDER_W: f64 = 6.0;
const PAD: f64 = 16.0;
const FIELD_H: f64 = 38.0;

/// Edit-form header band geometry. The header is two stacked rows so the
/// identity block and the Favorite control never overlap: row 1 is the
/// identity anchor (an avatar + the contact's name, mirroring the read view so
/// the switch into editing reads as the *same* record, not a new screen), row 2
/// holds the Favorite pill and the Save/Cancel buttons. `layout_form` and
/// `draw_form` both derive their offsets from these so the hit rects and the
/// painted content stay aligned.
const FORM_TITLE_TOP: f64 = 16.0; // identity row top, relative to detail pane top
const FORM_TITLE_H: f64 = 44.0; // identity row height (fits the avatar chip)
const FORM_ROW_GAP: f64 = 12.0; // vertical gap between header rows / before body

/// Avatar chip diameter in the edit-form identity row.
const FORM_AVATAR: f64 = 40.0;

/// Type labels offered in the phone/email/address chip menu.
const TYPE_OPTIONS: &[&str] = &["home", "work", "mobile", "main", "other"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    View,
    Edit,
}

/// Identifies which draft field an embedded [`TextInput`] maps to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slot {
    First,
    Last,
    Org,
    Title,
    Website,
    Birthday,
    Notes,
    Groups,
    Phone(usize),
    Email(usize),
    AddrStreet(usize),
    AddrCity(usize),
    AddrRegion(usize),
    AddrPostal(usize),
    AddrCountry(usize),
}

/// Direction of an arrow-key move between edit-form fields.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FieldDir {
    Up,
    Down,
    Left,
    Right,
}

/// The focus-search chord: `true` for Ctrl/Cmd + K (or Ctrl/Cmd + `/`). Both
/// are the conventional "jump to search" shortcut, wired here to focus this
/// app's own list search field (bare `/` does the same). Kept as a standalone
/// predicate so the production path and the test share one decision point.
fn is_focus_search_chord(ctrl: bool, key: &Key) -> bool {
    if !ctrl {
        return false;
    }
    matches!(key, Key::Character(s) if matches!(s.as_str(), "k" | "K" | "/"))
}

/// Placeholder hint shown in an empty, unfocused form field. The `ui`
/// `TextInput` in this toolkit build has no `with_placeholder` builder, so —
/// matching the convention used by the search box, dropdown, and command
/// palette — the parent draws the dim hint itself when the field is empty
/// and unfocused. Centralised here so every field gets one.
fn placeholder_for(slot: Slot) -> &'static str {
    match slot {
        Slot::First => "First name",
        Slot::Last => "Last name",
        Slot::Org => "Company or organization",
        Slot::Title => "Job title",
        Slot::Website => "https://example.com",
        Slot::Birthday => "YYYY-MM-DD",
        Slot::Notes => "Add a note…",
        Slot::Groups => "Work, Friends, Family…",
        Slot::Phone(_) => "Phone number",
        Slot::Email(_) => "name@example.com",
        Slot::AddrStreet(_) => "Street address",
        Slot::AddrCity(_) => "City",
        Slot::AddrRegion(_) => "State / region",
        Slot::AddrPostal(_) => "Postal code",
        Slot::AddrCountry(_) => "Country",
    }
}

struct FieldInput {
    slot: Slot,
    ti: TextInput,
}

/// The live editing surface: a draft contact plus the text inputs that
/// mirror it. Type chips live on `draft` directly (set via popup).
struct EditForm {
    draft: Contact,
    is_new: bool,
    inputs: Vec<FieldInput>,
}

impl EditForm {
    fn new(draft: Contact, is_new: bool) -> Self {
        let mut inputs = vec![
            field(Slot::First, &draft.first),
            field(Slot::Last, &draft.last),
            field(Slot::Org, &draft.org),
            field(Slot::Title, &draft.title),
        ];
        for (i, p) in draft.phones.iter().enumerate() {
            inputs.push(field(Slot::Phone(i), &p.value));
        }
        for (i, e) in draft.emails.iter().enumerate() {
            inputs.push(field(Slot::Email(i), &e.value));
        }
        for (i, a) in draft.addresses.iter().enumerate() {
            inputs.push(field(Slot::AddrStreet(i), &a.street));
            inputs.push(field(Slot::AddrCity(i), &a.city));
            inputs.push(field(Slot::AddrRegion(i), &a.region));
            inputs.push(field(Slot::AddrPostal(i), &a.postal));
            inputs.push(field(Slot::AddrCountry(i), &a.country));
        }
        inputs.push(field(
            Slot::Birthday,
            &draft.birthday.map(|b| b.iso()).unwrap_or_default(),
        ));
        inputs.push(field(Slot::Website, &draft.website));
        inputs.push(field(Slot::Notes, &draft.notes));
        inputs.push(field(Slot::Groups, &draft.groups.join(", ")));
        Self {
            draft,
            is_new,
            inputs,
        }
    }

    /// Pull every input's live value back into the draft.
    fn collect(&mut self) {
        for f in &self.inputs {
            let v = f.ti.value().to_string();
            match f.slot {
                Slot::First => self.draft.first = v,
                Slot::Last => self.draft.last = v,
                Slot::Org => self.draft.org = v,
                Slot::Title => self.draft.title = v,
                Slot::Website => self.draft.website = v,
                // Parse the typed birthday into a real date. Unparseable /
                // empty text clears it rather than storing junk.
                Slot::Birthday => self.draft.birthday = Birthday::parse(&v),
                Slot::Notes => self.draft.notes = v,
                Slot::Groups => {
                    self.draft.groups = v
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                Slot::Phone(i) => {
                    if let Some(p) = self.draft.phones.get_mut(i) {
                        p.value = v;
                    }
                }
                Slot::Email(i) => {
                    if let Some(e) = self.draft.emails.get_mut(i) {
                        e.value = v;
                    }
                }
                Slot::AddrStreet(i) => set_addr(&mut self.draft, i, |a| &mut a.street, v),
                Slot::AddrCity(i) => set_addr(&mut self.draft, i, |a| &mut a.city, v),
                Slot::AddrRegion(i) => set_addr(&mut self.draft, i, |a| &mut a.region, v),
                Slot::AddrPostal(i) => set_addr(&mut self.draft, i, |a| &mut a.postal, v),
                Slot::AddrCountry(i) => set_addr(&mut self.draft, i, |a| &mut a.country, v),
            }
        }
    }

    fn any_focused(&self) -> bool {
        self.inputs.iter().any(|f| f.ti.is_focused())
    }

    fn clear_focus(&mut self) {
        for f in &mut self.inputs {
            f.ti.set_focus(false);
        }
    }
}

fn set_addr(c: &mut Contact, i: usize, pick: impl Fn(&mut Address) -> &mut String, v: String) {
    if let Some(a) = c.addresses.get_mut(i) {
        *pick(a) = v;
    }
}

/// Pick the input (by index into the form's `inputs`) whose laid-out rect is
/// the nearest neighbour of `from` in `dir`. Candidates must lie strictly in
/// that direction; ties break toward the best axis alignment (primary-axis
/// distance plus a perpendicular penalty), so Up/Down prefer a vertically
/// aligned field and Left/Right the field beside it on the same row. Returns
/// `None` when nothing lies that way (already at the edge).
fn best_field_in_direction(
    from: Rect,
    dir: FieldDir,
    rects: &[(usize, Rect)],
    cur_idx: usize,
) -> Option<usize> {
    let fc = from.center();
    let mut best: Option<(f64, usize)> = None;
    for &(idx, r) in rects {
        if idx == cur_idx {
            continue;
        }
        let c = r.center();
        let in_dir = match dir {
            FieldDir::Up => c.y < fc.y - 1.0,
            FieldDir::Down => c.y > fc.y + 1.0,
            FieldDir::Left => c.x < fc.x - 1.0,
            FieldDir::Right => c.x > fc.x + 1.0,
        };
        if !in_dir {
            continue;
        }
        let (primary, perp) = match dir {
            FieldDir::Up | FieldDir::Down => ((c.y - fc.y).abs(), (c.x - fc.x).abs()),
            FieldDir::Left | FieldDir::Right => ((c.x - fc.x).abs(), (c.y - fc.y).abs()),
        };
        let score = primary + perp * 2.0;
        if best.is_none_or(|(bs, _)| score < bs) {
            best = Some((score, idx));
        }
    }
    best.map(|(_, i)| i)
}

/// Build a single form field input seeded with `value`.
fn field(slot: Slot, value: &str) -> FieldInput {
    FieldInput {
        slot,
        ti: TextInput::local().with_initial_value(value),
    }
}

/// Floating overlay state (only one open at a time).
enum Popup {
    /// Phone/email/address type chip menu.
    TypeMenu { target: TypeTarget, anchor: Rect },
    /// Group filter selector in the list header.
    GroupFilter { anchor: Rect },
    /// Birthday calendar.
    Calendar { year: i32, month: u32, anchor: Rect },
    /// Right-click menu on a contact row (Edit / Export / Delete).
    ContextMenu { target: ContactId, anchor: Rect },
}

/// Stable identifiers for the per-contact right-click context-menu items. The
/// visible label for [`ContextAction::Favorite`] depends on the contact's
/// current favorite state, so the menu is built per-contact at open time via
/// [`ContactsApp::context_actions`] rather than from a single static list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ContextAction {
    Edit,
    Favorite,
    Export,
    Delete,
}

impl ContextAction {
    /// The user-facing label. The Favorite entry reads "Add to favorites" or
    /// "Remove from favorites" depending on the contact's current state.
    fn label(self, is_favorite: bool) -> &'static str {
        match self {
            ContextAction::Edit => "Edit",
            ContextAction::Favorite => {
                if is_favorite {
                    "Remove from favorites"
                } else {
                    "Add to favorites"
                }
            }
            ContextAction::Export => "Export",
            ContextAction::Delete => "Delete",
        }
    }
}

/// Order of items in the per-contact right-click context menu.
const CONTEXT_ACTIONS: &[ContextAction] = &[
    ContextAction::Edit,
    ContextAction::Favorite,
    ContextAction::Export,
    ContextAction::Delete,
];

#[derive(Clone, Copy)]
enum TypeTarget {
    Phone(usize),
    Email(usize),
    Addr(usize),
}

/// Named hit rects recomputed every `apply_layout`, tested on click.
#[derive(Default, Clone)]
struct Hit {
    import: Rect,
    export: Rect,
    new: Rect,
    search: Rect,
    sort: Rect,
    group: Rect,
    // detail/view actions
    fav: Rect,
    edit: Rect,
    delete: Rect,
    // narrow single-pane push: Back affordance at the top of the detail pane
    back: Rect,
    // form actions
    save: Rect,
    cancel: Rect,
    add_phone: Rect,
    add_email: Rect,
    add_address: Rect,
    cal_btn: Rect,
    fav_toggle: Rect,
    // per-row dynamic buttons
    type_chips: Vec<(TypeTarget, Rect)>,
    remove_btns: Vec<(TypeTarget, Rect)>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across unrelated concerns (divider drag/hover, narrow-layout mode, dirty/relayout/seeded frame bookkeeping, and the demo-only sample-seeding flag), so grouping them would need a real design pass rather than a mechanical lint fix"
)]
pub struct ContactsApp {
    store: ContactStore,
    selected: Option<ContactId>,
    mode: Mode,
    form: Option<EditForm>,

    search: TextInput,
    sort: SortKey,
    group_filter: Option<String>,

    popup: Option<Popup>,
    toast: Option<(String, Instant)>,
    confirm_delete: Option<ContactId>,

    /// Open export-selection dialog: the set of contact ids currently ticked
    /// for export. `Some` ⇒ the modal is showing.
    export_sel: Option<std::collections::HashSet<ContactId>>,
    /// Scroll offset of the export dialog's contact list.
    export_scroll: f64,
    /// When switching to another contact would lose unsaved edits, the
    /// target id parks here while the "Save changes?" modal is shown.
    pending_switch: Option<ContactId>,

    // geometry
    rect: Rect,
    split_pos: f64,
    list_scroll: f64,
    detail_scroll: f64,
    divider_drag: bool,
    divider_hover: bool,

    list_rect: Rect,
    detail_rect: Rect,
    list_body: Rect,
    list_rows: Vec<(ContactId, Rect)>,
    hit: Hit,
    detail_content_h: f64,
    /// View-mode detail field value hit rects, recomputed every `draw_detail`.
    /// Double-clicking one jumps straight into editing that field.
    detail_field_hits: Vec<(Slot, Rect)>,
    /// The clickable website link shown in the view-mode detail pane, kept
    /// across frames so its hover/press state persists. Rebuilt when the
    /// selected contact's website changes; `None` when the contact has none.
    website_link: Option<Link>,
    /// Time + position of the last left-press in the detail pane, for
    /// double-click detection on field values.
    last_detail_click: Option<(Instant, Point)>,

    // Responsiveness: single-pane master-detail push below the breakpoint.
    push: PushNav,
    /// Whether the last layout resolved to the narrow single-pane mode.
    narrow: bool,

    last_mouse: Point,
    dirty: bool,
    relayout: bool,
    seeded: bool,

    /// Where the address book is persisted. `Some` for the live app (loaded
    /// on first `sync`, rewritten after every mutation); `None` for the
    /// hermetic `--shots` / test path so those never touch the user's disk.
    /// Legacy address-book file to seed from. `None` in production — the
    /// document arrives already loaded from the shared store.
    legacy_book: Option<PathBuf>,
    /// When `true`, a first `sync` onto an empty book seeds the varied sample
    /// address book. Only the `--shots` and test constructors set this — the
    /// live app never fabricates content.
    seed_samples: bool,

    /// In-flight import file-chooser: the portal worker's receiver, drained
    /// once per frame. `Some` ⇒ the host's multi-select Open dialog is up.
    import_rx: Option<portal::ImportChannel>,
    /// In-flight export Save dialog: the portal worker's receiver.
    export_rx: Option<portal::ExportChannel>,
    /// The contacts + vCard version parked while the export Save dialog is up,
    /// written to the chosen destination when the portal returns a path.
    pending_export: Option<(Vec<ContactId>, VCardVersion)>,

    /// Framework-owned action dispatch: every on-screen affordance routes its
    /// [`ContactsAction`](crate::actions::ContactsAction) through here so it
    /// brackets exactly one undo boundary, encodes under
    /// [`CONTACTS_ACTION_TYPE`](crate::actions::CONTACTS_ACTION_TYPE), and replays
    /// byte-identically on peers — the same path Ctrl+K takes.
    actions: ui::ActionHost,

    /// The document heads the in-memory [`ContactStore`] was last derived from.
    /// When the document moves past this out of band — a Ctrl+K palette action,
    /// or a peer's change merged in — the next `sync` re-reads the store so those
    /// mutations show up in the list, exactly as an on-screen edit already does.
    model_heads: Vec<ChangeHash>,
}

impl ContactsApp {
    /// The live app. Never fabricates sample content.
    ///
    /// Nothing here touches the disk any more: the address book lives in the
    /// shared Automerge document, which `main` opens through `ui::persist`
    /// and which is written back by `App::with_autosave`. The book this app
    /// used to keep in its own file is imported once, on first launch after
    /// the upgrade, by `ui::persist::open_migrating`.
    pub fn new() -> Self {
        Self::configured(None, false)
    }

    /// The hermetic `--shots` / test app: seeds the varied sample address book
    /// and never touches the user's disk, so a headless frame is populated and
    /// tests are deterministic.
    pub fn sample() -> Self {
        Self::configured(None, true)
    }

    /// An app that seeds itself from a legacy address-book file at `path`.
    ///
    /// Test-only. The legacy import in production runs one level up, in
    /// `ui::persist::open_migrating`, against the whole document rather than
    /// a projection of the contact list.
    #[cfg(test)]
    pub fn with_legacy_book(path: PathBuf) -> Self {
        Self::configured(Some(path), false)
    }

    fn configured(legacy_book: Option<PathBuf>, seed_samples: bool) -> Self {
        Self {
            legacy_book,
            seed_samples,
            store: ContactStore::new(),
            selected: None,
            mode: Mode::View,
            form: None,
            search: TextInput::local().with_id("contacts_search"),
            sort: SortKey::Name,
            group_filter: None,
            popup: None,
            toast: None,
            confirm_delete: None,
            export_sel: None,
            export_scroll: 0.0,
            pending_switch: None,
            rect: Rect::ZERO,
            split_pos: 0.34,
            list_scroll: 0.0,
            detail_scroll: 0.0,
            divider_drag: false,
            divider_hover: false,
            list_rect: Rect::ZERO,
            detail_rect: Rect::ZERO,
            list_body: Rect::ZERO,
            list_rows: Vec::new(),
            hit: Hit::default(),
            detail_content_h: 0.0,
            detail_field_hits: Vec::new(),
            website_link: None,
            last_detail_click: None,
            push: PushNav::default(),
            narrow: false,
            last_mouse: Point::ZERO,
            dirty: true,
            relayout: true,
            seeded: false,
            import_rx: None,
            export_rx: None,
            pending_export: None,
            actions: ui::ActionHost::new(),
            model_heads: Vec::new(),
        }
    }

    /// Deterministically snap the narrow single-pane push flow to the
    /// detail pane (with its Back affordance) for `--shots`, skipping the
    /// slide animation so a headless frame captures the settled pushed
    /// detail rather than one mid-transition.
    pub fn shots_show_detail(&mut self) {
        self.push.snap(Pane::Detail);
    }

    /// Snap into the New Contact form for `--shots` (placeholders + the
    /// repeated phone/email rows visible). Mirrors the `+ New` click. The
    /// form lives in the detail pane, so we also snap the responsive push to
    /// the detail pane — harmless on a wide viewport (the push is inert when
    /// docked side by side), and on a narrow viewport it captures the pushed
    /// form rather than the list.
    pub fn shots_new_contact(&mut self) {
        let mut c = Contact::new();
        c.phones.push(Labeled::new("mobile", ""));
        c.emails.push(Labeled::new("home", ""));
        self.begin_edit(c, true);
        self.push.snap(Pane::Detail);
    }

    /// Snap the EDIT form open on the selected contact for `--shots`, so a
    /// headless frame captures the edit view (title header row + Favorite
    /// control + Save/Cancel) for visual inspection of the header layout.
    pub fn shots_edit_contact(&mut self) {
        if let Some(c) = self.selected.and_then(|id| self.store.get(id)).cloned() {
            self.begin_edit(c, false);
            self.push.snap(Pane::Detail);
        }
    }

    /// Snap the EDIT form open with its first field focused for `--shots`, so a
    /// headless frame captures the keyboard-navigation focus ring on the first
    /// field (where opening the editor lands the keyboard).
    pub fn shots_edit_focused(&mut self) {
        if let Some(c) = self.selected.and_then(|id| self.store.get(id)).cloned() {
            self.begin_edit(c, false);
            // The form opens unfocused; land the keyboard on the first field so
            // the shot captures the focus ring where Tab-navigation begins.
            self.focus_form_slot(Slot::First);
            self.push.snap(Pane::Detail);
        }
    }

    /// Snap the export-selection dialog open for `--shots`.
    pub fn shots_show_export(&mut self) {
        self.open_export_dialog();
    }

    /// Snap the unsaved-changes modal open for `--shots`: enter edit on the
    /// selected contact, dirty a field, then attempt to switch to another
    /// contact so the guard fires.
    pub fn shots_show_unsaved(&mut self) {
        if let Some(c) = self.selected.and_then(|id| self.store.get(id)).cloned() {
            self.begin_edit(c, false);
            if let Some(form) = &mut self.form {
                // Simulate an unsaved edit to the title field.
                form.draft.title = format!("{} (edited)", form.draft.title);
                if let Some(fi) = form.inputs.iter_mut().find(|f| f.slot == Slot::Title) {
                    fi.ti.set_text(&form.draft.title);
                }
            }
            // Pick a *different* visible contact as the pending switch target.
            let target = self
                .visible_ids()
                .into_iter()
                .find(|id| Some(*id) != self.selected);
            self.pending_switch = target;
        }
    }

    /// Snap the delete-confirmation modal open on the selected contact for
    /// `--shots`.
    pub fn shots_show_confirm_delete(&mut self) {
        self.confirm_delete = self.selected;
    }

    /// Empty the book for `--shots` so a headless frame captures the genuine
    /// first-launch empty state (what the live app shows before any contact
    /// exists). Shots-only: it mutates nothing the live app relies on.
    pub fn shots_show_empty(&mut self) {
        self.store = ContactStore::new();
        self.selected = None;
        self.mode = Mode::View;
        self.form = None;
        self.relayout = true;
        self.dirty = true;
    }

    /// Snap a right-click context menu open on the selected contact row for
    /// `--shots`.
    pub fn shots_show_context_menu(&mut self) {
        if let Some(id) = self.selected {
            // Anchor the menu near the selected row in the list.
            let anchor = self
                .list_rows
                .iter()
                .find(|(rid, _)| *rid == id)
                .map_or_else(
                    || {
                        Rect::new(
                            self.list_rect.x0 + 60.0,
                            self.list_rect.y0 + 200.0,
                            self.list_rect.x0 + 60.0,
                            self.list_rect.y0 + 200.0,
                        )
                    },
                    |(_, r)| Rect::new(r.x0 + 40.0, r.center().y, r.x0 + 40.0, r.center().y),
                );
            self.popup = Some(Popup::ContextMenu { target: id, anchor });
        }
    }

    /// Ids currently visible after search + group filter, in sort order.
    fn visible_ids(&self) -> Vec<ContactId> {
        self.store
            .filtered_sorted(self.search.value(), self.group_filter.as_deref(), self.sort)
    }

    fn show_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
        self.dirty = true;
    }

    /// `true` when a toast is showing and has outlived [`TOAST_LIFETIME`].
    fn toast_expired(&self) -> bool {
        self.toast
            .as_ref()
            .is_some_and(|(_, shown)| shown.elapsed() >= TOAST_LIFETIME)
    }

    fn begin_edit(&mut self, draft: Contact, is_new: bool) {
        self.form = Some(EditForm::new(draft, is_new));
        self.mode = Mode::Edit;
        self.popup = None;
        self.detail_scroll = 0.0;
        // The edit form lives in the detail pane; on a narrow window push it
        // in so the form is visible (single-pane master-detail navigation).
        if self.narrow {
            self.push.show_detail();
        }
        // The form opens with no field focused: Tab / Enter / an arrow key
        // enters at the first field, and Shift+Tab enters at the last. This
        // keeps a single, predictable entry point for keyboard navigation.
        // Callers that want to land on a specific field (double-click-to-edit)
        // focus it explicitly with `focus_form_slot` right after.
        self.relayout = true;
        self.dirty = true;
    }

    fn cancel_edit(&mut self) {
        self.form = None;
        self.mode = Mode::View;
        self.popup = None;
        self.relayout = true;
        self.dirty = true;
    }

    fn save_edit(&mut self, doc: &mut AutoCommit) {
        if let Some(mut form) = self.form.take() {
            form.collect();
            let c = form.draft.clone();
            let id = c.id;
            self.store.upsert(c.clone());
            self.perform_action(doc, &crate::actions::upsert_contact_action(&c));
            self.selected = Some(id);
            Self::persist();
            self.show_toast(if form.is_new {
                "Contact created"
            } else {
                "Saved"
            });
        }
        self.mode = Mode::View;
        self.popup = None;
        self.relayout = true;
        self.dirty = true;
    }

    /// Formerly wrote the working address book out to the app's own file
    /// after every mutation. That write rebuilt a fresh `AutoCommit` from the
    /// contact list each time, so the saved document shared no history with
    /// any peer and could never merge — it was using Automerge as a
    /// serialisation format.
    ///
    /// Every mutation already writes the contact into the shared document via
    /// [`ContactsDoc`], and that document is what `ui::persist` saves, history
    /// and actor identity intact. So this is now nothing: the call sites are
    /// left in place because "and this is where the book is durable" is still
    /// true, one layer up.
    fn persist() {}

    /// Dispatch one [`ContactsAction`](crate::actions::ContactsAction) — the
    /// single CRDT-mutation path shared with the Ctrl+K palette. The action is
    /// applied through the qOS action executor (a pinned actor + committed
    /// change, so it replays byte-identically on peers), exactly as the palette
    /// does. The document heads captured just before the first action of the
    /// event are stashed so the host records **one** undo boundary bracketing it
    /// ([`Widget::take_committed_action`]). The in-memory store is updated by the
    /// caller for immediate UI feedback and reconciled from the document on the
    /// next `sync`.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &crate::actions::ContactsAction) {
        self.actions
            .dispatch(doc, crate::actions::CONTACTS_ACTION_TYPE, action);
    }

    /// The last action dispatched, for the `affordance == ActionSpec` covenant
    /// tests.
    #[cfg(test)]
    fn last_dispatched(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// The context Ctrl+K needs to build the contacts resource tree: a snapshot
    /// of the address book currently in the model.
    pub(crate) fn contacts_resource_ctx(&self) -> crate::actions::ContactsResourceCtx {
        crate::actions::ContactsResourceCtx {
            contacts: self.store.contacts.clone(),
        }
    }

    /// Whether the open edit form holds changes not yet written to the
    /// store. Collects the live input values into a throwaway draft and
    /// compares against the stored record (or, for a brand-new contact,
    /// reports dirty when any field has content). Drives the
    /// "Save changes?" guard when the user tries to switch contacts.
    fn form_is_dirty(&self) -> bool {
        let Some(form) = &self.form else { return false };
        // Fold the live input values into a candidate draft without mutating
        // the real form.
        let mut candidate = EditForm {
            draft: form.draft.clone(),
            is_new: form.is_new,
            inputs: form
                .inputs
                .iter()
                .map(|f| FieldInput {
                    slot: f.slot,
                    ti: TextInput::local().with_initial_value(f.ti.value()),
                })
                .collect(),
        };
        candidate.collect();
        let draft = candidate.draft;
        if form.is_new {
            // A fresh contact is "dirty" once it has any meaningful content
            // (the two seeded empty phone/email rows don't count).
            return !draft.first.trim().is_empty()
                || !draft.last.trim().is_empty()
                || !draft.org.trim().is_empty()
                || !draft.title.trim().is_empty()
                || !draft.website.trim().is_empty()
                || draft.birthday.is_some()
                || !draft.notes.trim().is_empty()
                || !draft.groups.is_empty()
                || draft.phones.iter().any(|p| !p.value.trim().is_empty())
                || draft.emails.iter().any(|e| !e.value.trim().is_empty())
                || draft.addresses.iter().any(|a| !a.is_empty());
        }
        match self.store.get(draft.id) {
            Some(stored) => *stored != draft,
            None => true,
        }
    }

    /// Switch the selected contact to `id`. If an edit form is open with
    /// unsaved changes, park the target in `pending_switch` and pop the
    /// "Save changes?" modal instead of switching directly.
    fn request_select(&mut self, id: ContactId) {
        if self.mode == Mode::Edit && self.form_is_dirty() {
            self.pending_switch = Some(id);
            self.dirty = true;
            return;
        }
        self.commit_select(id);
    }

    /// Open `id` from *outside* the list — the global Ctrl+K palette. Unlike a
    /// row click, the target may be hidden by the current group filter or in-app
    /// search, so "open" has to actively reveal it: if it is not in the visible
    /// set, drop those filters first, then select it. Otherwise the detail pane
    /// would show the contact while the list left the row filtered out — opened
    /// but not actually found.
    fn reveal_contact(&mut self, id: ContactId) {
        if !self.visible_ids().contains(&id) {
            self.group_filter = None;
            self.search.set_text("");
        }
        self.commit_select(id);
    }

    /// Unconditionally switch selection to `id` (leaving any edit form).
    fn commit_select(&mut self, id: ContactId) {
        self.form = None;
        self.mode = Mode::View;
        self.selected = Some(id);
        self.detail_scroll = 0.0;
        self.search.set_focus(false);
        if self.narrow {
            self.push.show_detail();
        }
        self.relayout = true;
        self.dirty = true;
    }

    fn delete_selected(&mut self, doc: &mut AutoCommit, id: ContactId) {
        self.store.delete(id);
        self.perform_action(doc, &crate::actions::delete_contact_action(id.to_string()));
        if self.selected == Some(id) {
            self.selected = self.visible_ids().first().copied();
        }
        self.confirm_delete = None;
        Self::persist();
        self.show_toast("Contact deleted");
        self.relayout = true;
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
        self.selected = Some(ids[next]);
        self.detail_scroll = 0.0;
        self.dirty = true;
        self.relayout = true;
    }

    /// Live Import action: open the host's native multi-select file chooser
    /// (XDG portal) filtered to vCard files. Non-blocking — the D-Bus
    /// round-trip and the host dialog run on a worker thread; the chosen paths
    /// arrive over a channel drained by [`drain_portal`]. An in-flight request
    /// is not restarted.
    fn request_import(&mut self) {
        if self.import_rx.is_some() {
            return;
        }
        self.import_rx = Some(portal::request_import());
    }

    /// Read, parse, and merge every vCard file in `paths` into the address
    /// book, persisting once if anything was added. Returns the number of
    /// contacts imported. This is the hermetic core shared by the live portal
    /// drain and the tests — it never touches the picker, so multi-file import
    /// can be driven directly without a desktop portal.
    fn import_paths(&mut self, doc: &mut AutoCommit, paths: &[PathBuf]) -> usize {
        if paths.is_empty() {
            // An empty selection (or a cancelled picker) fabricates nothing.
            return 0;
        }
        let mut n = 0usize;
        let mut first_new = None;
        let mut errors = 0usize;
        for path in paths {
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "contacts: import read failed");
                    errors += 1;
                    continue;
                }
            };
            // Each file may hold arbitrarily many, differently-shaped cards.
            for c in vcard::parse_vcards(&text) {
                if first_new.is_none() {
                    first_new = Some(c.id);
                }
                self.store.upsert(c.clone());
                // Route each imported card through the action model too, so the
                // whole import is one undo boundary (perform_action captures the
                // pre-import heads once) and replays on peers like any other
                // create.
                self.perform_action(doc, &crate::actions::upsert_contact_action(&c));
                n += 1;
            }
        }
        if let Some(id) = first_new {
            self.selected = Some(id);
        }
        if n > 0 {
            Self::persist();
            self.show_toast(format!(
                "Imported {n} contact{}",
                if n == 1 { "" } else { "s" }
            ));
            self.relayout = true;
        } else if errors > 0 {
            self.show_toast("Couldn't read the selected file(s)");
        } else {
            // A valid but empty/unparseable selection: say so rather than
            // silently doing nothing.
            self.show_toast("No contacts found in the selected file(s)");
        }
        n
    }

    /// Whether a file-chooser portal (import or export) is currently awaiting
    /// the user's choice. Keeps the host loop pumping frames so the outcome is
    /// drained promptly once the dialog closes.
    fn portal_in_flight(&self) -> bool {
        self.import_rx.is_some() || self.export_rx.is_some()
    }

    /// Drain any in-flight import/export portal request. Called once per frame
    /// from [`Widget::sync`]; a `try_recv` never blocks the UI thread. When an
    /// outcome arrives the receiver is dropped (idle again).
    fn drain_portal(&mut self, doc: &mut AutoCommit) {
        if let Some(rx) = &self.import_rx {
            match rx.try_recv() {
                Ok(ImportOutcome::Selected(paths)) => {
                    self.import_rx = None;
                    self.import_paths(doc, &paths);
                }
                Ok(ImportOutcome::Cancelled) => {
                    // A dismissed picker is a silent no-op — nothing to say.
                    self.import_rx = None;
                }
                Ok(ImportOutcome::Failed(reason)) => {
                    self.import_rx = None;
                    self.show_toast(format!("Import unavailable: {reason}"));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.import_rx = None;
                }
            }
        }
        if let Some(rx) = &self.export_rx {
            match rx.try_recv() {
                Ok(ExportOutcome::Opened(path)) => {
                    self.export_rx = None;
                    if let Some((ids, version)) = self.pending_export.take() {
                        self.write_export(&ids, version, &path);
                    }
                }
                Ok(ExportOutcome::Cancelled) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.export_rx = None;
                    self.pending_export = None;
                }
                Ok(ExportOutcome::Failed(reason)) => {
                    self.export_rx = None;
                    self.pending_export = None;
                    self.show_toast(format!("Export unavailable: {reason}"));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    /// Open the export-selection dialog, pre-ticking every contact currently
    /// visible in the list (search + group filtered) so the common "export
    /// what I'm looking at" case is one click; the user can then prune or
    /// add via the per-row checkboxes / Select all / Select none.
    fn open_export_dialog(&mut self) {
        let pre: std::collections::HashSet<ContactId> = self.visible_ids().into_iter().collect();
        self.export_sel = Some(pre);
        self.export_scroll = 0.0;
        self.popup = None;
        self.dirty = true;
    }

    /// Live Export action: park the chosen `ids` + vCard `version`, then open
    /// the host's native Save dialog (XDG portal) with a suggested file name.
    /// Non-blocking — when the portal returns a destination, [`drain_portal`]
    /// writes the file via [`write_export`]. An in-flight request is not
    /// restarted. Returns `false` (nothing parked) when the selection is empty.
    fn request_export(&mut self, ids: Vec<ContactId>, version: VCardVersion) -> bool {
        if ids.is_empty() {
            self.show_toast("Select at least one contact to export");
            return false;
        }
        if self.export_rx.is_some() {
            return false;
        }
        self.pending_export = Some((ids, version));
        self.export_rx = Some(portal::request_export(EXPORT_FILE_NAME));
        true
    }

    /// Write the chosen `ids` out as a `.vcf` of `version` to `path`, reporting
    /// the real destination. Hermetic core shared by the live portal drain and
    /// the tests — it takes an explicit path and never consults the picker or
    /// an env var.
    fn write_export(&mut self, ids: &[ContactId], version: VCardVersion, path: &std::path::Path) {
        let contacts: Vec<Contact> = ids
            .iter()
            .filter_map(|id| self.store.get(*id).cloned())
            .collect();
        if contacts.is_empty() {
            self.show_toast("Select at least one contact to export");
            return;
        }
        let body = vcard::to_vcards(&contacts, version);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(path, body) {
            Ok(()) => self.show_toast(format!(
                "Exported {} ({}) → {}",
                contacts.len(),
                version.label(),
                path.display()
            )),
            Err(e) => self.show_toast(format!("Export failed: {e}")),
        }
    }
}

impl Default for ContactsApp {
    fn default() -> Self {
        Self::new()
    }
}

// =====================================================================
// Layout
// =====================================================================

impl ContactsApp {
    fn layout(&mut self) {
        let r = self.rect;
        // Toolbar across the top.
        let content_top = r.y0 + TOOLBAR_H;
        let content = Rect::new(r.x0, content_top, r.x1, r.y1);

        // Toolbar buttons (right-aligned).
        let btn_y = r.y0 + (TOOLBAR_H - 32.0) / 2.0;
        let mut x = r.x1 - PAD;
        let new_w = 104.0;
        self.hit.new = Rect::new(x - new_w, btn_y, x, btn_y + 32.0);
        x -= new_w + 10.0;
        let exp_w = 86.0;
        self.hit.export = Rect::new(x - exp_w, btn_y, x, btn_y + 32.0);
        x -= exp_w + 10.0;
        let imp_w = 86.0;
        self.hit.import = Rect::new(x - imp_w, btn_y, x, btn_y + 32.0);

        // Responsive split: side-by-side when wide, single-pane push when
        // narrow. The choice is driven purely from the allocated width.
        let avail_w = content.width();
        self.narrow = PushNav::is_narrow(avail_w);
        if self.narrow {
            // Single-pane push: the list and detail each fill the whole
            // content width and slide horizontally as one swaps for the
            // other. `slide` (0 = list, 1 = detail) offsets both slabs so
            // the list slides off to the left while the detail slides in
            // from the right.
            let slide = self.push.slide();
            let list_x0 = content.x0 - avail_w * slide;
            self.list_rect = Rect::new(list_x0, content.y0, list_x0 + avail_w, content.y1);
            let detail_x0 = content.x0 + avail_w * (1.0 - slide);
            self.detail_rect = Rect::new(detail_x0, content.y0, detail_x0 + avail_w, content.y1);
            // Reserve a Back affordance pill at the top-left of the detail
            // pane (the master-detail return-to-list affordance).
            self.hit.back = Rect::new(
                self.detail_rect.x0 + PAD,
                self.detail_rect.y0 + 14.0,
                self.detail_rect.x0 + PAD + 92.0,
                self.detail_rect.y0 + 14.0 + 30.0,
            );
        } else {
            // Wide: the classic list (left) + detail (right) split with a
            // draggable divider between them.
            let list_w = (avail_w * self.split_pos)
                .clamp(260.0, avail_w - 360.0)
                .max(220.0);
            self.list_rect = Rect::new(content.x0, content.y0, content.x0 + list_w, content.y1);
            let div_x0 = self.list_rect.x1;
            self.detail_rect = Rect::new(div_x0 + DIVIDER_W, content.y0, content.x1, content.y1);
            self.hit.back = Rect::ZERO;
        }

        // List header: search + filter row.
        let search_y = self.list_rect.y0 + PAD;
        self.hit.search = Rect::new(
            self.list_rect.x0 + PAD,
            search_y,
            self.list_rect.x1 - PAD,
            search_y + FIELD_H,
        );
        self.search.apply_layout_external(self.hit.search);
        let filt_y = self.hit.search.y1 + 10.0;
        let half = (self.list_rect.width() - PAD * 2.0 - 8.0) / 2.0;
        self.hit.sort = Rect::new(
            self.list_rect.x0 + PAD,
            filt_y,
            self.list_rect.x0 + PAD + half,
            filt_y + 30.0,
        );
        self.hit.group = Rect::new(
            self.hit.sort.x1 + 8.0,
            filt_y,
            self.list_rect.x1 - PAD,
            filt_y + 30.0,
        );

        // List body (scroll region).
        let body_top = self.hit.group.y1 + 12.0;
        self.list_body = Rect::new(
            self.list_rect.x0,
            body_top,
            self.list_rect.x1,
            self.list_rect.y1,
        );

        // Clamp + compute row rects.
        let ids = self.visible_ids();
        let content_h = ids.len() as f64 * ROW_H;
        let max_scroll = (content_h - self.list_body.height()).max(0.0);
        self.list_scroll = self.list_scroll.clamp(0.0, max_scroll);
        self.list_rows.clear();
        let mut ry = self.list_body.y0 - self.list_scroll;
        for id in ids {
            let rr = Rect::new(self.list_body.x0, ry, self.list_body.x1, ry + ROW_H);
            self.list_rows.push((id, rr));
            ry += ROW_H;
        }

        // Detail / form layout.
        self.hit.type_chips.clear();
        self.hit.remove_btns.clear();
        if self.mode == Mode::Edit {
            self.layout_form();
        }
    }

    /// Extra vertical space reserved at the top of the detail pane when the
    /// narrow single-pane push layout is active, so the pushed detail's
    /// header (action buttons, avatar) clears the Back affordance pill.
    fn detail_top_offset(&self) -> f64 {
        if self.narrow { 44.0 } else { 0.0 }
    }

    /// View-mode detail action rects (favorite / Edit / Delete), pinned to
    /// the top-right of the detail pane. Recomputed whenever `detail_rect`
    /// moves (window resize, and every frame of the narrow push slide). On a
    /// narrow window they shift down to clear the Back affordance pill.
    fn layout_detail_actions(&mut self) {
        let ax1 = self.detail_rect.x1 - 24.0;
        let by = self.detail_rect.y0 + 30.0 + self.detail_top_offset();
        self.hit.delete = Rect::new(ax1 - 84.0, by, ax1, by + 32.0);
        self.hit.edit = Rect::new(
            self.hit.delete.x0 - 8.0 - 72.0,
            by,
            self.hit.delete.x0 - 8.0,
            by + 32.0,
        );
        self.hit.fav = Rect::new(
            self.hit.edit.x0 - 8.0 - 32.0,
            by,
            self.hit.edit.x0 - 8.0,
            by + 32.0,
        );
    }

    /// Assign rects to every form input + dynamic control. Mirrors the
    /// vertical order the form renders in. Two-phase to avoid holding a
    /// `self.form` borrow while writing `self.hit`: build a rect plan
    /// first, then apply it to the inputs.
    /// Assign rects to every form field, in vertical order. The repeated
    /// phone/email/address rows are built by per-group helpers purely for
    /// length; the geometry (and thus the rendered layout) is unchanged
    /// from one flat function.
    fn layout_form(&mut self) {
        let area = self.detail_rect;
        let off = self.detail_top_offset();
        let x0 = area.x0 + PAD;
        let x1 = area.x1 - PAD;
        let w = x1 - x0;
        // The (non-scrolled) header band is two stacked rows so nothing
        // collides: row 1 is the "Edit/New Contact" title, row 2 holds the
        // Favorite pill (left) and Cancel/Save (right). The scrollable body
        // begins below both. Keep these constants in sync with `draw_form`
        // (the title baseline and the body clip both derive from them).
        //
        // Action order matches this app's dialogs (delete-confirm, unsaved
        // guard, export): the primary action sits at the far right and the
        // secondary just left of it — one convention everywhere.
        let title_y = area.y0 + FORM_TITLE_TOP + off;
        let by = title_y + FORM_TITLE_H + FORM_ROW_GAP; // controls row top
        let top = by + 32.0 + FORM_ROW_GAP; // first field row top
        let mut y = top - self.detail_scroll;

        self.hit.save = Rect::new(x1 - 96.0, by, x1, by + 32.0);
        self.hit.cancel = Rect::new(
            self.hit.save.x0 - 8.0 - 88.0,
            by,
            self.hit.save.x0 - 8.0,
            by + 32.0,
        );
        self.hit.fav_toggle = Rect::new(x0, by, x0 + 120.0, by + 32.0);

        let label_h = 18.0;
        let gap = 12.0;
        let nh = FIELD_H;
        let half = (w - 10.0) / 2.0;

        let (n_phones, n_emails, n_addr) = {
            let d = &self.form.as_ref().unwrap().draft;
            (d.phones.len(), d.emails.len(), d.addresses.len())
        };

        let mut plan: Vec<(Slot, Rect)> = Vec::new();

        // Name row: First | Last
        y += label_h;
        plan.push((Slot::First, Rect::new(x0, y, x0 + half, y + nh)));
        plan.push((Slot::Last, Rect::new(x0 + half + 10.0, y, x1, y + nh)));
        y += nh + gap;

        // Org | Title
        y += label_h;
        plan.push((Slot::Org, Rect::new(x0, y, x0 + half, y + nh)));
        plan.push((Slot::Title, Rect::new(x0 + half + 10.0, y, x1, y + nh)));
        y += nh + gap;

        self.layout_form_phones(x0, x1, n_phones, &mut y, &mut plan);
        self.layout_form_emails(x0, x1, n_emails, &mut y, &mut plan);
        self.layout_form_addresses(x0, x1, half, n_addr, &mut y, &mut plan);

        // Birthday (input + calendar button) | Website
        y += label_h;
        let bday_w = half - 40.0;
        plan.push((Slot::Birthday, Rect::new(x0, y, x0 + bday_w, y + nh)));
        self.hit.cal_btn = Rect::new(x0 + bday_w + 6.0, y, x0 + half, y + nh);
        plan.push((Slot::Website, Rect::new(x0 + half + 10.0, y, x1, y + nh)));
        y += nh + gap;

        // Notes (full width)
        y += label_h;
        plan.push((Slot::Notes, Rect::new(x0, y, x1, y + nh)));
        y += nh + gap;

        // Groups (full width)
        y += label_h;
        plan.push((Slot::Groups, Rect::new(x0, y, x1, y + nh)));
        y += nh + gap;

        self.detail_content_h = (y + self.detail_scroll) - top + 24.0;

        // Apply the rect plan to the actual inputs.
        if let Some(form) = &mut self.form {
            for (slot, rect) in plan {
                if let Some(f) = form.inputs.iter_mut().find(|f| f.slot == slot) {
                    f.ti.apply_layout_external(rect);
                }
            }
        }
    }

    /// Phone rows: `[chip][value][remove]` per phone, then the "+ Add
    /// phone" button. Appends each row's plan entry and advances `y`.
    fn layout_form_phones(
        &mut self,
        x0: f64,
        x1: f64,
        n: usize,
        y: &mut f64,
        plan: &mut Vec<(Slot, Rect)>,
    ) {
        let label_h = 18.0;
        let gap = 12.0;
        let nh = FIELD_H;
        *y += label_h;
        for i in 0..n {
            let chip = Rect::new(x0, *y, x0 + 78.0, *y + nh);
            let rm = Rect::new(x1 - 30.0, *y, x1, *y + nh);
            plan.push((
                Slot::Phone(i),
                Rect::new(chip.x1 + 8.0, *y, rm.x0 - 8.0, *y + nh),
            ));
            self.hit.type_chips.push((TypeTarget::Phone(i), chip));
            self.hit.remove_btns.push((TypeTarget::Phone(i), rm));
            *y += nh + 8.0;
        }
        self.hit.add_phone = Rect::new(x0, *y, x0 + 130.0, *y + 28.0);
        *y += 28.0 + gap;
    }

    /// Email rows: `[chip][value][remove]` per email, then the "+ Add
    /// email" button. Appends each row's plan entry and advances `y`.
    fn layout_form_emails(
        &mut self,
        x0: f64,
        x1: f64,
        n: usize,
        y: &mut f64,
        plan: &mut Vec<(Slot, Rect)>,
    ) {
        let label_h = 18.0;
        let gap = 12.0;
        let nh = FIELD_H;
        *y += label_h;
        for i in 0..n {
            let chip = Rect::new(x0, *y, x0 + 78.0, *y + nh);
            let rm = Rect::new(x1 - 30.0, *y, x1, *y + nh);
            plan.push((
                Slot::Email(i),
                Rect::new(chip.x1 + 8.0, *y, rm.x0 - 8.0, *y + nh),
            ));
            self.hit.type_chips.push((TypeTarget::Email(i), chip));
            self.hit.remove_btns.push((TypeTarget::Email(i), rm));
            *y += nh + 8.0;
        }
        self.hit.add_email = Rect::new(x0, *y, x0 + 130.0, *y + 28.0);
        *y += 28.0 + gap;
    }

    /// Address rows: chip + street, then city/region, then postal/country,
    /// per address, then the "+ Add address" button. Appends each address's
    /// plan entries and advances `y`.
    fn layout_form_addresses(
        &mut self,
        x0: f64,
        x1: f64,
        half: f64,
        n: usize,
        y: &mut f64,
        plan: &mut Vec<(Slot, Rect)>,
    ) {
        let label_h = 18.0;
        let gap = 12.0;
        let nh = FIELD_H;
        *y += label_h;
        for i in 0..n {
            let chip = Rect::new(x0, *y, x0 + 78.0, *y + nh);
            let rm = Rect::new(x1 - 30.0, *y, x1, *y + nh);
            plan.push((
                Slot::AddrStreet(i),
                Rect::new(chip.x1 + 8.0, *y, rm.x0 - 8.0, *y + nh),
            ));
            self.hit.type_chips.push((TypeTarget::Addr(i), chip));
            self.hit.remove_btns.push((TypeTarget::Addr(i), rm));
            *y += nh + 8.0;
            plan.push((Slot::AddrCity(i), Rect::new(x0, *y, x0 + half, *y + nh)));
            plan.push((
                Slot::AddrRegion(i),
                Rect::new(x0 + half + 10.0, *y, x1, *y + nh),
            ));
            *y += nh + 8.0;
            plan.push((Slot::AddrPostal(i), Rect::new(x0, *y, x0 + half, *y + nh)));
            plan.push((
                Slot::AddrCountry(i),
                Rect::new(x0 + half + 10.0, *y, x1, *y + nh),
            ));
            *y += nh + 12.0;
        }
        self.hit.add_address = Rect::new(x0, *y, x0 + 150.0, *y + 28.0);
        *y += 28.0 + gap;
    }
}

// =====================================================================
// Geometry for popups (shared by draw + hit-test)
// =====================================================================

impl ContactsApp {
    fn popup_menu_geometry(&self, anchor: Rect, items: usize, width: f64) -> (Rect, Vec<Rect>) {
        let item_h = 32.0;
        let h = items as f64 * item_h + 8.0;
        let x0 = anchor
            .x0
            .min(self.rect.x1 - width - 8.0)
            .max(self.rect.x0 + 8.0);
        let mut y0 = anchor.y1 + 4.0;
        if y0 + h > self.rect.y1 - 8.0 {
            y0 = (anchor.y0 - h - 4.0).max(self.rect.y0 + 8.0);
        }
        let panel = Rect::new(x0, y0, x0 + width, y0 + h);
        let mut rows = Vec::new();
        for i in 0..items {
            let ry = panel.y0 + 4.0 + i as f64 * item_h;
            rows.push(Rect::new(panel.x0 + 4.0, ry, panel.x1 - 4.0, ry + item_h));
        }
        (panel, rows)
    }

    fn calendar_geometry(
        &self,
        anchor: Rect,
        year: i32,
        month: u32,
    ) -> (Rect, Rect, Rect, Vec<(u32, Rect)>) {
        let w = 252.0;
        let h = 250.0;
        let x0 = anchor
            .x0
            .min(self.rect.x1 - w - 8.0)
            .max(self.rect.x0 + 8.0);
        let mut y0 = anchor.y1 + 4.0;
        if y0 + h > self.rect.y1 - 8.0 {
            y0 = (anchor.y0 - h - 4.0).max(self.rect.y0 + 8.0);
        }
        let panel = Rect::new(x0, y0, x0 + w, y0 + h);
        let prev = Rect::new(
            panel.x0 + 8.0,
            panel.y0 + 8.0,
            panel.x0 + 36.0,
            panel.y0 + 36.0,
        );
        let next = Rect::new(
            panel.x1 - 36.0,
            panel.y0 + 8.0,
            panel.x1 - 8.0,
            panel.y0 + 36.0,
        );
        // Day grid: 7 cols, start under the weekday header.
        let grid_top = panel.y0 + 74.0;
        let cell = (w - 16.0) / 7.0;
        let first_wd = date::weekday(year, month, 1);
        let dim = date::days_in_month(year, month);
        let mut cells = Vec::new();
        for d in 1..=dim {
            let idx = f64::from(first_wd + (d - 1));
            let col = (idx % 7.0).floor();
            let row = (idx / 7.0).floor();
            let cx = panel.x0 + 8.0 + col * cell;
            let cy = grid_top + row * cell;
            cells.push((d, Rect::new(cx, cy, cx + cell, cy + cell)));
        }
        (panel, prev, next, cells)
    }
}

// =====================================================================
// Drawing
// =====================================================================

impl ContactsApp {
    fn render(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Advance the master↔detail push slide. `draw` is the per-frame
        // animation tick for a self-rendering widget (the live loop keeps
        // redrawing while `needs_animation_frame` is true); ticking here
        // keeps the eased slide in step with the frame it's drawn into. The
        // sliding pane rects are recomputed by re-running layout.
        if self.push.tick() {
            self.layout();
            self.layout_detail_actions();
            self.dirty = true;
        }

        // Expire a stale toast. `render` runs every animation frame while a
        // toast is up (see `needs_animation_frame`), so this fires promptly
        // once its lifetime elapses, then lets the loop settle back to idle.
        if self.toast_expired() {
            self.toast = None;
            self.dirty = true;
        }

        let c = &theme.tokens.colors;
        // App background.
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);

        self.draw_toolbar(scene, transform, tr, theme);

        if self.narrow {
            // Single-pane push: clip both slabs to the content area below the
            // toolbar so the off-screen pane (sliding in/out) never bleeds
            // past the edges, and skip whichever pane is fully off-screen.
            let content = Rect::new(
                self.rect.x0,
                self.rect.y0 + TOOLBAR_H,
                self.rect.x1,
                self.rect.y1,
            );
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                transform,
                &content,
            );
            let slide = self.push.slide();
            if slide < 0.999 {
                self.draw_list_pane(scene, transform, tr, theme, presence);
            }
            if slide > 0.001 {
                self.draw_detail_pane(scene, transform, tr, theme, presence);
            }
            scene.pop_layer();
        } else {
            self.draw_list_pane(scene, transform, tr, theme, presence);
            self.draw_divider(scene, transform, theme);
            self.draw_detail_pane(scene, transform, tr, theme, presence);
        }

        // Overlays.
        if let Some(popup) = &self.popup {
            self.draw_popup(scene, transform, tr, theme, popup);
        }
        if self.export_sel.is_some() {
            self.draw_export_dialog(scene, transform, tr, theme);
        }
        if self.pending_switch.is_some() {
            self.draw_unsaved_modal(scene, transform, tr, theme);
        }
        if let Some(id) = self.confirm_delete {
            self.draw_confirm(scene, transform, tr, theme, id);
        }
        if let Some((msg, _)) = &self.toast {
            draw_toast(scene, transform, tr, theme, self.rect, msg);
        }
    }

    /// The master pane: the contact list chrome, its embedded search input,
    /// and the empty-search placeholder. Bundled so the responsive renderer
    /// can place it as one slab (docked beside the detail when wide, or as a
    /// single full-width pane when narrow).
    fn draw_list_pane(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.draw_list(scene, transform, tr, theme, presence);
        // Search input draws on top of the list-pane chrome.
        self.search.draw(scene, transform, tr, theme, presence);
        // Placeholder hint when the search box is empty + unfocused.
        if self.search.value().is_empty() && !self.search.is_focused() {
            tr.draw_text(
                scene,
                "Search name, email, phone…",
                14.0,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((
                        self.hit.search.x0 + 12.0,
                        self.hit.search.center().y - 9.0,
                    )),
                None,
            );
        }
    }

    /// The detail pane: its background, the contact card or edit form, and —
    /// on a narrow window — the Back affordance that returns to the list.
    fn draw_detail_pane(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        // Detail panel background.
        scene.fill(
            Fill::NonZero,
            transform,
            c.surfaces.base,
            None,
            &self.detail_rect,
        );
        if self.mode == Mode::Edit {
            self.draw_form(scene, transform, tr, theme, presence);
        } else {
            self.draw_detail(scene, transform, tr, theme, presence);
        }
        // Narrow-window Back affordance (the master-detail return-to-list).
        if self.narrow && self.hit.back.width() > 0.0 {
            self.draw_back_button(scene, transform, tr, theme);
        }
    }

    /// The Back affordance pill at the top-left of the pushed detail pane
    /// (narrow windows only): a left chevron + "Back" that slides the list
    /// back into view.
    fn draw_back_button(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let rect = self.hit.back;
        let hovered = rect.contains(self.last_mouse);
        let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.full));
        let bg = if hovered {
            c.surfaces.raised
        } else {
            c.surfaces.base
        };
        scene.fill(Fill::NonZero, t, bg, None, &shape);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
        // Left chevron.
        let cyp = rect.center().y;
        let cxp = rect.x0 + 18.0;
        let mut p = BezPath::new();
        p.move_to((cxp + 4.0, cyp - 5.0));
        p.line_to((cxp - 1.0, cyp));
        p.line_to((cxp + 4.0, cyp + 5.0));
        scene.stroke(&Stroke::new(1.6), t, c.text_primary, None, &p);
        tr.draw_text_weighted(
            scene,
            "Back",
            13.0,
            c.text_primary,
            t * Affine::translate((rect.x0 + 30.0, rect.center().y - 8.0)),
            None,
            TextWeight::Semibold,
        );
    }

    fn draw_toolbar(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let bar = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + TOOLBAR_H,
        );
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &bar);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            c.border_standard,
            None,
            &Line::new((bar.x0, bar.y1), (bar.x1, bar.y1)),
        );
        // No app-name wordmark — the OS titlebar shows "Contacts". The toolbar's
        // left slot carries the functional people-count instead.
        let count = match self.store.len() {
            0 => "No contacts".to_string(),
            1 => "1 person".to_string(),
            n => format!("{n} people"),
        };
        // Draw the count only when it clears the right-aligned toolbar buttons —
        // on a narrow window the buttons crowd in and it would otherwise bleed
        // under the Import button.
        let count_x = self.rect.x0 + PAD;
        let count_w = f64::from(
            tr.measure_weighted(&count, 14.0, None, TextWeight::Semibold)
                .0,
        );
        if count_x + count_w < self.hit.import.x0 - 12.0 {
            tr.draw_text_weighted(
                scene,
                &count,
                14.0,
                c.text_primary,
                t * Affine::translate((count_x, self.rect.y0 + (TOOLBAR_H - 14.0) / 2.0)),
                None,
                TextWeight::Semibold,
            );
        }

        let hov = |r: Rect| r.contains(self.last_mouse);
        toolbar_button(
            scene,
            t,
            tr,
            theme,
            self.hit.import,
            "Import",
            false,
            hov(self.hit.import),
            false,
        );
        toolbar_button(
            scene,
            t,
            tr,
            theme,
            self.hit.export,
            "Export",
            false,
            hov(self.hit.export),
            false,
        );
        toolbar_button(
            scene,
            t,
            tr,
            theme,
            self.hit.new,
            "+ New",
            true,
            hov(self.hit.new),
            false,
        );
    }

    /// Draw the list pane: sort/group chips, every visible row, and the
    /// empty-state message when nothing matches. Split into per-row/
    /// empty-state helpers purely for length; the draw order is unchanged
    /// from one flat function.
    fn draw_list(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        // Pane background + right border.
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &self.list_rect);

        // Search field (draw the TextInput by temporarily taking &mut via ptr).
        // We can't borrow &mut self here, so the search input is drawn in the
        // mutable wrapper below — see render(). Placeholder background:
        // Sort + group chips.
        let sort_label = format!("Sort: {}", self.sort.label());
        chip(scene, t, tr, theme, self.hit.sort, &sort_label, false);
        let group_label = match &self.group_filter {
            Some(g) => format!("Group: {g}"),
            None => "Group: All".to_string(),
        };
        chip(
            scene,
            t,
            tr,
            theme,
            self.hit.group,
            &group_label,
            self.group_filter.is_some(),
        );

        // Rows (clipped to body).
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &self.list_body);
        for (id, rr) in &self.list_rows {
            if rr.y1 < self.list_body.y0 || rr.y0 > self.list_body.y1 {
                continue;
            }
            let Some(contact) = self.store.get(*id) else {
                continue;
            };
            let selected = self.selected == Some(*id);
            Self::draw_list_row(scene, t, tr, theme, contact, *rr, selected);
        }
        if self.list_rows.is_empty() {
            self.draw_list_empty_state(scene, t, tr, theme);
        }
        scene.pop_layer();
        let _ = presence;
    }

    /// Draw one contact row: selection highlight, avatar, name/subtitle
    /// (elided to fit), favourite star, and the bottom separator.
    fn draw_list_row(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        contact: &Contact,
        rr: Rect,
        selected: bool,
    ) {
        let c = &theme.tokens.colors;
        if selected {
            let bg = RoundedRect::from_rect(rr.inset(-0.0), f64::from(theme.tokens.radii.md));
            scene.fill(
                Fill::NonZero,
                t,
                theme.accent_primary.with_alpha_factor(0.18),
                None,
                &bg,
            );
            // Left accent bar.
            scene.fill(
                Fill::NonZero,
                t,
                theme.accent_primary,
                None,
                &Rect::new(rr.x0, rr.y0 + 8.0, rr.x0 + 3.0, rr.y1 - 8.0),
            );
        }
        // Avatar.
        let av = Rect::new(
            rr.x0 + 14.0,
            rr.y0 + 11.0,
            rr.x0 + 14.0 + 40.0,
            rr.y0 + 11.0 + 40.0,
        );
        draw_avatar(scene, t, tr, theme, av, &contact.initials());
        let tx = av.x1 + 12.0;
        // Elide name + subtitle to a single line each so a narrow list
        // pane (the docked split at 800px, or a long org name) never wraps
        // a row to two cramped lines inside its fixed height.
        let avail = (rr.x1 - tx - 26.0).max(24.0) as f32;
        let name =
            tr.elide_to_width_weighted(&contact.display_name(), 15.0, avail, TextWeight::Semibold);
        tr.draw_text_weighted(
            scene,
            &name,
            15.0,
            c.text_primary,
            t * Affine::translate((tx, rr.y0 + 13.0)),
            None,
            TextWeight::Semibold,
        );
        let sub = contact.subtitle();
        if !sub.is_empty() {
            let sub = tr.elide_to_width(&sub, 12.0, avail);
            tr.draw_text(
                scene,
                &sub,
                12.0,
                c.text_dim,
                t * Affine::translate((tx, rr.y0 + 33.0)),
                None,
            );
        }
        if contact.favorite {
            draw_star(
                scene,
                t,
                Point::new(rr.x1 - 18.0, rr.y0 + 20.0),
                6.0,
                theme.accent_primary,
                true,
            );
        }
        // Separator.
        scene.stroke(
            &Stroke::new(1.0),
            t,
            c.border_standard.with_alpha_factor(0.5),
            None,
            &Line::new((rr.x0 + 14.0, rr.y1), (rr.x1 - 14.0, rr.y1)),
        );
    }

    /// The "no contacts yet" / "no matches" placeholder shown when the
    /// (search + group filtered) list is empty.
    fn draw_list_empty_state(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        // Distinguish a genuinely empty book (first launch) from a filter
        // that matched nothing, so the message is never misleading.
        let (line1, line2): (&str, &str) = if self.store.is_empty() {
            ("No contacts yet", "Add one with + New, or Import a .vcf")
        } else {
            ("No matches", "Try a different search or group")
        };
        tr.draw_text_weighted(
            scene,
            line1,
            14.0,
            c.text_primary,
            t * Affine::translate((self.list_body.x0 + PAD, self.list_body.y0 + 18.0)),
            None,
            TextWeight::Semibold,
        );
        tr.draw_text(
            scene,
            line2,
            12.0,
            c.text_dim,
            t * Affine::translate((self.list_body.x0 + PAD, self.list_body.y0 + 40.0)),
            Some((self.list_body.width() - PAD * 2.0) as f32),
        );
    }

    fn draw_divider(&self, scene: &mut Scene, t: Affine, theme: &Theme) {
        let dx0 = self.list_rect.x1;
        let r = Rect::new(
            dx0,
            self.detail_rect.y0,
            dx0 + DIVIDER_W,
            self.detail_rect.y1,
        );
        let color = if self.divider_hover || self.divider_drag {
            theme.accent_primary
        } else {
            theme.tokens.colors.border_standard
        };
        scene.fill(Fill::NonZero, t, color, None, &r);
    }

    /// Draw the read-mode detail pane: the empty-state placeholder when
    /// nothing is selected, otherwise the header (avatar/name/actions) and
    /// the field sections below it. Split into per-band helpers purely for
    /// length; the draw order is unchanged from one flat function.
    fn draw_detail(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let area = self.detail_rect;
        let Some(id) = self.selected else {
            self.draw_detail_empty_state(scene, t, tr, theme);
            return;
        };
        let Some(contact) = self.store.get(id).cloned() else {
            return;
        };

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &area);
        let off = self.detail_top_offset();
        let x0 = area.x0 + 28.0;
        let x1 = area.x1 - 24.0;
        let top = area.y0 + 28.0 + off;
        let y = top - self.detail_scroll;

        // Header. The avatar sits top-left and the favorite/Edit/Delete action
        // band top-right (pinned by `layout_detail_actions`); the name,
        // subtitle and group badges take their own full-width rows *below* the
        // avatar. Keeping the name off the action band's row means even a long
        // name ("Katherine Johnson") uses the whole pane width and never
        // truncates at a moderate width.
        let (name_top, y) = self.draw_detail_header(scene, t, tr, theme, &contact, x0, x1, y);

        // Record field value hit rects so a double-click can jump straight
        // into editing the clicked field (see `handle_detail_double_click`).
        self.detail_field_hits.clear();
        let (mut hits, y) =
            self.draw_detail_sections(scene, t, tr, theme, presence, &contact, x0, x1, y);

        // Header double-click: the name row edits the first/last name.
        hits.push((
            Slot::First,
            Rect::new(x0, name_top - 6.0, x1, name_top + 30.0),
        ));

        self.detail_field_hits = hits;
        self.detail_content_h = (y + self.detail_scroll) - top + 24.0;
        scene.pop_layer();
    }

    /// The "no contact selected" placeholder shown in an empty detail pane.
    fn draw_detail_empty_state(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let area = self.detail_rect;
        let msg = if self.store.is_empty() {
            "No contact selected — add one with + New"
        } else {
            "Select a contact to see their details"
        };
        tr.draw_text(
            scene,
            msg,
            15.0,
            c.text_dim,
            t * Affine::translate((area.x0 + PAD, area.y0 + 40.0 + self.detail_top_offset())),
            None,
        );
        self.detail_content_h = 0.0;
        self.website_link = None;
    }

    /// Avatar + favourite/Edit/Delete action band, then the name/subtitle/
    /// group badges on their own rows beneath it. Returns `(name_top, y)`:
    /// the name row's top (for the header's double-click-to-edit hit rect)
    /// and the y cursor for the field sections that follow.
    #[allow(
        clippy::too_many_arguments,
        reason = "a draw helper's scene/transform/text-renderer/theme plus the geometry it paints are each independently meaningful, matching detail_field/toolbar_button/menu_item elsewhere in this file"
    )]
    fn draw_detail_header(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        contact: &Contact,
        x0: f64,
        x1: f64,
        y: f64,
    ) -> (f64, f64) {
        let c = &theme.tokens.colors;
        let av = Rect::new(x0, y, x0 + 72.0, y + 72.0);
        draw_avatar(scene, t, tr, theme, av, &contact.initials());

        // Action buttons (favorite star, Edit, Delete) — top right.
        draw_star(
            scene,
            t,
            self.hit.fav.center(),
            11.0,
            theme.accent_primary,
            contact.favorite,
        );
        toolbar_button(
            scene,
            t,
            tr,
            theme,
            self.hit.edit,
            "Edit",
            true,
            self.hit.edit.contains(self.last_mouse),
            false,
        );
        toolbar_button(
            scene,
            t,
            tr,
            theme,
            self.hit.delete,
            "Delete",
            false,
            self.hit.delete.contains(self.last_mouse),
            false,
        );

        // Name + subtitle on their own full-width rows beneath the avatar.
        let head_w = (x1 - x0).max(40.0) as f32;
        let name_y = y + 88.0;
        let name =
            tr.elide_to_width_weighted(&contact.display_name(), 26.0, head_w, TextWeight::Bold);
        tr.draw_text_weighted(
            scene,
            &name,
            26.0,
            c.text_primary,
            t * Affine::translate((x0, name_y)),
            None,
            TextWeight::Bold,
        );
        let mut hy = name_y + 36.0;
        let sub = contact.subtitle();
        if !sub.is_empty() {
            let sub = tr.elide_to_width(&sub, 14.0, head_w);
            tr.draw_text(
                scene,
                &sub,
                14.0,
                c.text_dim,
                t * Affine::translate((x0, hy)),
                None,
            );
            hy += 26.0;
        }
        // Group badges below the name/subtitle.
        if !contact.groups.is_empty() {
            let mut bx = x0;
            let by = hy;
            for g in &contact.groups {
                let tw = f64::from(tr.measure(g, 11.0, None).0);
                let bw = tw + 18.0;
                let br = Rect::new(bx, by, bx + bw, by + 20.0);
                let shape = RoundedRect::from_rect(br, 10.0);
                scene.fill(
                    Fill::NonZero,
                    t,
                    theme.accent_primary.with_alpha_factor(0.16),
                    None,
                    &shape,
                );
                tr.draw_text(
                    scene,
                    g,
                    11.0,
                    theme.accent_primary,
                    t * Affine::translate((bx + 9.0, by + 4.0)),
                    None,
                );
                bx += bw + 8.0;
                if bx > x1 - 60.0 {
                    break;
                }
            }
            hy += 28.0;
        }

        // Start of the field sections below the header block.
        (name_y, hy + 12.0)
    }

    /// Phone/email/address/birthday/website/notes sections, in order, each
    /// followed by its double-click hit rect. Returns the collected hits and
    /// the y cursor after the last section.
    #[allow(
        clippy::too_many_arguments,
        reason = "a draw helper's scene/transform/text-renderer/theme/presence plus the geometry it paints are each independently meaningful, matching detail_field/toolbar_button/menu_item elsewhere in this file"
    )]
    fn draw_detail_sections(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        contact: &Contact,
        x0: f64,
        x1: f64,
        mut y: f64,
    ) -> (Vec<(Slot, Rect)>, f64) {
        let c = &theme.tokens.colors;
        let mut hits: Vec<(Slot, Rect)> = Vec::new();

        // Sections (free helpers avoid closures double-borrowing scene/tr).
        if !contact.phones.is_empty() {
            detail_section(scene, t, tr, theme, x0, &mut y, "PHONE");
            for (i, p) in contact.phones.iter().enumerate() {
                let y0 = y;
                detail_field(scene, t, tr, theme, x0, x1, &mut y, &p.label, &p.value);
                hits.push((Slot::Phone(i), Rect::new(x0, y0 - 4.0, x1, y)));
            }
        }
        if !contact.emails.is_empty() {
            detail_section(scene, t, tr, theme, x0, &mut y, "EMAIL");
            for (i, e) in contact.emails.iter().enumerate() {
                let y0 = y;
                detail_field(scene, t, tr, theme, x0, x1, &mut y, &e.label, &e.value);
                hits.push((Slot::Email(i), Rect::new(x0, y0 - 4.0, x1, y)));
            }
        }
        if !contact.addresses.is_empty() {
            detail_section(scene, t, tr, theme, x0, &mut y, "ADDRESS");
            for (i, a) in contact.addresses.iter().enumerate() {
                let y0 = y;
                detail_field(scene, t, tr, theme, x0, x1, &mut y, &a.label, &a.one_line());
                // The address is one logical block; double-clicking it edits
                // its street line (the first address input).
                hits.push((Slot::AddrStreet(i), Rect::new(x0, y0 - 4.0, x1, y)));
            }
        }
        if let Some(bday) = &contact.birthday {
            detail_section(scene, t, tr, theme, x0, &mut y, "BIRTHDAY");
            let y0 = y;
            // Render the structured date, formatted; double-click still edits it.
            detail_field(scene, t, tr, theme, x0, x1, &mut y, "date", &bday.pretty());
            hits.push((Slot::Birthday, Rect::new(x0, y0 - 4.0, x1, y)));
        }
        if contact.website.is_empty() {
            self.website_link = None;
        } else {
            detail_section(scene, t, tr, theme, x0, &mut y, "WEBSITE");
            // "url" field label on the left, matching the other detail rows.
            tr.draw_text(
                scene,
                "url",
                12.0,
                c.text_dim,
                t * Affine::translate((x0, y + 2.0)),
                Some(64.0),
            );
            // The website is a live hyperlink (ui `Link` widget), not plain
            // text: single-click opens it via the OS handler. Rebuilt only when
            // the URL changes so its hover/press state survives across frames.
            let vx = x0 + 78.0;
            let vw = (x1 - vx).max(40.0);
            let needs_new = self
                .website_link
                .as_ref()
                .is_none_or(|l| l.url() != contact.website);
            if needs_new {
                self.website_link = Some(Link::new(&contact.website).with_font_size(14.0));
            }
            let (lw, lh) = tr.measure(&contact.website, 14.0, None);
            let lw = f64::from(lw).min(vw);
            let lh = f64::from(lh).max(20.0);
            let lrect = Rect::new(vx, y, vx + lw, y + lh);
            if let Some(link) = self.website_link.as_mut() {
                link.apply_layout_external(lrect);
                link.draw(scene, t, tr, theme, presence);
            }
            y += lh + 8.0;
        }
        if !contact.notes.is_empty() {
            detail_section(scene, t, tr, theme, x0, &mut y, "NOTES");
            let y0 = y;
            let h = f64::from(tr.measure(&contact.notes, 14.0, Some((x1 - x0) as f32)).1);
            tr.draw_text(
                scene,
                &contact.notes,
                14.0,
                c.text_primary,
                t * Affine::translate((x0, y)),
                Some((x1 - x0) as f32),
            );
            y += h + 8.0;
            hits.push((Slot::Notes, Rect::new(x0, y0 - 4.0, x1, y)));
        }

        (hits, y)
    }

    /// Draw the edit form: header (avatar/name/favourite/save/cancel), the
    /// scrollable body's section labels, the embedded field inputs, and the
    /// type chips / add buttons / calendar button. Split into per-band
    /// helpers purely to keep each piece readable; the draw order (and thus
    /// the paint/z-order) is unchanged from one flat function.
    fn draw_form(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let area = self.detail_rect;
        let off = self.detail_top_offset();

        // Header band (not scrolled), two stacked rows. Row 1 is the identity
        // anchor: a compact avatar chip + the record's name, the same shape the
        // read view leads with, so entering the editor reads as the *same*
        // contact rather than a new screen. Row 2 holds the Favorite toggle
        // (left) and the Cancel / Save actions (right). On a narrow window the
        // whole band is pushed down so the Back affordance pill sits above it.
        self.draw_form_header(scene, t, tr, theme);

        // Scrollable body, clipped below the two-row header band (title row +
        // controls row). Derived from the same constants as `layout_form` so
        // the clip starts exactly under the Favorite/Save/Cancel row.
        let body_top = area.y0 + FORM_TITLE_TOP + FORM_TITLE_H + FORM_ROW_GAP + 32.0 + off;
        let body = Rect::new(area.x0, body_top, area.x1, area.y1);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &body);

        // Section labels are drawn relative to the first input of each group.
        let inputs_snapshot: Vec<(Slot, Rect)> = self
            .form
            .as_ref()
            .unwrap()
            .inputs
            .iter()
            .map(|f| (f.slot, f.ti_rect()))
            .collect();
        let draft = self.form.as_ref().unwrap().draft.clone();
        self.draw_form_section_labels(scene, t, tr, theme, &inputs_snapshot);

        // Draw the embedded inputs, each followed by its dim placeholder hint
        // when empty + unfocused. The input paints its own opaque rounded-rect
        // field background first (TextInput::draw), so the placeholder — drawn
        // *after* the input, clipped to the field — sits cleanly on top of that
        // fill and nothing from a neighbouring row can bleed through behind it.
        self.draw_form_inputs(scene, t, tr, theme, presence);

        // Type chips + remove buttons, "+ Add …" buttons, and the calendar
        // button.
        self.draw_form_chips_and_buttons(scene, t, tr, theme, &draft);

        scene.pop_layer();
    }

    /// The form's non-scrolled header band: avatar + name identity row, the
    /// Favorite toggle pill, and the Save/Cancel buttons.
    fn draw_form_header(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let area = self.detail_rect;
        let off = self.detail_top_offset();
        let x0 = area.x0 + PAD;

        let (show_new, draft_name, initials) = {
            let f = self.form.as_ref().unwrap();
            let d = &f.draft;
            // A brand-new record still reads as "New Contact" only while it has
            // no identifying text; once a name/org is typed the heading follows
            // it live, so the identity anchor forms as you fill the record in.
            let blank =
                d.first.trim().is_empty() && d.last.trim().is_empty() && d.org.trim().is_empty();
            (f.is_new && blank, d.display_name(), d.initials())
        };
        let title_y = area.y0 + FORM_TITLE_TOP + off;
        // Avatar chip, vertically centred in the identity row.
        let av_y = title_y + (FORM_TITLE_H - FORM_AVATAR) / 2.0;
        let av = Rect::new(x0, av_y, x0 + FORM_AVATAR, av_y + FORM_AVATAR);
        draw_avatar(scene, t, tr, theme, av, &initials);
        // Heading: the forming name, or a clear "New Contact" while it is blank
        // so a fresh record never reads as the "No Name" placeholder.
        let heading = if show_new {
            "New Contact".to_string()
        } else {
            draft_name
        };
        let head_avail = (self.hit.cancel.x0 - 12.0 - (av.x1 + 12.0)).max(40.0) as f32;
        let heading = tr.elide_to_width_weighted(&heading, 20.0, head_avail, TextWeight::Bold);
        tr.draw_text_weighted(
            scene,
            &heading,
            20.0,
            c.text_primary,
            t * Affine::translate((av.x1 + 12.0, av.center().y - 13.0)),
            None,
            TextWeight::Bold,
        );
        let fav = self.form.as_ref().is_some_and(|f| f.draft.favorite);
        // Favorite toggle pill.
        {
            let r = self.hit.fav_toggle;
            let hovered = r.contains(self.last_mouse);
            let shape = RoundedRect::from_rect(r, f64::from(theme.tokens.radii.md));
            let bg = if fav {
                theme.accent_primary.with_alpha_factor(0.18)
            } else if hovered {
                theme.bg_hover
            } else {
                c.surfaces.raised
            };
            scene.fill(Fill::NonZero, t, bg, None, &shape);
            scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            draw_star(
                scene,
                t,
                Point::new(r.x0 + 16.0, r.center().y),
                7.0,
                theme.accent_primary,
                fav,
            );
            tr.draw_text(
                scene,
                if fav { "Favorited" } else { "Favorite" },
                12.0,
                if fav {
                    theme.accent_primary
                } else {
                    c.text_dim
                },
                t * Affine::translate((r.x0 + 30.0, r.center().y - 7.0)),
                None,
            );
        }
        toolbar_button(
            scene,
            t,
            tr,
            theme,
            self.hit.save,
            "Save",
            true,
            self.hit.save.contains(self.last_mouse),
            false,
        );
        toolbar_button(
            scene,
            t,
            tr,
            theme,
            self.hit.cancel,
            "Cancel",
            false,
            self.hit.cancel.contains(self.last_mouse),
            false,
        );
    }

    /// Draw the field section labels (NAME, ORGANIZATION, PHONE, …) anchored
    /// above their corresponding input rows (or the group's "+ Add" button
    /// when the group is empty).
    fn draw_form_section_labels(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        inputs_snapshot: &[(Slot, Rect)],
    ) {
        let c = &theme.tokens.colors;
        let x0 = self.detail_rect.x0 + PAD;
        let label = |scene: &mut Scene, tr: &mut TextRenderer, text: &str, rect: Rect| {
            tr.draw_text_weighted(
                scene,
                text,
                11.0,
                c.text_dim,
                t * Affine::translate((rect.x0, rect.y0 - 18.0)),
                None,
                TextWeight::Semibold,
            );
        };
        let find = |slot: Slot| {
            inputs_snapshot
                .iter()
                .find(|(s, _)| *s == slot)
                .map(|(_, r)| *r)
        };
        if let Some(r) = find(Slot::First) {
            label(scene, tr, "NAME", r);
        }
        if let Some(r) = find(Slot::Org) {
            label(scene, tr, "ORGANIZATION", r);
        }
        // Phone / email / address section headers anchored ABOVE the first
        // field row of each group (falling back to the group's "+ Add" button
        // when the group is empty). The header's *x* is pinned to the form's
        // left margin — the type-chip column — not the indented value input, so
        // PHONE / EMAIL / ADDRESS line up flush-left with NAME / ORGANIZATION
        // instead of floating in over the value column. Its *y* still tracks the
        // first row so it sits cleanly above the group.
        let section_anchor = |slot: Slot, add_btn: Rect| -> Rect {
            let r = find(slot).unwrap_or(add_btn);
            Rect::new(x0, r.y0, r.x1, r.y1)
        };
        label(
            scene,
            tr,
            "PHONE",
            section_anchor(Slot::Phone(0), self.hit.add_phone),
        );
        label(
            scene,
            tr,
            "EMAIL",
            section_anchor(Slot::Email(0), self.hit.add_email),
        );
        label(
            scene,
            tr,
            "ADDRESS",
            section_anchor(Slot::AddrStreet(0), self.hit.add_address),
        );
        if let Some(r) = find(Slot::Birthday) {
            label(scene, tr, "BIRTHDAY", r);
        }
        if let Some(r) = find(Slot::Website) {
            label(scene, tr, "WEBSITE", Rect::new(r.x0, r.y0, r.x1, r.y1));
        }
        if let Some(r) = find(Slot::Notes) {
            label(scene, tr, "NOTES", r);
        }
        if let Some(r) = find(Slot::Groups) {
            label(scene, tr, "GROUPS (comma separated)", r);
        }
    }

    /// Draw each embedded form input, with its dim placeholder hint when
    /// empty and unfocused.
    fn draw_form_inputs(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        for f in &mut self.form.as_mut().unwrap().inputs {
            f.ti.draw(scene, t, tr, theme, presence);
            if f.ti.value().is_empty() && !f.ti.is_focused() {
                let r = f.ti.get_rect();
                if r.width() > 0.0 {
                    let shape = RoundedRect::from_rect(r, f64::from(theme.tokens.radii.md));
                    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &shape);
                    tr.draw_text(
                        scene,
                        placeholder_for(f.slot),
                        14.0,
                        c.text_dim,
                        t * Affine::translate((r.x0 + 12.0, r.center().y - 9.0)),
                        Some((r.width() - 20.0) as f32),
                    );
                    scene.pop_layer();
                }
            }
        }
    }

    /// Draw the phone/email/address type chips, their remove buttons, the
    /// "+ Add …" buttons, and the birthday calendar button.
    fn draw_form_chips_and_buttons(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        draft: &Contact,
    ) {
        let c = &theme.tokens.colors;
        for (target, rect) in &self.hit.type_chips {
            let lbl = type_label(draft, *target);
            chip(scene, t, tr, theme, *rect, lbl, false);
        }
        for (_target, rect) in &self.hit.remove_btns {
            let shape = RoundedRect::from_rect(*rect, f64::from(theme.tokens.radii.md));
            scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            // minus glyph
            let cy = rect.center().y;
            scene.stroke(
                &Stroke::new(1.6),
                t,
                c.semantic.danger,
                None,
                &Line::new((rect.center().x - 5.0, cy), (rect.center().x + 5.0, cy)),
            );
        }
        add_button(scene, t, tr, theme, self.hit.add_phone, "+ Add phone");
        add_button(scene, t, tr, theme, self.hit.add_email, "+ Add email");
        add_button(scene, t, tr, theme, self.hit.add_address, "+ Add address");
        {
            let r = self.hit.cal_btn;
            let shape = RoundedRect::from_rect(r, f64::from(theme.tokens.radii.md));
            scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
            scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            draw_calendar_glyph(scene, t, r, theme);
        }
    }

    /// Draw the currently-open popup. Split per popup variant purely for
    /// length; the drawn content is unchanged from one flat `match`.
    fn draw_popup(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        popup: &Popup,
    ) {
        match popup {
            Popup::TypeMenu { anchor, .. } => {
                self.draw_popup_type_menu(scene, t, tr, theme, *anchor);
            }
            Popup::GroupFilter { anchor } => {
                self.draw_popup_group_filter(scene, t, tr, theme, *anchor);
            }
            Popup::Calendar {
                year,
                month,
                anchor,
            } => self.draw_popup_calendar(scene, t, tr, theme, *anchor, *year, *month),
            Popup::ContextMenu { target, anchor } => {
                self.draw_popup_context_menu(scene, t, tr, theme, *anchor, *target);
            }
        }
    }

    /// The phone/email/address type chip menu.
    fn draw_popup_type_menu(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        anchor: Rect,
    ) {
        let (panel, rows) = self.popup_menu_geometry(anchor, TYPE_OPTIONS.len(), 140.0);
        menu_panel(scene, t, theme, panel);
        for (i, r) in rows.iter().enumerate() {
            let hov = r.contains(self.last_mouse);
            menu_item(scene, t, tr, theme, *r, TYPE_OPTIONS[i], hov, false);
        }
    }

    /// The list header's group-filter menu ("All" + every group).
    fn draw_popup_group_filter(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        anchor: Rect,
    ) {
        let mut opts = vec!["All".to_string()];
        opts.extend(self.store.all_groups());
        let (panel, rows) = self.popup_menu_geometry(anchor, opts.len(), 180.0);
        menu_panel(scene, t, theme, panel);
        for (i, r) in rows.iter().enumerate() {
            let hov = r.contains(self.last_mouse);
            let sel = match &self.group_filter {
                None => i == 0,
                Some(g) => &opts[i] == g,
            };
            menu_item(scene, t, tr, theme, *r, &opts[i], hov, sel);
        }
    }

    /// The birthday calendar popup: month/year header with prev/next
    /// chevrons, the weekday header row, and every day cell (hover +
    /// selected-day highlight).
    #[allow(clippy::too_many_arguments)]
    fn draw_popup_calendar(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        anchor: Rect,
        year: i32,
        month: u32,
    ) {
        let c = &theme.tokens.colors;
        let (panel, prev, next, cells) = self.calendar_geometry(anchor, year, month);
        menu_panel(scene, t, theme, panel);
        // Header: < Month Year >
        let title = format!("{} {}", date::month_name(month), year);
        let tw = f64::from(tr.measure(&title, 14.0, None).0);
        tr.draw_text_weighted(
            scene,
            &title,
            14.0,
            c.text_primary,
            t * Affine::translate((panel.center().x - tw / 2.0, panel.y0 + 14.0)),
            None,
            TextWeight::Semibold,
        );
        draw_chevron(scene, t, prev, theme, true);
        draw_chevron(scene, t, next, theme, false);
        // Weekday header.
        let cell = (panel.width() - 16.0) / 7.0;
        for (i, wd) in ["S", "M", "T", "W", "T", "F", "S"].iter().enumerate() {
            let cx = panel.x0 + 8.0 + i as f64 * cell;
            tr.draw_text(
                scene,
                wd,
                11.0,
                c.text_dim,
                t * Affine::translate((cx + cell / 2.0 - 4.0, panel.y0 + 50.0)),
                None,
            );
        }
        let cur = self
            .form
            .as_ref()
            .and_then(|f| f.draft.birthday.map(|b| b.iso()))
            .unwrap_or_default();
        for (d, r) in &cells {
            let hov = r.contains(self.last_mouse);
            let is_sel = cur == format!("{year:04}-{month:02}-{d:02}");
            if hov || is_sel {
                let shape = Circle::new(r.center(), (r.width().min(r.height()) / 2.0) - 2.0);
                let col = if is_sel {
                    theme.accent_primary
                } else {
                    theme.accent_primary.with_alpha_factor(0.20)
                };
                scene.fill(Fill::NonZero, t, col, None, &shape);
            }
            let s = d.to_string();
            let dw = f64::from(tr.measure(&s, 12.0, None).0);
            let col = if is_sel { Color::WHITE } else { c.text_primary };
            tr.draw_text(
                scene,
                &s,
                12.0,
                col,
                t * Affine::translate((r.center().x - dw / 2.0, r.center().y - 8.0)),
                None,
            );
        }
    }

    /// The per-contact right-click context menu (Edit / Favorite / Export /
    /// Delete), with the Favorite label reflecting the target's current
    /// state and Delete painted in the danger color when not hovered.
    fn draw_popup_context_menu(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        anchor: Rect,
        target: ContactId,
    ) {
        let c = &theme.tokens.colors;
        let (panel, rows) = self.popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
        menu_panel(scene, t, theme, panel);
        // The Favorite item's label reflects the target contact's
        // current state ("Add to favorites" vs "Remove from favorites").
        let is_fav = self.store.get(target).is_some_and(|c| c.favorite);
        for (i, r) in rows.iter().enumerate() {
            let hov = r.contains(self.last_mouse);
            let action = CONTEXT_ACTIONS[i];
            let label = action.label(is_fav);
            // The destructive "Delete" item reads in the danger color
            // when not hovered (hover inverts to white-on-accent).
            if action == ContextAction::Delete && !hov {
                menu_item_colored(scene, t, tr, theme, *r, label, c.semantic.danger);
            } else {
                menu_item(scene, t, tr, theme, *r, label, hov, false);
            }
        }
    }

    /// The export-selection dialog: a centered modal listing every contact
    /// with a per-row checkbox, Select all / Select none, and the two vCard
    /// version export actions. The user exports exactly the ticked set.
    fn draw_export_dialog(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let sel = self.export_sel.clone().unwrap_or_default();
        // Scrim.
        scene.fill(Fill::NonZero, t, c.surfaces.overlay, None, &self.rect);
        let g = self.export_geometry();
        menu_panel(scene, t, theme, g.panel);
        // Title + count.
        tr.draw_text_weighted(
            scene,
            "Export contacts",
            16.0,
            c.text_primary,
            t * Affine::translate((g.panel.x0 + 18.0, g.panel.y0 + 18.0)),
            None,
            TextWeight::Bold,
        );
        tr.draw_text(
            scene,
            &format!("{} selected", sel.len()),
            12.0,
            c.text_dim,
            t * Affine::translate((g.panel.x0 + 18.0, g.panel.y0 + 40.0)),
            None,
        );
        // Select all / none chips.
        chip(scene, t, tr, theme, g.select_all, "Select all", false);
        chip(scene, t, tr, theme, g.select_none, "Select none", false);

        // Scrollable contact list with checkboxes.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &g.list_body);
        for (id, row, cb) in &g.rows {
            if row.y1 < g.list_body.y0 || row.y0 > g.list_body.y1 {
                continue;
            }
            let Some(contact) = self.store.get(*id) else {
                continue;
            };
            let checked = sel.contains(id);
            let hov = row.contains(self.last_mouse);
            if hov {
                let bg = RoundedRect::from_rect(*row, f64::from(theme.tokens.radii.md));
                scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &bg);
            }
            draw_checkbox(scene, t, theme, *cb, checked);
            tr.draw_text(
                scene,
                &contact.display_name(),
                14.0,
                c.text_primary,
                t * Affine::translate((cb.x1 + 12.0, row.center().y - 9.0)),
                Some((row.x1 - cb.x1 - 20.0) as f32),
            );
        }
        scene.pop_layer();

        // Footer buttons.
        let m = self.last_mouse;
        toolbar_button(
            scene,
            t,
            tr,
            theme,
            g.cancel,
            "Cancel",
            false,
            g.cancel.contains(m),
            false,
        );
        toolbar_button(
            scene,
            t,
            tr,
            theme,
            g.export_v3,
            "Export 3.0",
            false,
            g.export_v3.contains(m),
            false,
        );
        toolbar_button(
            scene,
            t,
            tr,
            theme,
            g.export_v4,
            "Export 4.0",
            true,
            g.export_v4.contains(m),
            false,
        );
        let _ = g.content_h;
    }

    /// The "Save changes?" guard modal shown when the user tries to switch
    /// contacts while an edit form holds unsaved changes.
    fn draw_unsaved_modal(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, t, c.surfaces.overlay, None, &self.rect);
        let g = self.unsaved_geometry();
        menu_panel(scene, t, theme, g.panel);
        tr.draw_text_weighted(
            scene,
            "Save changes?",
            16.0,
            c.text_primary,
            t * Affine::translate((g.panel.x0 + 20.0, g.panel.y0 + 20.0)),
            None,
            TextWeight::Bold,
        );
        let name = self
            .form
            .as_ref()
            .map(|f| f.draft.display_name())
            .unwrap_or_default();
        tr.draw_text(
            scene,
            &format!("You have unsaved changes to \u{201C}{name}\u{201D}."),
            13.0,
            c.text_dim,
            t * Affine::translate((g.panel.x0 + 20.0, g.panel.y0 + 48.0)),
            Some((g.panel.width() - 40.0) as f32),
        );
        let m = self.last_mouse;
        toolbar_button(
            scene,
            t,
            tr,
            theme,
            g.cancel,
            "Cancel",
            false,
            g.cancel.contains(m),
            false,
        );
        toolbar_button(
            scene,
            t,
            tr,
            theme,
            g.discard,
            "Discard",
            false,
            g.discard.contains(m),
            false,
        );
        toolbar_button(
            scene,
            t,
            tr,
            theme,
            g.save,
            "Save",
            true,
            g.save.contains(m),
            false,
        );
    }

    fn draw_confirm(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        id: ContactId,
    ) {
        let c = &theme.tokens.colors;
        // Scrim.
        scene.fill(Fill::NonZero, t, c.surfaces.overlay, None, &self.rect);
        let (panel, del, cancel) = self.confirm_geometry();
        menu_panel(scene, t, theme, panel);
        let name = self
            .store
            .get(id)
            .map(super::model::Contact::display_name)
            .unwrap_or_default();
        tr.draw_text_weighted(
            scene,
            "Delete contact?",
            16.0,
            c.text_primary,
            t * Affine::translate((panel.x0 + 20.0, panel.y0 + 20.0)),
            None,
            TextWeight::Bold,
        );
        tr.draw_text(
            scene,
            &format!("\u{201C}{name}\u{201D} will be removed from this address book."),
            13.0,
            c.text_dim,
            t * Affine::translate((panel.x0 + 20.0, panel.y0 + 48.0)),
            Some((panel.width() - 40.0) as f32),
        );
        let m = self.last_mouse;
        toolbar_button(
            scene,
            t,
            tr,
            theme,
            cancel,
            "Cancel",
            false,
            cancel.contains(m),
            false,
        );
        // Destructive confirm — the shared danger face, so it gives the same
        // hover feedback as every other button rather than reading as dead.
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(del, "Delete")
                .style(ButtonStyle::Danger)
                .radius(f64::from(theme.tokens.radii.md))
                .text_size(14.0)
                .weight(TextWeight::Semibold)
                .hovered(del.contains(m)),
        );
    }

    fn confirm_geometry(&self) -> (Rect, Rect, Rect) {
        let w = 380.0;
        let h = 150.0;
        let cx = self.rect.center().x;
        let cy = self.rect.center().y;
        let panel = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
        let by = panel.y1 - 48.0;
        let del = Rect::new(panel.x1 - 20.0 - 96.0, by, panel.x1 - 20.0, by + 32.0);
        let cancel = Rect::new(del.x0 - 8.0 - 88.0, by, del.x0 - 8.0, by + 32.0);
        (panel, del, cancel)
    }
}

/// Geometry of the export-selection dialog. Grouped so draw + hit-test share
/// one source of truth.
struct ExportGeom {
    panel: Rect,
    select_all: Rect,
    select_none: Rect,
    list_body: Rect,
    /// `(contact id, full row rect, checkbox rect)` for every contact.
    rows: Vec<(ContactId, Rect, Rect)>,
    export_v3: Rect,
    export_v4: Rect,
    cancel: Rect,
    /// Total scrollable content height (for clamping `export_scroll`).
    content_h: f64,
}

/// Geometry of the unsaved-changes ("Save changes?") modal.
struct UnsavedGeom {
    panel: Rect,
    save: Rect,
    discard: Rect,
    cancel: Rect,
}

impl ContactsApp {
    fn export_geometry(&self) -> ExportGeom {
        let w = 460.0_f64;
        let h = (self.rect.height() * 0.8)
            .clamp(320.0, 560.0)
            .min(self.rect.height() - 40.0);
        let cx = self.rect.center().x;
        let cy = self.rect.center().y;
        let panel = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
        // Header band: title + Select all / none on the right.
        let head_y = panel.y0 + 18.0;
        let select_none = Rect::new(
            panel.x1 - 16.0 - 96.0,
            head_y,
            panel.x1 - 16.0,
            head_y + 28.0,
        );
        let select_all = Rect::new(
            select_none.x0 - 8.0 - 96.0,
            head_y,
            select_none.x0 - 8.0,
            head_y + 28.0,
        );
        // Scrollable list region between header and footer.
        let list_top = panel.y0 + 64.0;
        let footer_top = panel.y1 - 56.0;
        let list_body = Rect::new(panel.x0 + 16.0, list_top, panel.x1 - 16.0, footer_top - 8.0);
        let ids = self.visible_export_ids();
        let row_h = 40.0;
        let mut rows = Vec::new();
        let mut ry = list_body.y0 - self.export_scroll;
        for id in &ids {
            let row = Rect::new(list_body.x0, ry, list_body.x1, ry + row_h);
            let cb = Rect::new(
                row.x0 + 4.0,
                row.center().y - 10.0,
                row.x0 + 24.0,
                row.center().y + 10.0,
            );
            rows.push((*id, row, cb));
            ry += row_h;
        }
        let content_h = ids.len() as f64 * row_h;
        // Footer buttons.
        let by = panel.y1 - 44.0;
        let export_v4 = Rect::new(panel.x1 - 16.0 - 120.0, by, panel.x1 - 16.0, by + 32.0);
        let export_v3 = Rect::new(
            export_v4.x0 - 8.0 - 120.0,
            by,
            export_v4.x0 - 8.0,
            by + 32.0,
        );
        let cancel = Rect::new(panel.x0 + 16.0, by, panel.x0 + 16.0 + 88.0, by + 32.0);
        ExportGeom {
            panel,
            select_all,
            select_none,
            list_body,
            rows,
            export_v3,
            export_v4,
            cancel,
            content_h,
        }
    }

    /// Contacts offered in the export dialog: the whole address book in the
    /// current sort order (export is not limited to the active filter — the
    /// dialog lets you pick from everyone).
    fn visible_export_ids(&self) -> Vec<ContactId> {
        self.store.filtered_sorted("", None, self.sort)
    }

    fn unsaved_geometry(&self) -> UnsavedGeom {
        let w = 420.0;
        let h = 156.0;
        let cx = self.rect.center().x;
        let cy = self.rect.center().y;
        let panel = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
        let by = panel.y1 - 48.0;
        let save = Rect::new(panel.x1 - 16.0 - 96.0, by, panel.x1 - 16.0, by + 32.0);
        let discard = Rect::new(save.x0 - 8.0 - 96.0, by, save.x0 - 8.0, by + 32.0);
        let cancel = Rect::new(panel.x0 + 16.0, by, panel.x0 + 16.0 + 88.0, by + 32.0);
        UnsavedGeom {
            panel,
            save,
            discard,
            cancel,
        }
    }
}

impl FieldInput {
    fn ti_rect(&self) -> Rect {
        self.ti.get_rect()
    }
}

// =====================================================================
// Drawing helpers (free functions)
// =====================================================================

fn detail_section(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    x0: f64,
    y: &mut f64,
    title: &str,
) {
    *y += 10.0;
    tr.draw_text_weighted(
        scene,
        title,
        11.0,
        theme.tokens.colors.text_dim,
        t * Affine::translate((x0, *y)),
        None,
        TextWeight::Semibold,
    );
    *y += 22.0;
}

#[allow(clippy::too_many_arguments)]
fn detail_field(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    x0: f64,
    x1: f64,
    y: &mut f64,
    label: &str,
    value: &str,
) {
    let c = &theme.tokens.colors;
    tr.draw_text(
        scene,
        label,
        12.0,
        c.text_dim,
        t * Affine::translate((x0, *y + 2.0)),
        Some(64.0),
    );
    let vw = (x1 - x0 - 78.0).max(40.0);
    let h = f64::from(tr.measure(value, 14.0, Some(vw as f32)).1);
    tr.draw_text(
        scene,
        value,
        14.0,
        c.text_primary,
        t * Affine::translate((x0 + 78.0, *y)),
        Some(vw as f32),
    );
    *y += h.max(20.0) + 8.0;
}

fn type_label(draft: &Contact, target: TypeTarget) -> &str {
    let s = match target {
        TypeTarget::Phone(i) => draft.phones.get(i).map(|p| p.label.as_str()),
        TypeTarget::Email(i) => draft.emails.get(i).map(|e| e.label.as_str()),
        TypeTarget::Addr(i) => draft.addresses.get(i).map(|a| a.label.as_str()),
    };
    match s {
        Some(x) if !x.is_empty() => x,
        _ => "type",
    }
}

/// A themed chrome button, painted through the shared [`ui::Button`] face
/// ([`Button::paint`]/[`ButtonFace`]) so every button in the app reads from the
/// one `face_colors` source of truth and gains real hover / press feedback,
/// rather than a hand-rolled fill that looks dead. `hovered`/`pressed` come from
/// the caller's own hit-testing against the last pointer position; the rest
/// look (radius, 13px medium label, accent-vs-raised fills) matches the chrome
/// this app has always drawn.
#[allow(
    clippy::too_many_arguments,
    reason = "a draw helper's scene/transform/text-renderer/theme plus the geometry and state it paints are each independently meaningful, matching detail_field/menu_item elsewhere in this file"
)]
fn toolbar_button(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    primary: bool,
    hovered: bool,
    pressed: bool,
) {
    if rect.width() <= 0.0 {
        return;
    }
    let style = if primary {
        ButtonStyle::Primary
    } else {
        ButtonStyle::Neutral
    };
    Button::paint(
        scene,
        t,
        tr,
        theme,
        &ButtonFace::new(rect, label)
            .style(style)
            .radius(f64::from(theme.tokens.radii.md))
            .text_size(13.0)
            .weight(TextWeight::Medium)
            .hovered(hovered)
            .pressed(pressed),
    );
}

fn add_button(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
) {
    let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.md));
    scene.stroke(
        &Stroke::new(1.0),
        t,
        theme.accent_primary.with_alpha_factor(0.6),
        None,
        &shape,
    );
    tr.draw_text_weighted(
        scene,
        label,
        12.0,
        theme.accent_primary,
        t * Affine::translate((rect.x0 + 10.0, rect.center().y - 7.0)),
        None,
        TextWeight::Medium,
    );
}

fn chip(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    active: bool,
) {
    Chip::draw_at(
        scene,
        t,
        rect,
        label,
        ChipLead::None,
        contact_chip_style(theme, active),
        theme,
        tr,
    );
}

/// Contacts owns chip hit-testing, while the toolkit owns their presentation.
/// Resolve selected-chip colours on the pane they are actually composited over
/// so light, dark and high-contrast themes all keep readable labels and edges.
fn contact_chip_style(theme: &Theme, active: bool) -> ChipStyle {
    if active {
        ChipStyle::measured(
            theme
                .tokens
                .colors
                .accent_chip_on(theme.tokens.colors.surfaces.base, ON_FILL_MIN_CONTRAST),
        )
    } else {
        ChipStyle::quiet()
    }
}

fn menu_panel(scene: &mut Scene, t: Affine, theme: &Theme, panel: Rect) {
    let c = &theme.tokens.colors;
    // Soft shadow.
    let shadow = RoundedRect::from_rect(panel.inset(2.0), f64::from(theme.tokens.radii.md) + 1.0);
    scene.fill(
        Fill::NonZero,
        t,
        Color::from_rgba8(0, 0, 0, 90),
        None,
        &shadow,
    );
    let shape = RoundedRect::from_rect(panel, f64::from(theme.tokens.radii.md));
    scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
    scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
}

#[allow(clippy::too_many_arguments)]
fn menu_item(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    hover: bool,
    selected: bool,
) {
    let c = &theme.tokens.colors;
    if hover {
        let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.md));
        scene.fill(Fill::NonZero, t, theme.accent_primary, None, &shape);
    } else if selected {
        let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.md));
        scene.fill(
            Fill::NonZero,
            t,
            theme.accent_primary.with_alpha_factor(0.16),
            None,
            &shape,
        );
    }
    let col = if hover { Color::WHITE } else { c.text_primary };
    tr.draw_text(
        scene,
        label,
        13.0,
        col,
        t * Affine::translate((rect.x0 + 12.0, rect.center().y - 8.0)),
        Some((rect.width() - 20.0) as f32),
    );
}

/// A non-hovered menu row that paints its label in `color` (used for the
/// destructive "Delete" action in the context menu). The hovered state is
/// drawn by the caller via the regular [`menu_item`] (white-on-accent).
fn menu_item_colored(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    _theme: &Theme,
    rect: Rect,
    label: &str,
    color: Color,
) {
    tr.draw_text(
        scene,
        label,
        13.0,
        color,
        t * Affine::translate((rect.x0 + 12.0, rect.center().y - 8.0)),
        Some((rect.width() - 20.0) as f32),
    );
}

fn draw_checkbox(scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect, checked: bool) {
    Checkbox::paint(
        scene,
        t,
        theme,
        &CheckboxFace::new(rect, checked).transparent_unchecked(),
    );
}

fn draw_avatar(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    initials: &str,
) {
    // Coloured initials disc sourced from the shared toolkit avatar so the
    // hue/shape/placement stay consistent with every other app's roster.
    let color = Avatar::color_for_initials(initials, theme);
    Avatar::draw_at(scene, t, rect, initials, color, theme, tr);
}

fn draw_star(scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color, filled: bool) {
    let rect = Rect::new(center.x - r, center.y - r, center.x + r, center.y + r);
    if filled {
        Icon::Star.draw_filled(scene, t, rect, color);
    } else {
        Icon::Star.draw(scene, t, rect, color);
    }
}

fn draw_calendar_glyph(scene: &mut Scene, t: Affine, rect: Rect, theme: &Theme) {
    let g = Rect::from_center_size(rect.center(), (18.0, 18.0));
    Icon::Calendar.draw(scene, t, g, theme.tokens.colors.text_dim);
}

fn draw_chevron(scene: &mut Scene, t: Affine, rect: Rect, theme: &Theme, left: bool) {
    let icon = if left {
        Icon::ChevronLeft
    } else {
        Icon::ChevronRight
    };
    icon.draw(scene, t, rect, theme.tokens.colors.text_primary);
}

fn draw_toast(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    area: Rect,
    msg: &str,
) {
    let c = &theme.tokens.colors;
    let tw = f64::from(tr.measure(msg, 13.0, None).0);
    let w = (tw + 36.0).min(area.width() - 40.0);
    let h = 38.0;
    let x0 = area.center().x - w / 2.0;
    let y0 = area.y1 - h - 20.0;
    let r = Rect::new(x0, y0, x0 + w, y0 + h);
    let shape = RoundedRect::from_rect(r, f64::from(theme.tokens.radii.md));
    scene.fill(
        Fill::NonZero,
        t,
        Color::from_rgba8(0, 0, 0, 110),
        None,
        &RoundedRect::from_rect(r.inset(2.0), 10.0),
    );
    scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
    scene.stroke(&Stroke::new(1.0), t, theme.accent_primary, None, &shape);
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &shape);
    tr.draw_text(
        scene,
        msg,
        13.0,
        c.text_primary,
        t * Affine::translate((r.x0 + 18.0, r.center().y - 8.0)),
        None,
    );
    scene.pop_layer();
}

// =====================================================================
// Widget impl
// =====================================================================

impl Widget for ContactsApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
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
            } else if let Some(path) = self.legacy_book.clone() {
                // Test-only: seed from a legacy address-book file. The live
                // app arrives with its document already loaded by the store.
                for c in crate::storage::load(&path) {
                    ContactsDoc::upsert_contact(state, &c);
                    self.store.upsert(c);
                }
            }
            if self.selected.is_none() {
                self.selected = prefer.or_else(|| self.visible_ids().first().copied());
            }
            self.seeded = true;
            self.model_heads = state.get_heads();
            self.relayout = true;
            self.dirty = true;
        }

        // Reconcile the in-memory store from the document when it has moved past
        // what the store was last derived from — a Ctrl+K palette action or a
        // merged peer change mutates only the document, so the list would
        // otherwise miss it. An on-screen affordance already updates the store
        // eagerly, so this is a no-op for it beyond re-adopting the same records.
        let heads = state.get_heads();
        if heads != self.model_heads {
            self.store.contacts = ContactsDoc::list_contacts(state);
            self.model_heads = heads;
            if let Some(sel) = self.selected
                && self.store.get(sel).is_none()
            {
                self.selected = self.visible_ids().first().copied();
            }
            self.relayout = true;
            self.dirty = true;
        }

        crate::priority_contacts::publish_if_changed(&self.store);

        self.search.sync(state, path);
        if let Some(form) = &mut self.form {
            for f in &mut form.inputs {
                f.ti.sync(state, path);
            }
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.layout();
        self.layout_detail_actions();
        self.relayout = false;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.render(scene, transform, tr, theme, presence);
    }

    /// Route one window event. Split into per-concern helpers purely for
    /// length; the guard order (which layer consumes the event) is
    /// unchanged from one flat function.
    #[allow(
        clippy::too_many_arguments,
        reason = "Widget::handle_event's arity is fixed by the trait (ui::widget::Widget); every parameter is independently meaningful, as the trait's own allow in ui/src/widget/mod.rs explains"
    )]
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Track logical cursor position.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
            // Update the website hyperlink's hover so its underline thickens as
            // the pointer passes over it (view mode only).
            if self.mode == Mode::View {
                let over = self
                    .website_link
                    .as_ref()
                    .is_some_and(|l| l.get_rect().contains(self.last_mouse));
                if let Some(l) = self.website_link.as_mut()
                    && l.set_hovered(over)
                {
                    self.dirty = true;
                }
            }
        }

        // Modal layers consume input first.
        if self.handle_event_modals(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            node_id,
            taffy,
        ) {
            return;
        }

        if self.handle_event_dispatch(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            node_id,
            taffy,
        ) {
            return;
        }

        // Forward pointer-move to focused inputs so hover/drag-select tracks.
        self.forward_event_to_inputs(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            node_id,
            taffy,
        );
    }

    fn name(&self) -> &'static str {
        "ContactsApp"
    }

    /// Publish the open book + its contacts so the host's Ctrl+K action palette
    /// scopes to them. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its contacts action, so it records one undo boundary for it (see
    /// [`Self::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.take_committed()
    }

    /// Fold the address book into the unified Ctrl+K palette: a snapshot
    /// [`ContactSearchProvider`](crate::search_provider::ContactSearchProvider)
    /// makes every contact findable there, alongside any actions the app
    /// exports.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(
            crate::search_provider::ContactSearchProvider::new(&self.store),
        )]
    }

    /// Navigate to the contact a chosen palette row identifies (selection is
    /// app-local presentation, not a shared-doc mutation).
    fn activate_search_result(&mut self, action_id: &str, _doc: &mut AutoCommit, _path: &ObjId) {
        if let Some(id) = crate::search_provider::ContactSearchProvider::parse_action_id(action_id)
            && self.store.get(id).is_some()
        {
            self.reveal_contact(id);
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The pointing-hand cursor over the view-mode website hyperlink; every
    /// other region defers to the default arrow.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.mode == Mode::View
            && let Some(l) = &self.website_link
            && l.get_rect().contains(point)
        {
            return Some(winit::window::CursorIcon::Pointer);
        }
        None
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.push.is_animating() || self.toast.is_some() || self.portal_in_flight()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the host pumping frames while the master↔detail push slide
        // eases into place on a narrow window, while a toast is up so its
        // timed dismissal fires without further input, or while a file-chooser
        // portal is in flight so its outcome is drained the moment it arrives.
        self.push.is_animating() || self.toast.is_some() || self.portal_in_flight()
    }

    fn is_layout_dirty(&self) -> bool {
        self.relayout
    }

    fn needs_commit(&self) -> bool {
        self.search.needs_commit()
            || self
                .form
                .as_ref()
                .is_some_and(|f| f.inputs.iter().any(|i| i.ti.needs_commit()))
    }

    fn on_commit(&mut self) {
        self.search.on_commit();
        if let Some(form) = &mut self.form {
            for f in &mut form.inputs {
                f.ti.on_commit();
            }
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.render(scene, transform, tr, theme, presence);
    }
}

impl ContactsApp {
    /// While a blocking modal (export / unsaved-guard / delete-confirm) is
    /// up, keyboard is limited to Escape — which dismisses it through
    /// `handle_key`'s escape branch — and every other key is swallowed so a
    /// shortcut behind the scrim can't fire. Pointer interaction still routes
    /// to the per-modal handlers here. Returns `true` if the caller should
    /// return immediately (this method already did everything needed).
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors handle_event's own event-routing signature; splitting further would need a context struct for a one-call-site helper"
    )]
    fn handle_event_modals(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if (self.export_sel.is_some()
            || self.pending_switch.is_some()
            || self.confirm_delete.is_some())
            && let WindowEvent::KeyboardInput { event: ke, .. } = event
        {
            if ke.state == ElementState::Pressed
                && matches!(ke.logical_key, Key::Named(NamedKey::Escape))
            {
                self.handle_key(
                    ke,
                    state,
                    path,
                    handled,
                    node_id,
                    taffy,
                    modifiers,
                    scale_factor,
                    event,
                );
            }
            *handled = true;
            return true;
        }
        if self.export_sel.is_some() {
            self.handle_export(event, handled);
            return true;
        }
        if self.pending_switch.is_some() {
            self.handle_unsaved(event, state, handled);
            return true;
        }
        if self.confirm_delete.is_some() {
            self.handle_confirm(event, state, handled);
            return true;
        }
        if self.popup.is_some() {
            self.handle_popup(event, state, handled);
            // Popups consume clicks; let hover fall through harmlessly.
            if *handled {
                return true;
            }
        }
        false
    }

    /// Divider drag, primary/secondary click, mouse wheel scroll, and
    /// keyboard — the main per-`WindowEvent`-variant dispatch. Returns
    /// `true` if `event` was consumed here (the caller then returns without
    /// forwarding to inputs); `false` for a variant this function doesn't
    /// own, so the caller still forwards it as a pointer-move.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors handle_event's own event-routing signature; splitting further would need a context struct for a one-call-site helper"
    )]
    fn handle_event_dispatch(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        match event {
            WindowEvent::CursorMoved { .. } => {
                // The divider only exists in the wide side-by-side layout.
                self.divider_hover = !self.narrow && self.divider_hit().contains(self.last_mouse);
                if self.divider_drag {
                    let content_w = (self.rect.width()).max(1.0);
                    let rel = (self.last_mouse.x - self.rect.x0) / content_w;
                    self.split_pos = rel.clamp(0.18, 0.6);
                    self.relayout = true;
                    self.dirty = true;
                    *handled = true;
                    return true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.divider_hover {
                    self.divider_drag = true;
                    *handled = true;
                    return true;
                }
                self.handle_click(state, handled, node_id, taffy, modifiers, path);
                return true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                // Right-click a contact row → per-contact context menu
                // (Edit / Export / Delete). `MouseInput` carries no position,
                // so we use the cursor cached on `CursorMoved`.
                self.handle_right_click(self.last_mouse, handled);
                return true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.divider_drag {
                    self.divider_drag = false;
                    *handled = true;
                    return true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(-y * 48.0),
                    MouseScrollDelta::PixelDelta(p) => -p.y,
                };
                if self.list_rect.contains(self.last_mouse) {
                    self.list_scroll += dy;
                } else if self.detail_rect.contains(self.last_mouse) {
                    let max = (self.detail_content_h - self.detail_rect.height() + 80.0).max(0.0);
                    self.detail_scroll = (self.detail_scroll + dy).clamp(0.0, max);
                }
                self.relayout = true;
                self.dirty = true;
                *handled = true;
                return true;
            }
            WindowEvent::KeyboardInput { event: ke, .. } => {
                self.handle_key(
                    ke,
                    state,
                    path,
                    handled,
                    node_id,
                    taffy,
                    modifiers,
                    scale_factor,
                    event,
                );
                return true;
            }
            _ => {}
        }
        false
    }
}

// =====================================================================
// Event handling helpers
// =====================================================================

impl ContactsApp {
    fn divider_hit(&self) -> Rect {
        let dx0 = self.list_rect.x1;
        Rect::new(
            dx0 - 3.0,
            self.detail_rect.y0,
            dx0 + DIVIDER_W + 3.0,
            self.detail_rect.y1,
        )
    }

    fn any_text_focused(&self) -> bool {
        self.search.is_focused() || self.form.as_ref().is_some_and(EditForm::any_focused)
    }

    #[allow(clippy::too_many_arguments)]
    fn forward_event_to_inputs(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let mut sink: Option<NodeId> = None;
        // Search box.
        self.search.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            &mut sink,
            node_id,
            taffy,
        );
        if let Some(form) = &mut self.form {
            for f in &mut form.inputs {
                let mut s2: Option<NodeId> = None;
                f.ti.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    &mut s2,
                    node_id,
                    taffy,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    /// Route a click to whichever affordance is under the cursor. Split into
    /// per-region helpers purely for length; the guard order (and thus which
    /// affordance wins when rects overlap) is unchanged from one flat
    /// function.
    fn handle_click(
        &mut self,
        state: &mut AutoCommit,
        handled: &mut bool,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
        modifiers: &winit::event::Modifiers,
        path: &ObjId,
    ) {
        let p = self.last_mouse;

        if self.handle_click_toolbar_and_header(p) {
            *handled = true;
            return;
        }

        // Search field click → focus it.
        if self.hit.search.contains(p) {
            self.set_only_focus_search();
        } else if self.list_body.contains(p) {
            // List row click. Routed through `request_select` so switching
            // away from an edit form with unsaved changes pops the guard
            // modal instead of silently discarding edits.
            if let Some((id, _)) = self.list_rows.iter().find(|(_, r)| r.contains(p)).copied() {
                self.request_select(id);
                *handled = true;
                return;
            }
        }

        if self.mode == Mode::View {
            if self.handle_click_view_mode(state, p) {
                *handled = true;
                return;
            }
            // Forward to search input for caret placement.
            self.forward_press_to_focused_input(state, path, node_id, taffy, modifiers);
            *handled = true;
            return;
        }

        // ----- Edit mode clicks -----
        if self.handle_click_edit_mode(state, p) {
            *handled = true;
            return;
        }

        // Otherwise: focus the form input under the cursor (caret placement).
        self.focus_input_at(p);
        self.forward_press_to_focused_input(state, path, node_id, taffy, modifiers);
        *handled = true;
    }

    /// The narrow-mode Back affordance, the toolbar buttons (Import/Export/
    /// New), and the list header chips (Sort/Group). Returns `true` if `p`
    /// landed on one of these.
    fn handle_click_toolbar_and_header(&mut self, p: Point) -> bool {
        // Narrow single-pane push: the Back affordance returns to the list.
        // Only hittable while the detail pane is the visible (pushed) slab.
        if self.narrow
            && self.push.pane() == Pane::Detail
            && self.hit.back.width() > 0.0
            && self.hit.back.contains(p)
        {
            self.push.show_list();
            self.dirty = true;
            self.relayout = true;
            return true;
        }

        // Toolbar.
        if self.hit.import.contains(p) {
            self.request_import();
            return true;
        }
        if self.hit.export.contains(p) {
            self.open_export_dialog();
            self.dirty = true;
            return true;
        }
        if self.hit.new.contains(p) {
            let mut c = Contact::new();
            c.phones.push(Labeled::new("mobile", ""));
            c.emails.push(Labeled::new("home", ""));
            self.begin_edit(c, true);
            return true;
        }

        // List header chips.
        if self.hit.sort.contains(p) {
            self.sort = match self.sort {
                SortKey::Name => SortKey::Org,
                SortKey::Org => SortKey::Name,
            };
            self.relayout = true;
            self.dirty = true;
            return true;
        }
        if self.hit.group.contains(p) {
            self.popup = Some(Popup::GroupFilter {
                anchor: self.hit.group,
            });
            self.dirty = true;
            return true;
        }

        false
    }

    /// View-mode clicks: the website link, a double-click into edit-on-field,
    /// and the favourite/edit/delete detail actions. Returns `true` if `p`
    /// was consumed by one of these (the caller still forwards an
    /// unconsumed click to the focused input for caret placement).
    fn handle_click_view_mode(&mut self, state: &mut AutoCommit, p: Point) -> bool {
        // A click on the website hyperlink opens it via the OS handler
        // (delegated to the ui `Link` widget — no hand-rolled open here).
        if self.selected.is_some()
            && self
                .website_link
                .as_ref()
                .is_some_and(|l| l.get_rect().contains(p))
        {
            if let Some(l) = self.website_link.as_mut() {
                l.activate();
            }
            self.dirty = true;
            return true;
        }
        // Double-click a detail field value → edit, focused on that field.
        if self.selected.is_some()
            && self.detail_rect.contains(p)
            && self.try_detail_double_click(p)
        {
            return true;
        }
        // Detail actions.
        if self.selected.is_some() {
            if self.hit.fav.contains(p) {
                if let Some(id) = self.selected {
                    let new_fav = !self.store.get(id).is_some_and(|c| c.favorite);
                    self.perform_action(
                        state,
                        &crate::actions::set_favorite_action(id.to_string(), new_fav),
                    );
                    if let Some(c) = self.store.get_mut(id) {
                        c.favorite = new_fav;
                    }
                    Self::persist();
                    self.dirty = true;
                }
                return true;
            }
            if self.hit.edit.contains(p) {
                if let Some(c) = self.selected.and_then(|id| self.store.get(id)).cloned() {
                    self.begin_edit(c, false);
                }
                return true;
            }
            if self.hit.delete.contains(p) {
                self.confirm_delete = self.selected;
                self.dirty = true;
                return true;
            }
        }
        false
    }

    /// Edit-mode clicks: Save/Cancel, the favourite toggle, the calendar
    /// button, the "+ Add …" buttons, and the type-chip / remove-button
    /// popups. Returns `true` if `p` was consumed (the caller still falls
    /// back to focusing the form input under the cursor otherwise).
    fn handle_click_edit_mode(&mut self, state: &mut AutoCommit, p: Point) -> bool {
        if self.hit.save.contains(p) {
            self.save_edit(state);
            return true;
        }
        if self.hit.cancel.contains(p) {
            self.cancel_edit();
            return true;
        }
        if self.hit.fav_toggle.contains(p) {
            if let Some(form) = &mut self.form {
                form.draft.favorite = !form.draft.favorite;
            }
            self.dirty = true;
            return true;
        }
        if self.hit.cal_btn.contains(p) {
            // Seed the calendar from whatever's currently typed in the birthday
            // field (parsed live), falling back to the draft, then today. A
            // year-less birthday opens on its month in the current year.
            let seed = self.form.as_ref().and_then(|f| {
                f.inputs
                    .iter()
                    .find(|i| i.slot == Slot::Birthday)
                    .and_then(|i| Birthday::parse(i.ti.value()))
                    .or(f.draft.birthday)
            });
            let (year, month) = if let Some(b) = seed {
                (b.year().unwrap_or_else(|| date::today().0), b.month())
            } else {
                let (y, m, _) = date::today();
                (y, m)
            };
            self.popup = Some(Popup::Calendar {
                year,
                month,
                anchor: self.hit.cal_btn,
            });
            self.dirty = true;
            return true;
        }
        if self.hit.add_phone.contains(p) {
            self.form_add(|d| d.phones.push(Labeled::new("mobile", "")));
            return true;
        }
        if self.hit.add_email.contains(p) {
            self.form_add(|d| d.emails.push(Labeled::new("home", "")));
            return true;
        }
        if self.hit.add_address.contains(p) {
            self.form_add(|d| {
                d.addresses.push(Address {
                    label: "home".into(),
                    ..Default::default()
                });
            });
            return true;
        }
        // Type chips.
        if let Some((target, _)) = self
            .hit
            .type_chips
            .iter()
            .find(|(_, r)| r.contains(p))
            .copied()
        {
            let anchor = self
                .hit
                .type_chips
                .iter()
                .find(|(_, r)| r.contains(p))
                .map(|(_, r)| *r)
                .unwrap();
            self.popup = Some(Popup::TypeMenu { target, anchor });
            self.dirty = true;
            return true;
        }
        // Remove buttons.
        if let Some((target, _)) = self
            .hit
            .remove_btns
            .iter()
            .find(|(_, r)| r.contains(p))
            .copied()
        {
            self.form_remove(target);
            return true;
        }
        false
    }

    fn set_only_focus_search(&mut self) {
        self.search.set_focus(true);
        if let Some(form) = &mut self.form {
            form.clear_focus();
        }
    }

    /// Handle a left-press in the detail pane that may be the *second* click
    /// of a double-click on a field value. Returns `true` when it resolved a
    /// double-click and entered edit mode focused on that field.
    fn try_detail_double_click(&mut self, p: Point) -> bool {
        const DOUBLE_CLICK_MS: u128 = 400;
        const DOUBLE_CLICK_RADIUS: f64 = 6.0;
        let now = Instant::now();
        let is_double = self.last_detail_click.is_some_and(|(t0, p0)| {
            now.duration_since(t0).as_millis() <= DOUBLE_CLICK_MS
                && p.distance(p0) <= DOUBLE_CLICK_RADIUS
        });
        // Record this click for the next press regardless.
        self.last_detail_click = Some((now, p));
        if !is_double {
            return false;
        }
        // Find the field value under the cursor.
        let slot = self
            .detail_field_hits
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(s, _)| *s);
        let Some(slot) = slot else { return false };
        let Some(c) = self.selected.and_then(|id| self.store.get(id)).cloned() else {
            return false;
        };
        self.begin_edit(c, false);
        self.focus_form_slot(slot);
        self.last_detail_click = None;
        true
    }

    /// Focus the form input mapped to `slot` (and clear focus elsewhere).
    /// Used after a double-click-to-edit jump so the caret lands in the
    /// field the user double-clicked.
    fn focus_form_slot(&mut self, slot: Slot) {
        self.search.set_focus(false);
        if let Some(form) = &mut self.form {
            form.clear_focus();
            if let Some(f) = form.inputs.iter_mut().find(|f| f.slot == slot) {
                f.ti.set_focus(true);
            }
        }
        self.dirty = true;
    }

    fn focus_input_at(&mut self, p: Point) {
        self.search.set_focus(self.hit.search.contains(p));
        if let Some(form) = &mut self.form {
            for f in &mut form.inputs {
                f.ti.set_focus(f.ti.get_rect().contains(p));
            }
        }
    }

    /// Move edit-form focus to the next (`delta = 1`) or previous
    /// (`delta = -1`) field in the form's natural order, wrapping at the ends.
    /// With no field currently focused, focuses the first (forward) or last
    /// (backward). Places the caret at the end of the newly-focused field so
    /// the user can keep typing. Returns whether a field was focused.
    fn focus_adjacent_field(&mut self, delta: isize) -> bool {
        // Tabbing into the form takes focus away from the list search box.
        self.search.set_focus(false);
        let Some(form) = &mut self.form else {
            return false;
        };
        let n = form.inputs.len();
        if n == 0 {
            return false;
        }
        let cur = form.inputs.iter().position(|f| f.ti.is_focused());
        let next = match cur {
            Some(i) => (i as isize + delta).rem_euclid(n as isize) as usize,
            None if delta >= 0 => 0,
            None => n - 1,
        };
        for (i, f) in form.inputs.iter_mut().enumerate() {
            let focus = i == next;
            f.ti.set_focus(focus);
            if focus {
                // Caret at the end of the field content (idiomatic tab-in).
                let end = f.ti.value().len();
                f.ti.set_selection(end, end);
            }
        }
        self.scroll_focused_field_into_view();
        self.dirty = true;
        self.relayout = true;
        true
    }

    /// Focus the field nearest the currently-focused one in `dir`, using the
    /// laid-out input rectangles (2D directional navigation, the same spatial
    /// model the OS uses between top-level widgets). Arrow keys drive this so
    /// the form is navigable without Tab. With no field focused, enters at the
    /// first (Down/Right) or last (Up/Left) field. Caret lands at the end of
    /// the newly-focused field. Returns whether focus moved onto a field.
    fn focus_field_directional(&mut self, dir: FieldDir) -> bool {
        self.search.set_focus(false);
        let Some(form) = &mut self.form else {
            return false;
        };
        // Only laid-out inputs (non-zero width) are navigable targets.
        let rects: Vec<(usize, Rect)> = form
            .inputs
            .iter()
            .enumerate()
            .filter(|(_, f)| f.ti.get_rect().width() > 0.0)
            .map(|(i, f)| (i, f.ti.get_rect()))
            .collect();
        if rects.is_empty() {
            return false;
        }
        let cur = form.inputs.iter().position(|f| f.ti.is_focused());
        let target = match cur {
            Some(ci) => best_field_in_direction(form.inputs[ci].ti.get_rect(), dir, &rects, ci),
            None => match dir {
                FieldDir::Down | FieldDir::Right => Some(rects.first().unwrap().0),
                FieldDir::Up | FieldDir::Left => Some(rects.last().unwrap().0),
            },
        };
        let Some(ti_idx) = target else {
            return false;
        };
        for (i, f) in form.inputs.iter_mut().enumerate() {
            let focus = i == ti_idx;
            f.ti.set_focus(focus);
            if focus {
                let end = f.ti.value().len();
                f.ti.set_selection(end, end);
            }
        }
        self.scroll_focused_field_into_view();
        self.dirty = true;
        self.relayout = true;
        true
    }

    /// Scroll the edit-form body so the just-focused field (and its label) is
    /// fully visible. Keyboard navigation (Tab / Shift+Tab / the arrow keys)
    /// can land on a field below or above the fold on a long form; without this
    /// the focus ring would sit on a field the user can't see. Uses the field's
    /// last-laid-out rect and the same body geometry `draw_form` clips to, and
    /// nudges `detail_scroll` by exactly the overflow so the next layout brings
    /// the field into view.
    fn scroll_focused_field_into_view(&mut self) {
        if self.mode != Mode::Edit {
            return;
        }
        let off = self.detail_top_offset();
        let body_top =
            self.detail_rect.y0 + FORM_TITLE_TOP + FORM_TITLE_H + FORM_ROW_GAP + 32.0 + off;
        let body_bot = self.detail_rect.y1;
        // The focused field's last-laid-out rect (reflecting the current
        // scroll). `Rect` is `Copy`, so the borrow of `self.form` ends here.
        let r = match self
            .form
            .as_ref()
            .and_then(|f| f.inputs.iter().find(|i| i.ti.is_focused()))
        {
            Some(fi) => fi.ti.get_rect(),
            None => return,
        };
        if r.width() <= 0.0 {
            return; // not laid out yet (e.g. focus set before the first frame)
        }
        // The section label is drawn ~18px above the field; keep it visible too.
        let top_margin = 22.0;
        let bottom_margin = 8.0;
        let delta = if r.y0 - top_margin < body_top {
            r.y0 - top_margin - body_top // negative: reveal upward
        } else if r.y1 + bottom_margin > body_bot {
            r.y1 + bottom_margin - body_bot // positive: reveal downward
        } else {
            0.0
        };
        if delta != 0.0 {
            self.detail_scroll = (self.detail_scroll + delta).max(0.0);
            self.relayout = true;
            self.dirty = true;
        }
    }

    /// Whether the focused edit-form field has its caret at the very start with
    /// nothing selected — the point at which a left-arrow should hop to the
    /// previous field instead of nudging the caret.
    fn form_caret_at_start(&self) -> bool {
        self.form
            .as_ref()
            .and_then(|f| f.inputs.iter().find(|i| i.ti.is_focused()))
            .is_some_and(|i| i.ti.cursor_pos() == 0 && i.ti.selection_range_public().is_none())
    }

    /// Whether the focused edit-form field has its caret at the very end with
    /// nothing selected — the point at which a right-arrow should hop to the
    /// next field.
    fn form_caret_at_end(&self) -> bool {
        self.form
            .as_ref()
            .and_then(|f| f.inputs.iter().find(|i| i.ti.is_focused()))
            .is_some_and(|i| {
                i.ti.cursor_pos() == i.ti.value().len() && i.ti.selection_range_public().is_none()
            })
    }

    /// Idiomatic Enter inside the edit form: advance to the next field, or —
    /// when the last field is already focused — submit the form (save). With
    /// no field focused, focuses the first. Returns whether it was handled.
    fn form_enter(&mut self, doc: &mut AutoCommit) -> bool {
        let Some(form) = &self.form else {
            return false;
        };
        let n = form.inputs.len();
        if n == 0 {
            return false;
        }
        match form.inputs.iter().position(|f| f.ti.is_focused()) {
            // On the last field → submit, matching the Save button.
            Some(i) if i + 1 == n => self.save_edit(doc),
            // Anywhere else (or nothing focused) → advance to the next field.
            _ => {
                self.focus_adjacent_field(1);
            }
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn forward_press_to_focused_input(
        &mut self,
        state: &mut AutoCommit,
        path: &ObjId,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
        modifiers: &winit::event::Modifiers,
    ) {
        // Reconstruct a press event at the current cursor and route it so
        // the focused input places its caret precisely.
        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        let mut h = false;
        let mut sink = None;
        if self.search.is_focused() {
            self.search.handle_event(
                &press, 1.0, modifiers, state, path, &mut h, &mut sink, node_id, taffy,
            );
        }
        if let Some(form) = &mut self.form {
            for f in &mut form.inputs {
                if f.ti.is_focused() {
                    let mut s2 = None;
                    f.ti.handle_event(
                        &press, 1.0, modifiers, state, path, &mut h, &mut s2, node_id, taffy,
                    );
                }
            }
        }
        // Note: cursor placement used logical coords (scale already applied
        // when we recorded last_mouse), so pass scale_factor = 1.0 above.
        self.dirty = true;
    }

    fn form_add(&mut self, f: impl Fn(&mut Contact)) {
        if let Some(form) = self.form.take() {
            let mut draft = form.draft;
            // First fold current input values back in.
            let mut tmp = EditForm {
                draft: draft.clone(),
                is_new: form.is_new,
                inputs: form.inputs,
            };
            tmp.collect();
            draft = tmp.draft;
            f(&mut draft);
            self.form = Some(EditForm::new(draft, form.is_new));
            self.relayout = true;
            self.dirty = true;
        }
    }

    fn form_remove(&mut self, target: TypeTarget) {
        if let Some(form) = self.form.take() {
            let mut tmp = EditForm {
                draft: form.draft.clone(),
                is_new: form.is_new,
                inputs: form.inputs,
            };
            tmp.collect();
            let mut draft = tmp.draft;
            match target {
                TypeTarget::Phone(i) if i < draft.phones.len() => {
                    draft.phones.remove(i);
                }
                TypeTarget::Email(i) if i < draft.emails.len() => {
                    draft.emails.remove(i);
                }
                TypeTarget::Addr(i) if i < draft.addresses.len() => {
                    draft.addresses.remove(i);
                }
                _ => {}
            }
            self.form = Some(EditForm::new(draft, form.is_new));
            self.relayout = true;
            self.dirty = true;
        }
    }

    /// Route a click inside the currently-open popup. Split per popup
    /// variant purely for length; the per-variant behaviour is unchanged
    /// from one flat `match`.
    fn handle_popup(&mut self, event: &WindowEvent, state: &mut AutoCommit, handled: &mut bool) {
        let press = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        );
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            self.dirty = true; // hover highlight
        }
        if !press {
            return;
        }
        let p = self.last_mouse;
        let popup = self.popup.take().unwrap();
        *handled = true;
        self.dirty = true;
        match popup {
            Popup::TypeMenu { target, anchor } => self.handle_popup_type_menu(target, anchor, p),
            Popup::GroupFilter { anchor } => self.handle_popup_group_filter(anchor, p),
            Popup::ContextMenu { target, anchor } => {
                self.handle_popup_context_menu(state, target, anchor, p);
            }
            Popup::Calendar {
                year,
                month,
                anchor,
            } => self.handle_popup_calendar(year, month, anchor, p),
        }
    }

    /// Type-chip menu selection: sets the picked label on the target phone/
    /// email/address row.
    fn handle_popup_type_menu(&mut self, target: TypeTarget, anchor: Rect, p: Point) {
        let (_panel, rows) = self.popup_menu_geometry(anchor, TYPE_OPTIONS.len(), 140.0);
        if let Some(i) = rows.iter().position(|r| r.contains(p)) {
            let label = TYPE_OPTIONS[i].to_string();
            if let Some(form) = &mut self.form {
                match target {
                    TypeTarget::Phone(idx) => {
                        if let Some(x) = form.draft.phones.get_mut(idx) {
                            x.label = label;
                        }
                    }
                    TypeTarget::Email(idx) => {
                        if let Some(x) = form.draft.emails.get_mut(idx) {
                            x.label = label;
                        }
                    }
                    TypeTarget::Addr(idx) => {
                        if let Some(x) = form.draft.addresses.get_mut(idx) {
                            x.label = label;
                        }
                    }
                }
            }
        }
    }

    /// Group-filter selection: narrows the list to the picked group (or
    /// clears it for "All"), and keeps the selection valid.
    fn handle_popup_group_filter(&mut self, anchor: Rect, p: Point) {
        let mut opts = vec!["All".to_string()];
        opts.extend(self.store.all_groups());
        let (_panel, rows) = self.popup_menu_geometry(anchor, opts.len(), 180.0);
        if let Some(i) = rows.iter().position(|r| r.contains(p)) {
            self.group_filter = if i == 0 { None } else { Some(opts[i].clone()) };
            self.list_scroll = 0.0;
            // Keep selection valid.
            if let Some(sel) = self.selected
                && !self.visible_ids().contains(&sel)
            {
                self.selected = self.visible_ids().first().copied();
            }
        }
        self.relayout = true;
    }

    /// Per-contact context-menu selection: Edit / Favorite / Export /
    /// Delete.
    fn handle_popup_context_menu(
        &mut self,
        state: &mut AutoCommit,
        target: ContactId,
        anchor: Rect,
        p: Point,
    ) {
        let (_panel, rows) = self.popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
        let Some(i) = rows.iter().position(|r| r.contains(p)) else {
            return;
        };
        match CONTEXT_ACTIONS[i] {
            ContextAction::Edit => {
                if let Some(c) = self.store.get(target).cloned() {
                    self.selected = Some(target);
                    self.begin_edit(c, false);
                }
            }
            ContextAction::Favorite => {
                // Toggle the favorite flag through the shared action
                // path (same action the view-mode star and Ctrl+K
                // dispatch), so the list star and the menu label flip
                // immediately and the change survives a reload.
                let new_fav = !self.store.get(target).is_some_and(|c| c.favorite);
                self.perform_action(
                    state,
                    &crate::actions::set_favorite_action(target.to_string(), new_fav),
                );
                if let Some(c) = self.store.get_mut(target) {
                    c.favorite = new_fav;
                }
                Self::persist();
                self.relayout = true;
            }
            ContextAction::Export => {
                // Open the export dialog pre-ticked with just this
                // contact.
                let mut set = std::collections::HashSet::new();
                set.insert(target);
                self.export_sel = Some(set);
                self.export_scroll = 0.0;
            }
            ContextAction::Delete => {
                self.selected = Some(target);
                self.confirm_delete = Some(target);
            }
        }
    }

    /// Calendar popup interaction: prev/next month, pick a day into the
    /// birthday field, or keep the popup open on an in-panel non-day click.
    fn handle_popup_calendar(&mut self, year: i32, month: u32, anchor: Rect, p: Point) {
        let (panel, prev, next, cells) = self.calendar_geometry(anchor, year, month);
        if prev.contains(p) {
            let (y, m) = date::prev_month(year, month);
            self.popup = Some(Popup::Calendar {
                year: y,
                month: m,
                anchor,
            });
        } else if next.contains(p) {
            let (y, m) = date::next_month(year, month);
            self.popup = Some(Popup::Calendar {
                year: y,
                month: m,
                anchor,
            });
        } else if panel.contains(p) {
            if let Some((d, _)) = cells.iter().find(|(_, r)| r.contains(p)) {
                let iso = format!("{year:04}-{month:02}-{d:02}");
                if let Some(form) = &mut self.form {
                    form.draft.birthday = Birthday::parse(&iso);
                    if let Some(fi) = form.inputs.iter_mut().find(|f| f.slot == Slot::Birthday) {
                        fi.ti.set_text(&iso);
                    }
                }
            } else {
                // Click inside panel but not a day: keep open.
                self.popup = Some(Popup::Calendar {
                    year,
                    month,
                    anchor,
                });
            }
        }
        // else: click outside dismisses (popup already taken).
    }

    fn handle_confirm(&mut self, event: &WindowEvent, state: &mut AutoCommit, handled: &mut bool) {
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            self.dirty = true;
        }
        let press = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        );
        if !press {
            return;
        }
        *handled = true;
        self.dirty = true;
        let (_panel, del, cancel) = self.confirm_geometry();
        let p = self.last_mouse;
        if del.contains(p)
            && let Some(id) = self.confirm_delete
        {
            self.delete_selected(state, id);
        }
        // Any press inside the modal (delete already handled above, cancel
        // or scrim) dismisses it.
        self.confirm_delete = None;
        let _ = cancel;
    }

    /// Open the per-contact right-click context menu if `p` lands on a row.
    fn handle_right_click(&mut self, p: Point, handled: &mut bool) {
        // Only the list rows carry a context menu.
        if let Some((id, _)) = self.list_rows.iter().find(|(_, r)| r.contains(p)).copied() {
            self.selected = Some(id);
            let anchor = Rect::new(p.x, p.y, p.x, p.y);
            self.popup = Some(Popup::ContextMenu { target: id, anchor });
            self.relayout = true;
            self.dirty = true;
            *handled = true;
        }
    }

    /// Drive the export-selection dialog: checkbox toggles, Select all/none,
    /// scroll, the two export actions, and dismissal.
    fn handle_export(&mut self, event: &WindowEvent, handled: &mut bool) {
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            self.dirty = true; // hover highlight
        }
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => f64::from(-y * 48.0),
                MouseScrollDelta::PixelDelta(p) => -p.y,
            };
            let g = self.export_geometry();
            let max = (g.content_h - g.list_body.height()).max(0.0);
            self.export_scroll = (self.export_scroll + dy).clamp(0.0, max);
            self.dirty = true;
            *handled = true;
            return;
        }
        let press = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        );
        if !press {
            return;
        }
        *handled = true;
        self.dirty = true;
        let p = self.last_mouse;
        let g = self.export_geometry();

        if g.select_all.contains(p) {
            let all: std::collections::HashSet<ContactId> =
                self.visible_export_ids().into_iter().collect();
            self.export_sel = Some(all);
            return;
        }
        if g.select_none.contains(p) {
            self.export_sel = Some(std::collections::HashSet::new());
            return;
        }
        // Row checkbox toggle (whole row is clickable).
        if g.list_body.contains(p) {
            if let Some((id, _, _)) = g.rows.iter().find(|(_, r, _)| r.contains(p))
                && let Some(set) = &mut self.export_sel
            {
                if set.contains(id) {
                    set.remove(id);
                } else {
                    set.insert(*id);
                }
            }
            return;
        }
        if g.cancel.contains(p) {
            self.export_sel = None;
            return;
        }
        if g.export_v3.contains(p) || g.export_v4.contains(p) {
            let version = if g.export_v4.contains(p) {
                VCardVersion::V4
            } else {
                VCardVersion::V3
            };
            let ids: Vec<ContactId> = self
                .visible_export_ids()
                .into_iter()
                .filter(|id| self.export_sel.as_ref().is_some_and(|s| s.contains(id)))
                .collect();
            // Close the selection dialog and hand off to the host Save dialog;
            // the file is written when the portal returns a destination.
            self.export_sel = None;
            self.request_export(ids, version);
            return;
        }
        // Click on the scrim (outside the panel) dismisses.
        if !g.panel.contains(p) {
            self.export_sel = None;
        }
    }

    /// Drive the "Save changes?" unsaved-changes guard modal: Save commits and
    /// switches, Discard drops edits and switches, Cancel keeps editing.
    fn handle_unsaved(&mut self, event: &WindowEvent, state: &mut AutoCommit, handled: &mut bool) {
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            self.dirty = true;
        }
        let press = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        );
        if !press {
            return;
        }
        *handled = true;
        self.dirty = true;
        let p = self.last_mouse;
        let g = self.unsaved_geometry();
        let Some(target) = self.pending_switch else {
            return;
        };
        if g.save.contains(p) {
            self.save_edit(state);
            self.pending_switch = None;
            self.commit_select(target);
        } else if g.discard.contains(p) {
            self.pending_switch = None;
            self.commit_select(target);
        } else if g.cancel.contains(p) || !g.panel.contains(p) {
            // Cancel / scrim: stay in the edit form, drop the pending switch.
            self.pending_switch = None;
        }
    }

    /// Route a keypress. Split into per-concern helpers purely for length;
    /// the guard order (which chord wins) is unchanged from one flat
    /// function.
    #[allow(clippy::too_many_arguments)]
    fn handle_key(
        &mut self,
        ke: &KeyEvent,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
        modifiers: &winit::event::Modifiers,
        scale_factor: f64,
        full_event: &WindowEvent,
    ) {
        if ke.state != ElementState::Pressed {
            // Route key-release to inputs too (harmless) then return.
            self.forward_event_to_inputs(
                full_event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                node_id,
                taffy,
            );
            return;
        }
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();

        // Ctrl/Cmd+K and Ctrl/Cmd+/ focus the list's search field — the
        // familiar "jump to search" chord, wired to this app's real search box
        // (bare `/` does the same in the no-field-focused branch below).
        if is_focus_search_chord(ctrl, &ke.logical_key) {
            self.set_only_focus_search();
            *handled = true;
            self.dirty = true;
            return;
        }

        // Escape closes popups / dialogs / cancels edit / returns from a
        // narrow pushed detail to the list / clears search focus.
        if matches!(ke.logical_key, Key::Named(NamedKey::Escape)) {
            self.handle_key_escape();
            *handled = true;
            self.dirty = true;
            return;
        }

        // Ctrl+S saves in edit mode.
        if ctrl
            && let Key::Character(c) = &ke.logical_key
            && c.eq_ignore_ascii_case("s")
            && self.mode == Mode::Edit
        {
            self.save_edit(state);
            *handled = true;
            return;
        }

        // Edit-form keyboard navigation. Tab / Shift+Tab move across the form
        // fields (wrapping); the arrow keys move directionally between fields
        // (Up/Down between rows; Left/Right hop to the neighbouring field only
        // when the caret is already at the field edge, so mid-field arrows
        // still nudge the caret); Enter advances and submits on the last.
        // Handled *before* the "route typing to the focused input" branch below
        // so the embedded TextInput never swallows Tab / Enter / the boundary
        // arrows (which it otherwise drops as control characters or caret
        // no-ops). Left/Right that are NOT at an edge fall through untouched so
        // the focused input moves its caret as usual.
        if self.mode == Mode::Edit
            && self.form.is_some()
            && self.handle_key_edit_form_nav(ke, state, modifiers)
        {
            *handled = true;
            self.dirty = true;
            return;
        }

        // If a text field is focused, route typing to it. In edit mode the
        // keystroke lands in the field's *local* (non-CRDT) buffer only — the
        // draft is folded into the store + CRDT solely by Save, so Cancel can
        // genuinely discard and the unsaved-changes guard can see real dirt.
        if self.any_text_focused() {
            self.forward_event_to_inputs(
                full_event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                node_id,
                taffy,
            );
            self.dirty = true;
            return;
        }

        // Global list shortcuts. These fire only in view mode with no popup
        // open, so a stray bare key ('n', 'e', '/', Delete, an arrow) can never
        // silently discard an open edit form or fire behind a transient popup.
        // In edit mode the form's own keyboard handling above owns navigation;
        // a printable key reaches a field only once that field is focused
        // (Tab / click), matching how a form should behave.
        if self.mode != Mode::View || self.popup.is_some() {
            return;
        }
        self.handle_key_list_shortcuts(ke, handled);
    }

    /// The Escape-key state transition: closes whichever dialog/popup/edit is
    /// topmost, in priority order, or (with nothing open) clears search/form
    /// focus. The caller sets `handled`/`dirty` uniformly afterward.
    fn handle_key_escape(&mut self) {
        if self.export_sel.is_some() {
            self.export_sel = None;
        } else if self.pending_switch.is_some() {
            self.pending_switch = None;
        } else if self.confirm_delete.is_some() {
            self.confirm_delete = None;
        } else if self.popup.is_some() {
            self.popup = None;
        } else if self.mode == Mode::Edit {
            self.cancel_edit();
            // On a narrow window, leaving the edit form returns to the
            // list (the form lived in the pushed detail pane).
            if self.narrow {
                self.push.show_list();
                self.relayout = true;
            }
        } else if self.narrow && self.push.pane() == Pane::Detail {
            // Back out of the pushed detail pane to the list.
            self.push.show_list();
            self.relayout = true;
        } else {
            self.search.set_focus(false);
            if let Some(f) = &mut self.form {
                f.clear_focus();
            }
        }
    }

    /// Edit-form keyboard navigation chords (see the call site's comment).
    /// Returns `true` if `ke` was one of them (the caller still routes an
    /// unconsumed Left/Right to the focused input's own caret movement).
    fn handle_key_edit_form_nav(
        &mut self,
        ke: &KeyEvent,
        state: &mut AutoCommit,
        modifiers: &winit::event::Modifiers,
    ) -> bool {
        match &ke.logical_key {
            Key::Named(NamedKey::Tab) => {
                let backward = modifiers.state().shift_key();
                self.focus_adjacent_field(if backward { -1 } else { 1 });
                true
            }
            Key::Named(NamedKey::Enter) => {
                self.form_enter(state);
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.focus_field_directional(FieldDir::Up);
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.focus_field_directional(FieldDir::Down);
                true
            }
            Key::Named(NamedKey::ArrowLeft) if self.form_caret_at_start() => {
                self.focus_field_directional(FieldDir::Left);
                true
            }
            Key::Named(NamedKey::ArrowRight) if self.form_caret_at_end() => {
                self.focus_field_directional(FieldDir::Right);
                true
            }
            _ => false,
        }
    }

    /// Global list shortcuts: arrow-key selection, Delete/Backspace opens the
    /// delete-confirm dialog, `n` starts a new contact, `e` edits the
    /// selection, `/` focuses search. Only reached in view mode with no
    /// popup open (see the call site's guard).
    fn handle_key_list_shortcuts(&mut self, ke: &KeyEvent, handled: &mut bool) {
        match &ke.logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                self.move_selection(1);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_selection(-1);
                *handled = true;
            }
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                if let Some(id) = self.selected {
                    self.confirm_delete = Some(id);
                    *handled = true;
                    self.dirty = true;
                }
            }
            Key::Character(c) => {
                let lc = c.to_lowercase();
                match lc.as_str() {
                    "n" => {
                        let mut nc = Contact::new();
                        nc.phones.push(Labeled::new("mobile", ""));
                        nc.emails.push(Labeled::new("home", ""));
                        self.begin_edit(nc, true);
                        *handled = true;
                    }
                    "e" => {
                        if let Some(c) = self.selected.and_then(|id| self.store.get(id)).cloned() {
                            self.begin_edit(c, false);
                            *handled = true;
                        }
                    }
                    "/" => {
                        self.set_only_focus_search();
                        *handled = true;
                        self.dirty = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;
    use automerge::ReadDoc;
    use taffy::{AvailableSpace, Dimension, Size, Style};
    use vello::kurbo::Point;

    #[test]
    fn shared_chip_style_keeps_selected_chips_legible_in_every_app_theme() {
        let quiet = contact_chip_style(&Theme::dark(), false);
        assert!(
            quiet.tint.is_none(),
            "inactive chips use the shared quiet state"
        );
        assert!(
            quiet.resolved.is_none(),
            "inactive chips do not accidentally inherit the selected tint"
        );

        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = theme.tokens.colors.surfaces.base;
            let selected = contact_chip_style(&theme, true)
                .resolved
                .expect("selected contacts chips resolve their shared accent treatment");
            assert!(
                ui::a11y::contrast_ratio(selected.ink, selected.fill) >= ON_FILL_MIN_CONTRAST,
                "selected chip label must remain readable on its resolved fill"
            );
            assert!(
                ui::a11y::contrast_ratio(selected.border, ground)
                    >= ui::tokens::colors::NON_TEXT_MIN_CONTRAST,
                "selected chip edge must remain distinguishable from its pane"
            );
        }
    }

    /// Contacts folds its address book into the unified Ctrl+K palette: the
    /// provider surfaces a real contact, and activating that row navigates the
    /// app to it (selection). Proves both `Widget` hooks on the real path.
    #[test]
    fn search_provider_row_navigates_to_the_contact() {
        let mut app = ContactsApp::new();
        for c in sample_contacts() {
            app.store.upsert(c);
        }
        // The unified palette collects this provider each time it opens.
        let providers = Widget::search_providers(&app);
        assert_eq!(providers.len(), 1, "contacts exposes one searchable source");
        let hits = providers[0].search("grace");
        let grace = hits
            .iter()
            .find(|r| r.title == "Grace Hopper")
            .expect("the provider surfaces the real contact");
        let action_id = grace.action_id.clone();

        // Activating the row navigates the app to that contact.
        assert!(app.selected.is_none(), "nothing selected yet");
        let mut doc = AutoCommit::new();
        Widget::activate_search_result(&mut app, &action_id, &mut doc, &ROOT);
        let id = crate::search_provider::ContactSearchProvider::parse_action_id(&action_id)
            .expect("the id parses back to a contact");
        assert_eq!(
            app.selected,
            Some(id),
            "the palette row selected the contact"
        );
    }

    /// Opening a contact from the global palette must *reveal* it: if the list
    /// is filtered to a group the target is not in, activating the search row
    /// drops that filter so the contact is actually visible, not just selected
    /// behind a filter. Drives the same two `Widget` hooks the shell palette
    /// calls — `search_providers()` then `activate_search_result(id, doc, root)`.
    #[test]
    fn search_activation_reveals_a_contact_hidden_by_the_group_filter() {
        let mut app = ContactsApp::new();
        for c in sample_contacts() {
            app.store.upsert(c);
        }
        // Grace Hopper is in "Work" only; narrowing the list to "Friends"
        // (Alan Turing's group) filters her out of the visible set.
        app.group_filter = Some("Friends".to_string());

        let providers = Widget::search_providers(&app);
        let hits = providers[0].search("grace");
        let grace = hits
            .iter()
            .find(|r| r.title == "Grace Hopper")
            .expect("global search finds Grace regardless of the in-app filter");
        let id = crate::search_provider::ContactSearchProvider::parse_action_id(&grace.action_id)
            .expect("the action id parses back to a contact");

        // Precondition: under the Friends filter, Grace is not in the list.
        assert!(
            !app.visible_ids().contains(&id),
            "the Friends group filter hides Grace before activation"
        );

        let mut doc = AutoCommit::new();
        Widget::activate_search_result(&mut app, &grace.action_id, &mut doc, &ROOT);

        assert_eq!(app.selected, Some(id), "activation selected Grace");
        assert!(
            app.group_filter.is_none(),
            "opening from search dropped the hiding filter"
        );
        assert!(
            app.visible_ids().contains(&id),
            "Grace is now visible in the list — opened, not just selected behind a filter"
        );
    }

    /// Drive one full five-pass frame against `app` at the given size,
    /// encoding the scene on the CPU so any panic in sync/layout/draw
    /// surfaces. (GPU rasterization is verified separately by `--shots`.)
    fn drive_frame(app: &mut ContactsApp, w: u32, h: u32) {
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let permission = PermissionContext::permissive();
        let mut tr = TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut doc = AutoCommit::new();

        app.sync(&mut doc, &ROOT);
        let child = app.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(w as f32),
                        height: Dimension::length(h as f32),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        {
            let measure_map = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    Size {
                        width: AvailableSpace::Definite(w as f32),
                        height: AvailableSpace::Definite(h as f32),
                    },
                    |known, avail, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, avail, trr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        app.apply_layout(&taffy, child, Point::ORIGIN);

        let mut scene = Scene::new();
        app.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
            &permission,
        );
        // Encoding the scene is enough to exercise every draw path; the
        // GPU blit is covered by the `--shots` harness.
        let _ = &scene;
    }

    #[test]
    fn seeds_sample_contacts_and_selects_one() {
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.store.len(), 7);
        assert!(app.selected.is_some(), "a contact must be auto-selected");
        // The seeded selection points at a real, richly-populated record.
        let sel = app.store.get(app.selected.unwrap()).unwrap();
        assert!(!sel.phones.is_empty());
    }

    #[test]
    fn view_mode_frame_does_not_panic() {
        let mut app = ContactsApp::sample();
        drive_frame(&mut app, 1280, 800);
    }

    #[test]
    fn narrow_frame_resolves_single_pane_push() {
        let mut app = ContactsApp::sample();
        // A narrow viewport drives the single-pane master-detail push.
        drive_frame(&mut app, 480, 860);
        assert!(app.narrow, "a narrow viewport resolves to single-pane push");
        assert_eq!(
            app.push.pane(),
            Pane::List,
            "a fresh narrow frame opens to the list"
        );
    }

    #[test]
    fn narrow_selection_pushes_detail_then_back_returns_to_list() {
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        drive_frame(&mut app, 480, 860);
        assert!(app.narrow);
        // Snap the push to the detail pane (as a row selection would) and
        // confirm the pushed-detail frame renders without panicking.
        app.shots_show_detail();
        assert_eq!(app.push.pane(), Pane::Detail);
        drive_frame(&mut app, 480, 860);
        // The Back affordance must be laid out (hittable) in the pushed pane.
        assert!(
            app.hit.back.width() > 0.0,
            "pushed detail shows a Back affordance"
        );
    }

    #[test]
    fn wide_frame_stays_docked_side_by_side() {
        let mut app = ContactsApp::sample();
        drive_frame(&mut app, 1280, 800);
        assert!(
            !app.narrow,
            "a desktop-width viewport keeps the docked split"
        );
        // The list pane sits left of the detail pane (classic split).
        assert!(app.list_rect.x1 <= app.detail_rect.x0 + 0.001);
    }

    #[test]
    fn edit_mode_with_repeated_fields_frame_does_not_panic() {
        let mut app = ContactsApp::sample();
        // Seed first.
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        // Enter edit on the selected contact and add repeated fields.
        let c = app
            .selected
            .and_then(|id| app.store.get(id))
            .cloned()
            .unwrap();
        app.begin_edit(c, false);
        app.form_add(|d| d.phones.push(Labeled::new("work", "")));
        app.form_add(|d| {
            d.addresses.push(Address {
                label: "work".into(),
                ..Default::default()
            });
        });
        drive_frame(&mut app, 1280, 800);
        assert_eq!(app.mode, Mode::Edit);
    }

    #[test]
    fn calendar_and_type_popups_frame_does_not_panic() {
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let c = app
            .selected
            .and_then(|id| app.store.get(id))
            .cloned()
            .unwrap();
        app.begin_edit(c, false);
        // Calendar popup.
        app.popup = Some(Popup::Calendar {
            year: 1815,
            month: 12,
            anchor: Rect::new(600.0, 400.0, 700.0, 432.0),
        });
        drive_frame(&mut app, 1280, 800);
        // Type menu popup.
        app.popup = Some(Popup::TypeMenu {
            target: TypeTarget::Phone(0),
            anchor: Rect::new(600.0, 200.0, 678.0, 238.0),
        });
        drive_frame(&mut app, 1280, 800);
    }

    #[test]
    fn save_round_trips_through_doc() {
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let mut c = app
            .selected
            .and_then(|id| app.store.get(id))
            .cloned()
            .unwrap();
        let id = c.id;
        c.title = "Edited Title".into();
        app.begin_edit(c, false);
        app.save_edit(&mut doc);
        assert_eq!(app.mode, Mode::View);
        let stored = ContactsDoc::list_contacts(&doc);
        let r = stored.iter().find(|x| x.id == id).unwrap();
        assert_eq!(r.title, "Edited Title");
    }

    // =================================================================
    // Interaction harness — drives real winit events through the
    // production `handle_event` path (not test-only shortcuts), so the
    // assertions prove the shipped behaviour.
    // =================================================================

    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
    use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr};

    /// Build a `KeyboardInput` winit event. winit 0.30's `KeyEvent` carries a
    /// `pub(crate)` `platform_specific` field, so it can't be struct-literalled
    /// — the whole struct is zero-initialised (the same trick `shell`'s and
    /// `terminal`'s test harnesses use) then the public fields we care about are
    /// overwritten. `text` is what a focused `TextInput` actually inserts.
    fn key_input_event(logical_key: Key, text: Option<&str>) -> WindowEvent {
        // SAFETY: every public `KeyEvent` field tolerates the all-zero bit
        // pattern (`Option` → `None`; `Key`/`PhysicalKey`/`KeyLocation`/
        // `ElementState` first variant; `bool` → false) and we overwrite the
        // ones we read below; the private field is inert for our widgets.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = text.map(SmolStr::new);
        WindowEvent::KeyboardInput {
            device_id: device_id(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn device_id() -> DeviceId {
        // winit gives no public DeviceId constructor; the production code
        // does exactly this for synthesised presses (see
        // `forward_press_to_focused_input`).
        unsafe { std::mem::zeroed() }
    }

    /// Owns a laid-out app + its taffy tree so tests can pump events through
    /// the production `Widget::handle_event`.
    struct Harness {
        app: ContactsApp,
        doc: AutoCommit,
        taffy: TaffyTree<()>,
        node: NodeId,
        w: u32,
        h: u32,
    }

    impl Harness {
        fn new(w: u32, h: u32) -> Self {
            let mut app = ContactsApp::sample();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &ROOT);
            let mut h = Harness {
                app,
                doc,
                taffy: TaffyTree::new(),
                node: NodeId::new(0),
                w,
                h,
            };
            h.relayout();
            h
        }

        /// Re-run build/compute/apply layout (after the model changed).
        fn relayout(&mut self) {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            self.taffy = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let child = self
                .app
                .build_layout(&mut self.taffy, &theme, &mut node_map);
            self.node = child;
            let root = self
                .taffy
                .new_with_children(
                    Style {
                        size: Size {
                            width: Dimension::length(self.w as f32),
                            height: Dimension::length(self.h as f32),
                        },
                        ..Default::default()
                    },
                    &[child],
                )
                .unwrap();
            {
                let measure_map = node_map.clone();
                let trr = &mut tr;
                self.taffy
                    .compute_layout_with_measure(
                        root,
                        Size {
                            width: AvailableSpace::Definite(self.w as f32),
                            height: AvailableSpace::Definite(self.h as f32),
                        },
                        |known, avail, node_id, _, _| {
                            if let Some(&ptr) = measure_map.get(&node_id) {
                                unsafe { (*ptr).measure(known, avail, trr) }
                            } else {
                                taffy::Size::ZERO
                            }
                        },
                    )
                    .unwrap();
            }
            self.app.apply_layout(&self.taffy, child, Point::ORIGIN);
            // A draw pass populates draw-time hit rects (e.g.
            // `detail_field_hits` for double-click-to-edit), exactly as the
            // live renderer does each frame.
            let presence = PresenceCtx::empty();
            let permission = PermissionContext::permissive();
            let mut scene = Scene::new();
            self.app.draw_with_presence(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &presence,
                &permission,
            );
        }

        fn dispatch(&mut self, event: &WindowEvent) -> bool {
            let mods = Modifiers::default();
            let mut handled = false;
            let mut focus = None;
            self.app.handle_event(
                event,
                1.0,
                &mods,
                &mut self.doc,
                &ROOT,
                &mut handled,
                &mut focus,
                self.node,
                &self.taffy,
            );
            handled
        }

        fn move_to(&mut self, p: Point) {
            self.dispatch(&WindowEvent::CursorMoved {
                device_id: device_id(),
                position: PhysicalPosition::new(p.x, p.y),
            });
        }

        fn press_left(&mut self, p: Point) -> bool {
            self.move_to(p);
            self.dispatch(&WindowEvent::MouseInput {
                device_id: device_id(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            })
        }

        fn press_right(&mut self, p: Point) -> bool {
            self.move_to(p);
            self.dispatch(&WindowEvent::MouseInput {
                device_id: device_id(),
                state: ElementState::Pressed,
                button: MouseButton::Right,
            })
        }

        /// A double click: two left presses at the same point in quick
        /// succession (the production double-click window is 400ms).
        fn double_click(&mut self, p: Point) {
            self.press_left(p);
            self.press_left(p);
        }

        /// First visible list row's center point.
        fn row_center(&self, index: usize) -> Point {
            self.app.list_rows[index].1.center()
        }

        fn row_id(&self, index: usize) -> ContactId {
            self.app.list_rows[index].0
        }

        /// Dispatch a key event with explicit modifier state through the real
        /// `handle_event` path.
        fn dispatch_key_mods(&mut self, event: &WindowEvent, mods: ModifiersState) -> bool {
            let modifiers: Modifiers = mods.into();
            let mut handled = false;
            let mut focus = None;
            self.app.handle_event(
                event,
                1.0,
                &modifiers,
                &mut self.doc,
                &ROOT,
                &mut handled,
                &mut focus,
                self.node,
                &self.taffy,
            );
            handled
        }

        /// Press a single `Named` key (no modifiers).
        fn press_named(&mut self, key: NamedKey) -> bool {
            self.dispatch_key_mods(
                &key_input_event(Key::Named(key), None),
                ModifiersState::empty(),
            )
        }

        /// Press a single character key (no modifiers) — a global shortcut like
        /// `n`, `e`, `/` when no field is focused.
        fn press_char(&mut self, c: &str) -> bool {
            self.dispatch_key_mods(
                &key_input_event(Key::Character(SmolStr::new(c)), Some(c)),
                ModifiersState::empty(),
            )
        }

        /// Press a character key with Ctrl held (e.g. Ctrl+S, Ctrl+K).
        fn press_char_ctrl(&mut self, c: &str) -> bool {
            self.dispatch_key_mods(
                &key_input_event(Key::Character(SmolStr::new(c)), Some(c)),
                ModifiersState::CONTROL,
            )
        }

        /// Press Shift+Tab (previous-field navigation) through the real path.
        fn press_shift_tab(&mut self) -> bool {
            self.dispatch_key_mods(
                &key_input_event(Key::Named(NamedKey::Tab), None),
                ModifiersState::SHIFT,
            )
        }

        /// The `Slot` of the currently-focused edit-form field, if any.
        fn focused_slot(&self) -> Option<Slot> {
            self.app
                .form
                .as_ref()
                .and_then(|f| f.inputs.iter().find(|i| i.ti.is_focused()).map(|i| i.slot))
        }

        /// Type a string one keystroke at a time into whichever input is
        /// focused, driving the production `handle_event` → `TextInput` path
        /// (NOT `set_text`), so the live edit semantics are what's exercised.
        fn type_text(&mut self, s: &str) {
            for ch in s.chars() {
                let buf = ch.to_string();
                self.dispatch_key_mods(
                    &key_input_event(Key::Character(SmolStr::new(&buf)), Some(&buf)),
                    ModifiersState::empty(),
                );
            }
        }

        /// Value currently held in the focused/edit form field for `slot`.
        fn field_value(&self, slot: Slot) -> String {
            self.app
                .form
                .as_ref()
                .and_then(|f| f.inputs.iter().find(|i| i.slot == slot))
                .map(|i| i.ti.value().to_string())
                .unwrap_or_default()
        }

        /// The stored (committed) contact for `id`, read back from the CRDT doc.
        fn doc_contact(&self, id: ContactId) -> Option<Contact> {
            ContactsDoc::list_contacts(&self.doc)
                .into_iter()
                .find(|c| c.id == id)
        }

        /// Focus the edit-form field for `slot` by clicking its laid-out rect
        /// through the production press path. Returns whether it was found.
        fn focus_field(&mut self, slot: Slot) -> bool {
            let rect = self
                .app
                .form
                .as_ref()
                .and_then(|f| f.inputs.iter().find(|i| i.slot == slot))
                .map(|i| i.ti.get_rect());
            match rect {
                Some(r) if r.width() > 0.0 => {
                    self.press_left(r.center());
                    self.app
                        .form
                        .as_ref()
                        .and_then(|f| f.inputs.iter().find(|i| i.slot == slot))
                        .is_some_and(|i| i.ti.is_focused())
                }
                _ => false,
            }
        }
    }

    #[test]
    fn narrow_row_press_pushes_detail_then_back_returns_to_list() {
        let mut h = Harness::new(460, 880);
        assert!(h.app.narrow);
        assert_eq!(h.app.push.pane(), Pane::List);
        // Press a list row → the detail pane is pushed in.
        let row = h.row_center(1);
        assert!(h.press_left(row));
        assert_eq!(
            h.app.push.pane(),
            Pane::Detail,
            "selecting pushes the detail in"
        );
        h.relayout();
        assert!(
            h.app.hit.back.width() > 0.0,
            "the Back affordance is laid out"
        );
        // Press Back → return to the list.
        let back = h.app.hit.back.center();
        assert!(h.press_left(back));
        assert_eq!(
            h.app.push.pane(),
            Pane::List,
            "Back returns to the full-width list"
        );
    }

    #[test]
    fn double_click_detail_field_enters_edit_focused_on_that_field() {
        let mut h = Harness::new(1280, 800);
        assert_eq!(h.app.mode, Mode::View);
        // Find the BIRTHDAY field value rect (every seed contact has one). The
        // website is deliberately excluded — it's a live hyperlink now, not a
        // double-click-to-edit target.
        let (slot, rect) = h
            .app
            .detail_field_hits
            .iter()
            .find(|(s, _)| *s == Slot::Birthday)
            .copied()
            .expect("selected contact shows a birthday field");
        assert_eq!(slot, Slot::Birthday);
        h.double_click(rect.center());
        assert_eq!(h.app.mode, Mode::Edit, "double-click enters edit mode");
        // The birthday input must be the focused one.
        let form = h.app.form.as_ref().unwrap();
        let birthday_focused = form
            .inputs
            .iter()
            .find(|f| f.slot == Slot::Birthday)
            .is_some_and(|f| f.ti.is_focused());
        assert!(
            birthday_focused,
            "the double-clicked field is focused for editing"
        );
    }

    /// Build an edit-mode harness on a known contact so the field order under
    /// Tab is deterministic: First, Last, Org, Title, Phone(0), Email(0),
    /// Birthday, Website, Notes, Groups.
    fn edit_harness() -> Harness {
        let mut h = Harness::new(1280, 800);
        let mut c = Contact::new();
        c.first = "Ada".into();
        c.last = "Byron".into();
        c.phones.push(Labeled::new("mobile", "+1 555 0100"));
        c.emails.push(Labeled::new("home", "ada@example.com"));
        h.app.begin_edit(c, true);
        h.relayout();
        h
    }

    #[test]
    fn tab_moves_focus_forward_across_the_form_fields() {
        let mut h = edit_harness();
        assert_eq!(h.app.mode, Mode::Edit);
        assert!(h.focused_slot().is_none(), "no field is focused until Tab");
        // Tab lands on the first field, then walks the natural order.
        assert!(
            h.press_named(NamedKey::Tab),
            "Tab is handled in the edit form"
        );
        assert_eq!(h.focused_slot(), Some(Slot::First));
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::Last));
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::Org));
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::Title));
        h.press_named(NamedKey::Tab);
        assert_eq!(
            h.focused_slot(),
            Some(Slot::Phone(0)),
            "repeated phone rows are in the order"
        );
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::Email(0)));
    }

    #[test]
    fn shift_tab_moves_focus_backward_and_wraps() {
        let mut h = edit_harness();
        // Forward twice → Last is focused.
        h.press_named(NamedKey::Tab);
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::Last));
        // Shift+Tab → back to First.
        assert!(h.press_shift_tab(), "Shift+Tab is handled");
        assert_eq!(h.focused_slot(), Some(Slot::First));
        // Shift+Tab from the first field wraps to the last (Groups).
        h.press_shift_tab();
        assert_eq!(
            h.focused_slot(),
            Some(Slot::Groups),
            "backward from the first wraps to the last"
        );
        // Tab from the last field wraps forward to the first.
        h.press_named(NamedKey::Tab);
        assert_eq!(
            h.focused_slot(),
            Some(Slot::First),
            "forward from the last wraps to the first"
        );
    }

    #[test]
    fn enter_advances_to_the_next_field_then_submits_on_the_last() {
        let mut h = edit_harness();
        // Enter with nothing focused focuses the first field.
        assert!(
            h.press_named(NamedKey::Enter),
            "Enter is handled in the edit form"
        );
        assert_eq!(h.focused_slot(), Some(Slot::First));
        // Enter advances to the next field.
        h.press_named(NamedKey::Enter);
        assert_eq!(h.focused_slot(), Some(Slot::Last));
        // Walk back to the last field: Last → First → (wrap) → Groups. Then
        // Enter on the last field submits (saves) instead of wrapping forward.
        h.press_shift_tab(); // Last → First
        assert_eq!(h.focused_slot(), Some(Slot::First));
        h.press_shift_tab(); // First → (wrap) → Groups
        assert_eq!(h.focused_slot(), Some(Slot::Groups));
        h.press_named(NamedKey::Enter);
        assert_eq!(
            h.app.mode,
            Mode::View,
            "Enter on the last field submits the form"
        );
        assert!(h.app.form.is_none(), "the edit form closed on submit");
        assert!(
            h.app
                .store
                .contacts
                .iter()
                .any(|c| c.first == "Ada" && c.last == "Byron"),
            "the edited contact was saved to the store",
        );
    }

    #[test]
    fn tab_into_a_field_places_the_caret_at_the_end() {
        // Tabbing into a pre-filled field puts the caret after its text so the
        // user can keep typing rather than overwriting.
        let mut h = edit_harness();
        h.press_named(NamedKey::Tab); // First ("Ada")
        let end = h.app.form.as_ref().unwrap().inputs[0].ti.value().len();
        assert_eq!(end, "Ada".len());
        h.type_text("!");
        assert_eq!(
            h.field_value(Slot::First),
            "Ada!",
            "typing appends at the caret"
        );
    }

    #[test]
    fn keyboard_focus_scrolls_a_below_the_fold_field_into_view() {
        // On a form taller than the pane, moving focus to a field past the fold
        // must scroll it into the visible body — otherwise the focus ring lands
        // on a field the user can't see.
        let mut h = Harness::new(1000, 420); // short pane → the form overflows
        assert!(!h.app.narrow, "1000px is the docked split");
        let mut c = Contact::new();
        c.first = "Long".into();
        c.last = "Form".into();
        for _ in 0..4 {
            c.phones.push(Labeled::new("mobile", ""));
        }
        for _ in 0..4 {
            c.emails.push(Labeled::new("home", ""));
        }
        h.app.begin_edit(c, true);
        h.relayout();

        let rect_of = |h: &Harness, slot: Slot| {
            h.app
                .form
                .as_ref()
                .unwrap()
                .inputs
                .iter()
                .find(|i| i.slot == slot)
                .unwrap()
                .ti
                .get_rect()
        };

        // The last field (Groups) starts well below the visible body.
        let body_bot = h.app.detail_rect.y1;
        assert!(
            rect_of(&h, Slot::Groups).y1 > body_bot,
            "precondition: the last field is off-screen below the fold",
        );

        // Shift+Tab (nothing focused) lands on the last field; it must now be
        // scrolled fully into view.
        assert!(h.press_shift_tab());
        assert_eq!(h.focused_slot(), Some(Slot::Groups));
        h.relayout(); // apply the scroll the focus move requested
        assert!(
            h.app.detail_scroll > 0.0,
            "focusing an off-screen field scrolled the form"
        );
        assert!(
            rect_of(&h, Slot::Groups).y1 <= h.app.detail_rect.y1 + 0.5,
            "the focused last field is now within the visible pane",
        );

        // Tab forward wraps back to the first field and scrolls back to the top.
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::First));
        h.relayout();
        let body_top = h.app.detail_rect.y0
            + FORM_TITLE_TOP
            + FORM_TITLE_H
            + FORM_ROW_GAP
            + 32.0
            + h.app.detail_top_offset();
        assert!(
            rect_of(&h, Slot::First).y0 >= body_top - 24.0,
            "wrapping to the first field scrolled back to the top",
        );
    }

    #[test]
    fn single_click_detail_field_does_not_enter_edit() {
        let mut h = Harness::new(1280, 800);
        let rect = h
            .app
            .detail_field_hits
            .iter()
            .find(|(s, _)| *s == Slot::Birthday)
            .map(|(_, r)| *r)
            .expect("birthday field present");
        h.press_left(rect.center());
        assert_eq!(
            h.app.mode,
            Mode::View,
            "a single click must not enter edit mode"
        );
    }

    #[test]
    fn website_renders_via_the_link_widget() {
        // The view-mode website is a real ui `Link` widget (not hand-drawn
        // text): the app holds a Link bound to the selected contact's URL, laid
        // out with a hit rect so it's clickable.
        let h = Harness::new(1280, 800);
        let id = h.app.selected.expect("a contact is selected");
        let website = h.app.store.get(id).unwrap().website.clone();
        assert!(!website.is_empty(), "the selected seed has a website");
        let link = h
            .app
            .website_link
            .as_ref()
            .expect("the website is rendered via a Link widget");
        assert_eq!(link.url(), website, "the link opens the contact's website");
        assert!(
            link.get_rect().width() > 0.0,
            "the link is laid out with a hit rect"
        );
    }

    #[test]
    fn clicking_the_website_opens_it_through_the_link_widget() {
        // Drive the whole click path (press → hit-test → Link::activate) and
        // confirm the contact's URL is what gets opened. The Link's `on_open`
        // sink records the URL instead of spawning the OS browser (its
        // documented test hook), so this exercises the production click path
        // hermetically — no xdg-open side effect.
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let url = h.app.store.get(id).unwrap().website.clone();
        assert!(!url.is_empty(), "the selected seed has a website");

        let recorded = std::rc::Rc::new(std::cell::RefCell::new(None::<String>));
        let sink = recorded.clone();
        // Same URL as the contact, so the detail draw preserves this instance
        // (it only rebuilds the link when the URL changes) and lays it out.
        h.app.website_link = Some(Link::new(&url).with_font_size(14.0).on_open(move |u| {
            *sink.borrow_mut() = Some(u.to_string());
        }));
        h.relayout();
        let rect = h.app.website_link.as_ref().unwrap().get_rect();
        assert!(
            rect.width() > 0.0,
            "the website link has a hit rect after layout"
        );

        assert!(
            h.press_left(rect.center()),
            "clicking the website link is handled"
        );
        assert_eq!(
            recorded.borrow().as_deref(),
            Some(url.as_str()),
            "the click opened the contact's website via the Link widget",
        );
        assert_eq!(
            h.app.mode,
            Mode::View,
            "opening a link does not enter edit mode"
        );
    }

    #[test]
    fn right_click_row_opens_context_menu_then_delete_opens_confirm() {
        let mut h = Harness::new(1280, 800);
        let row = h.row_center(0);
        let id = h.row_id(0);
        assert!(h.press_right(row));
        match &h.app.popup {
            Some(Popup::ContextMenu { target, .. }) => assert_eq!(*target, id),
            other => panic!(
                "right-click should open the context menu, got {:?}",
                other.is_some()
            ),
        }
        // Click the "Delete" row in the menu → the delete-confirm modal opens.
        let Some(Popup::ContextMenu { anchor, .. }) = h.app.popup else {
            unreachable!()
        };
        let (_panel, rows) = h
            .app
            .popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
        let delete_idx = CONTEXT_ACTIONS
            .iter()
            .position(|a| *a == ContextAction::Delete)
            .unwrap();
        h.press_left(rows[delete_idx].center());
        assert_eq!(
            h.app.confirm_delete,
            Some(id),
            "context-menu Delete routes through the confirmation modal"
        );
        assert!(
            h.app.popup.is_none(),
            "the context menu closes when an item is chosen"
        );
    }

    /// The context-menu Favorite item flips the contact's flag, persists it to
    /// the CRDT, and its label reflects the (now-updated) state on reopen.
    #[test]
    fn context_menu_favorite_toggles_flag_persists_and_relabels() {
        let mut h = Harness::new(1280, 800);
        let row = h.row_center(0);
        let id = h.row_id(0);

        // Read the CRDT-backed favorite state (not just the in-memory store).
        let fav_in_doc = |doc: &AutoCommit, id: ContactId| {
            ContactsDoc::list_contacts(doc)
                .into_iter()
                .find(|c| c.id == id)
                .map(|c| c.favorite)
                .expect("contact present in doc")
        };
        let before = h.app.store.get(id).unwrap().favorite;

        // The menu item index is stable; resolve it and confirm its label
        // matches the *current* (pre-toggle) favorite state.
        let fav_idx = CONTEXT_ACTIONS
            .iter()
            .position(|a| *a == ContextAction::Favorite)
            .unwrap();
        assert_eq!(
            CONTEXT_ACTIONS[fav_idx].label(before),
            if before {
                "Remove from favorites"
            } else {
                "Add to favorites"
            },
            "the Favorite item is labelled for the contact's current state",
        );

        // Open the menu and click the Favorite item.
        assert!(h.press_right(row));
        let Some(Popup::ContextMenu { anchor, .. }) = h.app.popup else {
            panic!("right-click should open the context menu");
        };
        let (_panel, rows) = h
            .app
            .popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
        h.press_left(rows[fav_idx].center());

        // The flag flipped in the store AND in the CRDT, and the menu closed.
        assert_eq!(
            h.app.store.get(id).unwrap().favorite,
            !before,
            "store flag flipped"
        );
        assert_eq!(
            fav_in_doc(&h.doc, id),
            !before,
            "CRDT favorite flag flipped"
        );
        assert!(
            h.app.popup.is_none(),
            "the menu closes after toggling favorite"
        );

        // Reopening the menu now shows the inverted label.
        assert!(h.press_right(h.row_center(0)));
        assert_eq!(
            CONTEXT_ACTIONS[fav_idx].label(!before),
            if before {
                "Add to favorites"
            } else {
                "Remove from favorites"
            },
            "the relabelled item reflects the toggled state",
        );

        // Toggling again restores the original state (round-trip).
        let Some(Popup::ContextMenu { anchor, .. }) = h.app.popup else {
            unreachable!()
        };
        let (_panel, rows) = h
            .app
            .popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
        h.press_left(rows[fav_idx].center());
        assert_eq!(
            fav_in_doc(&h.doc, id),
            before,
            "second toggle restores the CRDT flag"
        );
    }

    #[test]
    fn confirm_modal_delete_removes_contact_from_doc() {
        let mut h = Harness::new(1280, 800);
        let id = h.row_id(0);
        h.app.confirm_delete = Some(id);
        let before = ContactsDoc::list_contacts(&h.doc).len();
        let (_panel, del, _cancel) = h.app.confirm_geometry();
        h.press_left(del.center());
        assert!(
            h.app.confirm_delete.is_none(),
            "the modal closes after confirming"
        );
        let after = ContactsDoc::list_contacts(&h.doc).len();
        assert_eq!(
            after,
            before - 1,
            "Delete removes the contact from the CRDT doc"
        );
        assert!(
            !ContactsDoc::list_contacts(&h.doc)
                .iter()
                .any(|c| c.id == id),
            "the deleted contact is gone from the document"
        );
    }

    #[test]
    fn confirm_delete_dispatches_the_action_the_palette_builds() {
        // Single source of truth, proven on the real button path: confirming
        // a delete dispatches the byte-identical EncodedAction the Ctrl+K
        // "Delete contact" spec builds for the same contact.
        let mut h = Harness::new(1280, 800);
        let id = h.row_id(0);
        let expected =
            crate::actions::encode(&crate::actions::delete_contact_action(id.to_string()));
        h.app.confirm_delete = Some(id);
        let (_panel, del, _cancel) = h.app.confirm_geometry();
        h.press_left(del.center());
        assert_eq!(
            h.app.last_dispatched(),
            Some(&expected),
            "the confirm-delete button and the palette build the identical EncodedAction",
        );
        assert!(
            !ContactsDoc::list_contacts(&h.doc)
                .iter()
                .any(|c| c.id == id),
            "and it actually deleted the contact",
        );
    }

    #[test]
    fn favorite_toggle_dispatches_the_action_the_palette_builds() {
        // Same, on the favourite path: the context-menu Favorite item dispatches
        // the byte-identical EncodedAction the Ctrl+K "favorite" spec builds,
        // toggling to the negation of the current state.
        let mut h = Harness::new(1280, 800);
        let row = h.row_center(0);
        let id = h.row_id(0);
        let before = h.app.store.get(id).unwrap().favorite;
        let expected = crate::actions::encode(&crate::actions::set_favorite_action(
            id.to_string(),
            !before,
        ));

        assert!(h.press_right(row));
        let Some(Popup::ContextMenu { anchor, .. }) = h.app.popup else {
            panic!("right-click should open the context menu");
        };
        let (_panel, rows) = h
            .app
            .popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
        let fav_idx = CONTEXT_ACTIONS
            .iter()
            .position(|a| *a == ContextAction::Favorite)
            .unwrap();
        h.press_left(rows[fav_idx].center());
        assert_eq!(
            h.app.last_dispatched(),
            Some(&expected),
            "the favourite affordance and the palette build the identical EncodedAction",
        );
    }

    #[test]
    fn palette_action_reconciles_into_the_list_on_next_sync() {
        // A Ctrl+K palette action mutates only the document; the next sync must
        // reconcile the in-memory store so the change shows in the list — the
        // same way an on-screen edit already does.
        let mut h = Harness::new(1280, 800);
        let id = h.row_id(0);
        let before = h.app.store.len();
        // Dispatch a delete straight into the doc through the executor, bypassing
        // the app's own affordances (exactly as the shell's Ctrl+K palette would).
        let action = crate::actions::delete_contact_action(id.to_string());
        let encoded = crate::actions::encode(&action);
        let mut tx = qos_actions::LocalTransport::new();
        qos_actions::dispatch(&mut h.doc, &action, encoded, &mut tx).unwrap();
        // The store hasn't seen it yet — it still holds the contact.
        assert!(
            h.app.store.get(id).is_some(),
            "store is stale before the reconciling sync"
        );
        h.app.sync(&mut h.doc, &ROOT);
        assert!(
            h.app.store.get(id).is_none(),
            "the reconciling sync dropped the deleted contact"
        );
        assert_eq!(h.app.store.len(), before - 1);
    }

    #[test]
    fn switching_while_editing_dirty_pops_save_modal_then_save_commits() {
        let mut h = Harness::new(1280, 800);
        // Enter edit on the selected contact and dirty a field.
        let cur = h.app.selected.unwrap();
        let c = h.app.store.get(cur).cloned().unwrap();
        h.app.begin_edit(c, false);
        if let Some(form) = h.app.form.as_mut()
            && let Some(fi) = form.inputs.iter_mut().find(|f| f.slot == Slot::Title)
        {
            fi.ti.set_text("Brand New Title");
        }
        h.relayout();
        assert!(h.app.form_is_dirty(), "the edited form is dirty");
        // Click a different list row → the guard modal pops, no switch yet.
        let other_idx = (0..h.app.list_rows.len())
            .find(|i| h.app.list_rows[*i].0 != cur)
            .unwrap();
        let other_id = h.row_id(other_idx);
        let other_row = h.row_center(other_idx);
        h.press_left(other_row);
        assert_eq!(
            h.app.pending_switch,
            Some(other_id),
            "switch is deferred behind the modal"
        );
        assert_eq!(
            h.app.mode,
            Mode::Edit,
            "still editing while the modal is up"
        );
        // Press Save → commits the edit AND switches to the target.
        let g = h.app.unsaved_geometry();
        h.press_left(g.save.center());
        assert!(h.app.pending_switch.is_none());
        assert_eq!(h.app.mode, Mode::View, "saved and left the form");
        assert_eq!(
            h.app.selected,
            Some(other_id),
            "switched to the clicked contact"
        );
        let stored = ContactsDoc::list_contacts(&h.doc);
        let edited = stored.iter().find(|x| x.id == cur).unwrap();
        assert_eq!(
            edited.title, "Brand New Title",
            "the edit was saved to the doc"
        );
    }

    #[test]
    fn switching_while_editing_discard_drops_edits_and_switches() {
        let mut h = Harness::new(1280, 800);
        let cur = h.app.selected.unwrap();
        let original_title = h.app.store.get(cur).unwrap().title.clone();
        let c = h.app.store.get(cur).cloned().unwrap();
        h.app.begin_edit(c, false);
        if let Some(form) = h.app.form.as_mut()
            && let Some(fi) = form.inputs.iter_mut().find(|f| f.slot == Slot::Title)
        {
            fi.ti.set_text("Throwaway Title");
        }
        h.relayout();
        let other_idx = (0..h.app.list_rows.len())
            .find(|i| h.app.list_rows[*i].0 != cur)
            .unwrap();
        let other_id = h.row_id(other_idx);
        let other_row = h.row_center(other_idx);
        h.press_left(other_row);
        assert_eq!(h.app.pending_switch, Some(other_id));
        // Press Discard → drop edits, switch.
        let g = h.app.unsaved_geometry();
        h.press_left(g.discard.center());
        assert_eq!(h.app.selected, Some(other_id));
        let stored = ContactsDoc::list_contacts(&h.doc);
        let unchanged = stored.iter().find(|x| x.id == cur).unwrap();
        assert_eq!(
            unchanged.title, original_title,
            "discard left the doc unchanged"
        );
    }

    #[test]
    fn switching_with_no_changes_switches_directly_no_modal() {
        let mut h = Harness::new(1280, 800);
        let cur = h.app.selected.unwrap();
        let c = h.app.store.get(cur).cloned().unwrap();
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(!h.app.form_is_dirty(), "no edits ⇒ not dirty");
        let other_idx = (0..h.app.list_rows.len())
            .find(|i| h.app.list_rows[*i].0 != cur)
            .unwrap();
        let other_id = h.row_id(other_idx);
        let other_row = h.row_center(other_idx);
        h.press_left(other_row);
        assert!(
            h.app.pending_switch.is_none(),
            "no modal when there are no unsaved changes"
        );
        assert_eq!(h.app.selected, Some(other_id), "switched directly");
        assert_eq!(h.app.mode, Mode::View);
    }

    // =================================================================
    // Live-edit draft semantics — the structural fix. Editing mutates only
    // the form's local buffers/draft; nothing reaches the store or the CRDT
    // doc until Save. This is what makes Cancel real and the unsaved guard
    // reachable, so these drive the *real* keystroke path (handle_event),
    // never `set_text`.
    // =================================================================

    #[test]
    fn typing_in_edit_form_does_not_commit_and_cancel_discards() {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let original = h.app.store.get(id).unwrap().title.clone();
        let c = h.app.store.get(id).cloned().unwrap();
        h.app.begin_edit(c, false);
        h.relayout();
        // Focus the Title field via a real click, then type through the
        // production keystroke path (NOT `set_text`).
        assert!(
            h.focus_field(Slot::Title),
            "the title field focuses on click"
        );
        h.type_text("XYZ");
        // The keystrokes landed in the field's local buffer …
        let live = h.field_value(Slot::Title);
        assert!(
            live.contains("XYZ"),
            "the field shows the typed text: {live:?}"
        );
        assert_ne!(live, original);
        // … but NOTHING was committed: editing is a true draft now.
        assert_eq!(
            h.app.store.get(id).unwrap().title,
            original,
            "store untouched mid-edit"
        );
        assert_eq!(
            h.doc_contact(id).unwrap().title,
            original,
            "CRDT doc untouched mid-edit"
        );
        assert!(h.app.form_is_dirty(), "the live-typed form reports dirty");
        // Cancel genuinely discards the draft.
        assert!(
            h.app.hit.cancel.width() > 0.0,
            "the Cancel button is laid out"
        );
        h.press_left(h.app.hit.cancel.center());
        assert_eq!(h.app.mode, Mode::View, "Cancel leaves edit mode");
        assert!(h.app.form.is_none(), "Cancel drops the form");
        assert_eq!(
            h.app.store.get(id).unwrap().title,
            original,
            "Cancel discarded the edit (store)"
        );
        assert_eq!(
            h.doc_contact(id).unwrap().title,
            original,
            "Cancel discarded the edit (doc)"
        );
    }

    #[test]
    fn cancel_on_new_contact_leaves_no_phantom_record() {
        let mut h = Harness::new(1280, 800);
        let store_before = h.app.store.len();
        let doc_before = ContactsDoc::list_contacts(&h.doc).len();
        // Start a new contact via the keyboard shortcut, focus the name, type.
        assert!(h.press_char("n"), "'n' starts a new contact");
        assert_eq!(h.app.mode, Mode::Edit);
        assert!(h.app.form.as_ref().unwrap().is_new);
        h.relayout();
        assert!(h.focus_field(Slot::First));
        h.type_text("Ghost");
        // The half-typed new contact must not exist anywhere yet.
        assert_eq!(
            h.app.store.len(),
            store_before,
            "a half-typed new contact isn't in the store"
        );
        assert_eq!(
            ContactsDoc::list_contacts(&h.doc).len(),
            doc_before,
            "nor in the doc",
        );
        // Cancel → still nothing, no "No Name"/"Ghost" phantom left behind.
        h.press_left(h.app.hit.cancel.center());
        assert_eq!(h.app.mode, Mode::View);
        assert_eq!(
            h.app.store.len(),
            store_before,
            "Cancel on New leaves no phantom record"
        );
        assert!(
            !h.app
                .store
                .contacts
                .iter()
                .any(|c| c.display_name() == "Ghost"),
            "the abandoned draft never entered the book",
        );
    }

    /// The read-view Edit affordance is a plain click that opens the edit form
    /// on the *same* contact, and Cancel returns tidily to the read view — the
    /// clean view→edit→view round trip, driven entirely through the production
    /// click path (`handle_event`), not by poking `mode`/`begin_edit` directly.
    #[test]
    fn edit_affordance_opens_form_on_selected_contact_and_cancel_returns() {
        let mut h = Harness::new(1280, 800);
        assert_eq!(h.app.mode, Mode::View, "the app opens in the read view");
        let sel = h
            .app
            .selected
            .expect("a contact is selected in the read view");
        assert!(
            h.app.hit.edit.width() > 0.0,
            "the read-view Edit button is laid out"
        );

        // Clicking Edit enters the form on the shown contact (identity carried
        // across the transition — the header's avatar + name reflect this draft).
        h.press_left(h.app.hit.edit.center());
        assert_eq!(h.app.mode, Mode::Edit, "clicking Edit opens the edit form");
        {
            let form = h.app.form.as_ref().expect("an edit form is open");
            assert_eq!(
                form.draft.id, sel,
                "the form edits the same contact the view showed"
            );
            assert!(!form.is_new, "editing an existing record, not creating one");
        }

        // Leaving is just as direct: Cancel returns to the read view, no form.
        h.relayout();
        assert!(
            h.app.hit.cancel.width() > 0.0,
            "the Cancel button is laid out in the form"
        );
        h.press_left(h.app.hit.cancel.center());
        assert_eq!(h.app.mode, Mode::View, "Cancel returns to the read view");
        assert!(h.app.form.is_none(), "no edit form lingers after Cancel");
        assert_eq!(h.app.selected, Some(sel), "the same contact stays selected");
    }

    #[test]
    fn typed_edit_saved_with_ctrl_s_commits_through_the_keystroke_path() {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.get(id).cloned().unwrap();
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(h.focus_field(Slot::Org));
        h.type_text("Z");
        let typed = h.field_value(Slot::Org);
        assert!(typed.contains('Z'));
        // Ctrl+S saves; the typed value now round-trips into store + doc.
        assert!(h.press_char_ctrl("s"), "Ctrl+S is handled in edit mode");
        assert_eq!(h.app.mode, Mode::View, "Ctrl+S leaves the form");
        assert_eq!(
            h.app.store.get(id).unwrap().org,
            typed,
            "the typed org saved to the store"
        );
        assert_eq!(h.doc_contact(id).unwrap().org, typed, "and to the CRDT doc");
    }

    #[test]
    fn dirtying_via_keystrokes_makes_the_unsaved_guard_reachable() {
        // Proves the guard on the REAL keystroke path (not `set_text`): typing
        // leaves the store untouched, so `form_is_dirty` sees the divergence
        // and switching contacts pops the modal instead of losing the edit.
        let mut h = Harness::new(1280, 800);
        let cur = h.app.selected.unwrap();
        let c = h.app.store.get(cur).cloned().unwrap();
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(h.focus_field(Slot::Title));
        h.type_text("Q");
        assert!(
            h.app.form_is_dirty(),
            "keystroke-dirtied form is dirty via the production path"
        );
        let other_idx = (0..h.app.list_rows.len())
            .find(|i| h.app.list_rows[*i].0 != cur)
            .unwrap();
        let other_id = h.row_id(other_idx);
        h.press_left(h.row_center(other_idx));
        assert_eq!(
            h.app.pending_switch,
            Some(other_id),
            "the guard defers the switch"
        );
        assert_eq!(h.app.mode, Mode::Edit, "still editing behind the modal");
    }

    #[test]
    fn bare_keys_do_not_discard_an_open_edit_form() {
        // Regression: with the edit form open but no field yet focused (its
        // initial state right after opening), a stray 'n' used to start a
        // brand-new contact — silently discarding the in-progress edit with no
        // unsaved guard — and '/' yanked focus to the list search. The form's
        // own keys own the keyboard until a field is focused.
        let mut h = edit_harness();
        assert_eq!(h.app.mode, Mode::Edit);
        assert!(h.app.form.as_ref().unwrap().is_new);
        assert!(
            h.focused_slot().is_none(),
            "the form opens with no field focused"
        );
        let draft_id = h.app.form.as_ref().unwrap().draft.id;

        // 'n' must NOT start a new contact / swap the draft out from under us.
        h.press_char("n");
        assert_eq!(h.app.mode, Mode::Edit, "'n' does not leave the edit form");
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.id,
            draft_id,
            "'n' did not replace the in-progress draft",
        );

        // '/' must NOT steal focus to the list search while editing.
        h.press_char("/");
        assert!(
            !h.app.search.is_focused(),
            "'/' does not steal focus mid-edit"
        );
        assert_eq!(h.app.mode, Mode::Edit, "still editing");

        // Once a field IS focused, the very same key types into it normally.
        assert!(h.focus_field(Slot::First));
        h.type_text("n");
        assert!(
            h.field_value(Slot::First).contains('n'),
            "with a field focused the key types into it: {:?}",
            h.field_value(Slot::First),
        );
    }

    // =================================================================
    // List controls: search, sort, group filter, keyboard nav, scroll.
    // =================================================================

    #[test]
    fn typing_in_search_narrows_the_visible_list() {
        let mut h = Harness::new(1280, 800);
        assert_eq!(h.app.visible_ids().len(), 7);
        // Focus the search field with '/', then type a query via keystrokes.
        assert!(h.press_char("/"), "'/' focuses search");
        assert!(h.app.search.is_focused());
        h.type_text("hopper");
        assert_eq!(h.app.search.value(), "hopper");
        let visible = h.app.visible_ids();
        assert_eq!(visible.len(), 1, "only Grace Hopper matches 'hopper'");
        assert_eq!(h.app.store.get(visible[0]).unwrap().last, "Hopper");
        // The rendered rows reflect it after a layout pass.
        h.relayout();
        assert_eq!(
            h.app.list_rows.len(),
            1,
            "the list pane renders the single match"
        );
    }

    #[test]
    fn sort_chip_click_toggles_name_org_and_reorders() {
        let mut h = Harness::new(1280, 800);
        assert_eq!(h.app.sort, SortKey::Name);
        let first_by_name = h
            .app
            .store
            .get(h.app.visible_ids()[0])
            .unwrap()
            .display_name();
        assert_eq!(
            first_by_name, "Tim Berners-Lee",
            "name sort leads with Berners-Lee"
        );
        assert!(
            h.press_left(h.app.hit.sort.center()),
            "the sort chip is clickable"
        );
        assert_eq!(h.app.sort, SortKey::Org, "clicking toggles to Org sort");
        let first_by_org = h
            .app
            .store
            .get(h.app.visible_ids()[0])
            .unwrap()
            .display_name();
        assert_ne!(first_by_org, first_by_name, "the list reordered");
        assert_eq!(
            first_by_org, "Ada Lovelace",
            "org sort leads with Analytical Engines"
        );
        // Toggling again returns to Name sort.
        h.press_left(h.app.hit.sort.center());
        assert_eq!(h.app.sort, SortKey::Name);
    }

    #[test]
    fn group_filter_popup_selection_filters_and_keeps_selection_valid() {
        let mut h = Harness::new(1280, 800);
        // The seeded selection (Ada) is not in "Friends".
        let sel_before = h.app.selected.unwrap();
        assert!(
            !h.app
                .store
                .get(sel_before)
                .unwrap()
                .groups
                .iter()
                .any(|g| g.eq_ignore_ascii_case("Friends")),
        );
        // Open the group filter popup and choose "Friends".
        assert!(h.press_left(h.app.hit.group.center()));
        let Some(Popup::GroupFilter { anchor }) = h.app.popup else {
            panic!("the group chip opens the filter popup");
        };
        let mut opts = vec!["All".to_string()];
        opts.extend(h.app.store.all_groups());
        let idx = opts
            .iter()
            .position(|o| o == "Friends")
            .expect("Friends is a seeded group");
        let (_panel, rows) = h.app.popup_menu_geometry(anchor, opts.len(), 180.0);
        h.press_left(rows[idx].center());
        assert_eq!(
            h.app.group_filter.as_deref(),
            Some("Friends"),
            "the filter is applied"
        );
        let visible = h.app.visible_ids();
        assert!(!visible.is_empty());
        assert!(
            visible.iter().all(|id| h
                .app
                .store
                .get(*id)
                .unwrap()
                .groups
                .iter()
                .any(|g| g.eq_ignore_ascii_case("Friends"))),
            "every visible contact is in Friends",
        );
        // Selection stayed valid: it now points at a still-visible contact.
        assert!(
            visible.contains(&h.app.selected.unwrap()),
            "selection moved to a visible contact"
        );
    }

    #[test]
    fn arrow_keys_move_the_list_selection() {
        let mut h = Harness::new(1280, 800);
        let ids = h.app.visible_ids();
        let pos = ids
            .iter()
            .position(|x| *x == h.app.selected.unwrap())
            .unwrap();
        assert!(
            pos + 1 < ids.len(),
            "the seeded selection isn't the last row"
        );
        assert!(h.press_named(NamedKey::ArrowDown));
        assert_eq!(
            h.app.selected,
            Some(ids[pos + 1]),
            "ArrowDown selects the next contact"
        );
        assert!(h.press_named(NamedKey::ArrowUp));
        assert_eq!(
            h.app.selected,
            Some(ids[pos]),
            "ArrowUp returns to the previous"
        );
    }

    #[test]
    fn n_key_opens_new_and_e_key_edits_selection() {
        let mut h = Harness::new(1280, 800);
        assert!(h.press_char("n"));
        assert_eq!(h.app.mode, Mode::Edit);
        assert!(
            h.app.form.as_ref().unwrap().is_new,
            "'n' opens a NEW contact form"
        );
        // Escape out, then 'e' edits the existing selection.
        h.press_named(NamedKey::Escape);
        assert_eq!(h.app.mode, Mode::View);
        assert!(h.press_char("e"));
        assert_eq!(h.app.mode, Mode::Edit);
        assert!(
            !h.app.form.as_ref().unwrap().is_new,
            "'e' edits the selected contact"
        );
    }

    #[test]
    fn delete_key_opens_confirm_and_escape_dismisses_it() {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        assert!(h.press_named(NamedKey::Delete));
        assert_eq!(
            h.app.confirm_delete,
            Some(id),
            "Delete raises the confirm modal for the selection"
        );
        h.press_named(NamedKey::Escape);
        assert!(
            h.app.confirm_delete.is_none(),
            "Escape dismisses the confirm modal"
        );
        assert!(h.app.store.get(id).is_some(), "nothing was deleted");
    }

    #[test]
    fn dragging_the_divider_resizes_the_split() {
        let mut h = Harness::new(1280, 800);
        let before = h.app.split_pos;
        // Grab the divider (between list and detail).
        let grab = Point::new(
            h.app.list_rect.x1 + DIVIDER_W / 2.0,
            h.app.detail_rect.center().y,
        );
        h.move_to(grab);
        assert!(h.app.divider_hover, "cursor over the divider arms the drag");
        h.dispatch(&WindowEvent::MouseInput {
            device_id: device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        assert!(h.app.divider_drag, "pressing on the divider starts a drag");
        // Drag to ~55% of the width.
        h.move_to(Point::new(
            h.app.rect.x0 + h.app.rect.width() * 0.55,
            grab.y,
        ));
        assert!(
            h.app.split_pos > before,
            "dragging right widens the list pane"
        );
        assert!(
            (h.app.split_pos - 0.55).abs() < 0.02,
            "the split follows the cursor"
        );
        h.dispatch(&WindowEvent::MouseInput {
            device_id: device_id(),
            state: ElementState::Released,
            button: MouseButton::Left,
        });
        assert!(!h.app.divider_drag, "releasing ends the drag");
    }

    #[test]
    fn mouse_wheel_scrolls_the_overflowing_contact_list() {
        // A short, wide window overflows the list so scrolling has an effect.
        let mut h = Harness::new(1000, 320);
        assert!(!h.app.narrow, "1000px stays the docked split");
        assert_eq!(h.app.list_scroll, 0.0);
        h.move_to(h.app.list_rect.center());
        h.dispatch(&WindowEvent::MouseWheel {
            device_id: device_id(),
            delta: MouseScrollDelta::LineDelta(0.0, -3.0),
            phase: winit::event::TouchPhase::Moved,
        });
        h.relayout();
        assert!(
            h.app.list_scroll > 0.0,
            "scrolling down moves the list; clamp keeps it in range"
        );
    }

    // =================================================================
    // Edit-form controls: add/remove field, type chip, calendar, favorites.
    // =================================================================

    #[test]
    fn add_and_remove_phone_buttons_grow_and_shrink_the_draft() {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.get(id).cloned().unwrap();
        let base = c.phones.len();
        h.app.begin_edit(c, false);
        h.relayout();
        // Add-phone button appends a blank row.
        assert!(
            h.app.hit.add_phone.width() > 0.0,
            "the add-phone button is laid out"
        );
        h.press_left(h.app.hit.add_phone.center());
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.phones.len(),
            base + 1,
            "add-phone grew the draft"
        );
        h.relayout();
        // Remove-field button on the new phone drops it again.
        let rm = h
            .app
            .hit
            .remove_btns
            .iter()
            .find(|(t, _)| matches!(t, TypeTarget::Phone(i) if *i == base))
            .map(|(_, r)| *r)
            .expect("the new phone row has a remove button");
        h.press_left(rm.center());
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.phones.len(),
            base,
            "remove-field shrank the draft"
        );
    }

    #[test]
    fn type_chip_menu_selection_sets_label_in_draft_only() {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.get(id).cloned().unwrap();
        assert!(!c.phones.is_empty());
        let stored_label = c.phones[0].label.clone();
        h.app.begin_edit(c, false);
        h.relayout();
        // Click the type chip for the first phone.
        let chip = h
            .app
            .hit
            .type_chips
            .iter()
            .find(|(t, _)| matches!(t, TypeTarget::Phone(0)))
            .map(|(_, r)| *r)
            .expect("phone 0 has a type chip");
        h.press_left(chip.center());
        let Some(Popup::TypeMenu { anchor, .. }) = h.app.popup else {
            panic!("the type chip opens the type menu");
        };
        // Pick a label different from the current one.
        let pick = TYPE_OPTIONS
            .iter()
            .position(|l| *l != stored_label)
            .unwrap();
        let (_panel, rows) = h.app.popup_menu_geometry(anchor, TYPE_OPTIONS.len(), 140.0);
        h.press_left(rows[pick].center());
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.phones[0].label,
            TYPE_OPTIONS[pick],
            "the draft label updated",
        );
        // Draft-only: the committed store still holds the old label.
        assert_eq!(
            h.app.store.get(id).unwrap().phones[0].label,
            stored_label,
            "the label isn't committed until Save",
        );
    }

    #[test]
    fn calendar_day_click_sets_birthday_in_draft_only() {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.get(id).cloned().unwrap();
        let stored_bday = c.birthday;
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(
            h.app.hit.cal_btn.width() > 0.0,
            "the calendar button is laid out"
        );
        h.press_left(h.app.hit.cal_btn.center());
        let Some(Popup::Calendar {
            year,
            month,
            anchor,
        }) = h.app.popup
        else {
            panic!("the calendar button opens the calendar");
        };
        let (panel, _prev, _next, cells) = h.app.calendar_geometry(anchor, year, month);
        // Pick an in-panel day different from the stored one (10).
        let (day, rect) = cells
            .iter()
            .copied()
            .find(|(d, r)| *d != 10 && panel.contains(r.center()))
            .expect("a selectable in-panel day");
        h.press_left(rect.center());
        let expect = format!("{year:04}-{month:02}-{day:02}");
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.birthday,
            Some(Birthday::Full { year, month, day }),
            "the picked day set the draft birthday as a structured date",
        );
        assert_eq!(
            h.field_value(Slot::Birthday),
            expect,
            "and mirrored into the birthday field"
        );
        // Draft-only: not committed to the store yet.
        assert_eq!(
            h.app.store.get(id).unwrap().birthday,
            stored_bday,
            "birthday not committed until Save"
        );
    }

    #[test]
    fn detail_favorite_star_click_toggles_store_and_doc() {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let before = h.app.store.get(id).unwrap().favorite;
        assert!(
            h.app.hit.fav.width() > 0.0,
            "the detail favorite star is laid out"
        );
        h.press_left(h.app.hit.fav.center());
        assert_eq!(
            h.app.store.get(id).unwrap().favorite,
            !before,
            "the star flips the store flag"
        );
        assert_eq!(
            h.doc_contact(id).unwrap().favorite,
            !before,
            "and persists to the doc"
        );
    }

    #[test]
    fn edit_form_favorite_pill_click_toggles_draft_only() {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.get(id).cloned().unwrap();
        let before = c.favorite;
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(
            h.app.hit.fav_toggle.width() > 0.0,
            "the favorite pill is laid out in the form"
        );
        h.press_left(h.app.hit.fav_toggle.center());
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.favorite,
            !before,
            "the pill flips the draft flag"
        );
        assert_eq!(
            h.app.store.get(id).unwrap().favorite,
            before,
            "draft-only: store unchanged until Save"
        );
    }

    /// A unique scratch `.vcf` destination for an export test — the path the
    /// portal Save dialog would return, driven directly since the picker can't
    /// run headless.
    fn scratch_export() -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "contacts-export-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let vcf = dir.join("out.vcf");
        (dir, vcf)
    }

    #[test]
    #[serial_test::serial]
    fn export_selection_parks_then_save_portal_path_writes_the_vcf() {
        let (dir, vcf) = scratch_export();
        let mut h = Harness::new(1280, 800);
        // Open the export dialog via the toolbar Export button.
        h.press_left(h.app.hit.export.center());
        assert!(
            h.app.export_sel.is_some(),
            "Export opens the selection dialog"
        );
        // Pre-ticked with all visible contacts.
        let total = h.app.visible_export_ids().len();
        assert_eq!(
            h.app.export_sel.as_ref().unwrap().len(),
            total,
            "all pre-selected"
        );
        // Select none, then tick exactly one row.
        let g = h.app.export_geometry();
        h.press_left(g.select_none.center());
        let g = h.app.export_geometry();
        let (first_id, _row, cb) = g.rows[0];
        h.press_left(cb.center());
        assert_eq!(h.app.export_sel.as_ref().unwrap().len(), 1);
        assert!(h.app.export_sel.as_ref().unwrap().contains(&first_id));

        // Export 3.0 → closes the dialog and parks the selection for the Save
        // dialog. It must NOT write anything before the user chooses a path.
        let g = h.app.export_geometry();
        h.press_left(g.export_v3.center());
        assert!(
            h.app.export_sel.is_none(),
            "choosing a version closes the dialog"
        );
        {
            let (ids, version) = h.app.pending_export.as_ref().expect("selection is parked");
            assert_eq!(ids, &vec![first_id], "the one ticked contact is parked");
            assert_eq!(*version, VCardVersion::V3, "the chosen version is parked");
        }
        assert!(h.app.portal_in_flight(), "the Save dialog is in flight");
        assert!(
            !vcf.exists(),
            "nothing is written until a destination is chosen"
        );

        // Deliver the destination the Save portal would return, then drain it
        // through one real frame's sync — which writes the file.
        h.app.export_rx = Some(portal::inject_export_for_test(ExportOutcome::Opened(
            vcf.clone(),
        )));
        h.app.sync(&mut h.doc, &ROOT);
        assert!(
            h.app.pending_export.is_none(),
            "the parked selection was consumed"
        );
        assert!(!h.app.portal_in_flight(), "draining clears the request");
        let body = std::fs::read_to_string(&vcf).expect("export wrote a vcf to the chosen path");
        assert_eq!(
            body.matches("BEGIN:VCARD").count(),
            1,
            "exactly the ticked contact"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[serial_test::serial]
    fn write_export_round_trips_every_contact() {
        // The hermetic write core: hand it every visible id + a version and it
        // writes a matching .vcf to the given path.
        let (dir, vcf) = scratch_export();
        let mut h = Harness::new(1280, 800);
        let ids = h.app.visible_export_ids();
        let total = ids.len();
        h.app.write_export(&ids, VCardVersion::V4, &vcf);
        let body = std::fs::read_to_string(&vcf).unwrap();
        assert_eq!(
            body.matches("BEGIN:VCARD").count(),
            total,
            "every contact written"
        );
        assert!(body.contains("VERSION:4.0"), "Export 4.0 emits v4 cards");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_with_empty_selection_never_opens_the_portal() {
        // Choosing a version with nothing ticked must not spin up a Save
        // dialog or park anything — it guides the user instead.
        let mut h = Harness::new(1280, 800);
        let opened = h.app.request_export(Vec::new(), VCardVersion::V3);
        assert!(!opened, "an empty selection does not open the Save dialog");
        assert!(h.app.pending_export.is_none(), "nothing is parked");
        assert!(!h.app.portal_in_flight(), "no portal request is in flight");
        match &h.app.toast {
            Some((msg, _)) => assert!(msg.to_lowercase().contains("select")),
            None => panic!("an empty export must surface a guiding toast"),
        }
    }

    #[test]
    fn focus_search_chord_predicate_matches_ctrl_k_and_ctrl_slash() {
        assert!(is_focus_search_chord(
            true,
            &Key::Character(SmolStr::new("k"))
        ));
        assert!(is_focus_search_chord(
            true,
            &Key::Character(SmolStr::new("K"))
        ));
        assert!(is_focus_search_chord(
            true,
            &Key::Character(SmolStr::new("/"))
        ));
        // Not the chord: same keys without ctrl, or other ctrl chords.
        assert!(!is_focus_search_chord(
            false,
            &Key::Character(SmolStr::new("k"))
        ));
        assert!(!is_focus_search_chord(
            true,
            &Key::Character(SmolStr::new("s"))
        ));
        assert!(!is_focus_search_chord(true, &Key::Named(NamedKey::Escape)));
    }

    #[test]
    fn ctrl_k_focuses_the_search_field_through_handle_event() {
        // The chord used to be a dead seam that did nothing; it now performs a
        // real, in-app action — focusing the list search box — driven all the
        // way through the production `handle_event` path.
        let mut h = Harness::new(1280, 800);
        assert!(!h.app.search.is_focused(), "search starts unfocused");
        assert!(h.press_char_ctrl("k"), "Ctrl+K is handled by the app");
        assert!(h.app.search.is_focused(), "Ctrl+K focuses the search field");
        // Escape releases it, and Ctrl+/ focuses it again.
        h.press_named(NamedKey::Escape);
        assert!(!h.app.search.is_focused());
        assert!(h.press_char_ctrl("/"), "Ctrl+/ is handled too");
        assert!(
            h.app.search.is_focused(),
            "Ctrl+/ also focuses the search field"
        );
    }

    #[test]
    fn placeholders_cover_every_form_slot() {
        // Every editable slot must have a non-empty placeholder so the New
        // Contact form never shows a blank, unlabelled field.
        for slot in [
            Slot::First,
            Slot::Last,
            Slot::Org,
            Slot::Title,
            Slot::Website,
            Slot::Birthday,
            Slot::Notes,
            Slot::Groups,
            Slot::Phone(0),
            Slot::Email(0),
            Slot::AddrStreet(0),
            Slot::AddrCity(0),
            Slot::AddrRegion(0),
            Slot::AddrPostal(0),
            Slot::AddrCountry(0),
        ] {
            assert!(
                !placeholder_for(slot).is_empty(),
                "slot is missing a placeholder hint"
            );
        }
    }

    #[test]
    fn new_contact_form_is_not_dirty_until_a_field_is_filled() {
        // A freshly-opened New Contact form (with the two empty seeded
        // phone/email rows) must not be considered dirty — otherwise the
        // unsaved guard would fire on the very first contact switch.
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        app.shots_new_contact();
        assert!(
            !app.form_is_dirty(),
            "an untouched New Contact form is clean"
        );
        if let Some(form) = app.form.as_mut() {
            form.draft.first = "Jane".into();
            if let Some(fi) = form.inputs.iter_mut().find(|f| f.slot == Slot::First) {
                fi.ti.set_text("Jane");
            }
        }
        assert!(app.form_is_dirty(), "typing a name makes the form dirty");
    }

    // =================================================================
    // Real persistence + first-launch behaviour
    // =================================================================

    fn scratch_book() -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "contacts-app-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let path = dir.join("addressbook.automerge");
        (dir, path)
    }

    #[test]
    fn live_launch_with_no_saved_book_is_empty_not_fabricated() {
        // The live app must NOT invent sample contacts on an empty store.
        let (dir, _path) = scratch_book();
        let mut app = ContactsApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        assert!(
            app.store.is_empty(),
            "a fresh disk-backed launch shows an empty book"
        );
        assert!(
            app.selected.is_none(),
            "nothing is selected when the book is empty"
        );
        // And a full frame over the empty state must not panic.
        drive_frame(&mut app, 1280, 800);
        drive_frame(&mut app, 560, 720);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The production restart path: a contact created through the real edit
    /// flow goes into the shared document, the store writes that document,
    /// and a second launch reading the store finds the contact again.
    #[test]
    #[serial_test::serial]
    fn created_contact_persists_across_a_restart() {
        let home = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", home.path());
            std::env::remove_var("QOS_PACKAGE_ID");
        }

        // First launch.
        let mut store = ui::persist::DocStore::open_for("dev.qos.contacts").unwrap();
        let mut doc = store.load_or_new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        assert!(app.store.is_empty(), "a fresh store shows an empty book");
        let mut c = Contact::new();
        c.first = "Persisted".into();
        c.last = "Person".into();
        c.phones.push(Labeled::new("mobile", "+1 555 0100"));
        app.begin_edit(c, true);
        app.save_edit(&mut doc);
        assert_eq!(app.store.len(), 1, "the contact is in the working set");
        store.save(&mut doc).unwrap();
        drop((store, doc, app)); // the process exits here

        // Second launch ("reboot"): a new app over the reloaded document.
        let mut store2 = ui::persist::DocStore::open_for("dev.qos.contacts").unwrap();
        let mut doc2 = store2.load().unwrap().expect("the store has a document");
        let mut app2 = ContactsApp::new();
        app2.sync(&mut doc2, &ROOT);
        assert_eq!(
            app2.store.len(),
            1,
            "the saved contact loads on the next launch"
        );
        let loaded = &app2.store.contacts[0];
        assert_eq!(loaded.first, "Persisted");
        assert_eq!(loaded.phones[0].value, "+1 555 0100");
    }

    /// The property the old save path destroyed, asserted in the app that
    /// had the bug.
    ///
    /// `storage::save` rebuilt a fresh `AutoCommit` from the contact list on
    /// every write. The document it produced shared no history — and no
    /// object identity — with the one it came from, so merging it with a
    /// replica that had diverged produced two unrelated `contacts` objects
    /// and every record twice. Round-tripping through the store instead must
    /// leave the two sides mergeable: one shared object, each side's edit
    /// present exactly once.
    #[test]
    #[serial_test::serial]
    fn a_saved_book_still_merges_with_a_replica_that_diverged_before_the_save() {
        let home = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", home.path());
            std::env::remove_var("QOS_PACKAGE_ID");
        }
        let mut store = ui::persist::DocStore::open_for("dev.qos.contacts").unwrap();
        let mut doc = store.new_document();

        // A shared ancestor: one contact both sides know about.
        let mut shared = Contact::new();
        shared.first = "Ada".into();
        shared.last = "Lovelace".into();
        let shared_id = shared.id;
        ContactsDoc::upsert_contact(&mut doc, &shared);
        store.save(&mut doc).unwrap();

        // A peer forks here and adds a contact of its own.
        let mut peer = doc.fork();
        let mut theirs = Contact::new();
        theirs.first = "Grace".into();
        theirs.last = "Hopper".into();
        let their_id = theirs.id;
        ContactsDoc::upsert_contact(&mut peer, &theirs);

        // Meanwhile this app adds one and the session ends.
        let mut mine = Contact::new();
        mine.first = "Alan".into();
        mine.last = "Turing".into();
        let my_id = mine.id;
        ContactsDoc::upsert_contact(&mut doc, &mine);
        store.save(&mut doc).unwrap();
        drop((store, doc));

        // Restart, then merge the peer that diverged before the save.
        let mut store2 = ui::persist::DocStore::open_for("dev.qos.contacts").unwrap();
        let mut restored = store2.load().unwrap().expect("the saved document");
        restored
            .merge(&mut peer)
            .expect("a stored book merges with a peer");

        let book = ContactsDoc::list_contacts(&restored);
        assert_eq!(
            book.len(),
            3,
            "each side's contact appears exactly once, not duplicated: {:?}",
            book.iter().map(|c| c.first.clone()).collect::<Vec<_>>()
        );
        for (id, name) in [(shared_id, "Ada"), (my_id, "Alan"), (their_id, "Grace")] {
            let found = book.iter().filter(|c| c.id == id).count();
            assert_eq!(
                found, 1,
                "{name} must be present exactly once, found {found}"
            );
        }
        // The decisive check: both sides edited the *same* contacts object.
        // A rebuilt-from-scratch save would have made two, and the merge
        // would leave a conflict at the key.
        assert_eq!(
            restored.get_all(&ROOT, "contacts").unwrap().len(),
            1,
            "history was preserved, so the merge reconciled one object rather \
             than colliding two unrelated ones"
        );
    }

    /// An address book written by the release that kept its own file must
    /// come across on the first launch after the upgrade.
    #[test]
    #[serial_test::serial]
    fn a_legacy_address_book_is_imported_into_the_store() {
        let home = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", home.path());
            std::env::remove_var("QOS_PACKAGE_ID");
        }
        let (dir, legacy) = scratch_book();
        let book = sample_contacts();
        crate::storage::save(&legacy, &book).unwrap();

        let mut store = ui::persist::DocStore::open_for("dev.qos.contacts").unwrap();
        assert!(
            store.migrate_from(&legacy).unwrap(),
            "the legacy file is imported"
        );
        let mut doc = store.load().unwrap().expect("document after migration");

        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        assert_eq!(
            app.store.len(),
            book.len(),
            "every contact from the old file is in the book"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[serial_test::serial]
    fn import_reads_real_vcf_files_and_persists_them() {
        let (dir, path) = scratch_book();
        std::fs::create_dir_all(&dir).unwrap();
        // Two separate, differently-shaped .vcf files — the multi-select the
        // portal hands back. The first holds one card, the second holds two,
        // so the merge over arbitrarily many files is what's exercised.
        let vcf_a = dir.join("work.vcf");
        std::fs::write(
            &vcf_a,
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Imported Person\r\nN:Person;Imported;;;\r\nEMAIL;TYPE=HOME:imported@example.com\r\nEND:VCARD\r\n",
        )
        .unwrap();
        let vcf_b = dir.join("family.vcard");
        std::fs::write(
            &vcf_b,
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ada Byron\r\nN:Byron;Ada;;;\r\nTEL:+1 555 0100\r\nEND:VCARD\r\n\
             BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Grace Hopper\r\nN:Hopper;Grace;;;\r\nORG:US Navy\r\nEND:VCARD\r\n",
        )
        .unwrap();

        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        assert!(app.store.is_empty());

        // Drive the parse+merge core directly with the paths the portal would
        // have handed back (the picker itself can't run headless).
        let n = app.import_paths(&mut doc, &[vcf_a, vcf_b]);
        assert_eq!(n, 3, "all three cards across the two files were imported");
        assert_eq!(
            app.store.len(),
            3,
            "the parsed cards are in the working set"
        );
        let firsts: std::collections::HashSet<&str> = app
            .store
            .contacts
            .iter()
            .map(|c| c.first.as_str())
            .collect();
        assert!(firsts.contains("Imported"));
        assert!(firsts.contains("Ada"));
        assert!(firsts.contains("Grace"));

        // The import reached the shared document, which is what the store
        // writes — so it survives a restart.
        assert_eq!(
            ContactsDoc::list_contacts(&doc).len(),
            3,
            "written to the doc"
        );
        let _ = path;

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_drain_merges_selected_files_through_sync() {
        // The live plumbing end to end: an ImportOutcome::Selected on the
        // channel is drained by `sync` (the per-frame drain) and merged into
        // the book — exactly what the portal worker feeds after the user picks
        // files. Only the picker is bypassed; the drain + parse + merge is real.
        let (dir, path) = scratch_book();
        std::fs::create_dir_all(&dir).unwrap();
        let vcf = dir.join("incoming.vcf");
        std::fs::write(
            &vcf,
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Drained Person\r\nN:Person;Drained;;;\r\nEND:VCARD\r\n",
        )
        .unwrap();

        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        let _ = &path;
        app.sync(&mut doc, &ROOT);
        assert!(app.store.is_empty());

        // Install the outcome the portal worker would deliver, then run one
        // real frame's sync — which drains it.
        app.import_rx = Some(portal::inject_import_for_test(ImportOutcome::Selected(
            vec![vcf],
        )));
        assert!(
            app.portal_in_flight(),
            "a request is in flight until drained"
        );
        app.sync(&mut doc, &ROOT);
        assert!(
            !app.portal_in_flight(),
            "draining the outcome clears the request"
        );
        assert_eq!(app.store.len(), 1, "the drained selection was imported");
        assert_eq!(app.store.contacts[0].first, "Drained");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_of_nothing_is_a_clean_noop() {
        // An empty selection (or a cancelled picker) fabricates nothing: the
        // book stays empty and disk is untouched — the button never silently
        // invents contacts.
        let (dir, path) = scratch_book();
        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        assert!(app.store.is_empty());

        // Empty selection.
        assert_eq!(app.import_paths(&mut doc, &[]), 0);
        // A cancelled picker drained through the real path.
        app.import_rx = Some(portal::inject_import_for_test(ImportOutcome::Cancelled));
        app.sync(&mut doc, &ROOT);

        assert!(app.store.is_empty(), "import of nothing adds nothing");
        assert!(
            ContactsDoc::list_contacts(&doc).is_empty(),
            "and writes nothing to the doc"
        );
        assert!(!path.exists(), "and never touches disk");
        let _ = &path;
        assert!(app.toast.is_none(), "a cancelled/empty import stays silent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[serial_test::serial]
    fn deleting_a_contact_persists_the_removal() {
        let home = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", home.path());
            std::env::remove_var("QOS_PACKAGE_ID");
        }
        // Seed a book into the store, then delete one contact and restart.
        let book = sample_contacts();
        let removed = book[0].id;
        let mut store = ui::persist::DocStore::open_for("dev.qos.contacts").unwrap();
        let mut doc = store.new_document();
        for c in &book {
            ContactsDoc::upsert_contact(&mut doc, c);
        }
        store.save(&mut doc).unwrap();

        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        let before = app.store.len();
        assert_eq!(before, book.len(), "the seeded book loaded");
        app.delete_selected(&mut doc, removed);
        store.save(&mut doc).unwrap();
        drop((store, doc, app));

        // Reload from the store: the deletion stuck.
        let mut store2 = ui::persist::DocStore::open_for("dev.qos.contacts").unwrap();
        let doc2 = store2.load().unwrap().expect("document after restart");
        let reloaded = ContactsDoc::list_contacts(&doc2);
        assert_eq!(
            reloaded.len(),
            before - 1,
            "the removal was written to the store"
        );
        assert!(!reloaded.iter().any(|c| c.id == removed));
    }

    #[test]
    fn resizing_wide_to_narrow_reflows_the_same_instance() {
        // Guards against the "responsive on first launch, frozen after resize"
        // bug: the breakpoint must be re-derived from the CURRENT width on
        // every layout pass, not cached from the first frame.
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        // Lay the SAME instance out wide, then narrow, then wide again.
        apply_at(&mut app, 1280, 800);
        assert!(!app.narrow, "wide viewport is the docked split");
        assert!(
            app.list_rect.x1 <= app.detail_rect.x0 + 0.001,
            "list sits left of detail"
        );

        apply_at(&mut app, 560, 720);
        assert!(
            app.narrow,
            "shrinking below the breakpoint reflows to single-pane push"
        );

        apply_at(&mut app, 1280, 800);
        assert!(
            !app.narrow,
            "growing back above the breakpoint restores the docked split"
        );
    }

    #[test]
    fn detail_name_row_fits_every_full_name_at_moderate_width() {
        // Regression: at 800x600 the detail heading truncated the name
        // ("Ada Lovela…") because the top-right action band stole its
        // horizontal room. The name now sits on its own full-width row, so
        // for every seeded contact the row must be wide enough to fit the
        // full display name at its rendered 26px Bold — i.e. no elision.
        let mut h = Harness::new(800, 600);
        let mut tr = TextRenderer::new();
        let ids: Vec<ContactId> = h.app.store.contacts.iter().map(|c| c.id).collect();
        assert!(!h.app.narrow, "800px is the docked side-by-side split");
        for id in ids {
            h.app.commit_select(id);
            h.relayout();
            let name = h.app.store.get(id).unwrap().display_name();
            let name_rect = h
                .app
                .detail_field_hits
                .iter()
                .find(|(s, _)| *s == Slot::First)
                .map(|(_, r)| *r)
                .expect("the detail header exposes a name row hit rect");
            let measured = f64::from(tr.measure_weighted(&name, 26.0, None, TextWeight::Bold).0);
            assert!(
                name_rect.width() >= measured,
                "name {name:?} needs {measured:.0}px but its row is only {:.0}px — it would truncate",
                name_rect.width(),
            );
        }
    }

    #[test]
    fn toast_auto_dismisses_after_a_real_action() {
        // After Save/Delete/Import/Export the confirmation toast used to stay
        // forever: it stored an Instant nothing ever read. Drive a real action
        // (delete) so the production `show_toast` fires, then prove the frame
        // loop keeps itself awake and clears the toast once its lifetime
        // elapses, and idles afterwards.
        let mut app = ContactsApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let id = app.selected.unwrap();

        app.delete_selected(&mut doc, id);
        assert!(app.toast.is_some(), "deleting a contact raises a toast");
        assert!(
            app.needs_animation_frame(),
            "the loop stays awake so the timed dismissal fires without further input",
        );

        // A frame drawn before the lifetime elapses keeps the toast.
        drive_frame(&mut app, 1280, 800);
        assert!(
            app.toast.is_some(),
            "a fresh toast survives an immediate frame"
        );

        // Back-date the toast past its lifetime and draw again → it clears,
        // and the loop is then free to idle.
        if let Some((_, shown)) = app.toast.as_mut() {
            *shown = Instant::now()
                .checked_sub(TOAST_LIFETIME)
                .unwrap()
                .checked_sub(Duration::from_millis(50))
                .unwrap();
        }
        assert!(
            app.toast_expired(),
            "the back-dated toast is past its lifetime"
        );
        drive_frame(&mut app, 1280, 800);
        assert!(
            app.toast.is_none(),
            "the toast auto-dismisses once its lifetime elapses"
        );
        assert!(
            !app.needs_animation_frame(),
            "with the toast gone the loop can settle back to idle",
        );
    }

    /// Lay a single app instance out at `w`x`h` without rebuilding it, to
    /// simulate a live window resize (the ui loop re-runs compute+apply layout
    /// on the existing widget when the allocation changes).
    fn apply_at(app: &mut ContactsApp, w: u32, h: u32) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = app.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(w as f32),
                        height: Dimension::length(h as f32),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        {
            let measure_map = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    Size {
                        width: AvailableSpace::Definite(w as f32),
                        height: AvailableSpace::Definite(h as f32),
                    },
                    |known, avail, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, avail, trr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        app.apply_layout(&taffy, child, Point::ORIGIN);
    }
}
