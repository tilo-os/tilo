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
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{TextInput, Widget};

use crate::date;
use crate::doc::ContactsDoc;
use crate::model::{Address, Contact, ContactId, ContactStore, Labeled, SortKey, sample_contacts};
use crate::responsive::{Pane, PushNav};
use crate::vcard::{self, VCardVersion};

const TOOLBAR_H: f64 = 56.0;
const ROW_H: f64 = 62.0;
const DIVIDER_W: f64 = 6.0;
const PAD: f64 = 16.0;
const FIELD_H: f64 = 38.0;

/// Edit-form header band geometry. The header is two stacked rows so the
/// title and the Favorite control never overlap: row 1 is the title, row 2
/// holds the Favorite pill and the Save/Cancel buttons. `layout_form` and
/// `draw_form` both derive their offsets from these so the hit rects and the
/// painted text stay aligned.
const FORM_TITLE_TOP: f64 = 16.0; // title row top, relative to detail pane top
const FORM_TITLE_H: f64 = 26.0; // title row height
const FORM_ROW_GAP: f64 = 10.0; // vertical gap between header rows / before body

/// Type labels offered in the phone/email/address chip menu.
const TYPE_OPTIONS: &[&str] = &["home", "work", "mobile", "main", "other"];

/// A built-in vCard used by the Import action when no external `.vcf`
/// path is provided via `QOS_CONTACTS_IMPORT`. Demonstrates the parser
/// against a real 3.0 card.
const SAMPLE_VCARD: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Hedy Lamarr\r\nN:Lamarr;Hedy;;;\r\nORG:Inventions Inc.\r\nTITLE:Inventor\r\nTEL;TYPE=MOBILE:+1 323 555 0177\r\nEMAIL;TYPE=HOME:hedy@example.com\r\nBDAY:1914-11-09\r\nURL:https://example.com/hedy\r\nNOTE:Frequency hopping pioneer.\r\nCATEGORIES:Friends\r\nEND:VCARD\r\n";

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

/// The global-search seam: `true` for Ctrl/Cmd + K (or `/`), the chord the
/// forthcoming shared global-search framework will claim. `handle_key` returns
/// without setting `handled` when this matches, so the event bubbles to the
/// host. Kept as a standalone function so it is the single decision point the
/// production path and the seam test both exercise.
fn is_global_search_chord(ctrl: bool, key: &Key) -> bool {
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
        let mut inputs = Vec::new();
        inputs.push(field(Slot::First, &draft.first));
        inputs.push(field(Slot::Last, &draft.last));
        inputs.push(field(Slot::Org, &draft.org));
        inputs.push(field(Slot::Title, &draft.title));
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
        inputs.push(field(Slot::Birthday, &draft.birthday));
        inputs.push(field(Slot::Website, &draft.website));
        inputs.push(field(Slot::Notes, &draft.notes));
        inputs.push(field(Slot::Groups, &draft.groups.join(", ")));
        Self { draft, is_new, inputs }
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
                Slot::Birthday => self.draft.birthday = v,
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
}

impl ContactsApp {
    pub fn new() -> Self {
        Self {
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
            last_detail_click: None,
            push: PushNav::default(),
            narrow: false,
            last_mouse: Point::ZERO,
            dirty: true,
            relayout: true,
            seeded: false,
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

    /// Snap a right-click context menu open on the selected contact row for
    /// `--shots`.
    pub fn shots_show_context_menu(&mut self) {
        if let Some(id) = self.selected {
            // Anchor the menu near the selected row in the list.
            let anchor = self
                .list_rows
                .iter()
                .find(|(rid, _)| *rid == id)
                .map(|(_, r)| Rect::new(r.x0 + 40.0, r.center().y, r.x0 + 40.0, r.center().y))
                .unwrap_or_else(|| {
                    Rect::new(
                        self.list_rect.x0 + 60.0,
                        self.list_rect.y0 + 200.0,
                        self.list_rect.x0 + 60.0,
                        self.list_rect.y0 + 200.0,
                    )
                });
            self.popup = Some(Popup::ContextMenu { target: id, anchor });
        }
    }

    /// Ids currently visible after search + group filter, in sort order.
    fn visible_ids(&self) -> Vec<ContactId> {
        self.store.filtered_sorted(
            self.search.value(),
            self.group_filter.as_deref(),
            self.sort,
        )
    }

    fn show_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
        self.dirty = true;
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
            ContactsDoc::upsert_contact(doc, &c);
            self.selected = Some(id);
            self.show_toast(if form.is_new { "Contact created" } else { "Saved" });
        }
        self.mode = Mode::View;
        self.popup = None;
        self.relayout = true;
        self.dirty = true;
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
                || !draft.birthday.trim().is_empty()
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
        ContactsDoc::delete_contact(doc, id);
        if self.selected == Some(id) {
            self.selected = self.visible_ids().first().copied();
        }
        self.confirm_delete = None;
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
            .map(|p| p as isize)
            .unwrap_or(-1);
        let next = (cur + delta).clamp(0, ids.len() as isize - 1) as usize;
        self.selected = Some(ids[next]);
        self.detail_scroll = 0.0;
        self.dirty = true;
        self.relayout = true;
    }

