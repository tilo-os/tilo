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
use crate::vcard::{self, VCardVersion};

const TOOLBAR_H: f64 = 56.0;
const ROW_H: f64 = 62.0;
const DIVIDER_W: f64 = 6.0;
const PAD: f64 = 16.0;
const FIELD_H: f64 = 38.0;

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
#[derive(Clone, Copy, PartialEq, Eq)]
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
    /// vCard version chooser when exporting.
    ExportVersion { anchor: Rect },
    /// Birthday calendar.
    Calendar { year: i32, month: u32, anchor: Rect },
}

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
            last_mouse: Point::ZERO,
            dirty: true,
            relayout: true,
            seeded: false,
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

    fn export_selected(&mut self, version: VCardVersion) {
        let contacts: Vec<Contact> = match self.selected.and_then(|id| self.store.get(id)) {
            Some(c) => vec![c.clone()],
            None => self.store.contacts.clone(),
        };
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

        // Split panes.
        let avail_w = content.width();
        let list_w = (avail_w * self.split_pos).clamp(260.0, avail_w - 360.0).max(220.0);
        self.list_rect = Rect::new(content.x0, content.y0, content.x0 + list_w, content.y1);
        let div_x0 = self.list_rect.x1;
        self.detail_rect = Rect::new(div_x0 + DIVIDER_W, content.y0, content.x1, content.y1);

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

    /// Assign rects to every form input + dynamic control. Mirrors the
    /// vertical order the form renders in. Two-phase to avoid holding a
    /// `self.form` borrow while writing `self.hit`: build a rect plan
    /// first, then apply it to the inputs.
    fn layout_form(&mut self) {
        let area = self.detail_rect;
        let x0 = area.x0 + PAD;
        let x1 = area.x1 - PAD;
        let w = x1 - x0;
        let top = area.y0 + 64.0; // header band reserved for Save/Cancel
        let mut y = top - self.detail_scroll;

        // Save / Cancel live pinned in the header band (not scrolled).
        let by = area.y0 + 16.0;
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
        let c = &theme.tokens.colors;
        // App background.
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);

        self.draw_toolbar(scene, transform, tr, theme);
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
        self.draw_divider(scene, transform, theme);

        // Detail panel background.
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.detail_rect);
        if self.mode == Mode::Edit {
            self.draw_form(scene, transform, tr, theme, presence);
        } else {
            self.draw_detail(scene, transform, tr, theme);
        }

        // Overlays.
        if let Some(popup) = &self.popup {
            self.draw_popup(scene, transform, tr, theme, popup);
        }
        if let Some(id) = self.confirm_delete {
            self.draw_confirm(scene, transform, tr, theme, id);
        }
        if let Some((msg, _)) = &self.toast {
            draw_toast(scene, transform, tr, theme, self.rect, msg);
        }
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
                t * Affine::translate((area.x0 + PAD, area.y0 + 40.0)),
                None,
            );
            self.detail_content_h = 0.0;
            return;
        };
        let Some(contact) = self.store.get(id).cloned() else { return };

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &area);
        let x0 = area.x0 + 28.0;
        let x1 = area.x1 - 24.0;
        let mut y = area.y0 + 28.0 - self.detail_scroll;

        // Header: big avatar + name + subtitle.
        let av = Rect::new(x0, y, x0 + 72.0, y + 72.0);
        draw_avatar(scene, t, tr, theme, av, &contact.initials());
        let hx = av.x1 + 18.0;
        tr.draw_text_weighted(
            scene,
            &contact.display_name(),
            26.0,
            c.text_primary,
            t * Affine::translate((hx, y + 4.0)),
            Some((x1 - hx) as f32),
            TextWeight::Bold,
        );
        let sub = contact.subtitle();
        if !sub.is_empty() {
            tr.draw_text(
                scene,
                &sub,
                14.0,
                c.text_dim,
                t * Affine::translate((hx, y + 38.0)),
                Some((x1 - hx) as f32),
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

        // Sections (free helpers avoid closures double-borrowing scene/tr).
        if !contact.phones.is_empty() {
            detail_section(scene, t, tr, theme, x0, &mut y, "PHONE");
            for p in &contact.phones {
                detail_field(scene, t, tr, theme, x0, x1, &mut y, &p.label, &p.value);
            }
        }
        if !contact.emails.is_empty() {
            detail_section(scene, t, tr, theme, x0, &mut y, "EMAIL");
            for e in &contact.emails {
                detail_field(scene, t, tr, theme, x0, x1, &mut y, &e.label, &e.value);
            }
        }
        if !contact.addresses.is_empty() {
            detail_section(scene, t, tr, theme, x0, &mut y, "ADDRESS");
            for a in &contact.addresses {
                detail_field(scene, t, tr, theme, x0, x1, &mut y, &a.label, &a.one_line());
            }
        }
        if !contact.birthday.is_empty() {
            detail_section(scene, t, tr, theme, x0, &mut y, "BIRTHDAY");
            detail_field(scene, t, tr, theme, x0, x1, &mut y, "date", &date::pretty(&contact.birthday));
        }
        if !contact.website.is_empty() {
            detail_section(scene, t, tr, theme, x0, &mut y, "WEBSITE");
            detail_field(scene, t, tr, theme, x0, x1, &mut y, "url", &contact.website);
        }
        if !contact.notes.is_empty() {
            detail_section(scene, t, tr, theme, x0, &mut y, "NOTES");
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
        }

        self.detail_content_h = (y + self.detail_scroll) - (area.y0 + 28.0) + 24.0;
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
        let x0 = area.x0 + PAD;

        // Header band (not scrolled).
        let title = if self.form.as_ref().map(|f| f.is_new).unwrap_or(false) {
            "New Contact"
        } else {
            "Edit Contact"
        };
        tr.draw_text_weighted(
            scene,
            title,
            18.0,
            c.text_primary,
            t * Affine::translate((x0, area.y0 + 22.0)),
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

        // Scrollable body, clipped below the header band.
        let body = Rect::new(area.x0, area.y0 + 60.0, area.x1, area.y1);
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
        // Phone / email / address section headers anchored to add buttons.
        label(scene, tr, "PHONE", self.hit.add_phone);
        label(scene, tr, "EMAIL", self.hit.add_email);
        label(scene, tr, "ADDRESS", self.hit.add_address);
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

        // Draw the embedded inputs.
        for f in self.form.as_mut().unwrap().inputs.iter_mut() {
            f.ti.draw(scene, t, tr, theme, presence);
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
            Popup::ExportVersion { anchor } => {
                let opts = ["vCard 3.0", "vCard 4.0"];
                let (panel, rows) = self.popup_menu_geometry(*anchor, opts.len(), 150.0);
                menu_panel(scene, t, theme, panel);
                for (i, r) in rows.iter().enumerate() {
                    let hov = r.contains(self.last_mouse);
                    menu_item(scene, t, tr, theme, *r, opts[i], hov, false);
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
        }
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
        // View-mode detail action rects (top-right of the detail pane),
        // computed after `layout()` populates `detail_rect`.
        let ax1 = self.detail_rect.x1 - 24.0;
        let by = self.rect.y0 + TOOLBAR_H + 30.0;
        self.hit.delete = Rect::new(ax1 - 84.0, by, ax1, by + 32.0);
        self.hit.edit = Rect::new(self.hit.delete.x0 - 8.0 - 72.0, by, self.hit.delete.x0 - 8.0, by + 32.0);
        self.hit.fav = Rect::new(self.hit.edit.x0 - 8.0 - 32.0, by, self.hit.edit.x0 - 8.0, by + 32.0);
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
                self.divider_hover = self.divider_hit().contains(self.last_mouse);
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
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
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

        // Toolbar.
        if self.hit.import.contains(p) {
            self.import_vcards(state);
            *handled = true;
            return;
        }
        if self.hit.export.contains(p) {
            self.popup = Some(Popup::ExportVersion { anchor: self.hit.export });
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
        } else if self.mode == Mode::View {
            // List row click.
            if self.list_body.contains(p) {
                if let Some((id, _)) = self.list_rows.iter().find(|(_, r)| r.contains(p)) {
                    self.selected = Some(*id);
                    self.detail_scroll = 0.0;
                    self.dirty = true;
                    self.relayout = true;
                    self.search.set_focus(false);
                    *handled = true;
                    return;
                }
            }
        }

        if self.mode == Mode::View {
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
            Popup::ExportVersion { anchor } => {
                let opts = [VCardVersion::V3, VCardVersion::V4];
                let (_panel, rows) = self.popup_menu_geometry(anchor, opts.len(), 150.0);
                if let Some(i) = rows.iter().position(|r| r.contains(p)) {
                    self.export_selected(opts[i]);
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

        // Escape closes popups / cancels edit / clears search focus.
        if matches!(ke.logical_key, Key::Named(NamedKey::Escape)) {
            if self.popup.is_some() {
                self.popup = None;
            } else if self.mode == Mode::Edit {
                self.cancel_edit();
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
}