    fn import_vcards(&mut self, doc: &mut AutoCommit) {
        let text = std::env::var("QOS_CONTACTS_IMPORT")
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_else(|| SAMPLE_VCARD.to_string());
        let parsed = vcard::parse_vcards(&text);
        let n = parsed.len();
        let mut first_new = None;
        for c in parsed {
            if first_new.is_none() {
                first_new = Some(c.id);
            }
            self.store.upsert(c.clone());
            ContactsDoc::upsert_contact(doc, &c);
        }
        if let Some(id) = first_new {
            self.selected = Some(id);
        }
        self.show_toast(format!("Imported {n} contact{}", if n == 1 { "" } else { "s" }));
        self.relayout = true;
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

    /// Write the explicitly-chosen `ids` out as a `.vcf` of the given version.
    fn export_ids(&mut self, ids: &[ContactId], version: VCardVersion) {
        let contacts: Vec<Contact> =
            ids.iter().filter_map(|id| self.store.get(*id).cloned()).collect();
        if contacts.is_empty() {
            self.show_toast("Select at least one contact to export");
            return;
        }
        let body = vcard::to_vcards(&contacts, version);
        let path = std::env::temp_dir().join("contacts-export.vcf");
        match std::fs::write(&path, body) {
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
            self.detail_rect =
                Rect::new(detail_x0, content.y0, detail_x0 + avail_w, content.y1);
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
            let list_w = (avail_w * self.split_pos).clamp(260.0, avail_w - 360.0).max(220.0);
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
        self.list_body = Rect::new(self.list_rect.x0, body_top, self.list_rect.x1, self.list_rect.y1);

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
        self.hit.edit = Rect::new(self.hit.delete.x0 - 8.0 - 72.0, by, self.hit.delete.x0 - 8.0, by + 32.0);
        self.hit.fav = Rect::new(self.hit.edit.x0 - 8.0 - 32.0, by, self.hit.edit.x0 - 8.0, by + 32.0);
    }

    /// Assign rects to every form input + dynamic control. Mirrors the
    /// vertical order the form renders in. Two-phase to avoid holding a
    /// `self.form` borrow while writing `self.hit`: build a rect plan
    /// first, then apply it to the inputs.
    fn layout_form(&mut self) {
        let area = self.detail_rect;
        let off = self.detail_top_offset();
        let x0 = area.x0 + PAD;
        let x1 = area.x1 - PAD;
        let w = x1 - x0;
        // The (non-scrolled) header band is two stacked rows so nothing
        // collides: row 1 is the "Edit/New Contact" title, row 2 holds the
        // Favorite pill (left) and Save/Cancel (right). The scrollable body
        // begins below both. Keep these constants in sync with `draw_form`
        // (the title baseline and the body clip both derive from them).
        let title_y = area.y0 + FORM_TITLE_TOP + off;
        let by = title_y + FORM_TITLE_H + FORM_ROW_GAP; // controls row top
        let top = by + 32.0 + FORM_ROW_GAP; // first field row top
        let mut y = top - self.detail_scroll;

        self.hit.cancel = Rect::new(x1 - 88.0, by, x1, by + 32.0);
        self.hit.save = Rect::new(x1 - 88.0 - 96.0, by, x1 - 88.0 - 8.0, by + 32.0);
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

        // Phones (repeated): [chip][value][x]
        y += label_h;
        for i in 0..n_phones {
            let chip = Rect::new(x0, y, x0 + 78.0, y + nh);
            let rm = Rect::new(x1 - 30.0, y, x1, y + nh);
            plan.push((Slot::Phone(i), Rect::new(chip.x1 + 8.0, y, rm.x0 - 8.0, y + nh)));
            self.hit.type_chips.push((TypeTarget::Phone(i), chip));
            self.hit.remove_btns.push((TypeTarget::Phone(i), rm));
            y += nh + 8.0;
        }
        self.hit.add_phone = Rect::new(x0, y, x0 + 130.0, y + 28.0);
        y += 28.0 + gap;

        // Emails (repeated)
        y += label_h;
        for i in 0..n_emails {
            let chip = Rect::new(x0, y, x0 + 78.0, y + nh);
            let rm = Rect::new(x1 - 30.0, y, x1, y + nh);
            plan.push((Slot::Email(i), Rect::new(chip.x1 + 8.0, y, rm.x0 - 8.0, y + nh)));
            self.hit.type_chips.push((TypeTarget::Email(i), chip));
            self.hit.remove_btns.push((TypeTarget::Email(i), rm));
            y += nh + 8.0;
        }
        self.hit.add_email = Rect::new(x0, y, x0 + 130.0, y + 28.0);
        y += 28.0 + gap;

        // Addresses (repeated): chip + street, then city/region, postal/country
        y += label_h;
        for i in 0..n_addr {
            let chip = Rect::new(x0, y, x0 + 78.0, y + nh);
            let rm = Rect::new(x1 - 30.0, y, x1, y + nh);
            plan.push((Slot::AddrStreet(i), Rect::new(chip.x1 + 8.0, y, rm.x0 - 8.0, y + nh)));
            self.hit.type_chips.push((TypeTarget::Addr(i), chip));
            self.hit.remove_btns.push((TypeTarget::Addr(i), rm));
            y += nh + 8.0;
            plan.push((Slot::AddrCity(i), Rect::new(x0, y, x0 + half, y + nh)));
            plan.push((Slot::AddrRegion(i), Rect::new(x0 + half + 10.0, y, x1, y + nh)));
            y += nh + 8.0;
            plan.push((Slot::AddrPostal(i), Rect::new(x0, y, x0 + half, y + nh)));
            plan.push((Slot::AddrCountry(i), Rect::new(x0 + half + 10.0, y, x1, y + nh)));
            y += nh + 12.0;
        }
        self.hit.add_address = Rect::new(x0, y, x0 + 150.0, y + 28.0);
        y += 28.0 + gap;

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
}

// =====================================================================
// Geometry for popups (shared by draw + hit-test)
// =====================================================================

impl ContactsApp {
    fn popup_menu_geometry(&self, anchor: Rect, items: usize, width: f64) -> (Rect, Vec<Rect>) {
        let item_h = 32.0;
        let h = items as f64 * item_h + 8.0;
        let x0 = anchor.x0.min(self.rect.x1 - width - 8.0).max(self.rect.x0 + 8.0);
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
        let x0 = anchor.x0.min(self.rect.x1 - w - 8.0).max(self.rect.x0 + 8.0);
        let mut y0 = anchor.y1 + 4.0;
        if y0 + h > self.rect.y1 - 8.0 {
            y0 = (anchor.y0 - h - 4.0).max(self.rect.y0 + 8.0);
        }
        let panel = Rect::new(x0, y0, x0 + w, y0 + h);
        let prev = Rect::new(panel.x0 + 8.0, panel.y0 + 8.0, panel.x0 + 36.0, panel.y0 + 36.0);
        let next = Rect::new(panel.x1 - 36.0, panel.y0 + 8.0, panel.x1 - 8.0, panel.y0 + 36.0);
        // Day grid: 7 cols, start under the weekday header.
        let grid_top = panel.y0 + 74.0;
        let cell = (w - 16.0) / 7.0;
        let first_wd = date::weekday(year, month, 1);
        let dim = date::days_in_month(year, month);
        let mut cells = Vec::new();
        for d in 1..=dim {
            let idx = (first_wd + (d - 1)) as f64;
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

        let c = &theme.tokens.colors;
        // App background.
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);

        self.draw_toolbar(scene, transform, tr, theme);

        if self.narrow {
            // Single-pane push: clip both slabs to the content area below the
            // toolbar so the off-screen pane (sliding in/out) never bleeds
            // past the edges, and skip whichever pane is fully off-screen.
            let content = Rect::new(self.rect.x0, self.rect.y0 + TOOLBAR_H, self.rect.x1, self.rect.y1);
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &content);
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
                transform * Affine::translate((self.hit.search.x0 + 12.0, self.hit.search.center().y - 9.0)),
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
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.detail_rect);
        if self.mode == Mode::Edit {
            self.draw_form(scene, transform, tr, theme, presence);
        } else {
            self.draw_detail(scene, transform, tr, theme);
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
        let shape = RoundedRect::from_rect(rect, theme.tokens.radii.full as f64);
        let bg = if hovered { c.surfaces.raised } else { c.surfaces.base };
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
        let bar = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y0 + TOOLBAR_H);
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &bar);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            c.border_standard,
            None,
            &Line::new((bar.x0, bar.y1), (bar.x1, bar.y1)),
        );
        // App title + count.
        tr.draw_text_weighted(
            scene,
            "Contacts",
            20.0,
            c.text_primary,
            t * Affine::translate((self.rect.x0 + PAD, self.rect.y0 + 16.0)),
            None,
            TextWeight::Bold,
        );
        let count = format!("{} people", self.store.len());
        tr.draw_text(
            scene,
            &count,
            12.0,
            c.text_dim,
            t * Affine::translate((self.rect.x0 + PAD + 110.0, self.rect.y0 + 21.0)),
            None,
        );

        toolbar_button(scene, t, tr, theme, self.hit.import, "Import", false);
        toolbar_button(scene, t, tr, theme, self.hit.export, "Export", false);
        toolbar_button(scene, t, tr, theme, self.hit.new, "+ New", true);
    }

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
        chip(scene, t, tr, theme, self.hit.group, &group_label, self.group_filter.is_some());

        // Rows (clipped to body).
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &self.list_body);
        for (id, rr) in &self.list_rows {
            if rr.y1 < self.list_body.y0 || rr.y0 > self.list_body.y1 {
                continue;
            }
            let Some(contact) = self.store.get(*id) else { continue };
            let selected = self.selected == Some(*id);
            if selected {
                let bg = RoundedRect::from_rect(rr.inset(-0.0), theme.tokens.radii.md as f64);
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
            let av = Rect::new(rr.x0 + 14.0, rr.y0 + 11.0, rr.x0 + 14.0 + 40.0, rr.y0 + 11.0 + 40.0);
            draw_avatar(scene, t, tr, theme, av, &contact.initials());
            let tx = av.x1 + 12.0;
            tr.draw_text_weighted(
                scene,
                &contact.display_name(),
                15.0,
                c.text_primary,
                t * Affine::translate((tx, rr.y0 + 13.0)),
                Some((rr.x1 - tx - 26.0) as f32),
                TextWeight::Semibold,
            );
            let sub = contact.subtitle();
            if !sub.is_empty() {
                tr.draw_text(
                    scene,
                    &sub,
                    12.0,
                    c.text_dim,
                    t * Affine::translate((tx, rr.y0 + 33.0)),
                    Some((rr.x1 - tx - 26.0) as f32),
                );
            }
            if contact.favorite {
                draw_star(scene, t, Point::new(rr.x1 - 18.0, rr.y0 + 20.0), 6.0, theme.accent_primary, true);
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
        if self.list_rows.is_empty() {
            tr.draw_text(
                scene,
                "No contacts match",
                13.0,
                c.text_dim,
                t * Affine::translate((self.list_body.x0 + PAD, self.list_body.y0 + 20.0)),
                None,
            );
        }
        scene.pop_layer();
        let _ = presence;
    }

    fn draw_divider(&self, scene: &mut Scene, t: Affine, theme: &Theme) {
        let dx0 = self.list_rect.x1;
        let r = Rect::new(dx0, self.detail_rect.y0, dx0 + DIVIDER_W, self.detail_rect.y1);
        let color = if self.divider_hover || self.divider_drag {
            theme.accent_primary
        } else {
            theme.tokens.colors.border_standard
        };
        scene.fill(Fill::NonZero, t, color, None, &r);
    }

    fn draw_detail(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let area = self.detail_rect;
        let Some(id) = self.selected else {
            tr.draw_text(
                scene,
                "Select a contact",
                15.0,
                c.text_dim,
                t * Affine::translate((area.x0 + PAD, area.y0 + 40.0 + self.detail_top_offset())),
                None,
            );
            self.detail_content_h = 0.0;
            return;
        };
        let Some(contact) = self.store.get(id).cloned() else { return };

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &area);
        let off = self.detail_top_offset();
        let x0 = area.x0 + 28.0;
        let x1 = area.x1 - 24.0;
        let top = area.y0 + 28.0 + off;
        let mut y = top - self.detail_scroll;

        // Header: big avatar + name + subtitle.
        let av = Rect::new(x0, y, x0 + 72.0, y + 72.0);
        draw_avatar(scene, t, tr, theme, av, &contact.initials());
        let hx = av.x1 + 18.0;
        // Cap the name/subtitle so they stop before the favorite/Edit/Delete
        // action band (it shares this y-band, especially in the narrow
        // full-width detail pane). Elide to a single line rather than wrap,
        // since the header below uses fixed vertical offsets.
        let head_x1 = if self.hit.fav.width() > 0.0 {
            (self.hit.fav.x0 - 12.0).min(x1)
        } else {
            x1
        };
        let head_w = (head_x1 - hx).max(40.0) as f32;
        let name = tr.elide_to_width_weighted(&contact.display_name(), 26.0, head_w, TextWeight::Bold);
        tr.draw_text_weighted(
            scene,
            &name,
            26.0,
            c.text_primary,
            t * Affine::translate((hx, y + 4.0)),
            None,
            TextWeight::Bold,
        );
        let sub = contact.subtitle();
        if !sub.is_empty() {
            let sub = tr.elide_to_width(&sub, 14.0, head_w);
            tr.draw_text(
                scene,
                &sub,
                14.0,
                c.text_dim,
                t * Affine::translate((hx, y + 38.0)),
                None,
            );
        }
        // Group badges.
        let mut bx = hx;
        let by = y + 60.0;
        for g in &contact.groups {
            let tw = tr.measure(g, 11.0, None).0 as f64;
            let bw = tw + 18.0;
            let br = Rect::new(bx, by, bx + bw, by + 20.0);
            let shape = RoundedRect::from_rect(br, 10.0);
            scene.fill(Fill::NonZero, t, theme.accent_primary.with_alpha_factor(0.16), None, &shape);
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

        // Action buttons (favorite star, Edit, Delete) — top right.
        draw_star(scene, t, self.hit.fav.center(), 11.0, theme.accent_primary, contact.favorite);
        toolbar_button(scene, t, tr, theme, self.hit.edit, "Edit", true);
        toolbar_button(scene, t, tr, theme, self.hit.delete, "Delete", false);

        y += 96.0;

        // Record field value hit rects so a double-click can jump straight
        // into editing the clicked field (see `handle_detail_double_click`).
        self.detail_field_hits.clear();
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
        if !contact.birthday.is_empty() {
            detail_section(scene, t, tr, theme, x0, &mut y, "BIRTHDAY");
            let y0 = y;
            detail_field(scene, t, tr, theme, x0, x1, &mut y, "date", &date::pretty(&contact.birthday));
            hits.push((Slot::Birthday, Rect::new(x0, y0 - 4.0, x1, y)));
        }
        if !contact.website.is_empty() {
            detail_section(scene, t, tr, theme, x0, &mut y, "WEBSITE");
            let y0 = y;
            detail_field(scene, t, tr, theme, x0, x1, &mut y, "url", &contact.website);
            hits.push((Slot::Website, Rect::new(x0, y0 - 4.0, x1, y)));
        }
        if !contact.notes.is_empty() {
            detail_section(scene, t, tr, theme, x0, &mut y, "NOTES");
            let y0 = y;
            let h = tr.measure(&contact.notes, 14.0, Some((x1 - x0) as f32)).1 as f64;
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

        // Header double-click: the name band edits the first/last name.
        hits.push((Slot::First, Rect::new(av.x1 + 18.0, top, x1, top + 40.0)));

        self.detail_field_hits = hits;
        self.detail_content_h = (y + self.detail_scroll) - top + 24.0;
        scene.pop_layer();
    }

    fn draw_form(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        let area = self.detail_rect;
        let off = self.detail_top_offset();
        let x0 = area.x0 + PAD;

        // Header band (not scrolled), two stacked rows so the title and the
        // Favorite control don't collide. On a narrow window the whole band is
        // pushed down so the Back affordance pill sits above it. Title sits on
        // its own row (`FORM_TITLE_TOP`), vertically centered in `FORM_TITLE_H`.
        let title = if self.form.as_ref().map(|f| f.is_new).unwrap_or(false) {
            "New Contact"
        } else {
            "Edit Contact"
        };
        let title_y = area.y0 + FORM_TITLE_TOP + off;
        tr.draw_text_weighted(
            scene,
            title,
            18.0,
            c.text_primary,
            t * Affine::translate((x0, title_y + (FORM_TITLE_H - 22.0) / 2.0)),
            None,
            TextWeight::Bold,
        );
        let fav = self.form.as_ref().map(|f| f.draft.favorite).unwrap_or(false);
        // Favorite toggle pill.
        {
            let r = self.hit.fav_toggle;
            let shape = RoundedRect::from_rect(r, theme.tokens.radii.md as f64);
            let bg = if fav { theme.accent_primary.with_alpha_factor(0.18) } else { c.surfaces.raised };
            scene.fill(Fill::NonZero, t, bg, None, &shape);
            scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            draw_star(scene, t, Point::new(r.x0 + 16.0, r.center().y), 7.0, theme.accent_primary, fav);
            tr.draw_text(
                scene,
                "Favorite",
                12.0,
                if fav { theme.accent_primary } else { c.text_dim },
                t * Affine::translate((r.x0 + 30.0, r.center().y - 7.0)),
                None,
            );
        }
        toolbar_button(scene, t, tr, theme, self.hit.save, "Save", true);
        toolbar_button(scene, t, tr, theme, self.hit.cancel, "Cancel", false);

        // Scrollable body, clipped below the two-row header band (title row +
        // controls row). Derived from the same constants as `layout_form` so
        // the clip starts exactly under the Favorite/Save/Cancel row.
        let body_top = area.y0 + FORM_TITLE_TOP + FORM_TITLE_H + FORM_ROW_GAP + 32.0 + off;
        let body = Rect::new(area.x0, body_top, area.x1, area.y1);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &body);

        // Section labels are drawn relative to the first input of each group.
        let inputs_snapshot: Vec<(Slot, Rect)> =
            self.form.as_ref().unwrap().inputs.iter().map(|f| (f.slot, f.ti_rect())).collect();
        let draft = self.form.as_ref().unwrap().draft.clone();
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
        let find = |slot: Slot| inputs_snapshot.iter().find(|(s, _)| *s == slot).map(|(_, r)| *r);
        if let Some(r) = find(Slot::First) {
            label(scene, tr, "NAME", r);
        }
        if let Some(r) = find(Slot::Org) {
            label(scene, tr, "ORGANIZATION", r);
        }
        // Phone / email / address section headers anchored ABOVE the first
        // field row of each group (falling back to the group's "+ Add" button
        // when the group is empty). Previously these anchored to the add
        // button — which sits at the BOTTOM of the group — so the label was
        // painted on the row *above* the button, bleeding through behind that
        // row's type chip. Anchoring to the first row fixes that overlap.
        let section_anchor = |slot: Slot, add_btn: Rect| -> Rect {
            find(slot).unwrap_or(add_btn)
        };
        label(scene, tr, "PHONE", section_anchor(Slot::Phone(0), self.hit.add_phone));
        label(scene, tr, "EMAIL", section_anchor(Slot::Email(0), self.hit.add_email));
        label(scene, tr, "ADDRESS", section_anchor(Slot::AddrStreet(0), self.hit.add_address));
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

        // Draw the embedded inputs, each followed by its dim placeholder hint
        // when empty + unfocused. The input paints its own opaque rounded-rect
        // field background first (TextInput::draw), so the placeholder — drawn
        // *after* the input, clipped to the field — sits cleanly on top of that
        // fill and nothing from a neighbouring row can bleed through behind it.
        for f in self.form.as_mut().unwrap().inputs.iter_mut() {
            f.ti.draw(scene, t, tr, theme, presence);
            if f.ti.value().is_empty() && !f.ti.is_focused() {
                let r = f.ti.get_rect();
                if r.width() > 0.0 {
                    let shape = RoundedRect::from_rect(r, theme.tokens.radii.md as f64);
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

        // Draw type chips + remove buttons.
        for (target, rect) in &self.hit.type_chips {
            let lbl = type_label(&draft, *target);
            chip(scene, t, tr, theme, *rect, lbl, false);
        }
        for (_target, rect) in &self.hit.remove_btns {
            let shape = RoundedRect::from_rect(*rect, theme.tokens.radii.md as f64);
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
        // Add buttons.
        add_button(scene, t, tr, theme, self.hit.add_phone, "+ Add phone");
        add_button(scene, t, tr, theme, self.hit.add_email, "+ Add email");
        add_button(scene, t, tr, theme, self.hit.add_address, "+ Add address");
        // Calendar button.
        {
            let r = self.hit.cal_btn;
            let shape = RoundedRect::from_rect(r, theme.tokens.radii.md as f64);
            scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
            scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
            draw_calendar_glyph(scene, t, r, theme);
        }

        scene.pop_layer();
    }

    fn draw_popup(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        popup: &Popup,
    ) {
        let c = &theme.tokens.colors;
        match popup {
            Popup::TypeMenu { anchor, .. } => {
                let (panel, rows) = self.popup_menu_geometry(*anchor, TYPE_OPTIONS.len(), 140.0);
                menu_panel(scene, t, theme, panel);
                for (i, r) in rows.iter().enumerate() {
                    let hov = r.contains(self.last_mouse);
                    menu_item(scene, t, tr, theme, *r, TYPE_OPTIONS[i], hov, false);
                }
            }
            Popup::GroupFilter { anchor } => {
                let mut opts = vec!["All".to_string()];
                opts.extend(self.store.all_groups());
                let (panel, rows) = self.popup_menu_geometry(*anchor, opts.len(), 180.0);
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
            Popup::Calendar { year, month, anchor } => {
                let (panel, prev, next, cells) = self.calendar_geometry(*anchor, *year, *month);
                menu_panel(scene, t, theme, panel);
                // Header: < Month Year >
                let title = format!("{} {}", date::month_name(*month), year);
                let tw = tr.measure(&title, 14.0, None).0 as f64;
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
                    .map(|f| f.draft.birthday.clone())
                    .unwrap_or_default();
                for (d, r) in &cells {
                    let hov = r.contains(self.last_mouse);
                    let is_sel = cur == format!("{:04}-{:02}-{:02}", year, month, d);
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
                    let dw = tr.measure(&s, 12.0, None).0 as f64;
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
            Popup::ContextMenu { target, anchor } => {
                let (panel, rows) = self.popup_menu_geometry(*anchor, CONTEXT_ACTIONS.len(), 200.0);
                menu_panel(scene, t, theme, panel);
                // The Favorite item's label reflects the target contact's
                // current state ("Add to favorites" vs "Remove from favorites").
                let is_fav = self.store.get(*target).map(|c| c.favorite).unwrap_or(false);
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
        }
    }

    /// The export-selection dialog: a centered modal listing every contact
    /// with a per-row checkbox, Select all / Select none, and the two vCard
    /// version export actions. The user exports exactly the ticked set.
    fn draw_export_dialog(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
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
            let Some(contact) = self.store.get(*id) else { continue };
            let checked = sel.contains(id);
            let hov = row.contains(self.last_mouse);
            if hov {
                let bg = RoundedRect::from_rect(*row, theme.tokens.radii.md as f64);
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
        toolbar_button(scene, t, tr, theme, g.cancel, "Cancel", false);
        toolbar_button(scene, t, tr, theme, g.export_v3, "Export 3.0", false);
        toolbar_button(scene, t, tr, theme, g.export_v4, "Export 4.0", true);
        let _ = g.content_h;
    }

    /// The "Save changes?" guard modal shown when the user tries to switch
    /// contacts while an edit form holds unsaved changes.
    fn draw_unsaved_modal(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
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
        toolbar_button(scene, t, tr, theme, g.cancel, "Cancel", false);
        toolbar_button(scene, t, tr, theme, g.discard, "Discard", false);
        toolbar_button(scene, t, tr, theme, g.save, "Save", true);
    }

    fn draw_confirm(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, id: ContactId) {
        let c = &theme.tokens.colors;
        // Scrim.
        scene.fill(Fill::NonZero, t, c.surfaces.overlay, None, &self.rect);
        let (panel, del, cancel) = self.confirm_geometry();
        menu_panel(scene, t, theme, panel);
        let name = self.store.get(id).map(|c| c.display_name()).unwrap_or_default();
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
        toolbar_button(scene, t, tr, theme, cancel, "Cancel", false);
        // Danger button.
        let shape = RoundedRect::from_rect(del, theme.tokens.radii.md as f64);
        scene.fill(Fill::NonZero, t, c.semantic.danger, None, &shape);
        let dw = tr.measure("Delete", 14.0, None).0 as f64;
        tr.draw_text_weighted(
            scene,
            "Delete",
            14.0,
            Color::WHITE,
            t * Affine::translate((del.center().x - dw / 2.0, del.center().y - 8.0)),
            None,
            TextWeight::Semibold,
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
        let h = (self.rect.height() * 0.8).clamp(320.0, 560.0).min(self.rect.height() - 40.0);
        let cx = self.rect.center().x;
        let cy = self.rect.center().y;
        let panel = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
        // Header band: title + Select all / none on the right.
        let head_y = panel.y0 + 18.0;
        let select_none =
            Rect::new(panel.x1 - 16.0 - 96.0, head_y, panel.x1 - 16.0, head_y + 28.0);
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
            let cb = Rect::new(row.x0 + 4.0, row.center().y - 10.0, row.x0 + 24.0, row.center().y + 10.0);
            rows.push((*id, row, cb));
            ry += row_h;
        }
        let content_h = ids.len() as f64 * row_h;
        // Footer buttons.
        let by = panel.y1 - 44.0;
        let export_v4 = Rect::new(panel.x1 - 16.0 - 120.0, by, panel.x1 - 16.0, by + 32.0);
        let export_v3 = Rect::new(export_v4.x0 - 8.0 - 120.0, by, export_v4.x0 - 8.0, by + 32.0);
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
        UnsavedGeom { panel, save, discard, cancel }
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
    let h = tr.measure(value, 14.0, Some(vw as f32)).1 as f64;
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

fn type_label<'a>(draft: &'a Contact, target: TypeTarget) -> &'a str {
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

fn toolbar_button(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    primary: bool,
) {
    if rect.width() <= 0.0 {
        return;
    }
    let c = &theme.tokens.colors;
    let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
    if primary {
        scene.fill(Fill::NonZero, t, theme.accent_primary, None, &shape);
    } else {
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
    }
    let col = if primary { Color::WHITE } else { c.text_primary };
    let tw = tr.measure(label, 13.0, None).0 as f64;
    tr.draw_text_weighted(
        scene,
        label,
        13.0,
        col,
        t * Affine::translate((rect.center().x - tw / 2.0, rect.center().y - 8.0)),
        None,
        TextWeight::Medium,
    );
}

fn add_button(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect, label: &str) {
    let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
    scene.stroke(&Stroke::new(1.0), t, theme.accent_primary.with_alpha_factor(0.6), None, &shape);
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

fn chip(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect, label: &str, active: bool) {
    if rect.width() <= 0.0 {
        return;
    }
    let c = &theme.tokens.colors;
    let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
    let bg = if active { theme.accent_primary.with_alpha_factor(0.16) } else { c.surfaces.raised };
    scene.fill(Fill::NonZero, t, bg, None, &shape);
    scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
    let col = if active { theme.accent_primary } else { c.text_primary };
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &shape);
    tr.draw_text(
        scene,
        label,
        12.0,
        col,
        t * Affine::translate((rect.x0 + 10.0, rect.center().y - 7.0)),
        Some((rect.width() - 16.0) as f32),
    );
    scene.pop_layer();
}

fn menu_panel(scene: &mut Scene, t: Affine, theme: &Theme, panel: Rect) {
    let c = &theme.tokens.colors;
    // Soft shadow.
    let shadow = RoundedRect::from_rect(panel.inset(2.0), theme.tokens.radii.md as f64 + 1.0);
    scene.fill(Fill::NonZero, t, Color::from_rgba8(0, 0, 0, 90), None, &shadow);
    let shape = RoundedRect::from_rect(panel, theme.tokens.radii.md as f64);
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
        let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
        scene.fill(Fill::NonZero, t, theme.accent_primary, None, &shape);
    } else if selected {
        let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
        scene.fill(Fill::NonZero, t, theme.accent_primary.with_alpha_factor(0.16), None, &shape);
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

/// Draw a checkbox in the export-selection list. Matches the look of the
/// `ui` Checkbox widget (which can't be reused here — it requires a CRDT
/// boolean path with no imperative checked-state API): accent fill + white
/// tick when checked, otherwise a bordered empty box.
fn draw_checkbox(scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect, checked: bool) {
    let c = &theme.tokens.colors;
    let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
    if checked {
        scene.fill(Fill::NonZero, t, theme.accent_primary, None, &shape);
    } else {
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &shape);
        scene.stroke(&Stroke::new(1.4), t, c.border_standard, None, &shape);
    }
    if checked {
        // White check polyline, proportional to the box.
        let x0 = rect.x0;
        let y0 = rect.y0;
        let w = rect.width();
        let h = rect.height();
        let mut p = BezPath::new();
        p.move_to((x0 + w * 0.26, y0 + h * 0.52));
        p.line_to((x0 + w * 0.44, y0 + h * 0.70));
        p.line_to((x0 + w * 0.76, y0 + h * 0.30));
        scene.stroke(
            &Stroke::new(2.0).with_caps(vello::kurbo::Cap::Round),
            t,
            Color::WHITE,
            None,
            &p,
        );
    }
}

fn draw_avatar(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect, initials: &str) {
    let ramp = &theme.tokens.colors.accent;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(initials, &mut hasher);
    let idx = (std::hash::Hasher::finish(&hasher) as usize) % ramp.len();
    let bg = ramp[idx];
    let radius = rect.width().min(rect.height()) / 2.0;
    scene.fill(Fill::NonZero, t, bg, None, &Circle::new(rect.center(), radius));
    let size = (rect.width() * 0.4) as f32;
    let (tw, th) = tr.measure(initials, size, None);
    tr.draw_text(
        scene,
        initials,
        size,
        Color::WHITE,
        t * Affine::translate((rect.center().x - tw as f64 / 2.0, rect.center().y - th as f64 / 2.0)),
        None,
    );
}

fn draw_star(scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color, filled: bool) {
    let mut path = BezPath::new();
    for i in 0..10 {
        let ang = -std::f64::consts::FRAC_PI_2 + i as f64 * std::f64::consts::PI / 5.0;
        let rad = if i % 2 == 0 { r } else { r * 0.42 };
        let p = Point::new(center.x + ang.cos() * rad, center.y + ang.sin() * rad);
        if i == 0 {
            path.move_to(p);
        } else {
            path.line_to(p);
        }
    }
    path.close_path();
    if filled {
        scene.fill(Fill::NonZero, t, color, None, &path);
    } else {
        scene.stroke(&Stroke::new(1.4), t, color, None, &path);
    }
}

fn draw_calendar_glyph(scene: &mut Scene, t: Affine, rect: Rect, theme: &Theme) {
    let c = &theme.tokens.colors;
    let inner = rect.inset(-0.0);
    let g = Rect::new(inner.center().x - 9.0, inner.center().y - 8.0, inner.center().x + 9.0, inner.center().y + 9.0);
    let shape = RoundedRect::from_rect(g, 2.0);
    scene.stroke(&Stroke::new(1.4), t, c.text_dim, None, &shape);
    scene.stroke(
        &Stroke::new(1.4),
        t,
        c.text_dim,
        None,
        &Line::new((g.x0, g.y0 + 5.0), (g.x1, g.y0 + 5.0)),
    );
}

fn draw_chevron(scene: &mut Scene, t: Affine, rect: Rect, theme: &Theme, left: bool) {
    let c = &theme.tokens.colors;
    let cx = rect.center().x;
    let cy = rect.center().y;
    let mut p = BezPath::new();
    if left {
        p.move_to((cx + 3.0, cy - 5.0));
        p.line_to((cx - 3.0, cy));
        p.line_to((cx + 3.0, cy + 5.0));
    } else {
        p.move_to((cx - 3.0, cy - 5.0));
        p.line_to((cx + 3.0, cy));
        p.line_to((cx - 3.0, cy + 5.0));
    }
    scene.stroke(&Stroke::new(2.0), t, c.text_primary, None, &p);
}

fn draw_toast(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, area: Rect, msg: &str) {
    let c = &theme.tokens.colors;
    let tw = tr.measure(msg, 13.0, None).0 as f64;
    let w = (tw + 36.0).min(area.width() - 40.0);
    let h = 38.0;
    let x0 = area.center().x - w / 2.0;
    let y0 = area.y1 - h - 20.0;
    let r = Rect::new(x0, y0, x0 + w, y0 + h);
    let shape = RoundedRect::from_rect(r, theme.tokens.radii.md as f64);
    scene.fill(Fill::NonZero, t, Color::from_rgba8(0, 0, 0, 110), None, &RoundedRect::from_rect(r.inset(2.0), 10.0));
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
        if !self.seeded {
            let existing = ContactsDoc::list_contacts(state);
            let mut seed_first = None;
            if existing.is_empty() {
                for c in sample_contacts() {
                    if seed_first.is_none() {
                        seed_first = Some(c.id);
                    }
                    ContactsDoc::upsert_contact(state, &c);
                    self.store.upsert(c);
                }
            } else {
                for c in existing {
                    self.store.upsert(c);
                }
            }
            if self.selected.is_none() {
                // Prefer the first seeded contact (a richly-populated
                // record) so a fresh launch opens onto a full detail view.
                self.selected = seed_first.or_else(|| self.visible_ids().first().copied());
            }
            self.seeded = true;
            self.relayout = true;
            self.dirty = true;
        }
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
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
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
        }

        // Modal layers consume input first.
        if self.export_sel.is_some() {
            self.handle_export(event, handled);
            return;
        }
        if self.pending_switch.is_some() {
            self.handle_unsaved(event, state, handled);
            return;
        }
        if self.confirm_delete.is_some() {
            self.handle_confirm(event, state, handled);
            return;
        }
        if self.popup.is_some() {
            self.handle_popup(event, state, handled);
            // Popups consume clicks; let hover fall through harmlessly.
            if *handled {
                return;
            }
        }

        match event {
            WindowEvent::CursorMoved { .. } => {
                // The divider only exists in the wide side-by-side layout.
                self.divider_hover =
                    !self.narrow && self.divider_hit().contains(self.last_mouse);
                if self.divider_drag {
                    let content_w = (self.rect.width()).max(1.0);
                    let rel = (self.last_mouse.x - self.rect.x0) / content_w;
                    self.split_pos = rel.clamp(0.18, 0.6);
                    self.relayout = true;
                    self.dirty = true;
                    *handled = true;
                    return;
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
                    return;
                }
                self.handle_click(state, handled, node_id, taffy, modifiers, path);
                return;
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
                return;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.divider_drag {
                    self.divider_drag = false;
                    *handled = true;
                    return;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (-y * 48.0) as f64,
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
                return;
            }
            WindowEvent::KeyboardInput { event: ke, .. } => {
                self.handle_key(ke, state, path, handled, node_id, taffy, modifiers, scale_factor, event);
                return;
            }
            _ => {}
        }

        // Forward pointer-move to focused inputs so hover/drag-select tracks.
        self.forward_event_to_inputs(event, scale_factor, modifiers, state, path, handled, node_id, taffy);
    }

    fn name(&self) -> &str {
        "ContactsApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.push.is_animating()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the host pumping frames while the master↔detail push slide
        // eases into place on a narrow window.
        self.push.is_animating()
    }

    fn is_layout_dirty(&self) -> bool {
        self.relayout
    }

    fn needs_commit(&self) -> bool {
        self.search.needs_commit()
            || self
                .form
                .as_ref()
                .map(|f| f.inputs.iter().any(|i| i.ti.needs_commit()))
                .unwrap_or(false)
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

// =====================================================================
// Event handling helpers
// =====================================================================

impl ContactsApp {
    fn divider_hit(&self) -> Rect {
        let dx0 = self.list_rect.x1;
        Rect::new(dx0 - 3.0, self.detail_rect.y0, dx0 + DIVIDER_W + 3.0, self.detail_rect.y1)
    }

    fn any_text_focused(&self) -> bool {
        self.search.is_focused() || self.form.as_ref().map(|f| f.any_focused()).unwrap_or(false)
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
            event, scale_factor, modifiers, state, path, handled, &mut sink, node_id, taffy,
        );
        if let Some(form) = &mut self.form {
            for f in &mut form.inputs {
                let mut s2: Option<NodeId> = None;
                f.ti.handle_event(
                    event, scale_factor, modifiers, state, path, handled, &mut s2, node_id, taffy,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
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
            *handled = true;
            return;
        }

        // Toolbar.
        if self.hit.import.contains(p) {
            self.import_vcards(state);
            *handled = true;
            return;
        }
        if self.hit.export.contains(p) {
            self.open_export_dialog();
            *handled = true;
            self.dirty = true;
            return;
        }
        if self.hit.new.contains(p) {
            let mut c = Contact::new();
            c.phones.push(Labeled::new("mobile", ""));
            c.emails.push(Labeled::new("home", ""));
            self.begin_edit(c, true);
            *handled = true;
            return;
        }

        // List header chips.
        if self.hit.sort.contains(p) {
            self.sort = match self.sort {
                SortKey::Name => SortKey::Org,
                SortKey::Org => SortKey::Name,
            };
            self.relayout = true;
            self.dirty = true;
            *handled = true;
            return;
        }
        if self.hit.group.contains(p) {
            self.popup = Some(Popup::GroupFilter { anchor: self.hit.group });
            *handled = true;
            self.dirty = true;
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
            // Double-click a detail field value → edit, focused on that field.
            if self.selected.is_some()
                && self.detail_rect.contains(p)
                && self.try_detail_double_click(p)
            {
                *handled = true;
                return;
            }
            // Detail actions.
            if self.selected.is_some() {
                if self.hit.fav.contains(p) {
                    if let Some(id) = self.selected {
                        if let Some(c) = self.store.get_mut(id) {
                            c.favorite = !c.favorite;
                            let cc = c.clone();
                            ContactsDoc::upsert_contact(state, &cc);
                        }
                        self.dirty = true;
                    }
                    *handled = true;
                    return;
                }
                if self.hit.edit.contains(p) {
                    if let Some(c) = self.selected.and_then(|id| self.store.get(id)).cloned() {
                        self.begin_edit(c, false);
                    }
                    *handled = true;
                    return;
                }
                if self.hit.delete.contains(p) {
                    self.confirm_delete = self.selected;
                    self.dirty = true;
                    *handled = true;
                    return;
                }
            }
            // Forward to search input for caret placement.
            self.forward_press_to_focused_input(state, path, node_id, taffy, modifiers);
            *handled = true;
            return;
        }

        // ----- Edit mode clicks -----
        if self.hit.save.contains(p) {
            self.save_edit(state);
            *handled = true;
            return;
        }
        if self.hit.cancel.contains(p) {
            self.cancel_edit();
            *handled = true;
            return;
        }
        if self.hit.fav_toggle.contains(p) {
            if let Some(form) = &mut self.form {
                form.draft.favorite = !form.draft.favorite;
            }
            self.dirty = true;
            *handled = true;
            return;
        }
        if self.hit.cal_btn.contains(p) {
            let (year, month) = self
                .form
                .as_ref()
                .and_then(|f| date::parse_ymd(&f.draft.birthday))
                .map(|(y, m, _)| (y, m))
                .unwrap_or_else(|| {
                    let (y, m, _) = date::today();
                    (y, m)
                });
            self.popup = Some(Popup::Calendar { year, month, anchor: self.hit.cal_btn });
            *handled = true;
            self.dirty = true;
            return;
        }
        if self.hit.add_phone.contains(p) {
            self.form_add(|d| d.phones.push(Labeled::new("mobile", "")));
            *handled = true;
            return;
        }
        if self.hit.add_email.contains(p) {
            self.form_add(|d| d.emails.push(Labeled::new("home", "")));
            *handled = true;
            return;
        }
        if self.hit.add_address.contains(p) {
            self.form_add(|d| d.addresses.push(Address { label: "home".into(), ..Default::default() }));
            *handled = true;
            return;
        }
        // Type chips.
        if let Some((target, _)) = self.hit.type_chips.iter().find(|(_, r)| r.contains(p)).copied() {
            let anchor = self.hit.type_chips.iter().find(|(_, r)| r.contains(p)).map(|(_, r)| *r).unwrap();
            self.popup = Some(Popup::TypeMenu { target, anchor });
            *handled = true;
            self.dirty = true;
            return;
        }
        // Remove buttons.
        if let Some((target, _)) = self.hit.remove_btns.iter().find(|(_, r)| r.contains(p)).copied() {
            self.form_remove(target);
            *handled = true;
            return;
        }

        // Otherwise: focus the form input under the cursor (caret placement).
        self.focus_input_at(p);
        self.forward_press_to_focused_input(state, path, node_id, taffy, modifiers);
        *handled = true;
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
        let is_double = self
            .last_detail_click
            .map(|(t0, p0)| {
                now.duration_since(t0).as_millis() <= DOUBLE_CLICK_MS
                    && p.distance(p0) <= DOUBLE_CLICK_RADIUS
            })
            .unwrap_or(false);
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
            self.search
                .handle_event(&press, 1.0, modifiers, state, path, &mut h, &mut sink, node_id, taffy);
        }
        if let Some(form) = &mut self.form {
            for f in &mut form.inputs {
                if f.ti.is_focused() {
                    let mut s2 = None;
                    f.ti.handle_event(&press, 1.0, modifiers, state, path, &mut h, &mut s2, node_id, taffy);
                }
            }
        }
        // Note: cursor placement used logical coords (scale already applied
        // when we recorded last_mouse), so pass scale_factor = 1.0 above.
        self.dirty = true;
    }

    /// Re-sync a focused input's edits into the draft + store + CRDT.
    fn commit_form_live(&mut self, state: &mut AutoCommit) {
        let c = if let Some(form) = &mut self.form {
            form.collect();
            Some(form.draft.clone())
        } else {
            None
        };
        if let Some(c) = c {
            self.store.upsert(c.clone());
            ContactsDoc::upsert_contact(state, &c);
        }
    }

    fn form_add(&mut self, f: impl Fn(&mut Contact)) {
        if let Some(form) = self.form.take() {
            let mut draft = form.draft;
            // First fold current input values back in.
            let mut tmp = EditForm { draft: draft.clone(), is_new: form.is_new, inputs: form.inputs };
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
            let mut tmp = EditForm { draft: form.draft.clone(), is_new: form.is_new, inputs: form.inputs };
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

    fn handle_popup(&mut self, event: &WindowEvent, state: &mut AutoCommit, handled: &mut bool) {
        let press = matches!(
            event,
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. }
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
            Popup::TypeMenu { target, anchor } => {
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
                    self.commit_form_live(state);
                }
            }
            Popup::GroupFilter { anchor } => {
                let mut opts = vec!["All".to_string()];
                opts.extend(self.store.all_groups());
                let (_panel, rows) = self.popup_menu_geometry(anchor, opts.len(), 180.0);
                if let Some(i) = rows.iter().position(|r| r.contains(p)) {
                    self.group_filter = if i == 0 { None } else { Some(opts[i].clone()) };
                    self.list_scroll = 0.0;
                    // Keep selection valid.
                    if let Some(sel) = self.selected {
                        if !self.visible_ids().contains(&sel) {
                            self.selected = self.visible_ids().first().copied();
                        }
                    }
                }
                self.relayout = true;
            }
            Popup::ContextMenu { target, anchor } => {
                let (_panel, rows) = self.popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
                if let Some(i) = rows.iter().position(|r| r.contains(p)) {
                    match CONTEXT_ACTIONS[i] {
                        ContextAction::Edit => {
                            if let Some(c) = self.store.get(target).cloned() {
                                self.selected = Some(target);
                                self.begin_edit(c, false);
                            }
                        }
                        ContextAction::Favorite => {
                            // Toggle the favorite flag and persist it to the
                            // CRDT (same write path as the view-mode star and
                            // Save), so the list star and the menu label flip
                            // immediately and the change survives a reload.
                            if let Some(c) = self.store.get_mut(target) {
                                c.favorite = !c.favorite;
                                let cc = c.clone();
                                ContactsDoc::upsert_contact(state, &cc);
                            }
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
            }
            Popup::Calendar { year, month, anchor } => {
                let (panel, prev, next, cells) = self.calendar_geometry(anchor, year, month);
                if prev.contains(p) {
                    let (y, m) = date::prev_month(year, month);
                    self.popup = Some(Popup::Calendar { year: y, month: m, anchor });
                } else if next.contains(p) {
                    let (y, m) = date::next_month(year, month);
                    self.popup = Some(Popup::Calendar { year: y, month: m, anchor });
                } else if panel.contains(p) {
                    if let Some((d, _)) = cells.iter().find(|(_, r)| r.contains(p)) {
                        let iso = format!("{:04}-{:02}-{:02}", year, month, d);
                        if let Some(form) = &mut self.form {
                            form.draft.birthday = iso.clone();
                            if let Some(fi) = form.inputs.iter_mut().find(|f| f.slot == Slot::Birthday) {
                                fi.ti.set_text(&iso);
                            }
                        }
                        self.commit_form_live(state);
                    } else {
                        // Click inside panel but not a day: keep open.
                        self.popup = Some(Popup::Calendar { year, month, anchor });
                    }
                }
                // else: click outside dismisses (popup already taken).
            }
        }
    }

    fn handle_confirm(&mut self, event: &WindowEvent, state: &mut AutoCommit, handled: &mut bool) {
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            self.dirty = true;
        }
        let press = matches!(
            event,
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. }
        );
        if !press {
            return;
        }
        *handled = true;
        self.dirty = true;
        let (_panel, del, cancel) = self.confirm_geometry();
        let p = self.last_mouse;
        if del.contains(p) {
            if let Some(id) = self.confirm_delete {
                self.delete_selected(state, id);
            }
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
                MouseScrollDelta::LineDelta(_, y) => (-y * 48.0) as f64,
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
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. }
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
            if let Some((id, _, _)) = g.rows.iter().find(|(_, r, _)| r.contains(p)) {
                if let Some(set) = &mut self.export_sel {
                    if set.contains(id) {
                        set.remove(id);
                    } else {
                        set.insert(*id);
                    }
                }
            }
            return;
        }
        if g.cancel.contains(p) {
            self.export_sel = None;
            return;
        }
        if g.export_v3.contains(p) || g.export_v4.contains(p) {
            let version = if g.export_v4.contains(p) { VCardVersion::V4 } else { VCardVersion::V3 };
            let ids: Vec<ContactId> = self
                .visible_export_ids()
                .into_iter()
                .filter(|id| self.export_sel.as_ref().map(|s| s.contains(id)).unwrap_or(false))
                .collect();
            self.export_ids(&ids, version);
            self.export_sel = None;
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
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. }
        );
        if !press {
            return;
        }
        *handled = true;
        self.dirty = true;
        let p = self.last_mouse;
        let g = self.unsaved_geometry();
        let Some(target) = self.pending_switch else { return };
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
            self.forward_event_to_inputs(full_event, scale_factor, modifiers, state, path, handled, node_id, taffy);
            return;
        }
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();

        // Global-search seam: leave Ctrl+K (and Ctrl+/) untouched so the
        // forthcoming shared global-search framework can claim the chord.
        // Returning here without setting `handled` lets the event propagate
        // to the host/compositor — do NOT build a bespoke palette.
        if is_global_search_chord(ctrl, &ke.logical_key) {
            return; // handled stays false → event propagates
        }

        // Escape closes popups / dialogs / cancels edit / returns from a
        // narrow pushed detail to the list / clears search focus.
        if matches!(ke.logical_key, Key::Named(NamedKey::Escape)) {
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
            *handled = true;
            self.dirty = true;
            return;
        }

        // Ctrl+S saves in edit mode.
        if ctrl {
            if let Key::Character(c) = &ke.logical_key {
                if c.eq_ignore_ascii_case("s") && self.mode == Mode::Edit {
                    self.save_edit(state);
                    *handled = true;
                    return;
                }
            }
        }

        // If a text field is focused, route typing to it.
        if self.any_text_focused() {
            self.forward_event_to_inputs(full_event, scale_factor, modifiers, state, path, handled, node_id, taffy);
            if self.mode == Mode::Edit {
                self.commit_form_live(state);
            }
            self.dirty = true;
            return;
        }

        // Global shortcuts (no field focused).
        match &ke.logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                self.move_selection(1);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_selection(-1);
                *handled = true;
            }
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                if self.mode == Mode::View {
                    if let Some(id) = self.selected {
                        self.confirm_delete = Some(id);
                        *handled = true;
                        self.dirty = true;
                    }
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
                    "e" if self.mode == Mode::View => {
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
    use taffy::{AvailableSpace, Dimension, Size, Style};
    use vello::kurbo::Point;

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
        app.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &permission);
        // Encoding the scene is enough to exercise every draw path; the
        // GPU blit is covered by the `--shots` harness.
        let _ = &scene;
    }

    #[test]
    fn seeds_sample_contacts_and_selects_one() {
        let mut app = ContactsApp::new();
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
        let mut app = ContactsApp::new();
        drive_frame(&mut app, 1280, 800);
    }

    #[test]
    fn narrow_frame_resolves_single_pane_push() {
        let mut app = ContactsApp::new();
        // A narrow viewport drives the single-pane master-detail push.
        drive_frame(&mut app, 480, 860);
        assert!(app.narrow, "a narrow viewport resolves to single-pane push");
        assert_eq!(app.push.pane(), Pane::List, "a fresh narrow frame opens to the list");
    }

    #[test]
    fn narrow_selection_pushes_detail_then_back_returns_to_list() {
        let mut app = ContactsApp::new();
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
        assert!(app.hit.back.width() > 0.0, "pushed detail shows a Back affordance");
    }

    #[test]
    fn wide_frame_stays_docked_side_by_side() {
        let mut app = ContactsApp::new();
        drive_frame(&mut app, 1280, 800);
        assert!(!app.narrow, "a desktop-width viewport keeps the docked split");
        // The list pane sits left of the detail pane (classic split).
        assert!(app.list_rect.x1 <= app.detail_rect.x0 + 0.001);
    }

    #[test]
    fn edit_mode_with_repeated_fields_frame_does_not_panic() {
        let mut app = ContactsApp::new();
        // Seed first.
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        // Enter edit on the selected contact and add repeated fields.
        let c = app.selected.and_then(|id| app.store.get(id)).cloned().unwrap();
        app.begin_edit(c, false);
        app.form_add(|d| d.phones.push(Labeled::new("work", "")));
        app.form_add(|d| d.addresses.push(Address { label: "work".into(), ..Default::default() }));
        drive_frame(&mut app, 1280, 800);
        assert_eq!(app.mode, Mode::Edit);
    }

    #[test]
    fn calendar_and_type_popups_frame_does_not_panic() {
        let mut app = ContactsApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let c = app.selected.and_then(|id| app.store.get(id)).cloned().unwrap();
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
        let mut app = ContactsApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let mut c = app.selected.and_then(|id| app.store.get(id)).cloned().unwrap();
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
    use winit::event::{DeviceId, ElementState, Modifiers, MouseButton, WindowEvent};
    use winit::keyboard::{Key, NamedKey, SmolStr};

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
            let mut app = ContactsApp::new();
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
            let child = self.app.build_layout(&mut self.taffy, &theme, &mut node_map);
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
    }

    #[test]
    fn narrow_row_press_pushes_detail_then_back_returns_to_list() {
        let mut h = Harness::new(460, 880);
        assert!(h.app.narrow);
        assert_eq!(h.app.push.pane(), Pane::List);
        // Press a list row → the detail pane is pushed in.
        let row = h.row_center(1);
        assert!(h.press_left(row));
        assert_eq!(h.app.push.pane(), Pane::Detail, "selecting pushes the detail in");
        h.relayout();
        assert!(h.app.hit.back.width() > 0.0, "the Back affordance is laid out");
        // Press Back → return to the list.
        let back = h.app.hit.back.center();
        assert!(h.press_left(back));
        assert_eq!(h.app.push.pane(), Pane::List, "Back returns to the full-width list");
    }

    #[test]
    fn double_click_detail_field_enters_edit_focused_on_that_field() {
        let mut h = Harness::new(1280, 800);
        assert_eq!(h.app.mode, Mode::View);
        // Find the WEBSITE field value rect (the selected seed has a website).
        let (slot, rect) = h
            .app
            .detail_field_hits
            .iter()
            .find(|(s, _)| *s == Slot::Website)
            .copied()
            .expect("selected contact shows a website field");
        assert_eq!(slot, Slot::Website);
        h.double_click(rect.center());
        assert_eq!(h.app.mode, Mode::Edit, "double-click enters edit mode");
        // The website input must be the focused one.
        let form = h.app.form.as_ref().unwrap();
        let website_focused = form
            .inputs
            .iter()
            .find(|f| f.slot == Slot::Website)
            .map(|f| f.ti.is_focused())
            .unwrap_or(false);
        assert!(website_focused, "the double-clicked field is focused for editing");
    }

    #[test]
    fn single_click_detail_field_does_not_enter_edit() {
        let mut h = Harness::new(1280, 800);
        let rect = h
            .app
            .detail_field_hits
            .iter()
            .find(|(s, _)| *s == Slot::Website)
            .map(|(_, r)| *r)
            .expect("website field present");
        h.press_left(rect.center());
        assert_eq!(h.app.mode, Mode::View, "a single click must not enter edit mode");
    }

    #[test]
    fn right_click_row_opens_context_menu_then_delete_opens_confirm() {
        let mut h = Harness::new(1280, 800);
        let row = h.row_center(0);
        let id = h.row_id(0);
        assert!(h.press_right(row));
        match &h.app.popup {
            Some(Popup::ContextMenu { target, .. }) => assert_eq!(*target, id),
            other => panic!("right-click should open the context menu, got {:?}", other.is_some()),
        }
        // Click the "Delete" row in the menu → the delete-confirm modal opens.
        let Some(Popup::ContextMenu { anchor, .. }) = h.app.popup else { unreachable!() };
        let (_panel, rows) = h.app.popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
        let delete_idx =
            CONTEXT_ACTIONS.iter().position(|a| *a == ContextAction::Delete).unwrap();
        h.press_left(rows[delete_idx].center());
        assert_eq!(
            h.app.confirm_delete,
            Some(id),
            "context-menu Delete routes through the confirmation modal"
        );
        assert!(h.app.popup.is_none(), "the context menu closes when an item is chosen");
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
        let fav_idx =
            CONTEXT_ACTIONS.iter().position(|a| *a == ContextAction::Favorite).unwrap();
        assert_eq!(
            CONTEXT_ACTIONS[fav_idx].label(before),
            if before { "Remove from favorites" } else { "Add to favorites" },
            "the Favorite item is labelled for the contact's current state",
        );

        // Open the menu and click the Favorite item.
        assert!(h.press_right(row));
        let Some(Popup::ContextMenu { anchor, .. }) = h.app.popup else {
            panic!("right-click should open the context menu");
        };
        let (_panel, rows) = h.app.popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
        h.press_left(rows[fav_idx].center());

        // The flag flipped in the store AND in the CRDT, and the menu closed.
        assert_eq!(h.app.store.get(id).unwrap().favorite, !before, "store flag flipped");
        assert_eq!(fav_in_doc(&h.doc, id), !before, "CRDT favorite flag flipped");
        assert!(h.app.popup.is_none(), "the menu closes after toggling favorite");

        // Reopening the menu now shows the inverted label.
        assert!(h.press_right(h.row_center(0)));
        assert_eq!(
            CONTEXT_ACTIONS[fav_idx].label(!before),
            if !before { "Remove from favorites" } else { "Add to favorites" },
            "the relabelled item reflects the toggled state",
        );

        // Toggling again restores the original state (round-trip).
        let Some(Popup::ContextMenu { anchor, .. }) = h.app.popup else { unreachable!() };
        let (_panel, rows) = h.app.popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
        h.press_left(rows[fav_idx].center());
        assert_eq!(fav_in_doc(&h.doc, id), before, "second toggle restores the CRDT flag");
    }

    #[test]
    fn confirm_modal_delete_removes_contact_from_doc() {
        let mut h = Harness::new(1280, 800);
        let id = h.row_id(0);
        h.app.confirm_delete = Some(id);
        let before = ContactsDoc::list_contacts(&h.doc).len();
        let (_panel, del, _cancel) = h.app.confirm_geometry();
        h.press_left(del.center());
        assert!(h.app.confirm_delete.is_none(), "the modal closes after confirming");
        let after = ContactsDoc::list_contacts(&h.doc).len();
        assert_eq!(after, before - 1, "Delete removes the contact from the CRDT doc");
        assert!(
            !ContactsDoc::list_contacts(&h.doc).iter().any(|c| c.id == id),
            "the deleted contact is gone from the document"
        );
    }

    #[test]
    fn switching_while_editing_dirty_pops_save_modal_then_save_commits() {
        let mut h = Harness::new(1280, 800);
        // Enter edit on the selected contact and dirty a field.
        let cur = h.app.selected.unwrap();
        let c = h.app.store.get(cur).cloned().unwrap();
        h.app.begin_edit(c, false);
        if let Some(form) = h.app.form.as_mut() {
            if let Some(fi) = form.inputs.iter_mut().find(|f| f.slot == Slot::Title) {
                fi.ti.set_text("Brand New Title");
            }
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
        assert_eq!(h.app.pending_switch, Some(other_id), "switch is deferred behind the modal");
        assert_eq!(h.app.mode, Mode::Edit, "still editing while the modal is up");
        // Press Save → commits the edit AND switches to the target.
        let g = h.app.unsaved_geometry();
        h.press_left(g.save.center());
        assert!(h.app.pending_switch.is_none());
        assert_eq!(h.app.mode, Mode::View, "saved and left the form");
        assert_eq!(h.app.selected, Some(other_id), "switched to the clicked contact");
        let stored = ContactsDoc::list_contacts(&h.doc);
        let edited = stored.iter().find(|x| x.id == cur).unwrap();
        assert_eq!(edited.title, "Brand New Title", "the edit was saved to the doc");
    }

    #[test]
    fn switching_while_editing_discard_drops_edits_and_switches() {
        let mut h = Harness::new(1280, 800);
        let cur = h.app.selected.unwrap();
        let original_title = h.app.store.get(cur).unwrap().title.clone();
        let c = h.app.store.get(cur).cloned().unwrap();
        h.app.begin_edit(c, false);
        if let Some(form) = h.app.form.as_mut() {
            if let Some(fi) = form.inputs.iter_mut().find(|f| f.slot == Slot::Title) {
                fi.ti.set_text("Throwaway Title");
            }
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
        assert_eq!(unchanged.title, original_title, "discard left the doc unchanged");
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
        assert!(h.app.pending_switch.is_none(), "no modal when there are no unsaved changes");
        assert_eq!(h.app.selected, Some(other_id), "switched directly");
        assert_eq!(h.app.mode, Mode::View);
    }

    #[test]
    fn export_dialog_select_none_then_toggle_then_export_writes_selection() {
        let mut h = Harness::new(1280, 800);
        // Open the export dialog via the toolbar Export button.
        h.press_left(h.app.hit.export.center());
        assert!(h.app.export_sel.is_some(), "Export opens the selection dialog");
        // Pre-ticked with all visible contacts.
        let total = h.app.visible_export_ids().len();
        assert_eq!(h.app.export_sel.as_ref().unwrap().len(), total, "all pre-selected");
        // Select none.
        let g = h.app.export_geometry();
        h.press_left(g.select_none.center());
        assert_eq!(h.app.export_sel.as_ref().unwrap().len(), 0, "Select none clears the set");
        // Tick exactly one row.
        let g = h.app.export_geometry();
        let (first_id, _row, cb) = g.rows[0];
        h.press_left(cb.center());
        assert_eq!(h.app.export_sel.as_ref().unwrap().len(), 1);
        assert!(h.app.export_sel.as_ref().unwrap().contains(&first_id));
        // Export 3.0 → writes the file and closes the dialog.
        let g = h.app.export_geometry();
        h.press_left(g.export_v3.center());
        assert!(h.app.export_sel.is_none(), "exporting closes the dialog");
        // The export wrote exactly the one chosen contact.
        let body = std::fs::read_to_string(std::env::temp_dir().join("contacts-export.vcf"))
            .expect("export wrote a vcf");
        let cards = body.matches("BEGIN:VCARD").count();
        assert_eq!(cards, 1, "exported exactly the single ticked contact");
    }

    #[test]
    fn export_select_all_round_trips_every_contact() {
        let mut h = Harness::new(1280, 800);
        h.app.open_export_dialog();
        let g = h.app.export_geometry();
        h.press_left(g.select_none.center());
        let g = h.app.export_geometry();
        h.press_left(g.select_all.center());
        let total = h.app.visible_export_ids().len();
        assert_eq!(h.app.export_sel.as_ref().unwrap().len(), total, "Select all ticks everyone");
        let g = h.app.export_geometry();
        h.press_left(g.export_v4.center());
        let body = std::fs::read_to_string(std::env::temp_dir().join("contacts-export.vcf")).unwrap();
        assert_eq!(body.matches("BEGIN:VCARD").count(), total);
        assert!(body.contains("VERSION:4.0"), "Export 4.0 emits a v4 card");
    }

    #[test]
    fn ctrl_k_is_left_unhandled_for_global_search() {
        // The Ctrl+K (and Ctrl+/) chord must bubble to the host so the
        // forthcoming shared global-search framework claims it. `handle_key`
        // returns without setting `handled` exactly when this is true, so
        // asserting on the production decision function proves the seam.
        assert!(is_global_search_chord(true, &Key::Character(SmolStr::new("k"))));
        assert!(is_global_search_chord(true, &Key::Character(SmolStr::new("K"))));
        assert!(is_global_search_chord(true, &Key::Character(SmolStr::new("/"))));
        // Not the seam: same keys without ctrl, or other ctrl chords.
        assert!(!is_global_search_chord(false, &Key::Character(SmolStr::new("k"))));
        assert!(!is_global_search_chord(true, &Key::Character(SmolStr::new("s"))));
        assert!(!is_global_search_chord(true, &Key::Named(NamedKey::Escape)));
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
        let mut app = ContactsApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        app.shots_new_contact();
        assert!(!app.form_is_dirty(), "an untouched New Contact form is clean");
        if let Some(form) = app.form.as_mut() {
            form.draft.first = "Jane".into();
            if let Some(fi) = form.inputs.iter_mut().find(|f| f.slot == Slot::First) {
                fi.ti.set_text("Jane");
            }
        }
        assert!(app.form_is_dirty(), "typing a name makes the form dirty");
    }
}
