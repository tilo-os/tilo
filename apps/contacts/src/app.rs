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

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ChangeHash, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::a11y::{AccessNode, AccessState, Role};
use ui::chrome::{GLOW_ALPHA, SelectionState, SelectionTier};
use ui::icons::Icon;
use ui::permissions::PermissionContext;
use ui::portal::{
    FileFilter, MultiChannel as ImportChannel, MultiOutcome as ImportOutcome, PickRequest,
    PortalChannel as ExportChannel, PortalOutcome as ExportOutcome,
};
use ui::presence::PresenceCtx;
use ui::surface::{card, edge_on, edge_outline, separator, shadow};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::tokens::Metrics;
use ui::tokens::colors::ON_FILL_MIN_CONTRAST;
use ui::tokens::colors::{Colors, over};
use ui::tokens::spacing::Spacing;
use ui::tokens::typography::TypeScale;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    Avatar, Button, ButtonFace, ButtonStyle, Checkbox, CheckboxFace, Chip, ChipLead, ChipStyle,
    EmptyState, EmptyStateContent, Link, ModalDialog, Text, TextInput, Widget,
};

use crate::date;
use crate::doc::ContactsDoc;
use crate::export_dialog::ExportDialog;
use crate::model::{
    Address, Birthday, Contact, ContactId, ContactStore, GroupEntry, Labeled, SortKey,
    group_names_equal, normalize_text, sample_contacts,
};
use crate::vcard::{self, VCardVersion};
use ui::paint::PaintCtx;
use ui::widget::{PushNav, PushPane};

/// Suggested file name for the export Save dialog.
const EXPORT_FILE_NAME: &str = "contacts-export.vcf";
/// Private presentation token; it is never written to the shared document.
const FAVORITES_FILTER: &str = "\u{0}contacts:favorites";

fn contacts_import_request() -> PickRequest {
    PickRequest::open_multiple("Import contacts", "Import")
        .thread_name("contacts-portal-import")
        .filter(
            FileFilter::new("vCard")
                .mimetype("text/vcard")
                .mimetype("text/x-vcard")
                .mimetype("text/directory")
                .glob("*.vcf")
                .glob("*.vcard"),
        )
        .all_files()
}

fn contacts_export_request(current_name: &str) -> PickRequest {
    PickRequest::save("Export contacts", "Export", current_name)
        .thread_name("contacts-portal-export")
        .filter(
            FileFilter::new("vCard")
                .mimetype("text/vcard")
                .glob("*.vcf")
                .glob("*.vcard"),
        )
}

/// Base size of the toolbar button labels (13pt sits between the type
/// scale's `sm` and `base`, so it is named here); the people-count reads
/// `base`. The bar is an `s12` strip and its buttons the theme's minimum
/// pointer target, both grown with these lines (see `toolbar_h`).
const TOOLBAR_BUTTON_TEXT: f32 = 13.0;
/// Authored 1× widths of the toolbar faces: the primary New button and the
/// two file buttons (Import / Export), and duplicate-review action. Grown by
/// the measured label growth.
const TOOLBAR_NEW_W: f64 = 104.0;
const TOOLBAR_FILE_W: f64 = 86.0;
const TOOLBAR_DUPLICATE_W: f64 = 142.0;
/// The toolbar labels, shared by the painter and the width measurement so
/// the face is always sized for the text it actually shows.
const TOOLBAR_IMPORT_LABEL: &str = "Import";
const TOOLBAR_EXPORT_LABEL: &str = "Export";
const TOOLBAR_DUPLICATE_LABEL: &str = "Review duplicate";
const TOOLBAR_NEW_LABEL: &str = "+ New";
/// Authored 1× list row height; rows grow past it only when the measured
/// name + subtitle lines need more (see [`ContactRowMetrics`]).
const ROW_H: f64 = 62.0;
/// Base text size of a list row's name line (15pt sits between the type
/// scale's `base` and `lg`); the subtitle reads the scale's `sm`.
const ROW_NAME_TEXT: f32 = 15.0;
/// Authored 1× avatar diameter, in a list row and in the edit form's
/// identity row alike.
const ROW_AVATAR: f64 = 40.0;
/// Height of one line of `size`-pt text at `text_scale` — the same value
/// `TextRenderer::measure` reports for a single line, so the renderer-less
/// layout pass and the painter agree on every row. The crate's one copy of
/// the line-height formula.
fn line_h(size: f32, text_scale: f32) -> f64 {
    ui::text_metrics::line_height_px(size, text_scale)
}

/// The authored (or token) 1× minimum, or the measured line plus its theme
/// pad rounded up to a whole pixel once scaled type needs more. Reserve
/// (layout) and paint read the same tokens, so a row is never a fraction
/// short of the text plus the gap the painter leaves, and every height is a
/// whole pixel at every scale.
fn at_least_whole(minimum: f64, measured: f64) -> f64 {
    ui::text_metrics::at_least_whole(minimum, measured)
}

/// Base (unscaled) text sizes handed to the legacy renderer, read from the
/// default type scale so the app states no sizes of its own; the renderer
/// applies the accessibility scale. Sizes off the scale (11, 13, 15pt) are
/// named constants beside the surfaces that use them.
fn text_sm() -> f32 {
    TypeScale::default().sm
}
fn text_base() -> f32 {
    TypeScale::default().base
}
fn text_xl() -> f32 {
    TypeScale::default().xl
}

/// List-row geometry for the current text scale. Built once per layout /
/// paint and shared by the virtualised row rects, the scroll extent, the
/// keyboard ensure-visible path and the row painter.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ContactRowMetrics {
    /// Row height: the authored 62px, or name + subtitle lines plus padding.
    row_h: f64,
    /// Line heights of the two text rows.
    name_h: f64,
    sub_h: f64,
    /// Avatar diameter, never taller than the text block it sits beside.
    avatar: f64,
}

impl ContactRowMetrics {
    fn new(spacing: &Spacing, text_scale: f32) -> Self {
        let name_h = line_h(ROW_NAME_TEXT, text_scale);
        let sub_h = line_h(text_sm(), text_scale);
        // The authored 62px row at 1× (33.75 + 24 sits inside it), or the two
        // lines plus the `s3` the painter leaves above and below them, rounded
        // up — always a whole pixel (92 at 2×).
        let row_h = at_least_whole(ROW_H, name_h + sub_h + 2.0 * f64::from(spacing.s3));
        let avatar = (name_h + sub_h).max(ROW_AVATAR);
        Self {
            row_h,
            name_h,
            sub_h,
            avatar,
        }
    }
}
/// A contacts book can be shared with a directory service and contain many
/// thousands of people.  The in-memory model is intentionally complete (so
/// search and exact resource resolution remain truthful), but the hand-drawn
/// list only needs rows near the viewport.  Keeping a small overscan window
/// bounds geometry, paint work and the accessibility subtree on large books.
const LIST_OVERSCAN_ROWS: usize = 4;

/// How long an action toast ("Saved", "Contact deleted", …) stays on screen
/// before it fades away on its own. The host loop is kept awake (via
/// [`Widget::needs_animation_frame`]) only while a toast is up, so the frame
/// where this elapses clears it and the loop returns to idle.
const TOAST_LIFETIME: Duration = Duration::from_millis(2600);
const DIVIDER_W: f64 = 6.0;
const PAD: f64 = 16.0;
const FIELD_H: f64 = 38.0;
/// Authored 1× height of the Sort / Group filter pills under the search box.
const FILTER_H: f64 = 30.0;
/// File reads happen on the UI-owned import completion path, so keep them
/// bounded even when an automation supplies a path rather than the picker.
const MAX_IMPORT_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_IMPORT_FILES: usize = 16;
const MAX_IMPORT_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_IMPORT_TOTAL_CARDS: usize = 2_048;
const MAX_IMPORT_CONFLICTS: usize = 64;

/// Edit-form header band geometry. The header is two stacked rows so the
/// identity block and the Favorite control never overlap: row 1 is the
/// identity anchor (an avatar + the contact's name, mirroring the read view so
/// the switch into editing reads as the *same* record, not a new screen), row 2
/// holds the Favorite pill and the Save/Cancel buttons. `layout_form` and
/// `draw_form` both derive their offsets from these so the hit rects and the
/// painted content stay aligned.
/// The identity row is one theme panel row (`metrics.panel_row_height`)
/// under an `s4` top inset, the actions row the theme's minimum pointer
/// target, the gaps between them `s3`; see `FormMetrics`.
///
/// Authored 1× widths of the edit form's text faces (grown by their labels'
/// measured growth) and the deliberately off-scale label band: an 11pt
/// section label plus a 3px gap, which no spacing step matches.
const FORM_LABEL_H: f64 = 18.0; // section label band above a field group
const FORM_TYPE_W: f64 = 78.0; // phone / email / address type face
const FORM_REMOVE_W: f64 = 30.0; // the "−" remove button
const FORM_ADD_W: f64 = 130.0; // "+ Add phone" / "+ Add email"
const FORM_ADD_ADDRESS_W: f64 = 150.0; // "+ Add address"
const FORM_FAV_W: f64 = 120.0;
const FORM_SAVE_W: f64 = 96.0;
const FORM_CANCEL_W: f64 = 88.0;
/// Base size of a section label (11pt sits between the type scale's `xs`
/// and `sm`); the heading, fields and small faces read `xl`, `base`, `sm`.
const FORM_LABEL_TEXT: f32 = 11.0;
const FORM_ADD_PHONE_LABEL: &str = "+ Add phone";
const FORM_ADD_EMAIL_LABEL: &str = "+ Add email";
const FORM_ADD_ADDRESS_LABEL: &str = "+ Add address";
const FORM_FAVORITED_LABEL: &str = "Favorited";
const FORM_FAVORITE_LABEL: &str = "Favorite";
const FORM_SAVE_LABEL: &str = "Save";
const FORM_CANCEL_LABEL: &str = "Cancel";

/// How much wider each text-face label measures at the renderer's scale than
/// at 1×. Captured in `render` (the only pass with a renderer) and fed to the
/// layout, so a face keeps its authored 1× width and grows by exactly what
/// its scaled label needs — never by a blanket `× scale`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct LabelGrowth {
    import: f64,
    export: f64,
    duplicate: f64,
    new: f64,
    add_phone: f64,
    add_email: f64,
    add_address: f64,
    favorite: f64,
    save: f64,
    cancel: f64,
}

impl LabelGrowth {
    fn measure(tr: &mut TextRenderer) -> Self {
        let scale = tr.text_scale();
        let mut growth = |label: &str, size: f32, weight: TextWeight| -> f64 {
            if scale <= 1.0 {
                return 0.0;
            }
            let scaled = tr.measure_weighted(label, size, None, weight).0;
            // The renderer multiplies every size by its scale, so measuring at
            // `size / scale` yields the label's 1× width on the same renderer.
            let one_x = tr.measure_weighted(label, size / scale, None, weight).0;
            f64::from(scaled - one_x).max(0.0)
        };
        Self {
            import: growth(
                TOOLBAR_IMPORT_LABEL,
                TOOLBAR_BUTTON_TEXT,
                TextWeight::Medium,
            ),
            export: growth(
                TOOLBAR_EXPORT_LABEL,
                TOOLBAR_BUTTON_TEXT,
                TextWeight::Medium,
            ),
            duplicate: growth(
                TOOLBAR_DUPLICATE_LABEL,
                TOOLBAR_BUTTON_TEXT,
                TextWeight::Medium,
            ),
            new: growth(TOOLBAR_NEW_LABEL, TOOLBAR_BUTTON_TEXT, TextWeight::Medium),
            add_phone: growth(FORM_ADD_PHONE_LABEL, text_sm(), TextWeight::Medium),
            add_email: growth(FORM_ADD_EMAIL_LABEL, text_sm(), TextWeight::Medium),
            add_address: growth(FORM_ADD_ADDRESS_LABEL, text_sm(), TextWeight::Medium),
            favorite: growth(FORM_FAVORITED_LABEL, text_sm(), TextWeight::Regular),
            save: growth(FORM_SAVE_LABEL, TOOLBAR_BUTTON_TEXT, TextWeight::Medium),
            cancel: growth(FORM_CANCEL_LABEL, TOOLBAR_BUTTON_TEXT, TextWeight::Medium),
        }
    }

    /// Equal to within half a pixel — the threshold below which re-running
    /// layout would move nothing the eye or the pointer can tell apart.
    fn approx_eq(&self, other: &Self) -> bool {
        let pairs = [
            (self.import, other.import),
            (self.export, other.export),
            (self.duplicate, other.duplicate),
            (self.new, other.new),
            (self.add_phone, other.add_phone),
            (self.add_email, other.add_email),
            (self.add_address, other.add_address),
            (self.favorite, other.favorite),
            (self.save, other.save),
            (self.cancel, other.cancel),
        ];
        pairs.iter().all(|(a, b)| (a - b).abs() <= 0.5)
    }
}

/// Edit-form geometry for the current text scale: every row is its authored
/// 1× height grown by the line it carries (whole pixels, see `authored_h`),
/// and every text face keeps its authored width plus its label's measured
/// growth. `layout_form`, the form painters and the ensure-visible scroll all
/// read this one description, so hit rects and painted faces cannot drift.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FormMetrics {
    /// Identity row (avatar + heading) and the Favorite / Save / Cancel row.
    title_h: f64,
    action_h: f64,
    /// Section label band above a field group: the label line plus its gap.
    label_h: f64,
    /// A text field; the type face beside a value keeps its authored
    /// height (the shared `Chip` sizes its label from the pill height) and
    /// centres in the row, the remove button grows with the field.
    field_h: f64,
    type_w: f64,
    type_h: f64,
    remove_w: f64,
    /// The "+ Add …" faces.
    add_h: f64,
    add_w: f64,
    add_address_w: f64,
    /// Header controls.
    fav_w: f64,
    save_w: f64,
    cancel_w: f64,
}

impl FormMetrics {
    /// Every row is the theme's token for that control (panel row, minimum
    /// pointer target, selector height) or its authored 1× value, or the
    /// measured line plus the same theme pad the painter leaves, rounded up
    /// (`at_least_whole`) — so reserve and paint read one set of tokens.
    fn new(spacing: &Spacing, metrics: &Metrics, text_scale: f32, growth: &LabelGrowth) -> Self {
        let (s1, s2, s3) = (
            f64::from(spacing.s1),
            f64::from(spacing.s2),
            f64::from(spacing.s3),
        );
        let field_h = at_least_whole(FIELD_H, line_h(text_base(), text_scale) + 2.0 * s2);
        let face = |authored: f64, growth: f64| (authored + growth).ceil();
        Self {
            title_h: at_least_whole(
                f64::from(metrics.panel_row_height),
                line_h(text_xl(), text_scale) + s2,
            ),
            action_h: at_least_whole(
                f64::from(metrics.min_pointer_target),
                line_h(TOOLBAR_BUTTON_TEXT, text_scale) + s3,
            ),
            label_h: at_least_whole(FORM_LABEL_H, line_h(FORM_LABEL_TEXT, text_scale) + s1),
            field_h,
            type_w: FORM_TYPE_W,
            type_h: FIELD_H,
            remove_w: face(FORM_REMOVE_W, field_h - FIELD_H),
            add_h: at_least_whole(
                f64::from(metrics.selector_height),
                line_h(text_sm(), text_scale) + s3,
            ),
            add_w: face(FORM_ADD_W, growth.add_phone.max(growth.add_email)),
            add_address_w: face(FORM_ADD_ADDRESS_W, growth.add_address),
            fav_w: face(FORM_FAV_W, growth.favorite),
            save_w: face(FORM_SAVE_W, growth.save),
            cancel_w: face(FORM_CANCEL_W, growth.cancel),
        }
    }
}

/// Type labels offered in the phone/email/address chip menu.
const TYPE_OPTIONS: &[&str] = &["home", "work", "mobile", "main", "other"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    View,
    Edit,
}

/// The single contextual action in an empty list.  It is deliberately local:
/// query, filter, and focus are view state, while creating a contact crosses
/// the durable action boundary only when the user activates it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EmptyListAction {
    None,
    Create,
    ClearFilters,
}

/// Focus order for the unsaved-edit decision dialog.  The safe Cancel action
/// is the initial focus, so Enter never discards a draft by surprise.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UnsavedFocus {
    Cancel,
    Discard,
    Save,
}

#[derive(Debug, Default)]
struct ImportSummary {
    created: usize,
    unchanged: usize,
    conflicts: usize,
    errors: usize,
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

impl ImportSummary {
    fn accepted(&self) -> usize {
        self.created + self.unchanged
    }

    fn completed(&self) -> bool {
        self.accepted() > 0
    }

    fn failure_message(&self) -> String {
        if self.conflicts > 0 {
            return format!(
                "{} contact{} need{} review before merging",
                self.conflicts,
                if self.conflicts == 1 { "" } else { "s" },
                if self.conflicts == 1 { "s" } else { "" }
            );
        }
        if self.errors > 0 {
            return format!(
                "{} contact{} couldn't be imported",
                self.errors,
                if self.errors == 1 { "" } else { "s" }
            );
        }
        "no contacts were imported".into()
    }
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

/// Standard keyboard routes to a selected contact's action menu.  The Menu
/// key is the dedicated route; Shift+F10 is its long-standing equivalent on
/// compact laptops and screen-reader keyboards.  Keeping this as a predicate
/// makes the user-configurable keyboard layer's translated logical key the
/// one source of truth, instead of checking a hardware scancode.
fn is_context_menu_chord(shift: bool, key: &Key) -> bool {
    matches!(key, Key::Named(NamedKey::ContextMenu))
        || (shift && matches!(key, Key::Named(NamedKey::F10)))
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

/// The human-facing label and stable semantic suffix for an editable field.
/// Keeping these together means a screen-reader's "Phone 2" is the same
/// field identity an automation uses when it sends a value back to the form.
fn form_field_metadata(slot: Slot) -> (String, String) {
    match slot {
        Slot::First => ("First name".into(), "first".into()),
        Slot::Last => ("Last name".into(), "last".into()),
        Slot::Org => ("Organization".into(), "organization".into()),
        Slot::Title => ("Job title".into(), "title".into()),
        Slot::Website => ("Website".into(), "website".into()),
        Slot::Birthday => ("Birthday".into(), "birthday".into()),
        Slot::Notes => ("Notes".into(), "notes".into()),
        Slot::Groups => ("Groups".into(), "groups".into()),
        Slot::Phone(index) => (format!("Phone {}", index + 1), format!("phone-{index}")),
        Slot::Email(index) => (format!("Email {}", index + 1), format!("email-{index}")),
        Slot::AddrStreet(index) => (
            format!("Address {} street", index + 1),
            format!("address-{index}-street"),
        ),
        Slot::AddrCity(index) => (
            format!("Address {} city", index + 1),
            format!("address-{index}-city"),
        ),
        Slot::AddrRegion(index) => (
            format!("Address {} region", index + 1),
            format!("address-{index}-region"),
        ),
        Slot::AddrPostal(index) => (
            format!("Address {} postal code", index + 1),
            format!("address-{index}-postal"),
        ),
        Slot::AddrCountry(index) => (
            format!("Address {} country", index + 1),
            format!("address-{index}-country"),
        ),
    }
}

fn form_field_target(id: ContactId, slot: Slot) -> String {
    let (_, key) = form_field_metadata(slot);
    format!("contacts.form:{id}:{key}")
}

fn type_option_target(id: ContactId, target: TypeTarget, label: &str) -> String {
    format!(
        "contacts.form.type-option:{id}:{}:{label}",
        type_target_key(target)
    )
}

fn calendar_target(id: ContactId) -> String {
    format!("contacts.form.calendar:{id}")
}

fn calendar_day_target(id: ContactId, year: i32, month: u32, day: u32) -> String {
    format!("contacts.form.calendar-day:{id}:{year:04}-{month:02}-{day:02}")
}

fn group_option_target(group: &str) -> String {
    format!("contacts.group.option:{group}")
}

/// An imported UID is only an identity hint, never proof that a file is
/// authorised to alter an existing record.  Exact re-imports of our own
/// export are harmless no-ops; a differing payload is surfaced as a conflict
/// for review instead of silently overwriting personal data.
fn same_import_payload(existing: &Contact, incoming: &Contact) -> bool {
    existing.first == incoming.first
        && existing.last == incoming.last
        && existing.org == incoming.org
        && existing.title == incoming.title
        && existing.website == incoming.website
        && existing.birthday == incoming.birthday
        && existing.notes == incoming.notes
        // Favourite is an app-local presentation flag, deliberately absent
        // from interoperable vCard; it must not turn a faithful re-import
        // into a false conflict or be cleared by external data.
        && existing
            .groups
            .iter()
            .map(|entry| &entry.value)
            .eq(incoming.groups.iter().map(|entry| &entry.value))
        && existing
            .phones
            .iter()
            .map(|entry| (&entry.label, &entry.value))
            .eq(incoming.phones.iter().map(|entry| (&entry.label, &entry.value)))
        && existing
            .emails
            .iter()
            .map(|entry| (&entry.label, &entry.value))
            .eq(incoming.emails.iter().map(|entry| (&entry.label, &entry.value)))
        && existing
            .addresses
            .iter()
            .map(|entry| {
                (
                    &entry.label,
                    &entry.street,
                    &entry.city,
                    &entry.region,
                    &entry.postal,
                    &entry.country,
                )
            })
            .eq(incoming.addresses.iter().map(|entry| {
                (
                    &entry.label,
                    &entry.street,
                    &entry.city,
                    &entry.region,
                    &entry.postal,
                    &entry.country,
                )
            }))
}

struct FieldInput {
    slot: Slot,
    ti: TextInput,
}

/// The live editing surface: a draft contact plus the text inputs that
/// mirror it. Type chips live on `draft` directly (set via popup).
struct EditForm {
    /// Contact value when the editor opened. Save derives a granular patch
    /// against this stable base, rather than overwriting peer changes that
    /// arrived while the form was open.
    base: Contact,
    draft: Contact,
    is_new: bool,
    inputs: Vec<FieldInput>,
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

impl EditForm {
    fn new(draft: Contact, is_new: bool) -> Self {
        let base = draft.clone();
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
        let group_names = draft
            .groups
            .iter()
            .map(|group| group.value.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        inputs.push(field(Slot::Groups, &group_names));
        Self {
            base,
            draft,
            is_new,
            inputs,
        }
    }

    fn with_base(base: Contact, draft: Contact, is_new: bool) -> Self {
        let mut form = Self::new(draft, is_new);
        form.base = base;
        form
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
                    let mut previous = std::mem::take(&mut self.draft.groups);
                    self.draft.groups = v
                        .split(',')
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(|name| {
                            previous
                                .iter()
                                .position(|entry| group_names_equal(&entry.value, name))
                                .map_or_else(
                                    || GroupEntry::new(name),
                                    |index| previous.remove(index),
                                )
                        })
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

    /// Rebase only local differences onto the current canonical contact. Inputs
    /// stay local: unchanged fields adopt restored content, genuinely edited
    /// values (including temporarily invalid birthday text) remain unsaved.
    fn reconcile(&mut self, canonical: &Contact) {
        if self.is_new || self.base == *canonical {
            return;
        }
        let baseline_inputs = Self::new(self.base.clone(), false).inputs;
        self.collect();
        let old_draft = self.draft.clone();
        let draft = rebase_contact(&self.base, &self.draft, canonical);
        let mut next = Self::new(draft, false);
        let mut previous = std::mem::take(&mut self.inputs);
        for input in &mut next.inputs {
            let key = form_input_key(&next.draft, input.slot);
            if let Some(index) = previous
                .iter()
                .position(|old| form_input_key(&old_draft, old.slot) == key)
            {
                let mut old = previous.remove(index);
                let base_value = baseline_inputs
                    .iter()
                    .find(|base| form_input_key(&self.base, base.slot) == key)
                    .map(|base| base.ti.value());
                if base_value == Some(old.ti.value()) && old.ti.value() != input.ti.value() {
                    old.ti.set_text(input.ti.value());
                }
                input.ti = old.ti;
            }
        }
        next.base = canonical.clone();
        *self = next;
    }

    fn has_local_edits(&mut self) -> bool {
        let baseline = Self::new(self.base.clone(), false);
        let raw_edit = self.inputs.iter().any(|input| {
            let key = form_input_key(&self.draft, input.slot);
            baseline
                .inputs
                .iter()
                .find(|old| form_input_key(&self.base, old.slot) == key)
                .is_none_or(|old| old.ti.value() != input.ti.value())
        });
        self.collect();
        raw_edit || self.draft != self.base
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

/// Stable identity across reordered repeated fields; indices are layout slots.
fn form_input_key(contact: &Contact, slot: Slot) -> String {
    match slot {
        Slot::Phone(i) => format!("phone:{:?}", contact.phones.get(i).map(|row| row.id)),
        Slot::Email(i) => format!("email:{:?}", contact.emails.get(i).map(|row| row.id)),
        Slot::AddrStreet(i)
        | Slot::AddrCity(i)
        | Slot::AddrRegion(i)
        | Slot::AddrPostal(i)
        | Slot::AddrCountry(i) => {
            let kind = match slot {
                Slot::AddrStreet(_) => "street",
                Slot::AddrCity(_) => "city",
                Slot::AddrRegion(_) => "region",
                Slot::AddrPostal(_) => "postal",
                _ => "country",
            };
            format!(
                "address:{:?}:{kind}",
                contact.addresses.get(i).map(|row| row.id)
            )
        }
        _ => format!("{slot:?}"),
    }
}

fn rebase_entries<T: Clone + PartialEq>(
    base: &[T],
    draft: &[T],
    canonical: &[T],
    id: impl Fn(&T) -> uuid::Uuid,
    merge: impl Fn(&T, &T, &T) -> T,
) -> Vec<T> {
    let mut result: Vec<T> = canonical
        .iter()
        .filter(|row| {
            !base.iter().any(|old| id(old) == id(row))
                || draft.iter().any(|local| id(local) == id(row))
        })
        .cloned()
        .collect();
    for local in draft {
        let old = base.iter().find(|row| id(row) == id(local));
        if old == Some(local) {
            continue;
        }
        if let Some(current) = result.iter_mut().find(|row| id(row) == id(local)) {
            *current = old.map_or_else(|| local.clone(), |old| merge(old, local, current));
        } else {
            result.push(local.clone());
        }
    }
    result
}

fn rebase_contact(base: &Contact, local: &Contact, canonical: &Contact) -> Contact {
    let mut result = canonical.clone();
    macro_rules! local_fields {
        ($out:expr, $old:expr, $local:expr, $($field:ident),+ $(,)?) => { $(if $old.$field != $local.$field { $out.$field.clone_from(&$local.$field); })+ };
    }
    local_fields!(
        result, base, local, first, last, org, title, website, notes, birthday, favorite
    );
    result.groups = rebase_entries(
        &base.groups,
        &local.groups,
        &canonical.groups,
        |row| row.id,
        |old, local, current| {
            let mut result = current.clone();
            local_fields!(result, old, local, value);
            result
        },
    );
    let labeled = |old: &Labeled, local: &Labeled, current: &Labeled| {
        let mut result = current.clone();
        local_fields!(result, old, local, label, value);
        result
    };
    result.phones = rebase_entries(
        &base.phones,
        &local.phones,
        &canonical.phones,
        |row| row.id,
        labeled,
    );
    result.emails = rebase_entries(
        &base.emails,
        &local.emails,
        &canonical.emails,
        |row| row.id,
        labeled,
    );
    result.addresses = rebase_entries(
        &base.addresses,
        &local.addresses,
        &canonical.addresses,
        |row| row.id,
        |old, local, current| {
            let mut result = current.clone();
            local_fields!(
                result, old, local, label, street, city, region, postal, country
            );
            result
        },
    );
    result
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
    let mut input = TextInput::local().with_initial_value(value);
    input.set_multiline(slot == Slot::Notes);
    FieldInput { slot, ti: input }
}

/// Floating overlay state (only one open at a time).
enum Popup {
    /// Phone/email/address type chip menu.
    TypeMenu { target: TypeTarget, anchor: Rect },
    /// Explicit sort selector in the list header.
    Sort { anchor: Rect },
    /// Group filter selector in the list header.
    GroupFilter { anchor: Rect },
    /// Birthday calendar.
    Calendar { year: i32, month: u32, anchor: Rect },
    /// Right-click menu on a contact row (Edit / Export / Delete).
    ContextMenu { target: ContactId, anchor: Rect },
}

/// The live delete confirmation: the shared modal plus callback flags drained
/// by the immediate-mode host after each event. The target remains in
/// `ContactsApp::confirm_delete`, keeping the mutation gate separate from the
/// presentation widget.
struct DeleteDialog {
    target: ContactId,
    dialog: ModalDialog<Text>,
    confirm: Rc<Cell<bool>>,
    cancel: Rc<Cell<bool>>,
}

/// Private, review-only state for a potential duplicate.  It is rebuilt from
/// the current collaborative contacts whenever opened; no tentative choices
/// leak into the shared document until the explicit Merge button emits the
/// typed `ContactsAction::MergeContacts` envelope.
struct DuplicateReview {
    survivor: Contact,
    source: Contact,
    reasons: Vec<crate::actions::DuplicateReason>,
    take_from_source: std::collections::HashSet<crate::actions::MergeField>,
    focus: usize,
    scroll: f64,
}

struct DuplicateReviewGeometry {
    panel: Rect,
    list: Rect,
    rows: Vec<DuplicateReviewRow>,
    merge: Rect,
    cancel: Rect,
    full_screen: bool,
    max_scroll: f64,
}

struct DuplicateReviewRow {
    field: crate::actions::MergeField,
    card: Rect,
    current: Rect,
    other: Rect,
}

/// The explicit decision point for an untrusted imported UID collision.  The
/// primary action is intentionally destructive: importing under a new local
/// identity adds data to the shared address book, whereas keeping the current
/// record simply discards the untrusted candidate.
struct ImportConflictDialog {
    token: uuid::Uuid,
    dialog: ModalDialog<Text>,
    import_as_new: Rc<Cell<bool>>,
    keep_existing: Rc<Cell<bool>>,
}

fn import_conflict_target(token: uuid::Uuid, action: &str) -> String {
    format!("contacts.import-conflict:{token}:{action}")
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
    /// Stable action component used by the semantic menu.  This is kept
    /// separate from the display label because the favourite label changes
    /// with state while an assistive client needs an address that remains
    /// stable for the lifetime of this one popup.
    fn id(self) -> &'static str {
        match self {
            ContextAction::Edit => "edit",
            ContextAction::Favorite => "favorite",
            ContextAction::Export => "export",
            ContextAction::Delete => "delete",
        }
    }

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

/// Stable target for one item in a contact's transient context menu.  The
/// menu itself is presentation state, but its actions are the same canonical
/// edit/favourite/delete/export paths exposed elsewhere in the app.
fn context_action_target(id: ContactId, action: ContextAction) -> String {
    format!("contacts.context:{id}:{}", action.id())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TypeTarget {
    Phone(usize),
    Email(usize),
    Addr(usize),
}

fn type_target_metadata(target: TypeTarget) -> (&'static str, usize) {
    match target {
        TypeTarget::Phone(index) => ("phone", index),
        TypeTarget::Email(index) => ("email", index),
        TypeTarget::Addr(index) => ("address", index),
    }
}

fn type_target_name(target: TypeTarget) -> String {
    let (kind, index) = type_target_metadata(target);
    format!("{} {} type", kind, index + 1)
}

fn type_target_key(target: TypeTarget) -> String {
    let (kind, index) = type_target_metadata(target);
    format!("{kind}-{index}")
}

fn type_chip_target(id: ContactId, target: TypeTarget) -> String {
    format!("contacts.form.type:{id}:{}", type_target_key(target))
}

fn remove_type_target(id: ContactId, target: TypeTarget) -> String {
    format!("contacts.form.remove:{id}:{}", type_target_key(target))
}

/// Named hit rects recomputed every `apply_layout`, tested on click.
#[derive(Default, Clone)]
struct Hit {
    import: Rect,
    export: Rect,
    /// A review-only route for the first deterministic potential duplicate.
    /// It is zero when there is nothing to review, so it can never become a
    /// stale invisible target after a collaborator resolves the pair.
    duplicates: Rect,
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
    resource_index: ContactsResourceIndex,
    /// Retained complete Ctrl+K catalogue. A provider clone is O(1), so opening
    /// search never reindexes or clones the address book.
    search_provider: crate::search_provider::ContactSearchProvider,
    search_heads: Vec<ChangeHash>,
    #[cfg(test)]
    resource_snapshot_materialized: Cell<usize>,
    #[cfg(test)]
    resource_exact_materialized: Cell<usize>,
    #[cfg(test)]
    resource_index_rebuilds: Cell<usize>,
    #[cfg(test)]
    local_change_index_lookups: Cell<usize>,
    selected: Option<ContactId>,
    mode: Mode,
    form: Option<EditForm>,

    search: TextInput,
    sort: SortKey,
    group_filter: Option<String>,

    popup: Option<Popup>,
    /// Keyboard focus within the currently-open local popup.  This is view
    /// state only; popup choices either change a local preference or update a
    /// draft that remains local until Save.
    popup_focus: usize,
    toast: Option<(String, Instant)>,
    confirm_delete: Option<ContactId>,
    /// Built lazily while `confirm_delete` is armed and discarded when that
    /// target resolves. `ModalDialog` owns the scrim, panel, buttons, Escape /
    /// backdrop dismissal, focus trap, and accessible dialog role.
    delete_dialog: Option<DeleteDialog>,
    /// The visual, confirmation-protected front end for the first pending
    /// imported-UID collision. Remaining collisions are presented one at a
    /// time after each explicit decision.
    import_conflict_dialog: Option<ImportConflictDialog>,
    duplicate_review: Option<DuplicateReview>,
    /// The first bounded duplicate candidate, recomputed only when the
    /// authoritative document generation changes. This keeps a very large
    /// address book from being rescanned on every layout, paint, tooltip, or
    /// accessibility query merely to decide whether the review affordance is
    /// visible.
    duplicate_candidate: Option<crate::actions::DuplicateCandidate>,
    duplicate_candidate_heads: Vec<ChangeHash>,

    /// Self-contained export selection modal: state, drawing and input routing.
    export_dialog: ExportDialog,
    /// When switching to another contact would lose unsaved edits, the
    /// target id parks here while the "Save changes?" modal is shown.
    pending_switch: Option<ContactId>,
    unsaved_focus: UnsavedFocus,

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
    /// Number of records matching the current search/filter.  This is kept
    /// separately from `list_rows`, which is the bounded viewport window.
    visible_row_count: usize,
    /// Absolute index in the filtered/sorted result set of `list_rows[0]`.
    /// It preserves correct assistive-technology positions after virtualising
    /// the hand-drawn list.
    list_row_start: usize,
    list_rows: Vec<(ContactId, Rect)>,
    /// The first-run call to action painted inside an empty list. Keeping its
    /// geometry here gives pointer and assistive activation the same route as
    /// the toolbar's New button instead of leaving a decorative dead end.
    empty_action_rect: Rect,
    empty_action_focused: bool,
    empty_list_action: EmptyListAction,
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
    /// The theme's text scale, cached during layout so row heights, the
    /// push breakpoint and header stacking follow the type they carry.
    text_scale: f32,
    /// How much wider each text-face label (toolbar buttons, form actions,
    /// "+ Add …" buttons) measures at the renderer's scale than at 1×,
    /// captured at the top of each paint; the layout grows the authored face
    /// widths by exactly this, so a scaled label never truncates and the 1×
    /// design is untouched.
    label_growth: LabelGrowth,
    /// The theme's spacing and density metrics, cached in `build_layout` so
    /// the renderer-less layout pass reads the same tokens the painters do.
    spacing: Spacing,
    metrics: Metrics,
    /// The edit form's row grid, solved once per `layout_form` and read by
    /// every form painter and the ensure-visible scroll.
    form_m: FormMetrics,

    last_mouse: Point,
    dirty: bool,
    relayout: bool,
    seeded: bool,

    /// When `true`, a first `sync` onto an empty book seeds the varied sample
    /// address book. Only the `--shots` and test constructors set this — the
    /// live app never fabricates content.
    seed_samples: bool,

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
    resource_invocation: Option<tilo_actions::InvocationId>,
    completed_resource_invocation: Option<tilo_actions::InvocationUpdate>,
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
    /// or a peer's change merged in — the next `sync` re-reads the store so those
    /// mutations show up in the list, exactly as an on-screen edit already does.
    model_heads: Vec<ChangeHash>,
    /// Heads produced by local actions whose one-contact store mutations have
    /// already been applied by their UI call sites.  `sync` can safely take
    /// the incremental retained-index path only for this exact generation;
    /// any unseen peer/bulk change falls back to authoritative reconciliation.
    local_projection_heads: Option<Vec<ChangeHash>>,
    pending_local_changes: Vec<LocalContactChange>,
}

impl ContactsApp {
    /// The live app. Never fabricates sample content.
    ///
    /// Nothing here touches the disk any more: the address book lives in the
    /// shared Automerge document, which `main` opens through `ui::persist`
    /// and which is written back by `App::with_autosave`.
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
        Self {
            seed_samples,
            search_provider: crate::search_provider::ContactSearchProvider::new(&store),
            store,
            resource_index: ContactsResourceIndex::default(),
            search_heads: Vec::new(),
            #[cfg(test)]
            resource_snapshot_materialized: Cell::new(0),
            #[cfg(test)]
            resource_exact_materialized: Cell::new(0),
            #[cfg(test)]
            resource_index_rebuilds: Cell::new(0),
            #[cfg(test)]
            local_change_index_lookups: Cell::new(0),
            selected: None,
            mode: Mode::View,
            form: None,
            search: TextInput::local().with_id("contacts_search"),
            sort: SortKey::Name,
            group_filter: None,
            popup: None,
            popup_focus: 0,
            toast: None,
            confirm_delete: None,
            delete_dialog: None,
            import_conflict_dialog: None,
            duplicate_review: None,
            duplicate_candidate: None,
            duplicate_candidate_heads: Vec::new(),
            export_dialog: ExportDialog::new(),
            pending_switch: None,
            unsaved_focus: UnsavedFocus::Cancel,
            rect: Rect::ZERO,
            split_pos: 0.34,
            list_scroll: 0.0,
            detail_scroll: 0.0,
            divider_drag: false,
            divider_hover: false,
            list_rect: Rect::ZERO,
            detail_rect: Rect::ZERO,
            list_body: Rect::ZERO,
            visible_row_count: 0,
            list_row_start: 0,
            list_rows: Vec::new(),
            empty_action_rect: Rect::ZERO,
            empty_action_focused: false,
            empty_list_action: EmptyListAction::None,
            hit: Hit::default(),
            detail_content_h: 0.0,
            detail_field_hits: Vec::new(),
            website_link: None,
            last_detail_click: None,
            push: PushNav::default(),
            narrow: false,
            text_scale: 1.0,
            label_growth: LabelGrowth::default(),
            spacing: Spacing::default(),
            metrics: Metrics::BASE,
            form_m: FormMetrics::new(
                &Spacing::default(),
                &Metrics::BASE,
                1.0,
                &LabelGrowth::default(),
            ),
            last_mouse: Point::ZERO,
            dirty: true,
            relayout: true,
            seeded: false,
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

    /// Deterministically snap the narrow single-pane push flow to the
    /// detail pane (with its Back affordance) for `--shots`, skipping the
    /// slide animation so a headless frame captures the settled pushed
    /// detail rather than one mid-transition.
    pub fn shots_show_detail(&mut self) {
        self.push.snap(PushPane::Detail);
    }

    /// Snap into the New Contact form for `--shots` (placeholders + the
    /// repeated phone/email rows visible). Mirrors the `+ New` click. The
    /// form lives in the detail pane, so we also snap the responsive push to
    /// the detail pane — harmless on a wide viewport (the push is inert when
    /// docked side by side), and on a narrow viewport it captures the pushed
    /// form rather than the list.
    pub fn shots_new_contact(&mut self) {
        self.begin_new_contact();
        self.push.snap(PushPane::Detail);
    }

    /// Snap the EDIT form open on the selected contact for `--shots`, so a
    /// headless frame captures the edit view (title header row + Favorite
    /// control + Save/Cancel) for visual inspection of the header layout.
    pub fn shots_edit_contact(&mut self) {
        if let Some(c) = self.selected.and_then(|id| self.store.get(id)).cloned() {
            self.begin_edit(c, false);
            self.push.snap(PushPane::Detail);
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
            self.push.snap(PushPane::Detail);
        }
    }

    /// Show the lower half of the long edit form. This makes the 200%/phone
    /// screenshot matrix prove that address, birthday, website, notes and
    /// groups remain reachable rather than merely checking the sticky header.
    pub fn shots_edit_lower(&mut self) {
        self.shots_edit_contact();
        self.detail_scroll = 1_840.0;
        self.relayout = true;
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

    /// Show the imported-UID conflict review for screenshot inspection. The
    /// candidate is deliberately different from the selected sample record,
    /// matching the safe runtime path without mutating the shot document.
    pub fn shots_show_import_conflict(&mut self) {
        if let Some(existing) = self.selected.and_then(|id| self.store.get(id)).cloned() {
            let mut incoming = existing.clone();
            incoming.notes = "Imported file has different notes".into();
            self.import_conflicts = vec![ImportConflict {
                token: uuid::Uuid::new_v4(),
                existing: existing.id,
                incoming,
            }];
            self.import_conflict_dialog = None;
            self.dirty = true;
        }
    }

    /// Max-conflict duplicate review for the visual matrix.  This touches only
    /// the hermetic shot store: it proves the seven-row field chooser remains
    /// readable at 390×844/200% without manufacturing a collaborative write.
    pub fn shots_show_duplicate_review(&mut self) {
        let mut contacts = self.store.contacts.clone();
        contacts.sort_by_key(|contact| contact.id);
        let Some(survivor) = contacts.first().cloned() else {
            return;
        };
        let Some(mut source) = contacts.get(1).cloned() else {
            return;
        };
        source.first = "Augusta".into();
        source.last = "King".into();
        source.org = "Alternative organization".into();
        source.title = "Different role".into();
        source.website = "https://other.example.test".into();
        source.notes = "A deliberately longer alternate note for the review layout.".into();
        source.birthday = Some(Birthday::MonthDay { month: 12, day: 10 });
        source.emails = vec![Labeled::new("home", "ada@example.test")];
        let mut survivor = survivor;
        survivor.org = "Primary organization".into();
        survivor.title = "Primary role".into();
        survivor.website = "https://primary.example.test".into();
        survivor.notes = "A deliberately longer primary note for the review layout.".into();
        survivor.birthday = Some(Birthday::MonthDay { month: 1, day: 1 });
        survivor.emails = vec![Labeled::new("home", "ada@example.test")];
        self.store.upsert(survivor.clone());
        self.store.upsert(source.clone());
        if let Some(candidate) = crate::actions::duplicate_candidate(&survivor, &source) {
            self.duplicate_review = Some(DuplicateReview {
                survivor: survivor.clone(),
                source,
                reasons: candidate.reasons,
                take_from_source: std::collections::HashSet::new(),
                focus: 0,
                scroll: 0.0,
            });
            self.selected = Some(survivor.id);
            self.dirty = true;
            self.relayout = true;
        }
    }

    /// Stage one local duplicate suggestion for the screenshot matrix. The
    /// live app obtains this candidate only from synced address-book data;
    /// this hook does not write a contact or a merge decision into the shot
    /// document.
    pub fn shots_show_duplicate_discovery(&mut self) {
        let Some(survivor) = self.selected.and_then(|id| self.store.get(id)).cloned() else {
            return;
        };
        let mut source = survivor;
        source.id = uuid::Uuid::new_v4();
        source.notes = "A second import with the same identity; review before merging.".into();
        self.store.upsert(source);
        self.duplicate_candidate = crate::actions::duplicate_candidates(&self.store.contacts, 1)
            .into_iter()
            .next();
        self.duplicate_candidate_heads.clone_from(&self.model_heads);
        self.dirty = true;
        self.relayout = true;
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

    /// Populate a deterministic filtered-result state for visual review.
    pub fn shots_show_search(&mut self) {
        self.search.set_text("Grace");
        self.selected = self.visible_ids().first().copied();
        self.dirty = true;
        self.relayout = true;
    }

    /// Keep an inert parser receiver alive for one screenshot frame so the
    /// toolbar exposes the real busy copy and accessibility state without
    /// opening a host file portal.
    pub fn shots_show_loading(&mut self) {
        let (_sender, receiver) = mpsc::channel();
        self.import_parse_rx = Some(receiver);
        self.dirty = true;
    }

    /// Show the non-blocking failure feedback used by import/export errors.
    pub fn shots_show_error(&mut self) {
        self.show_toast("Import failed: this vCard could not be read");
    }

    /// Snap a right-click context menu open on the selected contact row for
    /// `--shots`.
    pub fn shots_show_context_menu(&mut self) {
        if let Some(id) = self.selected {
            // The screenshot hook runs before its first layout pass. Defer the
            // anchor instead of guessing a row position that could point at a
            // different person than the menu target.
            self.open_popup(Popup::ContextMenu {
                target: id,
                anchor: Rect::ZERO,
            });
        }
    }

    /// Open the durable group filter for `--shots`.  This gives visual
    /// regression review a direct state for the group workflow rather than
    /// relying on a pointer interaction unavailable to the one-frame runner.
    pub fn shots_show_group_filter(&mut self) {
        self.open_popup(Popup::GroupFilter {
            anchor: self.hit.group,
        });
        self.dirty = true;
    }

    /// Ids currently visible after search + group filter, in sort order.
    fn visible_ids(&self) -> Vec<ContactId> {
        if self.group_filter.as_deref() == Some(FAVORITES_FILTER) {
            let query = self.search.value();
            let mut contacts = self
                .store
                .contacts
                .iter()
                .filter(|contact| contact.favorite && contact.matches(query))
                .collect::<Vec<_>>();
            contacts.sort_by(|left, right| crate::model::sort_cmp(left, right, self.sort));
            return contacts.into_iter().map(|contact| contact.id).collect();
        }
        self.store
            .filtered_sorted(self.search.value(), self.group_filter.as_deref(), self.sort)
    }

    fn group_filter_options(&self) -> Vec<String> {
        let mut options = vec!["All".to_string(), "Favorites".to_string()];
        options.extend(self.store.all_groups());
        options
    }

    fn show_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), ui::time::now()));
        self.dirty = true;
    }

    fn open_popup(&mut self, popup: Popup) {
        self.popup = Some(popup);
        self.popup_focus = 0;
        self.dirty = true;
    }

    fn clear_filters(&mut self) {
        self.search.set_text("");
        self.group_filter = None;
        self.list_scroll = 0.0;
        if self.selected.is_none() {
            self.selected = self.visible_ids().first().copied();
        }
        self.empty_action_focused = false;
        self.relayout = true;
        self.dirty = true;
    }

    fn activate_empty_list_action(&mut self) -> bool {
        match self.empty_list_action {
            EmptyListAction::Create => self.begin_new_contact(),
            EmptyListAction::ClearFilters => self.clear_filters(),
            EmptyListAction::None => return false,
        }
        true
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

    /// Enter the same blank-contact flow from every affordance (toolbar,
    /// first-run state, keyboard and assistive technology).
    fn begin_new_contact(&mut self) {
        let mut contact = Contact::new();
        contact.phones.push(Labeled::new("mobile", ""));
        contact.emails.push(Labeled::new("home", ""));
        self.empty_action_focused = false;
        self.begin_edit(contact, true);
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
            let action = if form.is_new {
                crate::actions::create_contact_action(&c)
            } else {
                crate::actions::patch_contact_action(&form.base, &c)
            };
            match self.try_perform_action(doc, &action) {
                Ok(()) => {
                    self.store.upsert(c);
                    self.selected = Some(id);
                    Self::persist();
                    self.show_toast(if form.is_new {
                        "Contact created"
                    } else {
                        "Saved"
                    });
                    self.mode = Mode::View;
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
                    self.mode = Mode::Edit;
                    self.show_toast(format!("Couldn't save contact: {error}"));
                }
            }
        }
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

    /// Attempt the canonical action route shared with the Ctrl+K palette.
    /// Unlike the compatibility dispatch wrapper, this exposes a rejected
    /// operation so local projection updates cannot falsely claim success.
    fn try_perform_action(
        &mut self,
        doc: &mut AutoCommit,
        action: &crate::actions::ContactsAction,
    ) -> Result<(), tilo_actions::ActionError> {
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
        let stale: Vec<_> = self
            .store
            .contacts
            .iter()
            .map(|contact| contact.id)
            .filter(|id| !keep.contains(id))
            .collect();
        for id in stale {
            self.store.delete(id);
        }
        for contact in &state.contacts {
            self.store.upsert(contact.clone());
        }
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
            // optimistic list update. Repairing legacy row ids is the same:
            // the next sync reads the repaired book.
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
        error: &tilo_actions::ActionError,
    ) {
        self.store.contacts = ContactsDoc::list_contacts(doc);
        if self.selected.is_some_and(|id| self.store.get(id).is_none()) {
            self.selected = self.visible_ids().first().copied();
        }
        self.show_toast(format!("Couldn't {operation}: {error}"));
        self.relayout = true;
        self.dirty = true;
    }

    /// Toggle the favourite flag through the sole published action route.
    /// Every painted, semantic, keyboard, and menu affordance calls this so a
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
                Self::persist();
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
            self.show_toast("Imported conflicting contact as new");
        } else {
            self.show_toast("Kept the existing contact");
        }
        self.import_conflicts.remove(index);
        self.import_conflict_dialog = None;
        self.relayout = true;
        self.dirty = true;
        Ok(())
    }

    /// The last action dispatched, for the `affordance == ActionSpec` covenant
    /// tests.
    #[cfg(test)]
    fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
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
    /// hot layout/paint/a11y paths. The result is derived presentation state:
    /// no duplicate index or merge intent is written into the shared book.
    fn refresh_duplicate_candidate(&mut self) {
        if self.duplicate_candidate_heads == self.model_heads {
            return;
        }
        self.duplicate_candidate = crate::actions::duplicate_candidates(&self.store.contacts, 1)
            .into_iter()
            .next();
        self.duplicate_candidate_heads.clone_from(&self.model_heads);
    }

    fn has_duplicate_candidate(&self) -> bool {
        self.duplicate_candidate.is_some()
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
            export_busy: self.export_dialog.is_open() || self.export_rx.is_some(),
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
            export_busy: self.export_dialog.is_open() || self.export_rx.is_some(),
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
    /// store. Collects the live input values into a throwaway draft and
    /// compares against the stored record (or, for a brand-new contact,
    /// reports dirty when any field has content). Drives the
    /// "Save changes?" guard when the user tries to switch contacts.
    fn form_is_dirty(&self) -> bool {
        let Some(form) = &self.form else { return false };
        // Fold the live input values into a candidate draft without mutating
        // the real form.
        let mut candidate = EditForm {
            base: form.base.clone(),
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
            self.unsaved_focus = UnsavedFocus::Cancel;
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

    /// Reveal a durable membership group through its canonical view action.
    /// This is intentionally presentation-only: the group already exists as
    /// collaborative contact-row data, so opening it must not fabricate a
    /// second mutable index or history entry. In narrow mode the useful result
    /// is the filtered list, not a hidden detail pane.
    fn reveal_group(&mut self, requested: &str) -> Result<(), String> {
        let group = self
            .store
            .all_groups()
            .into_iter()
            .find(|group| group_names_equal(group, requested))
            .ok_or_else(|| "group no longer exists".to_string())?;
        self.search.set_text("");
        self.set_group_filter(Some(group));
        if self.narrow {
            self.push.show_master();
        }
        self.dirty = true;
        Ok(())
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
        let action = crate::actions::delete_contact_action(id.to_string());
        match self.try_perform_action(doc, &action) {
            Ok(()) => {
                self.store.delete(id);
                if self.selected == Some(id) {
                    self.selected = self.visible_ids().first().copied();
                }
                Self::persist();
                self.show_toast("Contact deleted");
            }
            Err(error) => self.reconcile_refused_action(doc, "delete contact", &error),
        }
        self.confirm_delete = None;
        self.delete_dialog = None;
        self.relayout = true;
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
        self.scroll_list_index_into_view(next, ids.len());
        self.detail_scroll = 0.0;
        self.dirty = true;
        self.relayout = true;
    }

    /// Open the selected contact's existing contextual actions from the
    /// keyboard.  The visual menu remains a transient view concern, but every
    /// item delegates to the canonical action / operation route in
    /// [`Self::execute_context_action`].  On the narrow pushed detail we
    /// anchor at the visible Edit control rather than an off-screen list row.
    fn open_selected_context_menu(&mut self) -> bool {
        let Some(id) = self.selected.filter(|id| self.store.get(*id).is_some()) else {
            return false;
        };

        let anchor = if !self.narrow || self.push.pane() == PushPane::Master {
            let ids = self.visible_ids();
            let Some(index) = ids.iter().position(|candidate| *candidate == id) else {
                return false;
            };
            self.scroll_list_index_into_view(index, ids.len());
            // The selected row may have just been scrolled into the
            // virtualised window; refresh its bounded geometry before using
            // it as the popup anchor.
            self.layout();
            self.list_rows
                .iter()
                .find(|(row_id, _)| *row_id == id)
                .map(|(_, row)| {
                    let center = Point::new(row.x0 + 40.0, row.center().y);
                    Rect::new(center.x, center.y, center.x, center.y)
                })
        } else {
            let center = self.hit.edit.center();
            Some(Rect::new(center.x, center.y, center.x, center.y))
        };

        let Some(anchor) = anchor else {
            return false;
        };
        self.open_popup(Popup::ContextMenu { target: id, anchor });
        self.dirty = true;
        true
    }

    /// Keep keyboard selection discoverable.  The list's physical rows are
    /// virtualised, so moving the selection beyond the viewport must also move
    /// the viewport before the next layout creates that row.
    fn scroll_list_index_into_view(&mut self, index: usize, total: usize) {
        if total == 0 || self.list_body.height() <= 0.0 {
            return;
        }
        let row_h = self.row_metrics().row_h;
        let row_top = index as f64 * row_h;
        let row_bottom = row_top + row_h;
        let viewport_top = self.list_scroll;
        let viewport_bottom = viewport_top + self.list_body.height();
        if row_top < viewport_top {
            self.list_scroll = row_top;
        } else if row_bottom > viewport_bottom {
            self.list_scroll = row_bottom - self.list_body.height();
        }
        let max_scroll = (total as f64 * row_h - self.list_body.height()).max(0.0);
        self.list_scroll = self.list_scroll.clamp(0.0, max_scroll);
    }

    /// Live Import action: open the host's native multi-select file chooser
    /// (XDG portal) filtered to vCard files. Non-blocking — the D-Bus
    /// round-trip and the host dialog run on a worker thread; the chosen paths
    /// arrive over a channel drained by [`drain_portal`]. An in-flight request
    /// is not restarted.
    fn request_import(&mut self) {
        if self.import_rx.is_some() || self.import_parse_rx.is_some() {
            return;
        }
        self.import_rx = Some(contacts_import_request().send_multiple());
    }

    /// Read and parse every vCard file in `paths` into the address
    /// book, persisting once if anything was added. An external `UID` is only
    /// used to identify an existing contact: a changed collision is kept out
    /// of the shared document and returned as a review conflict. This is the
    /// hermetic core shared by the live portal
    /// drain and the tests — it never touches the picker, so multi-file import
    /// can be driven directly without a desktop portal.
    #[cfg(test)]
    fn import_paths(&mut self, doc: &mut AutoCommit, paths: &[PathBuf]) -> ImportSummary {
        if paths.is_empty() {
            // An empty selection (or a cancelled picker) fabricates nothing.
            return ImportSummary::default();
        }
        let mut summary = ImportSummary::default();
        let mut first_new = None;
        for path in paths {
            let text = match Self::read_import_file(path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "contacts: import read failed");
                    summary.errors += 1;
                    continue;
                }
            };
            // Each file may hold many, differently-shaped cards; parsing is
            // intentionally bounded so one large or malformed import cannot
            // monopolise the UI completion path.
            let report = vcard::parse_vcards_report(&text);
            summary.errors += report.skipped_cards;
            summary.errors += usize::from(report.limit_reached);
            for c in report.contacts {
                self.absorb_import_contact(doc, c, &mut summary, &mut first_new);
            }
        }
        if let Some(id) = first_new {
            self.selected = Some(id);
        }
        if summary.created > 0 {
            Self::persist();
            self.show_import_summary(&summary);
            self.relayout = true;
        } else if summary.unchanged > 0 || summary.conflicts > 0 || summary.errors > 0 {
            self.show_import_summary(&summary);
        } else {
            // A valid but empty/unparseable selection: say so rather than
            // silently doing nothing.
            self.show_toast("No contacts found in the selected file(s)");
        }
        summary
    }

    fn absorb_import_contact(
        &mut self,
        doc: &mut AutoCommit,
        c: Contact,
        summary: &mut ImportSummary,
        first_new: &mut Option<ContactId>,
    ) {
        if let Some(existing) = self.store.get(c.id) {
            if same_import_payload(existing, &c) {
                summary.unchanged += 1;
            } else {
                let contact_id = c.id;
                if self.import_conflicts.len() < MAX_IMPORT_CONFLICTS {
                    self.import_conflicts.push(ImportConflict {
                        token: uuid::Uuid::new_v4(),
                        existing: existing.id,
                        incoming: c,
                    });
                    summary.conflicts += 1;
                } else {
                    // Do not claim this collision is reviewable when the
                    // bounded review queue deliberately declined it.
                    summary.errors += 1;
                }
                tracing::warn!(%contact_id, "contacts: import UID collision needs review");
            }
            return;
        }
        let action = crate::actions::create_contact_action(&c);
        match self.try_perform_action(doc, &action) {
            Ok(()) => {
                if first_new.is_none() {
                    *first_new = Some(c.id);
                }
                self.store.upsert(c);
                summary.created += 1;
            }
            Err(error) => {
                summary.errors += 1;
                tracing::warn!(error = %error, "contacts: import action refused");
                self.store.contacts = ContactsDoc::list_contacts(doc);
            }
        }
    }

    fn apply_parsed_import(&mut self, doc: &mut AutoCommit, parsed: ParsedImport) -> ImportSummary {
        let mut summary = ImportSummary {
            errors: parsed.errors,
            ..ImportSummary::default()
        };
        let mut first_new = None;
        for contact in parsed.contacts {
            self.absorb_import_contact(doc, contact, &mut summary, &mut first_new);
        }
        if let Some(id) = first_new {
            self.selected = Some(id);
        }
        if summary.created > 0 {
            Self::persist();
            self.relayout = true;
        }
        if summary.created > 0
            || summary.unchanged > 0
            || summary.conflicts > 0
            || summary.errors > 0
        {
            self.show_import_summary(&summary);
        } else {
            self.show_toast("No contacts found in the selected file(s)");
        }
        summary
    }

    fn start_import_parse(&mut self, paths: Vec<PathBuf>) -> Result<(), String> {
        if paths.len() > MAX_IMPORT_FILES {
            return Err(format!(
                "select at most {MAX_IMPORT_FILES} vCard files at once"
            ));
        }
        let mut total = 0_u64;
        for path in &paths {
            let metadata = file_metadata(path)?;
            if metadata.kind != tilo_files::Kind::File {
                return Err("selected path is not a regular file".into());
            }
            total = total.saturating_add(metadata.len);
            if total > MAX_IMPORT_TOTAL_BYTES {
                return Err(format!(
                    "selected files exceed the {MAX_IMPORT_TOTAL_BYTES}-byte import limit"
                ));
            }
        }
        let generation = self.advance_import_generation()?;
        let (sender, receiver) = mpsc::sync_channel(1);
        ui::rt::spawn_thread("contacts-vcard-parser", move || {
            let mut contacts = Vec::new();
            let mut errors = 0;
            for path in paths {
                match Self::read_import_file(&path) {
                    Ok(text) => {
                        let report = vcard::parse_vcards_report(&text);
                        errors += report.skipped_cards + usize::from(report.limit_reached);
                        for c in report.contacts {
                            if contacts.len() < MAX_IMPORT_TOTAL_CARDS {
                                contacts.push(c);
                            } else {
                                errors += 1;
                                break;
                            }
                        }
                    }
                    Err(_) => errors += 1,
                }
            }
            let _ = sender.send(ParsedImport {
                generation,
                contacts,
                errors,
            });
        })
        .map_err(|error| error.to_string())?;
        self.import_parse_rx = Some(receiver);
        Ok(())
    }

    /// Allocate a parser generation without ever reusing the initial or a
    /// previously-issued value.  A wrap would let a delayed worker from an
    /// ancient import look current, so exhaustion permanently closes imports
    /// for this app instance instead of silently returning to generation 0.
    fn advance_import_generation(&mut self) -> Result<u64, String> {
        let generation = self.import_generation.checked_add(1).ok_or_else(|| {
            "import generation exhausted; restart Contacts before importing again".to_string()
        })?;
        self.import_generation = generation;
        Ok(generation)
    }

    fn read_import_file(path: &std::path::Path) -> Result<String, String> {
        let metadata = file_metadata(path)?;
        if metadata.kind != tilo_files::Kind::File {
            return Err("selected path is not a regular file".into());
        }
        if metadata.len > MAX_IMPORT_FILE_BYTES {
            return Err(format!(
                "file exceeds the {MAX_IMPORT_FILE_BYTES}-byte import limit"
            ));
        }
        let bytes = tilo_files::file_at(path)
            .and_then(|(dir, name)| tilo_files::blocking::read(&dir, &name))
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_IMPORT_FILE_BYTES {
            return Err(format!(
                "file exceeds the {MAX_IMPORT_FILE_BYTES}-byte import limit"
            ));
        }
        String::from_utf8(bytes).map_err(|_| "vCard file is not valid UTF-8 text".into())
    }

    fn show_import_summary(&mut self, summary: &ImportSummary) {
        let mut parts = Vec::new();
        if summary.created > 0 {
            parts.push(format!(
                "Imported {} contact{}",
                summary.created,
                if summary.created == 1 { "" } else { "s" }
            ));
        }
        if summary.unchanged > 0 {
            parts.push(format!("{} already current", summary.unchanged));
        }
        if summary.conflicts > 0 {
            parts.push(format!(
                "{} need{} review before merging",
                summary.conflicts,
                if summary.conflicts == 1 { "s" } else { "" }
            ));
        }
        if summary.errors > 0 {
            parts.push(format!("{} couldn't be imported", summary.errors));
        }
        self.show_toast(parts.join("; "));
    }

    /// Whether a file-chooser portal (import or export) is currently awaiting
    /// the user's choice. Keeps the host loop pumping frames so the outcome is
    /// drained promptly once the dialog closes.
    fn portal_in_flight(&self) -> bool {
        self.import_rx.is_some() || self.import_parse_rx.is_some() || self.export_rx.is_some()
    }

    fn finish_resource_file(&mut self, success: bool, error: Option<String>) {
        let Some(invocation) = self.resource_invocation.take() else {
            return;
        };
        self.completed_resource_invocation = Some(if success {
            tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            }
        } else if let Some(message) = error {
            tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "contacts.file_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            }
        } else {
            tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("file selection cancelled".into()),
            }
        });
    }

    /// Drain any in-flight import/export portal request. Called once per frame
    /// from [`Widget::sync`]; a `try_recv` never blocks the UI thread. When an
    /// outcome arrives the receiver is dropped (idle again).
    fn drain_portal(&mut self, doc: &mut AutoCommit) {
        if let Some(rx) = &self.import_rx {
            match rx.try_recv() {
                Ok(ImportOutcome::Selected(paths)) => {
                    self.import_rx = None;
                    if let Err(error) = self.start_import_parse(paths) {
                        self.show_toast(format!("Couldn't start import: {error}"));
                        self.finish_resource_file(false, Some(error));
                    }
                }
                Ok(ImportOutcome::Cancelled) => {
                    // A dismissed picker is a silent no-op — nothing to say.
                    self.import_rx = None;
                    self.finish_resource_file(false, None);
                }
                Ok(ImportOutcome::Failed(reason)) => {
                    self.import_rx = None;
                    self.show_toast(format!("Import unavailable: {reason}"));
                    self.finish_resource_file(false, Some(reason));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.import_rx = None;
                    self.finish_resource_file(
                        false,
                        Some("import picker stopped unexpectedly".into()),
                    );
                }
            }
        }
        if let Some(rx) = &self.import_parse_rx {
            match rx.try_recv() {
                Ok(parsed) => {
                    self.import_parse_rx = None;
                    if parsed.generation == self.import_generation {
                        let imported = self.apply_parsed_import(doc, parsed);
                        let success = imported.completed();
                        self.finish_resource_file(
                            success,
                            (!success).then(|| imported.failure_message()),
                        );
                    } else {
                        // A superseded parser is never allowed to apply its
                        // contacts.  It must still settle a previously
                        // accepted broker invocation rather than leaving that
                        // job pending forever.
                        self.finish_resource_file(
                            false,
                            Some("import was superseded before parsing finished".into()),
                        );
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.import_parse_rx = None;
                    self.finish_resource_file(
                        false,
                        Some("import parser stopped unexpectedly".into()),
                    );
                }
            }
        }
        if let Some(rx) = &self.export_rx {
            match rx.try_recv() {
                Ok(ExportOutcome::Opened(path)) => {
                    self.export_rx = None;
                    if let Some((ids, version)) = self.pending_export.take() {
                        self.write_export(&ids, version, &path);
                        self.finish_resource_file(
                            path.is_file(),
                            (!path.is_file()).then(|| "vCard export was not written".into()),
                        );
                    }
                }
                Ok(ExportOutcome::Cancelled) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.export_rx = None;
                    self.pending_export = None;
                    self.finish_resource_file(false, None);
                }
                Ok(ExportOutcome::Failed(reason)) => {
                    self.export_rx = None;
                    self.pending_export = None;
                    self.show_toast(format!("Export unavailable: {reason}"));
                    self.finish_resource_file(false, Some(reason));
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
        self.export_dialog.open(pre);
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
        self.export_rx = Some(contacts_export_request(EXPORT_FILE_NAME).send());
        true
    }

    /// Start the version-selection workflow for one canonical contact
    /// resource.  This is intentionally the only path behind both the
    /// contact resource's `export-contact` operation and the row context
    /// menu: a stale/busy check therefore has identical behavior for agents,
    /// keyboard, assistive technology, and pointer input.
    fn open_contact_export(&mut self, id: ContactId) -> Result<(), String> {
        if self.store.get(id).is_none() {
            return Err("contact no longer exists".into());
        }
        if self.export_dialog.is_open()
            || self.export_rx.is_some()
            || self.resource_invocation.is_some()
        {
            return Err("a contact export is already in progress".into());
        }
        self.export_dialog.open([id]);
        self.dirty = true;
        Ok(())
    }

    /// Serialize the same canonical operation published by the contact
    /// resource, then route it through the normal decoder/validator.  The
    /// context-menu affordance intentionally does not call
    /// [`Self::open_contact_export`] directly: this keeps pointer, keyboard,
    /// AT-SPI and agent activation on one typed operation boundary.
    fn invoke_contact_export_operation(
        &mut self,
        state: &AutoCommit,
        id: ContactId,
    ) -> Result<bool, String> {
        let operation = crate::actions::export_contact_operation(id);
        let payload = serde_json::to_vec(&operation)
            .map_err(|error| format!("couldn't encode contact export: {error}"))?;
        self.answer_resource_operation(
            tilo_actions::OperationKind::Effect,
            crate::actions::CONTACTS_OPERATION_TYPE,
            &payload,
            state,
            &automerge::ROOT,
        )
    }

    /// Open the locally surfaced candidate through the exact typed operation
    /// owned by its canonical duplicate resource. The toolbar is therefore a
    /// discoverability affordance, not a second private merge path: global
    /// search, an agent, keyboard semantics, and pointer activation all land
    /// in the same field-by-field review surface.
    fn invoke_duplicate_review_operation(&mut self, state: &AutoCommit) -> Result<bool, String> {
        let candidate = self
            .duplicate_candidate
            .as_ref()
            .ok_or_else(|| "there are no duplicate contacts to review".to_string())?;
        let operation = crate::actions::open_duplicate_review_operation(candidate);
        let payload = serde_json::to_vec(&operation)
            .map_err(|error| format!("couldn't encode duplicate review: {error}"))?;
        self.answer_resource_operation(
            tilo_actions::OperationKind::View,
            crate::actions::CONTACTS_OPERATION_TYPE,
            &payload,
            state,
            &automerge::ROOT,
        )
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
        let written = tilo_files::file_at(path).and_then(|(dir, name)| {
            tilo_files::blocking::create_dir_all(&dir, &tilo_files::VPath::root())?;
            tilo_files::blocking::save_document(&dir, &name, body.as_bytes())
        });
        match written {
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
    fn layout_scale(&self) -> f64 {
        f64::from(self.text_scale.clamp(1.0, 2.0))
    }

    #[cfg(test)]
    fn row_h(&self) -> f64 {
        ROW_H * self.layout_scale()
    }

    fn layout(&mut self) {
        let r = self.rect;
        // Toolbar across the top; its height follows the people-count line.
        let toolbar_h = self.toolbar_h();
        let content_top = r.y0 + toolbar_h;
        let content = Rect::new(r.x0, content_top, r.x1, r.y1);

        self.layout_toolbar_buttons(r, toolbar_h);

        // Responsive split: side-by-side when wide, single-pane push when
        // narrow. The choice is driven purely from the allocated width.
        let scale = self.layout_scale();
        let avail_w = content.width();
        self.narrow = crate::responsive::is_narrow_at(avail_w, self.text_scale);
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
                self.detail_rect.y0 + 14.0 * scale,
                self.detail_rect.x0 + PAD + 92.0 * scale.min(1.35),
                self.detail_rect.y0 + 14.0 * scale + 34.0 * scale,
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

        // List header: search + filter row. Control heights follow the text
        // they carry, so a 2× placeholder or filter label is never clipped by
        // a 1× field; at 1× the authored heights win.
        // The authored 38px field (17.5 + 16 sits inside it), or the
        // placeholder line plus an `s2` above and below, rounded up.
        let search_h = at_least_whole(
            FIELD_H,
            line_h(text_base(), self.text_scale) + 2.0 * f64::from(self.spacing.s2),
        );
        // The filter pills keep their authored height: the shared `Chip`
        // derives its label size from the pill height, so growing the pill
        // here would only enlarge a label the toolkit already double-scales
        // (see the review's framework findings).
        let filter_h = FILTER_H;
        let search_y = self.list_rect.y0 + PAD;
        self.hit.search = Rect::new(
            self.list_rect.x0 + PAD,
            search_y,
            self.list_rect.x1 - PAD,
            search_y + search_h,
        );
        self.search.apply_layout_external(self.hit.search);
        let filt_y = self.hit.search.y1 + 10.0;
        let half = (self.list_rect.width() - PAD * 2.0 - 8.0) / 2.0;
        self.hit.sort = Rect::new(
            self.list_rect.x0 + PAD,
            filt_y,
            self.list_rect.x0 + PAD + half,
            filt_y + filter_h,
        );
        self.hit.group = Rect::new(
            self.hit.sort.x1 + 8.0,
            filt_y,
            self.list_rect.x1 - PAD,
            filt_y + filter_h,
        );

        // List body (scroll region).
        let body_top = self.hit.group.y1 + 12.0;
        self.list_body = Rect::new(
            self.list_rect.x0,
            body_top,
            self.list_rect.x1,
            self.list_rect.y1,
        );

        // Clamp + compute only the viewport window of row rects.  Do not make
        // one Rect per contact here: large shared books should stay equally
        // responsive whether they contain ten people or ten thousand.
        let ids = self.visible_ids();
        self.visible_row_count = ids.len();
        self.empty_list_action = self.empty_list_action_for(ids.is_empty());
        let row_h = self.row_metrics().row_h;
        let content_h = ids.len() as f64 * row_h;
        let max_scroll = (content_h - self.list_body.height()).max(0.0);
        self.list_scroll = self.list_scroll.clamp(0.0, max_scroll);
        self.list_rows.clear();
        let first_visible = (self.list_scroll / row_h).floor().max(0.0) as usize;
        let visible_rows = (self.list_body.height() / row_h).ceil().max(1.0) as usize;
        let start = first_visible
            .saturating_sub(LIST_OVERSCAN_ROWS)
            .min(ids.len());
        let end = first_visible
            .saturating_add(visible_rows)
            .saturating_add(LIST_OVERSCAN_ROWS)
            .min(ids.len());
        self.list_row_start = start;
        let mut ry = self.list_body.y0 - self.list_scroll + start as f64 * row_h;
        for id in ids
            .iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .copied()
        {
            let rr = Rect::new(self.list_body.x0, ry, self.list_body.x1, ry + row_h);
            self.list_rows.push((id, rr));
            ry += row_h;
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
        if self.narrow {
            44.0 * self.layout_scale()
        } else {
            0.0
        }
    }

    /// At phone-sized widths, preserve all three toolbar actions without
    /// letting fixed English labels overlap.  The compact glyph labels have
    /// explicit accessible names and host tooltips (see [`Widget::tooltip`]).
    /// Below this width the toolbar buttons collapse to icon-only faces. The
    /// breakpoint grows with the same factor the labelled button widths do,
    /// so a scaled row never asks for more width than the window has.
    fn toolbar_is_compact(&self) -> bool {
        self.rect.width() < 520.0 * f64::from(self.text_scale.max(1.0))
    }

    /// List-row geometry at the current text scale — one description shared
    /// by the virtualised row rects, scroll extent, ensure-visible and paint.
    fn row_metrics(&self) -> ContactRowMetrics {
        ContactRowMetrics::new(&self.spacing, self.text_scale)
    }

    /// Toolbar height: the theme's `s12` strip (48px at 1×), or the
    /// people-count line plus an `s3` above and below once the type has grown
    /// past that. Layout and paint share it.
    fn toolbar_h(&self) -> f64 {
        at_least_whole(
            f64::from(self.spacing.s12),
            line_h(text_base(), self.text_scale) + 2.0 * f64::from(self.spacing.s3),
        )
    }

    /// Toolbar button height: the theme's minimum pointer target (32px at
    /// 1×), or the 13pt label line plus an `s2`, so a scaled "Import" never
    /// spills out of its face.
    fn toolbar_button_h(&self) -> f64 {
        at_least_whole(
            f64::from(self.metrics.min_pointer_target),
            line_h(TOOLBAR_BUTTON_TEXT, self.text_scale) + f64::from(self.spacing.s2),
        )
    }

    /// Toolbar buttons (right-aligned), centred on the bar. Each face keeps
    /// its authored 1× width and grows by exactly how much wider its measured
    /// label is at the current scale (`toolbar_label_growth`, captured in
    /// `render`); the compact icon-only faces stay square to the button height.
    fn layout_toolbar_buttons(&mut self, r: Rect, toolbar_h: f64) {
        let btn_h = self.toolbar_button_h();
        let btn_y = r.y0 + (toolbar_h - btn_h) / 2.0;
        let compact_toolbar = self.toolbar_is_compact();
        let g = self.label_growth;
        let mut x = r.x1 - PAD;
        let compact_w = 40.0_f64.max(btn_h);
        let face_w = |authored: f64, growth: f64| {
            if compact_toolbar {
                compact_w
            } else {
                (authored + growth).ceil()
            }
        };
        let new_w = face_w(TOOLBAR_NEW_W, g.new);
        self.hit.new = Rect::new(x - new_w, btn_y, x, btn_y + btn_h);
        x -= new_w + if compact_toolbar { 6.0 } else { 10.0 };
        let exp_w = face_w(TOOLBAR_FILE_W, g.export);
        self.hit.export = Rect::new(x - exp_w, btn_y, x, btn_y + btn_h);
        let gap = if compact_toolbar { 6.0 } else { 10.0 };
        x -= exp_w + gap;
        self.hit.duplicates = Rect::ZERO;
        if self.has_duplicate_candidate() {
            // This is the exact face drawn by `draw_toolbar`; its accessible
            // and pointer targets therefore share one responsive rectangle.
            let duplicate_w = face_w(TOOLBAR_DUPLICATE_W, g.duplicate);
            self.hit.duplicates = Rect::new(x - duplicate_w, btn_y, x, btn_y + btn_h);
            x -= duplicate_w + gap;
        }
        let imp_w = face_w(TOOLBAR_FILE_W, g.import);
        self.hit.import = Rect::new(x - imp_w, btn_y, x, btn_y + btn_h);
    }

    /// Measure how much wider each text-face label is at the renderer's
    /// scale than at 1× and, when that changed, request a relayout so the
    /// next frame's layout pass sizes the faces (and their hit rects) for it.
    /// Runs at the top of `render` — paint never re-runs layout itself; at 1×
    /// every growth is zero and nothing happens.
    fn refresh_label_growth(&mut self, tr: &mut TextRenderer) {
        let next = LabelGrowth::measure(tr);
        if !next.approx_eq(&self.label_growth) {
            self.label_growth = next;
            self.relayout = true;
            self.dirty = true;
        }
    }

    /// The edit form's row grid, as solved by the last `layout_form`.
    fn form_metrics(&self) -> FormMetrics {
        self.form_m
    }

    /// Top of the form's scrollable body: directly under the identity row
    /// and the actions row (the first field row sits one `s3` below). One
    /// derivation for `layout_form`, the body clip in `draw_form` and the
    /// ensure-visible scroll, so none of them can disagree.
    fn form_body_top(&self) -> f64 {
        let m = self.form_metrics();
        self.detail_rect.y0
            + f64::from(self.spacing.s4)
            + m.title_h
            + f64::from(self.spacing.s3)
            + m.action_h
            + self.detail_top_offset()
    }

    fn empty_list_action_for(&self, is_empty: bool) -> EmptyListAction {
        if self.store.is_empty() && self.mode == Mode::View {
            EmptyListAction::Create
        } else if is_empty && (!self.search.value().is_empty() || self.group_filter.is_some()) {
            EmptyListAction::ClearFilters
        } else {
            EmptyListAction::None
        }
    }

    /// View-mode detail action rects (favorite / Edit / Delete), pinned to
    /// the top-right of the detail pane. Recomputed whenever `detail_rect`
    /// moves (window resize, and every frame of the narrow push slide). On a
    /// narrow window they shift down to clear the Back affordance pill.
    fn layout_detail_actions(&mut self) {
        let scale = self.layout_scale();
        let ax1 = self.detail_rect.x1 - 24.0;
        let by = self.detail_rect.y0 + 30.0 * scale + self.detail_top_offset();
        let button_h = 32.0 * scale;
        self.hit.delete = Rect::new(ax1 - 84.0, by, ax1, by + button_h);
        self.hit.edit = Rect::new(
            self.hit.delete.x0 - 8.0 - 72.0,
            by,
            self.hit.delete.x0 - 8.0,
            by + button_h,
        );
        self.hit.fav = Rect::new(
            self.hit.edit.x0 - 8.0 - 32.0,
            by,
            self.hit.edit.x0 - 8.0,
            by + button_h,
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
    #[expect(
        clippy::too_many_lines,
        reason = "every form field's rect is assigned in one vertical pass sharing the running y cursor; the repeated groups are already split out"
    )]
    fn layout_form(&mut self) {
        let large_text = self.text_scale >= 1.75;
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
        // Solve the row grid once per layout; painters read the cached copy.
        self.form_m = FormMetrics::new(
            &self.spacing,
            &self.metrics,
            self.text_scale,
            &self.label_growth,
        );
        let m = self.form_m;
        let (s3, s4) = (f64::from(self.spacing.s3), f64::from(self.spacing.s4));
        let title_y = area.y0 + s4 + off;
        let by = title_y + m.title_h + s3; // controls row top
        let top = self.form_body_top() + s3; // first field row top
        let mut y = top - self.detail_scroll;

        self.hit.save = Rect::new(x1 - m.save_w, by, x1, by + m.action_h);
        self.hit.cancel = Rect::new(
            self.hit.save.x0 - 8.0 - m.cancel_w,
            by,
            self.hit.save.x0 - 8.0,
            by + m.action_h,
        );
        self.hit.fav_toggle = Rect::new(x0, by, x0 + m.fav_w, by + m.action_h);

        let label_h = m.label_h;
        let gap = 12.0;
        let nh = m.field_h;
        let half = (w - 10.0) / 2.0;

        let (n_phones, n_emails, n_addr) = {
            let Some(form) = self.form.as_ref() else {
                return;
            };
            let d = &form.draft;
            (d.phones.len(), d.emails.len(), d.addresses.len())
        };

        let mut plan: Vec<(Slot, Rect)> = Vec::new();

        // Name row: First | Last. At 200% each field gets a full line so its
        // text and caret are never squeezed into a half-width phone column.
        y += label_h;
        plan.push((
            Slot::First,
            Rect::new(x0, y, if large_text { x1 } else { x0 + half }, y + nh),
        ));
        y += nh + gap;
        if large_text {
            plan.push((Slot::Last, Rect::new(x0, y, x1, y + nh)));
            y += nh + gap;
        } else {
            plan.push((
                Slot::Last,
                Rect::new(x0 + half + 10.0, y - nh - gap, x1, y - gap),
            ));
        }

        // Org | Title
        y += label_h;
        plan.push((
            Slot::Org,
            Rect::new(x0, y, if large_text { x1 } else { x0 + half }, y + nh),
        ));
        y += nh + gap;
        if large_text {
            plan.push((Slot::Title, Rect::new(x0, y, x1, y + nh)));
            y += nh + gap;
        } else {
            plan.push((
                Slot::Title,
                Rect::new(x0 + half + 10.0, y - nh - gap, x1, y - gap),
            ));
        }

        self.layout_form_phones(x0, x1, n_phones, &mut y, &mut plan);
        self.layout_form_emails(x0, x1, n_emails, &mut y, &mut plan);
        self.layout_form_addresses(x0, x1, half, n_addr, &mut y, &mut plan);

        // Birthday (input + calendar button) | Website
        y += label_h;
        let birthday_span = if large_text { w } else { half };
        let bday_w = birthday_span - 40.0;
        plan.push((Slot::Birthday, Rect::new(x0, y, x0 + bday_w, y + nh)));
        self.hit.cal_btn = Rect::new(x0 + bday_w + 6.0, y, x0 + birthday_span, y + nh);
        y += nh + gap;
        if large_text {
            y += label_h;
            plan.push((Slot::Website, Rect::new(x0, y, x1, y + nh)));
            y += nh + gap;
        } else {
            plan.push((
                Slot::Website,
                Rect::new(x0 + half + 10.0, y - nh - gap, x1, y - gap),
            ));
        }

        // Notes (full width)
        y += label_h;
        let notes_h = nh * 3.0;
        plan.push((Slot::Notes, Rect::new(x0, y, x1, y + notes_h)));
        y += notes_h + gap;

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
        let m = self.form_metrics();
        let (label_h, gap, nh) = (m.label_h, 12.0, m.field_h);
        *y += label_h;
        for i in 0..n {
            // The type face keeps its authored height and centres in the
            // (possibly taller) row; see `FormMetrics::type_h`.
            let chip_y = *y + (nh - m.type_h) / 2.0;
            let chip = Rect::new(x0, chip_y, x0 + m.type_w, chip_y + m.type_h);
            let rm = Rect::new(x1 - m.remove_w, *y, x1, *y + nh);
            plan.push((
                Slot::Phone(i),
                Rect::new(chip.x1 + 8.0, *y, rm.x0 - 8.0, *y + nh),
            ));
            self.hit.type_chips.push((TypeTarget::Phone(i), chip));
            self.hit.remove_btns.push((TypeTarget::Phone(i), rm));
            *y += nh + 8.0;
        }
        self.hit.add_phone = Rect::new(x0, *y, x0 + m.add_w, *y + m.add_h);
        *y += m.add_h + gap;
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
        let m = self.form_metrics();
        let (label_h, gap, nh) = (m.label_h, 12.0, m.field_h);
        *y += label_h;
        for i in 0..n {
            // The type face keeps its authored height and centres in the
            // (possibly taller) row; see `FormMetrics::type_h`.
            let chip_y = *y + (nh - m.type_h) / 2.0;
            let chip = Rect::new(x0, chip_y, x0 + m.type_w, chip_y + m.type_h);
            let rm = Rect::new(x1 - m.remove_w, *y, x1, *y + nh);
            plan.push((
                Slot::Email(i),
                Rect::new(chip.x1 + 8.0, *y, rm.x0 - 8.0, *y + nh),
            ));
            self.hit.type_chips.push((TypeTarget::Email(i), chip));
            self.hit.remove_btns.push((TypeTarget::Email(i), rm));
            *y += nh + 8.0;
        }
        self.hit.add_email = Rect::new(x0, *y, x0 + m.add_w, *y + m.add_h);
        *y += m.add_h + gap;
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
        let large_text = self.text_scale >= 1.5;
        let m = self.form_metrics();
        let (label_h, gap, nh) = (m.label_h, 12.0, m.field_h);
        *y += label_h;
        for i in 0..n {
            // The type face keeps its authored height and centres in the
            // (possibly taller) row; see `FormMetrics::type_h`.
            let chip_y = *y + (nh - m.type_h) / 2.0;
            let chip = Rect::new(x0, chip_y, x0 + m.type_w, chip_y + m.type_h);
            let rm = Rect::new(x1 - m.remove_w, *y, x1, *y + nh);
            plan.push((
                Slot::AddrStreet(i),
                Rect::new(chip.x1 + 8.0, *y, rm.x0 - 8.0, *y + nh),
            ));
            self.hit.type_chips.push((TypeTarget::Addr(i), chip));
            self.hit.remove_btns.push((TypeTarget::Addr(i), rm));
            *y += nh + 8.0;
            plan.push((
                Slot::AddrCity(i),
                Rect::new(x0, *y, if large_text { x1 } else { x0 + half }, *y + nh),
            ));
            *y += nh + 8.0;
            if large_text {
                plan.push((Slot::AddrRegion(i), Rect::new(x0, *y, x1, *y + nh)));
                *y += nh + 8.0;
            } else {
                plan.push((
                    Slot::AddrRegion(i),
                    Rect::new(x0 + half + 10.0, *y - nh - 8.0, x1, *y - 8.0),
                ));
            }
            plan.push((
                Slot::AddrPostal(i),
                Rect::new(x0, *y, if large_text { x1 } else { x0 + half }, *y + nh),
            ));
            *y += nh + 12.0;
            if large_text {
                plan.push((Slot::AddrCountry(i), Rect::new(x0, *y, x1, *y + nh)));
                *y += nh + 12.0;
            } else {
                plan.push((
                    Slot::AddrCountry(i),
                    Rect::new(x0 + half + 10.0, *y - nh - 12.0, x1, *y - 12.0),
                ));
            }
        }
        self.hit.add_address = Rect::new(x0, *y, x0 + m.add_address_w, *y + m.add_h);
        *y += m.add_h + gap;
    }
}

// =====================================================================
// Geometry for popups (shared by draw + hit-test)
// =====================================================================

impl ContactsApp {
    fn popup_menu_geometry(&self, anchor: Rect, items: usize, width: f64) -> (Rect, Vec<Rect>) {
        let scale = self.layout_scale();
        let item_h = 32.0 * scale;
        let width = (width * scale).min((self.rect.width() - 16.0).max(80.0));
        let h = items as f64 * item_h + 8.0 * scale;
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
            let ry = panel.y0 + 4.0 * scale + i as f64 * item_h;
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
        let scale = self.layout_scale();
        let w = (252.0 * scale).min((self.rect.width() - 16.0).max(160.0));
        let h = (250.0 * scale).min((self.rect.height() - 16.0).max(220.0));
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
            panel.y0 + 8.0 * scale,
            panel.x0 + 36.0 * scale,
            panel.y0 + 36.0 * scale,
        );
        let next = Rect::new(
            panel.x1 - 36.0 * scale,
            panel.y0 + 8.0 * scale,
            panel.x1 - 8.0 * scale,
            panel.y0 + 36.0 * scale,
        );
        // Day grid: 7 cols, start under the weekday header.
        let grid_top = panel.y0 + 74.0 * scale;
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
    #[allow(
        clippy::too_many_lines,
        reason = "the root render keeps ordered modal and pane composition in one place"
    )]
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
        if self.push.tick(theme.reduce_motion) {
            self.layout();
            self.layout_detail_actions();
            self.dirty = true;
        }
        self.refresh_label_growth(tr);

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

        if self.store.is_empty() && self.mode == Mode::View {
            self.draw_first_run_state(scene, transform, tr, theme);
        } else if self.narrow {
            // Single-pane push: clip both slabs to the content area below the
            // toolbar so the off-screen pane (sliding in/out) never bleeds
            // past the edges, and skip whichever pane is fully off-screen.
            let content = Rect::new(
                self.rect.x0,
                self.rect.y0 + self.toolbar_h(),
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
        // `--shots` scenarios are configured after document sync but before
        // the first layout pass, so their group-menu anchor starts as ZERO.
        // Resolve that one deferred anchor now that `layout` has produced the
        // real chip rectangle.  Live interaction always supplies the rect at
        // activation time, so this is otherwise a harmless no-op.
        if matches!(self.popup, Some(Popup::GroupFilter { anchor }) if anchor == Rect::ZERO) {
            self.open_popup(Popup::GroupFilter {
                anchor: self.hit.group,
            });
        }
        if let Some(Popup::ContextMenu { target, anchor }) = self.popup
            && anchor == Rect::ZERO
            && let Some((_, row)) = self.list_rows.iter().find(|(id, _)| *id == target)
        {
            self.open_popup(Popup::ContextMenu {
                target,
                anchor: Rect::new(row.x0 + 40.0, row.center().y, row.x0 + 40.0, row.center().y),
            });
        }
        if let Some(popup) = &self.popup {
            self.draw_popup(scene, transform, tr, theme, popup);
        }
        if self.export_dialog.is_open() {
            self.export_dialog.draw(
                &mut PaintCtx::new(scene, transform, tr, theme),
                self.rect,
                &self.store,
                self.sort,
                self.last_mouse,
            );
        }
        if let Some(review) = self.duplicate_review.as_ref() {
            let geometry = self.duplicate_review_geometry(review);
            self.draw_duplicate_review(scene, transform, tr, theme, review, &geometry);
        }
        if self.pending_switch.is_some() {
            self.draw_unsaved_modal(scene, transform, tr, theme);
        }
        if !self.import_conflicts.is_empty() {
            self.ensure_import_conflict_dialog();
            if let Some(review) = self.import_conflict_dialog.as_mut() {
                review.dialog.layout_in(self.rect, theme, tr);
                review.dialog.draw(scene, transform, tr, theme, presence);
            }
        }
        if let Some(id) = self.confirm_delete {
            self.ensure_delete_dialog(id);
            if let Some(confirm) = self.delete_dialog.as_mut() {
                confirm.dialog.layout_in(self.rect, theme, tr);
                confirm.dialog.draw(scene, transform, tr, theme, presence);
            }
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

    #[allow(
        clippy::too_many_lines,
        reason = "the duplicate review keeps paint, clipping, selected states, and exact choice geometry together"
    )]
    fn draw_duplicate_review(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        review: &DuplicateReview,
        geometry: &DuplicateReviewGeometry,
    ) {
        let c = &theme.tokens.colors;
        if geometry.full_screen {
            scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);
        } else {
            scene.fill(
                Fill::NonZero,
                transform,
                Color::BLACK.with_alpha_factor(0.72),
                None,
                &self.rect,
            );
        }
        let shape = RoundedRect::from_rect(geometry.panel, f64::from(theme.tokens.radii.md));
        if !geometry.full_screen {
            scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &shape);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                c.border_standard,
                None,
                &shape,
            );
        }
        tr.draw_text(
            scene,
            if geometry.full_screen {
                "Review duplicate"
            } else {
                "Review possible duplicate"
            },
            16.0,
            c.text_primary,
            transform * Affine::translate((geometry.panel.x0 + 18.0, geometry.panel.y0 + 18.0)),
            Some((geometry.panel.width() - 36.0).max(40.0) as f32),
        );
        let reasons = review
            .reasons
            .iter()
            .map(crate::actions::DuplicateReason::label)
            .collect::<Vec<_>>()
            .join(", ");
        let intro = if geometry.full_screen {
            format!("{reasons}. Unique phones, emails, groups, and addresses are kept.")
        } else {
            format!("Matches: {reasons}. Unique phones, emails, groups, and addresses are kept.")
        };
        tr.draw_text(
            scene,
            &intro,
            10.0,
            c.text_dim,
            transform
                * Affine::translate((
                    geometry.panel.x0 + 18.0,
                    geometry.panel.y0 + if geometry.full_screen { 56.0 } else { 54.0 },
                )),
            Some((geometry.panel.width() - 36.0).max(40.0) as f32),
        );
        if geometry.full_screen {
            tr.draw_text(
                scene,
                &format!(
                    "{} conflicts. Scroll, then choose Current or Other.",
                    geometry.rows.len()
                ),
                9.0,
                c.text_tertiary,
                transform
                    * Affine::translate((geometry.panel.x0 + 18.0, geometry.panel.y0 + 122.0)),
                Some((geometry.panel.width() - 36.0).max(40.0) as f32),
            );
        } else {
            tr.draw_text(
                scene,
                "Choose current or other for each conflicting value.",
                11.0,
                c.text_tertiary,
                transform * Affine::translate((geometry.panel.x0 + 18.0, geometry.panel.y0 + 82.0)),
                Some((geometry.panel.width() - 36.0).max(40.0) as f32),
            );
        }
        if geometry.full_screen {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                transform,
                &geometry.list,
            );
        }
        for (index, row) in geometry.rows.iter().enumerate() {
            let field = row.field;
            let rect = row.card;
            if geometry.full_screen
                && (rect.x1 <= geometry.list.x0
                    || rect.x0 >= geometry.list.x1
                    || rect.y1 <= geometry.list.y0
                    || rect.y0 >= geometry.list.y1)
            {
                continue;
            }
            let take_source = review.take_from_source.contains(&field);
            let selected_name = if take_source { "other" } else { "current" };
            let selected_value = if take_source {
                Self::duplicate_field_value(&review.source, field)
            } else {
                Self::duplicate_field_value(&review.survivor, field)
            };
            let selected_value = if selected_value.chars().count() > 30 {
                format!("{}…", selected_value.chars().take(29).collect::<String>())
            } else {
                selected_value
            };
            let fill = if review.focus == index {
                c.overlay_medium
            } else {
                c.overlay_subtle
            };
            let card = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.sm));
            scene.fill(Fill::NonZero, transform, fill, None, &card);
            scene.stroke(
                &Stroke::new(if review.focus == index { 2.0 } else { 1.0 }),
                transform,
                if review.focus == index {
                    c.border_focus
                } else {
                    c.border_standard
                },
                None,
                &card,
            );
            tr.draw_text(
                scene,
                Self::duplicate_field_label(field),
                8.0,
                c.text_dim,
                transform * Affine::translate((rect.x0 + 10.0, rect.y0 + 6.0)),
                None,
            );
            if geometry.full_screen {
                let current_value = Self::duplicate_field_value(&review.survivor, field);
                let other_value = Self::duplicate_field_value(&review.source, field);
                let current_label = tr.elide_to_width(
                    &format!("Current: {current_value}"),
                    10.0,
                    (row.current.width() - 24.0).max(24.0) as f32,
                );
                let other_label = tr.elide_to_width(
                    &format!("Other: {other_value}"),
                    10.0,
                    (row.other.width() - 24.0).max(24.0) as f32,
                );
                let current = ButtonFace::new(row.current, &current_label)
                    .style(if take_source {
                        ButtonStyle::Neutral
                    } else {
                        ButtonStyle::Primary
                    })
                    .text_size(10.0);
                Button::paint(scene, transform, tr, theme, &current);
                let other = ButtonFace::new(row.other, &other_label)
                    .style(if take_source {
                        ButtonStyle::Primary
                    } else {
                        ButtonStyle::Neutral
                    })
                    .text_size(10.0);
                Button::paint(scene, transform, tr, theme, &other);
            } else {
                tr.draw_text(
                    scene,
                    &format!("Keep {selected_name}: {selected_value}"),
                    10.0,
                    c.text_primary,
                    transform * Affine::translate((rect.x0 + 10.0, rect.y0 + 25.0)),
                    Some((rect.width() - 20.0).max(40.0) as f32),
                );
            }
        }
        if geometry.full_screen {
            if geometry.max_scroll > 0.0 {
                let thumb_h = (geometry.list.height() * geometry.list.height()
                    / (geometry.list.height() + geometry.max_scroll))
                    .max(40.0);
                let progress = review.scroll / geometry.max_scroll;
                let thumb_y = geometry.list.y0
                    + (geometry.list.height() - thumb_h) * progress.clamp(0.0, 1.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    c.text_tertiary,
                    None,
                    &RoundedRect::from_rect(
                        Rect::new(
                            geometry.list.x1 - 5.0,
                            thumb_y,
                            geometry.list.x1 - 1.0,
                            thumb_y + thumb_h,
                        ),
                        2.0,
                    ),
                );
            }
            scene.pop_layer();
            scene.fill(
                Fill::NonZero,
                transform,
                c.surfaces.raised,
                None,
                &Rect::new(
                    self.rect.x0,
                    geometry.cancel.y0 - 10.0,
                    self.rect.x1,
                    self.rect.y1,
                ),
            );
        }
        let cancel = ButtonFace::new(geometry.cancel, "Cancel").style(ButtonStyle::Neutral);
        Button::paint(scene, transform, tr, theme, &cancel);
        let merge = ButtonFace::new(geometry.merge, "Merge contacts").style(ButtonStyle::Danger);
        Button::paint(scene, transform, tr, theme, &merge);
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
            let placeholder = tr.elide_to_width(
                "Search name, email, phone…",
                14.0,
                (self.hit.search.width() - 24.0).max(24.0) as f32,
            );
            tr.draw_text(
                scene,
                &placeholder,
                14.0,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((
                        self.hit.search.x0 + 12.0,
                        self.hit.search.center().y - 9.0 * self.layout_scale(),
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
        let bg = if hovered {
            c.surfaces.raised
        } else {
            c.surfaces.base
        };
        // A capsule with its derived edge (HIG §9.5); `radii.full` is the
        // 9999 sentinel, so the capsule radius is the pill's own half height.
        card(scene, t, rect, rect.height() / 2.0, bg);
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

    #[allow(
        clippy::too_many_lines,
        reason = "the toolbar keeps its responsive labels, busy states, and the review-only duplicate affordance in one paint route"
    )]
    fn draw_toolbar(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let compact = self.toolbar_is_compact();
        let toolbar_h = self.toolbar_h();
        let importing = self.import_rx.is_some() || self.import_parse_rx.is_some();
        let exporting = self.export_rx.is_some();
        let bar = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + toolbar_h,
        );
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &bar);
        // Bar role: one chrome hairline under the app bar (HIG §9.5).
        scene.fill(
            Fill::NonZero,
            t,
            ui::chrome::chrome_hairline(theme),
            None,
            &Rect::new(bar.x0, bar.y1 - 1.0, bar.x1, bar.y1),
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
        let (count_w, count_h) =
            tr.measure_weighted(&count, text_base(), None, TextWeight::Semibold);
        if count_x + f64::from(count_w) < self.hit.import.x0 - 12.0 {
            tr.draw_text_weighted(
                scene,
                &count,
                text_base(),
                c.text_primary,
                t * Affine::translate((
                    count_x,
                    self.rect.y0 + (toolbar_h - f64::from(count_h)) / 2.0,
                )),
                None,
                TextWeight::Semibold,
            );
        }

        let hov = |r: Rect| r.contains(self.last_mouse);
        toolbar_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            self.hit.import,
            if importing {
                if compact { "…" } else { "Importing…" }
            } else if compact {
                "↓"
            } else {
                "Import"
            },
            false,
            hov(self.hit.import),
            false,
        );
        // First run has one obvious primary action in the content area. Keep
        // Import available as the alternate path, but do not duplicate Create
        // or offer an Export action that cannot succeed yet.
        if !self.store.is_empty() {
            toolbar_button(
                &mut PaintCtx::new(scene, t, tr, theme),
                self.hit.export,
                if exporting {
                    if compact { "…" } else { "Exporting…" }
                } else if compact {
                    "↑"
                } else {
                    "Export"
                },
                false,
                hov(self.hit.export),
                false,
            );
            if self.has_duplicate_candidate() {
                toolbar_button(
                    &mut PaintCtx::new(scene, t, tr, theme),
                    self.hit.duplicates,
                    if compact {
                        "2×"
                    } else {
                        TOOLBAR_DUPLICATE_LABEL
                    },
                    false,
                    hov(self.hit.duplicates),
                    false,
                );
            }
            toolbar_button(
                &mut PaintCtx::new(scene, t, tr, theme),
                self.hit.new,
                if compact { "+" } else { TOOLBAR_NEW_LABEL },
                true,
                hov(self.hit.new),
                false,
            );
        }
    }

    /// A genuinely empty address book is one app-wide first-run state, not an
    /// empty list beside a second, contradictory empty detail pane.
    fn draw_first_run_state(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let area = Rect::new(
            self.rect.x0,
            self.rect.y0 + self.toolbar_h(),
            self.rect.x1,
            self.rect.y1,
        );
        self.empty_action_rect = EmptyState::paint(
            &mut PaintCtx::new(scene, t, tr, theme),
            area,
            &EmptyStateContent {
                icon: Some(Icon::FileText),
                title: "No contacts yet",
                message: Some("Create a contact, or import an existing vCard address book."),
                action_label: Some("Create contact"),
            },
            self.last_mouse,
        )
        .unwrap_or(Rect::ZERO);
        self.empty_list_action = EmptyListAction::Create;

        if self.empty_action_focused && self.empty_action_rect.width() > 0.0 {
            scene.stroke(
                &Stroke::new(2.0),
                t,
                theme.tokens.colors.border_focus,
                None,
                &RoundedRect::from_rect(self.empty_action_rect.inflate(3.0, 3.0), 11.0),
            );
        }
        self.detail_content_h = 0.0;
        self.website_link = None;
    }

    /// Draw the list pane: sort/group chips, every visible row, and the
    /// empty-state message when nothing matches. Split into per-row/
    /// empty-state helpers purely for length; the draw order is unchanged
    /// from one flat function.
    fn draw_list(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        // Pane background + right border.
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &self.list_rect);

        // The retained search input is drawn by `render`; this pass paints the
        // surrounding list chrome and its immediate-mode content.
        let sort_label = format!("Sort: {}", self.sort.label());
        chip(scene, t, tr, theme, self.hit.sort, &sort_label, false);
        let group_label = match &self.group_filter {
            Some(filter) if filter == FAVORITES_FILTER => "Favorites".to_string(),
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
        let metrics = self.row_metrics();
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &self.list_body);
        for (index, (id, rr)) in self.list_rows.iter().enumerate() {
            if rr.y1 < self.list_body.y0 || rr.y0 > self.list_body.y1 {
                continue;
            }
            let Some(contact) = self.store.get(*id) else {
                continue;
            };
            let selected = self.selected == Some(*id);
            // The selected row's pill replaces the separators on *both* of
            // its sides: the one under it and the one under the row above,
            // so the pill keeps the same clear band above and below.
            let next_selected = self
                .list_rows
                .get(index.saturating_add(1))
                .is_some_and(|(next, _)| self.selected == Some(*next));
            Self::draw_list_row(
                &mut PaintCtx::new(scene, t, tr, theme),
                contact,
                *rr,
                selected,
                !selected && !next_selected,
                &metrics,
            );
        }
        if self.list_rows.is_empty() {
            self.draw_list_empty_state(scene, t, tr, theme);
        }
        scene.pop_layer();
        let _ = presence;
    }

    /// Draw one contact row: selection highlight, avatar, name/subtitle
    /// (elided to fit), favourite star, and — when `separator_below` — the
    /// rule under it (the caller drops it on either side of a selected row).
    fn draw_list_row(
        ctx: &mut PaintCtx<'_>,
        contact: &Contact,
        rr: Rect,
        selected: bool,
        separator_below: bool,
        m: &ContactRowMetrics,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let sp = &theme.tokens.spacing;
        if selected {
            // Selection is a lift (HIG §9.3/§9.4): the toolkit's selection
            // tier — fill, derived hairline, inner glow — on a pill inset
            // `s2` from the pane sides and half an `s1` from the rows above
            // and below, never an accent wash or bar.
            let pill = selection_pill(rr, sp);
            let radius = f64::from(theme.tokens.radii.md);
            let tier = row_tier_on(c.surfaces.base, SelectionState::active(true));
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                tier.fill,
                None,
                &RoundedRect::from_rect(pill, radius),
            );
            if let Some(border) = tier.border {
                ctx.scene.stroke(
                    &Stroke::new(1.0),
                    ctx.transform,
                    border,
                    None,
                    &edge_outline(pill, radius),
                );
            }
            if let Some(glow) = tier.inner_glow {
                ctx.scene.stroke(
                    &Stroke::new(1.0),
                    ctx.transform,
                    glow,
                    None,
                    &edge_outline(pill.inset(-1.0), (radius - 1.0).max(0.0)),
                );
            }
        }
        // Avatar: the same `PAD` inset as the search and filter controls
        // above the list, vertically centred in the row.
        let av_y = rr.y0 + (rr.height() - m.avatar) / 2.0;
        let av = Rect::new(rr.x0 + PAD, av_y, rr.x0 + PAD + m.avatar, av_y + m.avatar);
        draw_avatar(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            av,
            &contact.initials(),
        );
        let tx = av.x1 + f64::from(sp.s3);
        // Name and subtitle stack as one measured block centred in the row,
        // each elided to a single line so a narrow pane never wraps a row.
        let star_w = if contact.favorite { PAD + 6.0 } else { 0.0 };
        let avail = (rr.x1 - tx - PAD - star_w).max(24.0) as f32;
        let block_h = m.name_h + m.sub_h;
        let name_y = rr.y0 + (rr.height() - block_h) / 2.0;
        let name = ctx.text.elide_to_width_weighted(
            &contact.display_name(),
            ROW_NAME_TEXT,
            avail,
            TextWeight::Semibold,
        );
        ctx.text.draw_text_weighted(
            ctx.scene,
            &name,
            ROW_NAME_TEXT,
            c.text_primary,
            ctx.transform * Affine::translate((tx, name_y)),
            None,
            TextWeight::Semibold,
        );
        let sub = contact.subtitle();
        if !sub.is_empty() {
            let sub = ctx.text.elide_to_width(&sub, text_sm(), avail);
            ctx.text.draw_text(
                ctx.scene,
                &sub,
                text_sm(),
                c.text_dim,
                ctx.transform * Affine::translate((tx, name_y + m.name_h)),
                None,
            );
        }
        if contact.favorite {
            // A trailing indicator sits on the row's centre line (HIG §7),
            // like the avatar, inset like the avatar.
            draw_star(
                ctx.scene,
                ctx.transform,
                Point::new(rr.x1 - PAD - 6.0, rr.center().y),
                6.0,
                theme.accent_primary,
                true,
            );
        }
        // Separator between rows, derived against the list ground and inset
        // like a Panel separator (HIG §9.5); a selection pill's own edge
        // replaces it above and below that row.
        if separator_below {
            separator(
                ctx.scene,
                ctx.transform,
                rr.x0,
                rr.x1,
                rr.y1 - 0.5,
                PAD,
                c.surfaces.base,
            );
        }
    }

    /// The "no contacts yet" / "no matches" placeholder shown when the
    /// (search + group filtered) list is empty.
    fn draw_list_empty_state(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        // Distinguish a genuinely empty book (first launch) from a filter
        // that matched nothing, so the message is never misleading.
        let (title, message, action): (&str, &str, Option<&str>) = if self.store.is_empty() {
            (
                "No contacts yet",
                "Add someone here, or import an existing vCard address book.",
                Some("Create contact"),
            )
        } else if self.empty_list_action == EmptyListAction::ClearFilters {
            (
                "No matching contacts",
                "Try another name, company, or group.",
                Some("Clear filters"),
            )
        } else {
            (
                "No matching contacts",
                "Try another name, company, or group.",
                None,
            )
        };
        self.empty_action_rect = EmptyState::paint(
            &mut PaintCtx::new(scene, t, tr, theme),
            self.list_body,
            &EmptyStateContent {
                icon: Some(Icon::FileText),
                title,
                message: Some(message),
                action_label: action,
            },
            self.last_mouse,
        )
        .unwrap_or(Rect::ZERO);

        if self.empty_action_focused && self.empty_action_rect.width() > 0.0 {
            scene.stroke(
                &Stroke::new(2.0),
                t,
                theme.tokens.colors.border_focus,
                None,
                &RoundedRect::from_rect(self.empty_action_rect.inflate(3.0, 3.0), 11.0),
            );
        }
    }

    fn draw_divider(&self, scene: &mut Scene, t: Affine, theme: &Theme) {
        let dx0 = self.list_rect.x1;
        let r = Rect::new(
            dx0,
            self.detail_rect.y0,
            dx0 + DIVIDER_W,
            self.detail_rect.y1,
        );
        // The splitter is one derived hairline step off the base ground it
        // divides — a contrast step on every preset (the declared
        // `border_standard` vanished on dark; the raised tier is white on
        // white in light) — and accent only while it is live under the
        // pointer.
        let color = if self.divider_hover || self.divider_drag {
            theme.accent_primary
        } else {
            row_tier_on(
                theme.tokens.colors.surfaces.base,
                SelectionState::active(true),
            )
            .fill
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
        let (name_top, y) =
            self.draw_detail_header(&mut PaintCtx::new(scene, t, tr, theme), &contact, x0, x1, y);

        // Record field value hit rects so a double-click can jump straight
        // into editing the clicked field (see `handle_detail_double_click`).
        self.detail_field_hits.clear();
        let (mut hits, y) = self.draw_detail_sections(
            &mut PaintCtx::new(scene, t, tr, theme),
            presence,
            &contact,
            x0,
            x1,
            y,
        );

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
    fn draw_detail_actions(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        contact: &Contact,
    ) {
        draw_star(
            scene,
            t,
            self.hit.fav.center(),
            11.0,
            theme.accent_primary,
            contact.favorite,
        );
        for (rect, label, primary) in [
            (
                self.hit.edit,
                if theme.text_scale >= 1.75 {
                    "edit-icon"
                } else {
                    "Edit"
                },
                true,
            ),
            (
                self.hit.delete,
                if theme.text_scale >= 1.75 {
                    "delete-icon"
                } else {
                    "Delete"
                },
                false,
            ),
        ] {
            toolbar_button(
                &mut PaintCtx::new(scene, t, tr, theme),
                rect,
                label,
                primary,
                rect.contains(self.last_mouse),
                false,
            );
        }
    }

    fn draw_detail_header(
        &self,
        ctx: &mut PaintCtx<'_>,
        contact: &Contact,
        x0: f64,
        x1: f64,
        y: f64,
    ) -> (f64, f64) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let sp = &theme.tokens.spacing;
        // Every header row is stacked from its measured line: name, subtitle
        // and chips can no longer land on each other when the type scales.
        let name_h = f64::from(ctx.text.measure("Ag", 26.0, None).1);
        let sub_h = f64::from(ctx.text.measure("Ag", 14.0, None).1);
        let chip_text_h = f64::from(ctx.text.measure("Ag", 11.0, None).1);
        let av_size = (name_h + sub_h).max(72.0);
        let av = Rect::new(x0, y, x0 + av_size, y + av_size);
        draw_avatar(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            av,
            &contact.initials(),
        );

        // Action buttons (favorite star, Edit, Delete) — top right.
        self.draw_detail_actions(ctx.scene, ctx.transform, ctx.text, theme, contact);

        // Name + subtitle on their own full-width rows beneath the avatar.
        let head_w = (x1 - x0).max(40.0) as f32;
        let name_y = y + av_size + f64::from(sp.s4);
        let name = ctx.text.elide_to_width_weighted(
            &contact.display_name(),
            26.0,
            head_w,
            TextWeight::Bold,
        );
        ctx.text.draw_text_weighted(
            ctx.scene,
            &name,
            26.0,
            c.text_primary,
            ctx.transform * Affine::translate((x0, name_y)),
            None,
            TextWeight::Bold,
        );
        let mut hy = name_y + name_h + f64::from(sp.s1);
        let sub = contact.subtitle();
        if !sub.is_empty() {
            let sub = ctx.text.elide_to_width(&sub, 14.0, head_w);
            ctx.text.draw_text(
                ctx.scene,
                &sub,
                14.0,
                c.text_dim,
                ctx.transform * Affine::translate((x0, hy)),
                None,
            );
            hy += sub_h + f64::from(sp.s2);
        }
        // Group badges below the name/subtitle: pills sized from their label
        // line, wrapping onto further rows rather than being dropped.
        if !contact.groups.is_empty() {
            let chip_h = (chip_text_h + f64::from(sp.s2)).max(20.0);
            let pad_x = f64::from(sp.s2);
            let gap = f64::from(sp.s2);
            let mut bx = x0;
            let mut by = hy;
            for g in &contact.groups {
                let tw = f64::from(ctx.text.measure(&g.value, 11.0, None).0);
                let bw = tw + 2.0 * pad_x;
                if bx > x0 && bx + bw > x1 {
                    bx = x0;
                    by += chip_h + gap;
                }
                let br = Rect::new(bx, by, bx + bw, by + chip_h);
                let shape = RoundedRect::from_rect(br, chip_h / 2.0);
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    theme.accent_primary.with_alpha_factor(0.16),
                    None,
                    &shape,
                );
                let label =
                    ctx.text
                        .elide_to_width(&g.value, 11.0, (bw - 2.0 * pad_x).max(12.0) as f32);
                ctx.text.draw_text(
                    ctx.scene,
                    &label,
                    11.0,
                    theme.accent_primary,
                    ctx.transform
                        * Affine::translate((bx + pad_x, by + (chip_h - chip_text_h) / 2.0)),
                    None,
                );
                bx += bw + gap;
            }
            hy = by + chip_h + gap;
        }

        // Start of the field sections below the header block.
        (name_y, hy + f64::from(sp.s3))
    }

    /// Phone/email/address/birthday/website/notes sections, in order, each
    /// followed by its double-click hit rect. Returns the collected hits and
    /// the y cursor after the last section.
    fn draw_detail_sections(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
        contact: &Contact,
        x0: f64,
        x1: f64,
        mut y: f64,
    ) -> (Vec<(Slot, Rect)>, f64) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let scale = f64::from(theme.text_scale).clamp(1.0, 2.0);
        let mut hits: Vec<(Slot, Rect)> = Vec::new();

        // Sections (free helpers avoid closures double-borrowing scene/tr).
        if !contact.phones.is_empty() {
            detail_section(ctx, x0, &mut y, "PHONE");
            for (i, p) in contact.phones.iter().enumerate() {
                let y0 = y;
                detail_field(ctx, x0, x1, &mut y, &p.label, &p.value);
                hits.push((Slot::Phone(i), Rect::new(x0, y0 - 4.0, x1, y)));
            }
        }
        if !contact.emails.is_empty() {
            detail_section(ctx, x0, &mut y, "EMAIL");
            for (i, e) in contact.emails.iter().enumerate() {
                let y0 = y;
                detail_field(ctx, x0, x1, &mut y, &e.label, &e.value);
                hits.push((Slot::Email(i), Rect::new(x0, y0 - 4.0, x1, y)));
            }
        }
        if !contact.addresses.is_empty() {
            detail_section(ctx, x0, &mut y, "ADDRESS");
            for (i, a) in contact.addresses.iter().enumerate() {
                let y0 = y;
                detail_field(ctx, x0, x1, &mut y, &a.label, &a.one_line());
                // The address is one logical block; double-clicking it edits
                // its street line (the first address input).
                hits.push((Slot::AddrStreet(i), Rect::new(x0, y0 - 4.0, x1, y)));
            }
        }
        if let Some(bday) = &contact.birthday {
            detail_section(ctx, x0, &mut y, "BIRTHDAY");
            let y0 = y;
            // Render the structured date, formatted; double-click still edits it.
            detail_field(ctx, x0, x1, &mut y, "date", &bday.pretty());
            hits.push((Slot::Birthday, Rect::new(x0, y0 - 4.0, x1, y)));
        }
        if contact.website.is_empty() {
            self.website_link = None;
        } else {
            detail_section(ctx, x0, &mut y, "WEBSITE");
            // "url" field label on the left, matching the other detail rows.
            ctx.text.draw_text(
                ctx.scene,
                "url",
                12.0,
                c.text_dim,
                ctx.transform * Affine::translate((x0, y + 2.0)),
                Some(64.0),
            );
            // The website is a live hyperlink (ui `Link` widget), not plain
            // text: single-click opens it via the OS handler. Rebuilt only when
            // the URL changes so its hover/press state survives across frames.
            let stacked = theme.text_scale >= 1.75;
            if stacked {
                y += 22.0 * scale;
            }
            let vx = if stacked { x0 } else { x0 + 78.0 };
            let vw = (x1 - vx).max(40.0);
            let needs_new = self
                .website_link
                .as_ref()
                .is_none_or(|l| l.url() != contact.website);
            if needs_new {
                self.website_link = Some(Link::new(&contact.website).with_font_size(14.0));
            }
            let (lw, lh) = ctx.text.measure(&contact.website, 14.0, None);
            let lw = f64::from(lw).min(vw);
            let lh = f64::from(lh).max(20.0);
            let lrect = Rect::new(vx, y, vx + lw, y + lh);
            if let Some(link) = self.website_link.as_mut() {
                link.apply_layout_external(lrect);
                link.draw(ctx.scene, ctx.transform, ctx.text, theme, presence);
            }
            y += lh + 8.0 * scale;
        }
        if !contact.notes.is_empty() {
            detail_section(ctx, x0, &mut y, "NOTES");
            let y0 = y;
            let h = f64::from(
                ctx.text
                    .measure(&contact.notes, 14.0, Some((x1 - x0) as f32))
                    .1,
            );
            ctx.text.draw_text(
                ctx.scene,
                &contact.notes,
                14.0,
                c.text_primary,
                ctx.transform * Affine::translate((x0, y)),
                Some((x1 - x0) as f32),
            );
            y += h + 8.0 * scale;
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
        let body_top = self.form_body_top();
        let body = Rect::new(area.x0, body_top, area.x1, area.y1);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &body);

        // Section labels are drawn relative to the first input of each group.
        let Some(form) = self.form.as_ref() else {
            return;
        };
        let inputs_snapshot: Vec<(Slot, Rect)> =
            form.inputs.iter().map(|f| (f.slot, f.ti_rect())).collect();
        let draft = form.draft.clone();
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

        let Some(f) = self.form.as_ref() else {
            return;
        };
        let (show_new, draft_name, initials) = {
            let d = &f.draft;
            // A brand-new record still reads as "New Contact" only while it has
            // no identifying text; once a name/org is typed the heading follows
            // it live, so the identity anchor forms as you fill the record in.
            let blank =
                d.first.trim().is_empty() && d.last.trim().is_empty() && d.org.trim().is_empty();
            (f.is_new && blank, d.display_name(), d.initials())
        };
        let title_y = area.y0 + f64::from(self.spacing.s4) + off;
        // Avatar chip, vertically centred in the identity row.
        let av_y = title_y + (self.form_metrics().title_h - ROW_AVATAR) / 2.0;
        let av = Rect::new(x0, av_y, x0 + ROW_AVATAR, av_y + ROW_AVATAR);
        draw_avatar(scene, t, tr, theme, av, &initials);
        // Heading: the forming name, or a clear "New Contact" while it is blank
        // so a fresh record never reads as the "No Name" placeholder.
        let heading = if show_new {
            "New Contact".to_string()
        } else {
            draft_name
        };
        // Save/Cancel live on the second header row. On a narrow detail pane
        // they must not steal the identity row's width and turn a real name
        // into an unnecessary ellipsis.
        let heading_right = if self.narrow {
            area.x1 - PAD
        } else {
            self.hit.cancel.x0 - 12.0
        };
        let head_avail = (heading_right - (av.x1 + 12.0)).max(40.0) as f32;
        let heading = tr.elide_to_width_weighted(&heading, text_xl(), head_avail, TextWeight::Bold);
        // Centred on the measured heading line, on the avatar's axis.
        let (_, heading_h) = tr.measure_weighted(&heading, text_xl(), None, TextWeight::Bold);
        tr.draw_text_weighted(
            scene,
            &heading,
            text_xl(),
            c.text_primary,
            t * Affine::translate((av.x1 + 12.0, av.center().y - f64::from(heading_h) / 2.0)),
            None,
            TextWeight::Bold,
        );
        let fav = self.form.as_ref().is_some_and(|f| f.draft.favorite);
        // Favorite toggle pill.
        {
            let r = self.hit.fav_toggle;
            let hovered = r.contains(self.last_mouse);
            // The on state is the accent wash resolved onto the pane, so the
            // pill is an opaque surface its own edge derives from.
            let bg = if fav {
                over(c.surfaces.base, theme.accent_primary, 0.18)
            } else if hovered {
                theme.bg_hover
            } else {
                c.surfaces.raised
            };
            card(scene, t, r, f64::from(theme.tokens.radii.md), bg);
            draw_star(
                scene,
                t,
                Point::new(r.x0 + 16.0, r.center().y),
                7.0,
                theme.accent_primary,
                fav,
            );
            let fav_label = if fav {
                FORM_FAVORITED_LABEL
            } else {
                FORM_FAVORITE_LABEL
            };
            let (_, fav_h) = tr.measure(fav_label, text_sm(), None);
            tr.draw_text(
                scene,
                fav_label,
                text_sm(),
                if fav {
                    theme.accent_primary
                } else {
                    c.text_dim
                },
                t * Affine::translate((r.x0 + 30.0, r.center().y - f64::from(fav_h) / 2.0)),
                None,
            );
        }
        toolbar_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            self.hit.save,
            "Save",
            true,
            self.hit.save.contains(self.last_mouse),
            false,
        );
        toolbar_button(
            &mut PaintCtx::new(scene, t, tr, theme),
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
        // The label sits in the band `layout_form` reserved above its group
        // (`FormMetrics::label_h`), so a scaled label never overprints the
        // field below or the row above.
        let label_h = self.form_metrics().label_h;
        let label = |scene: &mut Scene, tr: &mut TextRenderer, text: &str, rect: Rect| {
            tr.draw_text_weighted(
                scene,
                text,
                FORM_LABEL_TEXT,
                c.text_dim,
                t * Affine::translate((rect.x0, rect.y0 - label_h)),
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
        let Some(form) = self.form.as_mut() else {
            return;
        };
        for f in &mut form.inputs {
            f.ti.draw(scene, t, tr, theme, presence);
            if f.ti.value().is_empty() && !f.ti.is_focused() {
                let r = f.ti.get_rect();
                if r.width() > 0.0 {
                    let shape = RoundedRect::from_rect(r, f64::from(theme.tokens.radii.md));
                    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &shape);
                    let hint = placeholder_for(f.slot);
                    let (_, hint_h) = tr.measure(hint, text_base(), None);
                    tr.draw_text(
                        scene,
                        hint,
                        text_base(),
                        c.text_dim,
                        t * Affine::translate((
                            r.x0 + 12.0,
                            r.center().y - f64::from(hint_h) / 2.0,
                        )),
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
            card(
                scene,
                t,
                *rect,
                f64::from(theme.tokens.radii.md),
                c.surfaces.raised,
            );
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
            card(
                scene,
                t,
                r,
                f64::from(theme.tokens.radii.md),
                c.surfaces.raised,
            );
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
            Popup::Sort { anchor } => self.draw_popup_sort(scene, t, tr, theme, *anchor),
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
            if let Some(label) = TYPE_OPTIONS.get(i).copied() {
                menu_item(scene, t, tr, theme, *r, label, hov, false);
                self.draw_popup_focus_ring(scene, t, theme, *r, i);
            }
        }
    }

    /// The list sort selector. This deliberately mirrors the group selector:
    /// a chip opens a choice list instead of silently cycling a hidden state.
    fn draw_popup_sort(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        anchor: Rect,
    ) {
        let options = [(SortKey::Name, "Name"), (SortKey::Org, "Organization")];
        let (panel, rows) = self.popup_menu_geometry(anchor, options.len(), 180.0);
        menu_panel(scene, t, theme, panel);
        for (index, ((key, label), row)) in options.iter().zip(rows).enumerate() {
            menu_item(
                scene,
                t,
                tr,
                theme,
                row,
                label,
                row.contains(self.last_mouse),
                *key == self.sort,
            );
            self.draw_popup_focus_ring(scene, t, theme, row, index);
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
        let mut opts = vec!["All".to_string(), "Favorites".to_string()];
        opts.extend(self.store.all_groups());
        let (panel, rows) = self.popup_menu_geometry(anchor, opts.len(), 180.0);
        menu_panel(scene, t, theme, panel);
        for (i, r) in rows.iter().enumerate() {
            let hov = r.contains(self.last_mouse);
            let Some(opt) = opts.get(i) else {
                continue;
            };
            let sel = match &self.group_filter {
                None => i == 0,
                Some(filter) if filter == FAVORITES_FILTER => opt == "Favorites",
                Some(g) => opt == g,
            };
            menu_item(scene, t, tr, theme, *r, opt, hov, sel);
            self.draw_popup_focus_ring(scene, t, theme, *r, i);
        }
    }

    /// The birthday calendar popup: month/year header with prev/next
    /// chevrons, the weekday header row, and every day cell (hover +
    /// selected-day highlight).
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
        let scale = self.layout_scale();
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
            t * Affine::translate((panel.center().x - tw / 2.0, panel.y0 + 14.0 * scale)),
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
                t * Affine::translate((cx + cell / 2.0 - 4.0 * scale, panel.y0 + 50.0 * scale)),
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
            let col = if is_sel {
                c.on_accent(4.5)
            } else {
                c.text_primary
            };
            tr.draw_text(
                scene,
                &s,
                12.0,
                col,
                t * Affine::translate((r.center().x - dw / 2.0, r.center().y - 8.0 * scale)),
                None,
            );
            // Calendar focus indexes reserve 0/1 for previous/next month;
            // day cells begin at 2 (the same order keyboard navigation uses).
            self.draw_popup_focus_ring(scene, t, theme, *r, *d as usize + 1);
        }
        self.draw_popup_focus_ring(scene, t, theme, prev, 0);
        self.draw_popup_focus_ring(scene, t, theme, next, 1);
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
            let Some(action) = CONTEXT_ACTIONS.get(i).copied() else {
                continue;
            };
            let label = action.label(is_fav);
            // The destructive "Delete" item reads in the danger color
            // when not hovered (hover inverts to white-on-accent).
            if action == ContextAction::Delete && !hov {
                menu_item_colored(scene, t, tr, theme, *r, label, c.semantic.danger);
            } else {
                menu_item(scene, t, tr, theme, *r, label, hov, false);
            }
            self.draw_popup_focus_ring(scene, t, theme, *r, i);
        }
    }

    fn draw_popup_focus_ring(
        &self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        rect: Rect,
        index: usize,
    ) {
        if self.popup_focus == index {
            scene.stroke(
                &Stroke::new(2.0),
                t,
                theme.tokens.colors.border_focus,
                None,
                &RoundedRect::from_rect(rect.inset(1.0), f64::from(theme.tokens.radii.sm)),
            );
        }
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
        let scale = self.layout_scale();
        scene.fill(Fill::NonZero, t, c.surfaces.overlay, None, &self.rect);
        let g = self.unsaved_geometry();
        menu_panel(scene, t, theme, g.panel);
        tr.draw_text_weighted(
            scene,
            "Save changes?",
            16.0,
            c.text_primary,
            t * Affine::translate((g.panel.x0 + 20.0, g.panel.y0 + 20.0 * scale)),
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
            t * Affine::translate((g.panel.x0 + 20.0, g.panel.y0 + 48.0 * scale)),
            Some((g.panel.width() - 40.0) as f32),
        );
        let m = self.last_mouse;
        toolbar_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            g.cancel,
            "Cancel",
            false,
            g.cancel.contains(m),
            false,
        );
        toolbar_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            g.discard,
            "Discard",
            false,
            g.discard.contains(m),
            false,
        );
        toolbar_button(
            &mut PaintCtx::new(scene, t, tr, theme),
            g.save,
            "Save",
            true,
            g.save.contains(m),
            false,
        );
        let focused = match self.unsaved_focus {
            UnsavedFocus::Cancel => g.cancel,
            UnsavedFocus::Discard => g.discard,
            UnsavedFocus::Save => g.save,
        };
        scene.stroke(
            &Stroke::new(2.0),
            t,
            c.border_focus,
            None,
            &RoundedRect::from_rect(focused.inflate(2.0, 2.0), f64::from(theme.tokens.radii.md)),
        );
    }

    /// Build the shared destructive confirmation for one contact. Callback
    /// cells keep the widget presentation-only; the host still dispatches the
    /// canonical Contacts action after the dialog resolves.
    fn build_delete_dialog(&self, id: ContactId) -> DeleteDialog {
        let name = self
            .store
            .get(id)
            .map(super::model::Contact::display_name)
            .unwrap_or_default();
        let confirm = Rc::new(Cell::new(false));
        let cancel = Rc::new(Cell::new(false));
        let body = format!("\u{201C}{name}\u{201D} will be removed from this address book.");
        let dialog = ModalDialog::new("Delete contact?", Text::new(&body).with_size(13.0))
            .open(true)
            .with_max_size(420.0, 190.0)
            .with_destructive(true)
            .primary_action("Delete", {
                let confirm = Rc::clone(&confirm);
                move || confirm.set(true)
            })
            .secondary_action("Cancel", {
                let cancel = Rc::clone(&cancel);
                move || cancel.set(true)
            })
            .on_dismiss({
                let cancel = Rc::clone(&cancel);
                move || cancel.set(true)
            });
        DeleteDialog {
            target: id,
            dialog,
            confirm,
            cancel,
        }
    }

    fn ensure_delete_dialog(&mut self, id: ContactId) {
        if self
            .delete_dialog
            .as_ref()
            .is_none_or(|dialog| dialog.target != id)
        {
            self.delete_dialog = Some(self.build_delete_dialog(id));
        }
    }

    fn duplicate_conflicts(review: &DuplicateReview) -> Vec<crate::actions::MergeField> {
        use crate::actions::MergeField;
        let differs = |left: &str, right: &str| {
            !left.trim().is_empty() && !right.trim().is_empty() && left != right
        };
        let mut fields = Vec::new();
        for (field, left, right) in [
            (
                MergeField::First,
                &review.survivor.first,
                &review.source.first,
            ),
            (MergeField::Last, &review.survivor.last, &review.source.last),
            (
                MergeField::Organization,
                &review.survivor.org,
                &review.source.org,
            ),
            (
                MergeField::Title,
                &review.survivor.title,
                &review.source.title,
            ),
            (
                MergeField::Website,
                &review.survivor.website,
                &review.source.website,
            ),
            (
                MergeField::Notes,
                &review.survivor.notes,
                &review.source.notes,
            ),
        ] {
            if differs(left, right) {
                fields.push(field);
            }
        }
        if review.survivor.birthday.is_some()
            && review.source.birthday.is_some()
            && review.survivor.birthday != review.source.birthday
        {
            fields.push(MergeField::Birthday);
        }
        fields
    }

    fn duplicate_field_label(field: crate::actions::MergeField) -> &'static str {
        use crate::actions::MergeField;
        match field {
            MergeField::First => "First name",
            MergeField::Last => "Last name",
            MergeField::Organization => "Organization",
            MergeField::Title => "Job title",
            MergeField::Website => "Website",
            MergeField::Birthday => "Birthday",
            MergeField::Notes => "Notes",
        }
    }

    fn duplicate_field_value(contact: &Contact, field: crate::actions::MergeField) -> String {
        use crate::actions::MergeField;
        match field {
            MergeField::First => contact.first.clone(),
            MergeField::Last => contact.last.clone(),
            MergeField::Organization => contact.org.clone(),
            MergeField::Title => contact.title.clone(),
            MergeField::Website => contact.website.clone(),
            MergeField::Birthday => contact
                .birthday
                .map(|birthday| birthday.pretty())
                .unwrap_or_default(),
            MergeField::Notes => contact.notes.clone(),
        }
    }

    fn duplicate_field_from_token(raw: &str) -> Option<crate::actions::MergeField> {
        use crate::actions::MergeField;
        match raw {
            "First" => Some(MergeField::First),
            "Last" => Some(MergeField::Last),
            "Organization" => Some(MergeField::Organization),
            "Title" => Some(MergeField::Title),
            "Website" => Some(MergeField::Website),
            "Birthday" => Some(MergeField::Birthday),
            "Notes" => Some(MergeField::Notes),
            _ => None,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "desktop dialog and large-text full-screen geometry are two auditable modes of one component"
    )]
    fn duplicate_review_geometry(&self, review: &DuplicateReview) -> DuplicateReviewGeometry {
        let scale = self.text_scale.max(1.0);
        let conflicts = Self::duplicate_conflicts(review);
        let full_screen = self.narrow || scale >= 1.75;
        if full_screen {
            let panel = self.rect;
            let header_h = 166.0;
            let footer_h = 118.0;
            let list = Rect::new(
                panel.x0 + 16.0,
                panel.y0 + header_h,
                panel.x1 - 16.0,
                panel.y1 - footer_h,
            );
            let card_h = 104.0;
            let content_h = conflicts.len() as f64 * (card_h + 10.0);
            let max_scroll = (content_h - list.height()).max(0.0);
            let scroll = review.scroll.clamp(0.0, max_scroll);
            let rows = conflicts
                .into_iter()
                .enumerate()
                .map(|(index, field)| {
                    let y = list.y0 + index as f64 * (card_h + 10.0) - scroll;
                    let card = Rect::new(list.x0, y, list.x1, y + card_h);
                    DuplicateReviewRow {
                        field,
                        card,
                        current: Rect::new(
                            card.x0 + 10.0,
                            card.y0 + 29.0,
                            card.x1 - 10.0,
                            card.y0 + 60.0,
                        ),
                        other: Rect::new(
                            card.x0 + 10.0,
                            card.y0 + 66.0,
                            card.x1 - 10.0,
                            card.y0 + 97.0,
                        ),
                    }
                })
                .collect();
            return DuplicateReviewGeometry {
                panel,
                list,
                rows,
                cancel: Rect::new(
                    panel.x0 + 16.0,
                    panel.y1 - 106.0,
                    panel.x1 - 16.0,
                    panel.y1 - 64.0,
                ),
                merge: Rect::new(
                    panel.x0 + 16.0,
                    panel.y1 - 54.0,
                    panel.x1 - 16.0,
                    panel.y1 - 12.0,
                ),
                full_screen,
                max_scroll,
            };
        }
        let row_h = 42.0;
        let header_h = 118.0;
        let wanted_h = header_h + row_h * conflicts.len() as f64 + 62.0;
        let w = (self.rect.width() - 32.0).clamp(280.0, 680.0);
        let h = wanted_h.min((self.rect.height() - 24.0).max(240.0));
        let panel = Rect::new(
            self.rect.center().x - w / 2.0,
            self.rect.center().y - h / 2.0,
            self.rect.center().x + w / 2.0,
            self.rect.center().y + h / 2.0,
        );
        let mut y = panel.y0 + header_h;
        let rows = conflicts
            .into_iter()
            .map(|field| {
                let card = Rect::new(panel.x0 + 18.0, y, panel.x1 - 18.0, y + row_h - 6.0);
                y += row_h;
                DuplicateReviewRow {
                    field,
                    card,
                    current: card,
                    other: card,
                }
            })
            .collect();
        let footer_y = panel.y1 - 50.0;
        DuplicateReviewGeometry {
            panel,
            list: Rect::new(
                panel.x0 + 18.0,
                panel.y0 + header_h,
                panel.x1 - 18.0,
                footer_y - 8.0,
            ),
            rows,
            merge: Rect::new(panel.x1 - 142.0, footer_y, panel.x1 - 18.0, footer_y + 34.0),
            cancel: Rect::new(
                panel.x1 - 266.0,
                footer_y,
                panel.x1 - 152.0,
                footer_y + 34.0,
            ),
            full_screen,
            max_scroll: 0.0,
        }
    }

    fn open_duplicate_review(
        &mut self,
        survivor_id: ContactId,
        source_id: ContactId,
    ) -> Result<(), String> {
        let survivor = self
            .store
            .get(survivor_id)
            .cloned()
            .ok_or_else(|| "surviving contact no longer exists".to_string())?;
        let source = self
            .store
            .get(source_id)
            .cloned()
            .ok_or_else(|| "source contact no longer exists".to_string())?;
        let candidate = crate::actions::duplicate_candidate(&survivor, &source)
            .ok_or_else(|| "contacts are no longer a likely duplicate".to_string())?;
        self.selected = Some(survivor_id);
        self.duplicate_review = Some(DuplicateReview {
            survivor,
            source,
            reasons: candidate.reasons,
            take_from_source: std::collections::HashSet::new(),
            focus: 0,
            scroll: 0.0,
        });
        self.popup = None;
        self.dirty = true;
        Ok(())
    }

    fn merge_duplicate_review(&mut self, state: &mut AutoCommit) {
        let Some(review) = self.duplicate_review.take() else {
            return;
        };
        let action = crate::actions::merge_contacts_action(
            &review.survivor,
            &review.source,
            &review.take_from_source,
        );
        match self.try_perform_action(state, &action) {
            Ok(()) => {
                self.selected = Some(review.survivor.id);
                self.show_toast(format!(
                    "Merged {} into {}. Undo is available in history.",
                    review.source.display_name(),
                    review.survivor.display_name()
                ));
            }
            Err(error) => self.show_toast(format!("Couldn't merge contacts: {error}")),
        }
        self.dirty = true;
    }

    fn build_import_conflict_dialog(&self, conflict: &ImportConflict) -> ImportConflictDialog {
        let import_as_new = Rc::new(Cell::new(false));
        let keep_existing = Rc::new(Cell::new(false));
        let existing_name = self
            .store
            .get(conflict.existing)
            .map_or_else(|| "the current contact".to_string(), Contact::display_name);
        let body = format!(
            "\u{201C}{}\u{201D} has the same vCard UID as \u{201C}{existing_name}\u{201D}, but its details differ. \
             Import as new adds a separate contact; Keep existing discards the imported version.",
            conflict.incoming.display_name(),
        );
        let dialog = ModalDialog::new("Review imported contact", Text::new(&body).with_size(13.0))
            .open(true)
            .with_max_size(480.0, 210.0)
            .with_destructive(true)
            .primary_action("Import as new", {
                let import_as_new = Rc::clone(&import_as_new);
                move || import_as_new.set(true)
            })
            .secondary_action("Keep existing", {
                let keep_existing = Rc::clone(&keep_existing);
                move || keep_existing.set(true)
            });
        ImportConflictDialog {
            token: conflict.token,
            dialog,
            import_as_new,
            keep_existing,
        }
    }

    fn ensure_import_conflict_dialog(&mut self) {
        let Some(conflict) = self.import_conflicts.first() else {
            self.import_conflict_dialog = None;
            return;
        };
        if self
            .import_conflict_dialog
            .as_ref()
            .is_none_or(|dialog| dialog.token != conflict.token)
        {
            self.import_conflict_dialog = Some(self.build_import_conflict_dialog(conflict));
        }
    }
}

/// Geometry of the unsaved-changes ("Save changes?") modal.
struct UnsavedGeom {
    panel: Rect,
    save: Rect,
    discard: Rect,
    cancel: Rect,
}

impl ContactsApp {
    fn unsaved_geometry(&self) -> UnsavedGeom {
        let scale = self.layout_scale();
        let w = (420.0 * scale).min((self.rect.width() - 32.0).max(280.0));
        let h = (156.0 * scale).min((self.rect.height() - 32.0).max(220.0));
        let cx = self.rect.center().x;
        let cy = self.rect.center().y;
        let panel = Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
        let button_h = 32.0 * scale;
        let by = panel.y1 - (16.0 * scale + button_h);
        let save = Rect::new(panel.x1 - 16.0 - 96.0, by, panel.x1 - 16.0, by + button_h);
        let discard = Rect::new(save.x0 - 8.0 - 96.0, by, save.x0 - 8.0, by + button_h);
        let cancel = Rect::new(panel.x0 + 16.0, by, panel.x0 + 16.0 + 88.0, by + button_h);
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

pub(crate) fn detail_section(ctx: &mut PaintCtx<'_>, x0: f64, y: &mut f64, title: &str) {
    let theme = ctx.theme;
    let scale = f64::from(theme.text_scale).clamp(1.0, 2.0);
    *y += 10.0 * scale;
    ctx.text.draw_text_weighted(
        ctx.scene,
        title,
        11.0,
        theme.tokens.colors.text_dim,
        ctx.transform * Affine::translate((x0, *y)),
        None,
        TextWeight::Semibold,
    );
    *y += 22.0 * scale;
}

pub(crate) fn detail_field(
    ctx: &mut PaintCtx<'_>,
    x0: f64,
    x1: f64,
    y: &mut f64,
    label: &str,
    value: &str,
) {
    let theme = ctx.theme;
    let c = &theme.tokens.colors;
    let sp = &theme.tokens.spacing;
    // The label column is sized from the widest label the app can show, so
    // every value in a card starts on one edge and a scaled label never runs
    // under its value. At 1× the authored 78px column still wins.
    let widest = f64::from(ctx.text.measure(DETAIL_WIDEST_LABEL, text_sm(), None).0);
    let label_col = (widest + f64::from(sp.s4)).max(DETAIL_LABEL_COL);
    let (_, label_h) = ctx.text.measure(label, text_sm(), None);
    let (_, value_line_h) = ctx.text.measure("Ag", text_base(), None);
    // Label and the value's first line share a centre.
    let label_y = *y + (f64::from(value_line_h) - f64::from(label_h)) / 2.0;
    ctx.text.draw_text(
        ctx.scene,
        label,
        text_sm(),
        c.text_dim,
        ctx.transform * Affine::translate((x0, label_y)),
        Some((label_col - f64::from(sp.s2)) as f32),
    );
    let vw = (x1 - x0 - label_col).max(40.0);
    let h = f64::from(ctx.text.measure(value, text_base(), Some(vw as f32)).1);
    ctx.text.draw_text(
        ctx.scene,
        value,
        text_base(),
        c.text_primary,
        ctx.transform * Affine::translate((x0 + label_col, *y)),
        Some(vw as f32),
    );
    *y += h.max(20.0) + f64::from(sp.s2);
}

/// Authored 1× width of the detail label column.
const DETAIL_LABEL_COL: f64 = 78.0;
/// The widest label a detail card can show (see `TYPE_OPTIONS`, "date",
/// "url"); it sizes the label column so values align across every row.
const DETAIL_WIDEST_LABEL: &str = "mobile";

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
pub(crate) fn toolbar_button(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    label: &str,
    primary: bool,
    hovered: bool,
    pressed: bool,
) {
    let theme = ctx.theme;
    if rect.width() <= 0.0 {
        return;
    }
    let style = if primary {
        ButtonStyle::Primary
    } else {
        ButtonStyle::Neutral
    };
    let icon = match label {
        "↓" => Some(Icon::Download),
        "↑" => Some(Icon::ArrowUp),
        "+" => Some(Icon::Plus),
        "edit-icon" => Some(Icon::Pencil),
        "delete-icon" => Some(Icon::Trash),
        _ => None,
    };
    let mut face = ButtonFace::new(rect, if icon.is_some() { "" } else { label })
        .style(style)
        .radius(f64::from(theme.tokens.radii.md))
        .text_size(13.0)
        .weight(TextWeight::Medium)
        .hovered(hovered)
        .pressed(pressed);
    if let Some(icon) = icon {
        face = face.icon(icon);
    }
    Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
}

fn add_button(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
) {
    // The shared button face: it centres its label on the measured line and
    // carries hover/press/theming, where the previous local stroke-and-text
    // sat the label on the font size (1.5px low) and had no states at all.
    // Neutral matches the type buttons beside each row, so the add action
    // reads as one more bounded control in the same family.
    Button::paint(
        scene,
        t,
        tr,
        theme,
        &ButtonFace::new(rect, label)
            .style(ButtonStyle::Neutral)
            .radius(f64::from(theme.tokens.radii.md))
            .text_size(12.0)
            .weight(TextWeight::Medium),
    );
}

pub(crate) fn chip(
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

/// A floating Panel (HIG §9.1/§9.5): the theme's elevation shadow, the Panel
/// ground and the derived Panel border — not a literal black halo under a
/// declared border that vanishes on the dark preset.
pub(crate) fn menu_panel(scene: &mut Scene, t: Affine, theme: &Theme, panel: Rect) {
    let radius = f64::from(theme.tokens.radii.md);
    shadow(scene, t, panel, radius, &theme.tokens.elevation.e3);
    let shape = RoundedRect::from_rect(panel, radius);
    scene.fill(
        Fill::NonZero,
        t,
        ui::chrome::panel_ground(theme),
        None,
        &shape,
    );
    scene.stroke(
        &Stroke::new(1.0),
        t,
        ui::chrome::panel_border(theme),
        None,
        &edge_outline(panel, radius),
    );
}

/// The lift a chosen or hovered row takes on `ground` (HIG §9.3), derived
/// against that ground. `chrome::selection_tier` measures its lift from
/// `surfaces.base`; a row inside a menu or dialog sits on the Panel ground,
/// which on the dark and high-contrast presets lies within 0.01 of that
/// lift, so the chosen menu row was invisible there. Until
/// `chrome::selection_tier_on(theme, state, ground)` is available, the row
/// takes the ground's own registered hairline step as its lift (the full
/// step when active, half of it on hover) and derives its edge and glow
/// from the fill that results.
pub(crate) fn row_tier_on(ground: Color, state: SelectionState) -> SelectionTier {
    let lifted = state.active || state.pressed;
    let step = edge_on(ground);
    let fill = if lifted {
        over(ground, step, step.components[3])
    } else if state.hover {
        over(ground, step, step.components[3] / 2.0)
    } else {
        Color::TRANSPARENT
    };
    SelectionTier {
        fill,
        border: lifted.then(|| edge_on(fill)),
        inner_glow: lifted.then(|| Colors::hairline_on(fill, GLOW_ALPHA)),
        scale: 1.0,
    }
}

/// The selected list row's pill: inset `s2` from the pane sides and half an
/// `s1` from the row band's top and bottom, so neighbouring rows' pills
/// keep one `s1` between them (HIG §7: nothing touches).
fn selection_pill(row: Rect, sp: &Spacing) -> Rect {
    let half_gap = f64::from(sp.s1) / 2.0;
    Rect::new(
        row.x0 + f64::from(sp.s2),
        row.y0 + half_gap,
        row.x1 - f64::from(sp.s2),
        row.y1 - half_gap,
    )
}

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
    // Hover and the chosen item are lifts (HIG §9.3) off the menu's Panel
    // ground, on a pill inset half an `s1` from the row band so neighbouring
    // rows keep a token gap.
    let half_gap = f64::from(theme.tokens.spacing.s1) / 2.0;
    let pill = Rect::new(rect.x0, rect.y0 + half_gap, rect.x1, rect.y1 - half_gap);
    let radius = f64::from(theme.tokens.radii.sm);
    let tier = row_tier_on(
        ui::chrome::panel_ground(theme),
        SelectionState::new(selected, hover, false),
    );
    if tier.fill.components[3] > 0.0 {
        scene.fill(
            Fill::NonZero,
            t,
            tier.fill,
            None,
            &RoundedRect::from_rect(pill, radius),
        );
        if let Some(border) = tier.border {
            scene.stroke(
                &Stroke::new(1.0),
                t,
                border,
                None,
                &edge_outline(pill, radius),
            );
        }
    }
    let col = c.text_primary;
    tr.draw_text(
        scene,
        label,
        13.0,
        col,
        t * Affine::translate((
            rect.x0 + 12.0,
            rect.center().y - 8.0 * f64::from(theme.text_scale),
        )),
        Some((rect.width() - 20.0) as f32),
    );
}

/// A non-hovered menu row that paints its label in `color` (used for the
/// destructive "Delete" action in the context menu). The hovered state is
/// drawn by the caller via the regular [`menu_item`] (the hover lift).
fn menu_item_colored(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    color: Color,
) {
    tr.draw_text(
        scene,
        label,
        13.0,
        color,
        t * Affine::translate((
            rect.x0 + 12.0,
            rect.center().y - 8.0 * f64::from(theme.text_scale),
        )),
        Some((rect.width() - 20.0) as f32),
    );
}

pub(crate) fn draw_checkbox(
    scene: &mut Scene,
    t: Affine,
    theme: &Theme,
    rect: Rect,
    checked: bool,
) {
    Checkbox::paint(
        scene,
        t,
        theme,
        &CheckboxFace::new(rect, checked).transparent_unchecked(),
    );
}

pub(crate) fn draw_avatar(
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

// =====================================================================
// Widget impl
// =====================================================================

impl ContactsApp {
    /// Build the retained semantic representation of the selected contact.
    /// The card is hand-painted, so without this adapter a screen reader could
    /// reach the list but not the person's information or its durable actions.
    fn accessibility_detail(&self) -> Option<AccessNode> {
        let id = self.selected?;
        let contact = self.store.get(id)?;
        let mut children = vec![
            AccessNode::new(Role::Heading { level: 1 })
                .with_name(contact.display_name())
                .with_hashed_key(("contacts.detail.heading", id)),
            AccessNode::new(Role::StaticText)
                .with_optional_name((!contact.subtitle().is_empty()).then(|| contact.subtitle()))
                .with_hashed_key(("contacts.detail.subtitle", id)),
            AccessNode::new(Role::ToggleButton)
                .with_name(if contact.favorite {
                    "Remove from favorites"
                } else {
                    "Add to favorites"
                })
                .with_action_target(format!("contacts.detail.favorite:{id}"))
                .with_bounds(self.hit.fav)
                .with_state(AccessState::new().focusable(true).checked(contact.favorite)),
            AccessNode::new(Role::Button)
                .with_name("Edit contact")
                .with_action_target(format!("contacts.detail.edit:{id}"))
                .with_bounds(self.hit.edit)
                .with_state(AccessState::new().focusable(true)),
            AccessNode::new(Role::Button)
                .with_name("Delete contact")
                .with_action_target(format!("contacts.detail.delete:{id}"))
                .with_bounds(self.hit.delete)
                .with_state(AccessState::new().focusable(true)),
        ];

        for phone in &contact.phones {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(format!("{} phone", phone.label))
                    .with_value(phone.value.clone())
                    .with_hashed_key(("contacts.detail.phone", id, &phone.label, &phone.value)),
            );
        }
        for email in &contact.emails {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(format!("{} email", email.label))
                    .with_value(email.value.clone())
                    .with_hashed_key(("contacts.detail.email", id, &email.label, &email.value)),
            );
        }
        for address in &contact.addresses {
            let value = address.one_line();
            if !value.is_empty() {
                children.push(
                    AccessNode::new(Role::StaticText)
                        .with_name(format!("{} address", address.label))
                        .with_value(value)
                        .with_hashed_key(("contacts.detail.address", id, &address.label)),
                );
            }
        }
        if let Some(birthday) = contact.birthday {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name("Birthday")
                    .with_value(birthday.pretty())
                    .with_hashed_key(("contacts.detail.birthday", id)),
            );
        }
        if !contact.website.is_empty() {
            let mut website = AccessNode::new(Role::Link)
                .with_name(contact.website.clone())
                .with_action_target(format!("contacts.detail.website:{id}"))
                .with_state(AccessState::new().focusable(true));
            if let Some(link) = &self.website_link {
                website = website.with_bounds(link.get_rect());
            }
            children.push(website);
        }
        if !contact.notes.is_empty() {
            children.push(
                AccessNode::new(Role::Paragraph)
                    .with_name("Notes")
                    .with_value(contact.notes.clone())
                    .with_hashed_key(("contacts.detail.notes", id)),
            );
        }

        Some(
            AccessNode::new(Role::Group)
                .with_name("Contact details")
                .with_hashed_key(("contacts.detail", id))
                .with_children(children),
        )
    }

    /// Build semantics for the locally-buffered edit form. `SetValue` routes
    /// to the exact same draft fields as physical typing; Save is the sole
    /// durable boundary and dispatches the canonical [`ContactsAction`](crate::actions::ContactsAction).
    #[allow(
        clippy::too_many_lines,
        reason = "the hand-painted editor's semantic tree stays beside the corresponding stable targets"
    )]
    fn accessibility_form(&self) -> Option<AccessNode> {
        let form = self.form.as_ref()?;
        let id = form.draft.id;
        let mut children = vec![
            AccessNode::new(Role::Heading { level: 1 })
                .with_name(if form.is_new {
                    "New contact"
                } else {
                    "Edit contact"
                })
                .with_hashed_key(("contacts.form.heading", id)),
            AccessNode::new(Role::ToggleButton)
                .with_name("Favorite")
                .with_action_target(format!("contacts.form.favorite:{id}"))
                .with_bounds(self.hit.fav_toggle)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .checked(form.draft.favorite),
                ),
            AccessNode::new(Role::Button)
                .with_name("Cancel")
                .with_action_target(format!("contacts.form.cancel:{id}"))
                .with_bounds(self.hit.cancel)
                .with_state(AccessState::new().focusable(true)),
            AccessNode::new(Role::Button)
                .with_name(if form.is_new {
                    "Create contact"
                } else {
                    "Save changes"
                })
                .with_action_target(format!("contacts.form.save:{id}"))
                .with_bounds(self.hit.save)
                .with_state(AccessState::new().focusable(true)),
        ];
        for input in &form.inputs {
            let (name, _) = form_field_metadata(input.slot);
            let (anchor, focus) = input.ti.selection_anchor_focus_public().unwrap_or_else(|| {
                let cursor = input.ti.cursor_pos();
                (cursor, cursor)
            });
            let role = if input.slot == Slot::Notes {
                Role::MultilineTextInput
            } else {
                Role::TextInput
            };
            children.push(
                AccessNode::new(role)
                    .with_name(name)
                    .with_value(input.ti.value())
                    .with_text_selection(anchor, focus)
                    .with_action_target(form_field_target(id, input.slot))
                    .with_bounds(input.ti.get_rect())
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(input.ti.is_focused())
                            .multiline(input.slot == Slot::Notes),
                    ),
            );
        }
        for (target, bounds) in &self.hit.type_chips {
            let expanded = matches!(
                self.popup,
                Some(Popup::TypeMenu { target: open, .. }) if open == *target
            );
            children.push(
                AccessNode::new(Role::ComboBox)
                    .with_name(type_target_name(*target))
                    .with_value(type_label(&form.draft, *target))
                    .with_action_target(type_chip_target(id, *target))
                    .with_bounds(*bounds)
                    .with_state(AccessState::new().focusable(true).expanded(expanded)),
            );
        }
        for (target, bounds) in &self.hit.remove_btns {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(format!("Remove {}", type_target_name(*target)))
                    .with_description("Remove this field from the draft")
                    .with_action_target(remove_type_target(id, *target))
                    .with_bounds(*bounds)
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        children.extend([
            AccessNode::new(Role::Button)
                .with_name("Add phone")
                .with_action_target(format!("contacts.form.add-phone:{id}"))
                .with_bounds(self.hit.add_phone)
                .with_state(AccessState::new().focusable(true)),
            AccessNode::new(Role::Button)
                .with_name("Add email")
                .with_action_target(format!("contacts.form.add-email:{id}"))
                .with_bounds(self.hit.add_email)
                .with_state(AccessState::new().focusable(true)),
            AccessNode::new(Role::Button)
                .with_name("Add address")
                .with_action_target(format!("contacts.form.add-address:{id}"))
                .with_bounds(self.hit.add_address)
                .with_state(AccessState::new().focusable(true)),
            AccessNode::new(Role::Button)
                .with_name("Choose birthday from calendar")
                .with_optional_value(form.draft.birthday.map(|birthday| birthday.pretty()))
                .with_action_target(calendar_target(id))
                .with_bounds(self.hit.cal_btn)
                .with_state(AccessState::new().focusable(true)),
        ]);

        Some(
            AccessNode::new(Role::Group)
                .with_name("Contact editor")
                .with_hashed_key(("contacts.form", id))
                .with_children(children),
        )
    }

    fn accessibility_type_menu(
        &self,
        id: ContactId,
        target: TypeTarget,
        anchor: Rect,
    ) -> AccessNode {
        let (panel, rows) = self.popup_menu_geometry(anchor, TYPE_OPTIONS.len(), 140.0);
        let selected = self
            .form
            .as_ref()
            .map_or("type", |form| type_label(&form.draft, target));
        let children = TYPE_OPTIONS
            .iter()
            .zip(rows)
            .enumerate()
            .map(|(index, (label, bounds))| {
                AccessNode::new(Role::RadioButton)
                    .with_name(*label)
                    .with_action_target(type_option_target(id, target, label))
                    .with_bounds(bounds)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.popup_focus == index)
                            .checked(*label == selected),
                    )
            })
            .collect();
        AccessNode::new(Role::Menu)
            .with_name(format!("Choose {}", type_target_name(target)))
            .with_hashed_key(("contacts.form.type-menu", id, type_target_key(target)))
            .with_bounds(panel)
            .with_children(children)
    }

    fn accessibility_group_menu(&self, anchor: Rect) -> AccessNode {
        let options = self.group_filter_options();
        let (panel, rows) = self.popup_menu_geometry(anchor, options.len(), 180.0);
        let children = options
            .iter()
            .zip(rows)
            .enumerate()
            .map(|(index, (option, bounds))| {
                let selected = match self.group_filter.as_deref() {
                    None => option == "All",
                    Some(FAVORITES_FILTER) => option == "Favorites",
                    Some(group) => option == group,
                };
                AccessNode::new(Role::RadioButton)
                    .with_name(option.clone())
                    .with_action_target(group_option_target(option))
                    .with_bounds(bounds)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.popup_focus == index)
                            .checked(selected),
                    )
            })
            .collect();
        AccessNode::new(Role::Menu)
            .with_name("Filter contacts by group")
            .with_hashed_key("contacts.group.menu")
            .with_bounds(panel)
            .with_children(children)
    }

    fn accessibility_sort_menu(&self, anchor: Rect) -> AccessNode {
        let options = [(SortKey::Name, "Name"), (SortKey::Org, "Organization")];
        let (panel, rows) = self.popup_menu_geometry(anchor, options.len(), 180.0);
        let children = options
            .iter()
            .zip(rows)
            .enumerate()
            .map(|(index, ((sort, label), bounds))| {
                AccessNode::new(Role::RadioButton)
                    .with_name(*label)
                    .with_action_target(format!("contacts.sort.option:{}", sort.label()))
                    .with_bounds(bounds)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.popup_focus == index)
                            .checked(*sort == self.sort),
                    )
            })
            .collect();
        AccessNode::new(Role::Menu)
            .with_name("Sort contacts")
            .with_hashed_key("contacts.sort.menu")
            .with_bounds(panel)
            .with_children(children)
    }

    /// The visual row menu is also a real menu to AT-SPI/AccessKit.  This is
    /// important even though Edit, Favourite, and Delete have other routes:
    /// Exporting one selected contact is intentionally contextual, and a
    /// screen-reader user must not need a mouse to reach it.  The target ids
    /// resolve back to the same implementation used by pointer and keyboard
    /// activation below.
    fn accessibility_context_menu(&self, id: ContactId, anchor: Rect) -> AccessNode {
        let (panel, rows) = self.popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
        let favorite = self.store.get(id).is_some_and(|contact| contact.favorite);
        let children = CONTEXT_ACTIONS
            .iter()
            .zip(rows)
            .enumerate()
            .map(|(index, (action, bounds))| {
                AccessNode::new(Role::MenuItem)
                    .with_name(action.label(favorite))
                    .with_action_target(context_action_target(id, *action))
                    .with_bounds(bounds)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.popup_focus == index),
                    )
            })
            .collect();
        AccessNode::new(Role::Menu)
            .with_name("Contact actions")
            .with_hashed_key(("contacts.context-menu", id))
            .with_bounds(panel)
            .with_children(children)
    }

    fn accessibility_calendar(
        &self,
        id: ContactId,
        year: i32,
        month: u32,
        anchor: Rect,
    ) -> AccessNode {
        let (panel, prev, next, cells) = self.calendar_geometry(anchor, year, month);
        let selected = self
            .form
            .as_ref()
            .and_then(|form| form.draft.birthday)
            .map(|birthday| birthday.iso());
        let mut children = vec![
            AccessNode::new(Role::Button)
                .with_name("Previous month")
                .with_action_target(format!("contacts.form.calendar-prev:{id}"))
                .with_bounds(prev)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.popup_focus == 0),
                ),
            AccessNode::new(Role::Button)
                .with_name("Next month")
                .with_action_target(format!("contacts.form.calendar-next:{id}"))
                .with_bounds(next)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.popup_focus == 1),
                ),
        ];
        children.extend(cells.into_iter().map(|(day, bounds)| {
            let iso = format!("{year:04}-{month:02}-{day:02}");
            AccessNode::new(Role::Button)
                .with_name(format!("{} {day}, {year}", date::month_name(month)))
                .with_action_target(calendar_day_target(id, year, month, day))
                .with_bounds(bounds)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.popup_focus == day as usize + 1)
                        .selected(selected.as_deref() == Some(iso.as_str())),
                )
        }));
        AccessNode::new(Role::Group)
            .with_name(format!(
                "Birthday calendar, {} {year}",
                date::month_name(month)
            ))
            .with_hashed_key(("contacts.form.calendar", id))
            .with_bounds(panel)
            .with_children(children)
    }
}

impl Widget for ContactsApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource,
        dyn ui::undo::UndoPoints
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::CONTACTS_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        if let Some((token, import_as_new)) = self.pending_conflict_resolution.take()
            && let Err(error) = self.resolve_import_conflict(state, &token, import_as_new)
        {
            self.show_toast(format!("Couldn't resolve import review: {error}"));
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
                self.relayout = true;
                self.dirty = true;
            }
            if let Some(form) = &mut self.form {
                if let Some(canonical) = self.store.get(form.draft.id) {
                    form.reconcile(canonical);
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
        }

        crate::priority_contacts::publish_if_changed(&self.store);

        if self.search_heads != self.model_heads {
            self.refresh_search_provider();
        }
        self.refresh_duplicate_candidate();

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
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.text_scale = theme.text_scale;
        self.spacing = theme.tokens.spacing;
        self.metrics = theme.tokens.metrics;
        self.export_dialog.set_text_scale(theme.text_scale);
        // `new_leaf` is typed `-> TaffyResult<NodeId>` for API generality but
        // cannot actually return `Err` for a fresh leaf (see the
        // `TaffyTreeExt` doc comment in `ui::widget::mod`), and
        // `Widget::build_layout` returns a bare `NodeId` across every
        // implementor, so there is nowhere to surface a theoretical error.
        #[allow(
            clippy::unwrap_used,
            reason = "taffy::TaffyTree::new_leaf cannot return Err; see ui::widget::TaffyTreeExt"
        )]
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
        let l = taffy.layout(node).copied().unwrap_or_default();
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
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Track logical cursor position.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
        if self.handle_event_modals(event, ctx) {
            return;
        }

        if self.handle_event_dispatch(event, ctx) {
            return;
        }

        // Forward pointer-move to focused inputs so hover/drag-select tracks.
        self.forward_event_to_inputs(event, ctx);
    }

    fn name(&self) -> &'static str {
        "ContactsApp"
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
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if cancellation_requested && self.resource_invocation == Some(invocation) {
            self.resource_invocation = None;
            self.import_rx = None;
            self.import_parse_rx = None;
            let exhausted = self.advance_import_generation().is_err();
            self.export_rx = None;
            self.pending_export = None;
            if exhausted {
                self.show_toast(
                    "Import cancelled; restart Contacts before importing again (generation exhausted)",
                );
            }
            return Some(tilo_actions::InvocationUpdate::Cancelled {
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
        self.confirm_delete.is_some()
            || !self.import_conflicts.is_empty()
            || self.export_dialog.is_open()
            || self.duplicate_review.is_some()
            || self.pending_switch.is_some()
    }

    fn modal_capture(&self) -> Option<Rect> {
        if let Some(review) = self.duplicate_review.as_ref() {
            return Some(self.duplicate_review_geometry(review).panel);
        }
        self.import_conflict_dialog
            .as_ref()
            .and_then(|review| review.dialog.modal_capture())
            .or_else(|| {
                self.delete_dialog
                    .as_ref()
                    .and_then(|confirm| confirm.dialog.modal_capture())
            })
            .or_else(|| {
                self.export_dialog.is_open().then(|| {
                    self.export_dialog
                        .geometry(self.rect, &self.store, self.sort)
                        .panel
                })
            })
            .or_else(|| {
                self.pending_switch
                    .is_some()
                    .then(|| self.unsaved_geometry().panel)
            })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the accessibility tree mirrors the toolbar, empty state, search, and contact list in one auditable hierarchy"
    )]
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        if let Some(review) = self.duplicate_review.as_ref() {
            let geometry = self.duplicate_review_geometry(review);
            let mut children = Vec::new();
            for (index, row) in geometry.rows.iter().enumerate() {
                let field = row.field;
                let take_source = review.take_from_source.contains(&field);
                if geometry.full_screen {
                    let current_value = Self::duplicate_field_value(&review.survivor, field);
                    let other_value = Self::duplicate_field_value(&review.source, field);
                    children.push(
                        AccessNode::new(Role::Group)
                            .with_name(Self::duplicate_field_label(field))
                            .with_bounds(row.card)
                            .with_children(vec![
                                AccessNode::new(Role::RadioButton)
                                    .with_name(format!("Current: {current_value}"))
                                    .with_action_target(format!(
                                        "contacts.duplicate.field:{field:?}:current"
                                    ))
                                    .with_bounds(row.current)
                                    .with_state(
                                        AccessState::new()
                                            .focusable(true)
                                            .focused(review.focus == index && !take_source)
                                            .checked(!take_source),
                                    ),
                                AccessNode::new(Role::RadioButton)
                                    .with_name(format!("Other: {other_value}"))
                                    .with_action_target(format!(
                                        "contacts.duplicate.field:{field:?}:other"
                                    ))
                                    .with_bounds(row.other)
                                    .with_state(
                                        AccessState::new()
                                            .focusable(true)
                                            .focused(review.focus == index && take_source)
                                            .checked(take_source),
                                    ),
                            ]),
                    );
                } else {
                    children.push(
                        AccessNode::new(Role::RadioButton)
                            .with_name(format!(
                                "{}: keep {}",
                                Self::duplicate_field_label(field),
                                if take_source {
                                    review.source.display_name()
                                } else {
                                    review.survivor.display_name()
                                }
                            ))
                            .with_description("Activate to choose the other contact's value")
                            .with_action_target(format!("contacts.duplicate.field:{field:?}"))
                            .with_bounds(row.card)
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .focused(review.focus == index)
                                    .checked(take_source),
                            ),
                    );
                }
            }
            children.extend([
                AccessNode::new(Role::Button)
                    .with_name("Cancel merge")
                    .with_action_target("contacts.duplicate.cancel")
                    .with_bounds(geometry.cancel)
                    .with_state(AccessState::new().focusable(true)),
                AccessNode::new(Role::Button)
                    .with_name("Merge contacts")
                    .with_description("Confirmation action. The source contact is removed; Undo is available in history.")
                    .with_action_target("contacts.duplicate.merge")
                    .with_bounds(geometry.merge)
                    .with_state(AccessState::new().focusable(true)),
            ]);
            return Some(
                AccessNode::new(Role::Dialog)
                    .with_name("Review possible duplicate")
                    .with_description("Review every conflicting scalar field before merging. Unique repeated data is kept.")
                    .with_bounds(geometry.panel)
                    .with_state(AccessState::new().modal(true))
                    .with_children(children),
            );
        }
        if self.export_dialog.is_open() {
            return Some(
                self.export_dialog
                    .accessibility(self.rect, &self.store, self.sort),
            );
        }
        if self.pending_switch.is_some() {
            let g = self.unsaved_geometry();
            return Some(
                AccessNode::new(Role::Dialog)
                    .with_name("Save changes?")
                    .with_description(
                        "You have unsaved changes. Save, discard, or cancel switching contacts.",
                    )
                    .with_bounds(g.panel)
                    .with_state(AccessState::new().modal(true))
                    .with_children(vec![
                        AccessNode::new(Role::Button)
                            .with_name("Cancel")
                            .with_action_target("contacts.unsaved.cancel")
                            .with_bounds(g.cancel)
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .focused(self.unsaved_focus == UnsavedFocus::Cancel),
                            ),
                        AccessNode::new(Role::Button)
                            .with_name("Discard changes")
                            .with_action_target("contacts.unsaved.discard")
                            .with_bounds(g.discard)
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .focused(self.unsaved_focus == UnsavedFocus::Discard),
                            ),
                        AccessNode::new(Role::Button)
                            .with_name("Save changes")
                            .with_action_target("contacts.unsaved.save")
                            .with_bounds(g.save)
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .focused(self.unsaved_focus == UnsavedFocus::Save),
                            ),
                    ]),
            );
        }
        if let Some(conflict) = self.import_conflicts.first() {
            let existing_name = self
                .store
                .get(conflict.existing)
                .map_or_else(|| "current contact".to_string(), Contact::display_name);
            let description = format!(
                "{} shares a vCard UID with {existing_name}, but the details differ. Choose how to resolve this import.",
                conflict.incoming.display_name()
            );
            return Some(
                AccessNode::new(Role::Dialog)
                    .with_name("Review imported contact")
                    .with_description(description)
                    .with_state(AccessState::new().modal(true))
                    .with_children(vec![
                        AccessNode::new(Role::Button)
                            .with_name("Keep existing contact")
                            .with_description("Discard the imported version and keep the current contact")
                            .with_action_target(import_conflict_target(
                                conflict.token,
                                "keep-existing",
                            ))
                            .with_state(AccessState::new().focusable(true)),
                        AccessNode::new(Role::Button)
                            .with_name("Import as new contact")
                            .with_description("Confirmation action: add the imported version as a separate contact")
                            .with_action_target(import_conflict_target(
                                conflict.token,
                                "import-as-new",
                            ))
                            .with_state(AccessState::new().focusable(true)),
                    ]),
            );
        }
        if let Some(dialog) = self
            .delete_dialog
            .as_ref()
            .and_then(|confirm| confirm.dialog.accessibility())
        {
            return Some(dialog);
        }

        let importing = self.import_rx.is_some() || self.import_parse_rx.is_some();
        let exporting = self.export_rx.is_some();
        let mut toolbar_actions = vec![
            AccessNode::new(Role::Button)
                .with_name(if importing {
                    "Importing contacts"
                } else {
                    "Import contacts"
                })
                .with_action_target("contacts.import")
                .with_bounds(self.hit.import)
                .with_state(
                    AccessState::new()
                        .focusable(!importing)
                        .disabled(importing)
                        .busy(importing),
                ),
        ];
        if !self.store.is_empty() {
            toolbar_actions.extend([AccessNode::new(Role::Button)
                .with_name(if exporting {
                    "Exporting contacts"
                } else {
                    "Export contacts"
                })
                .with_action_target("contacts.export")
                .with_bounds(self.hit.export)
                .with_state(
                    AccessState::new()
                        .focusable(!exporting)
                        .disabled(exporting)
                        .busy(exporting),
                )]);
            if self.has_duplicate_candidate() {
                toolbar_actions.push(
                    AccessNode::new(Role::Button)
                        .with_name("Review possible duplicate")
                        .with_description(
                            "Compare the matching contacts before any merge. Nothing is merged automatically.",
                        )
                        .with_action_target("contacts.duplicates")
                        .with_bounds(self.hit.duplicates)
                        .with_state(AccessState::new().focusable(true)),
                );
            }
            toolbar_actions.push(
                AccessNode::new(Role::Button)
                    .with_name("Create contact")
                    .with_action_target("contacts.new")
                    .with_bounds(self.hit.new)
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        let toolbar = AccessNode::new(Role::Toolbar)
            .with_name("Contact actions")
            .with_children(toolbar_actions);

        // `list_rows` is deliberately only the viewport + a tiny overscan
        // window.  Mirror that in the semantic tree: exposing every one of a
        // large directory's rows would undo the rendering bound and can stall
        // an assistive client.  Positions still describe the full result set,
        // and global search / canonical resource resolution reach every exact
        // contact outside this window.
        let list_children = if self.visible_row_count == 0 {
            match self.empty_list_action {
                EmptyListAction::Create => vec![
                    AccessNode::new(Role::Button)
                        .with_name("Create contact")
                        .with_description("Start an empty contact card")
                        .with_action_target("contacts.empty.new")
                        .with_bounds(self.empty_action_rect)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.empty_action_focused),
                        ),
                ],
                EmptyListAction::ClearFilters => vec![
                    AccessNode::new(Role::Button)
                        .with_name("Clear search and group filter")
                        .with_description("Show every contact in this address book")
                        .with_action_target("contacts.empty.clear-filters")
                        .with_bounds(self.empty_action_rect)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.empty_action_focused),
                        ),
                ],
                EmptyListAction::None => vec![
                    AccessNode::new(Role::StaticText)
                        .with_name("No matching contacts")
                        .with_description("Try another name, company, or group."),
                ],
            }
        } else {
            self.list_rows
                .iter()
                .enumerate()
                .filter_map(|(offset, (id, bounds))| {
                    let contact = self.store.get(*id)?;
                    let index = self.list_row_start + offset;
                    Some(
                        AccessNode::new(Role::ListItem)
                            .with_name(contact.display_name())
                            .with_optional_value(
                                (!contact.subtitle().is_empty()).then(|| contact.subtitle()),
                            )
                            .with_action_target(format!("contacts.select:{id}"))
                            .with_position(index + 1, self.visible_row_count)
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .context_menu(true)
                                    .selected(self.selected == Some(*id)),
                            )
                            .with_bounds(*bounds)
                            .with_children(Vec::new()),
                    )
                })
                .collect()
        };

        if self.store.is_empty() && self.mode == Mode::View {
            let first_run = AccessNode::new(Role::Group)
                .with_name("No contacts yet")
                .with_description("Create a contact, or import an existing vCard address book.")
                .with_children(list_children);
            return Some(
                AccessNode::new(Role::Window)
                    .with_name("Contacts")
                    .with_children(vec![toolbar, first_run]),
            );
        }

        let search = AccessNode::new(Role::TextInput)
            .with_name("Search contacts")
            .with_value(self.search.value())
            .with_action_target("contacts.search")
            .with_bounds(self.hit.search)
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.search.is_focused()),
            );
        let list = AccessNode::new(Role::List)
            .with_name("Contacts")
            .with_bounds(self.list_body)
            .with_children(list_children);
        let list_controls = AccessNode::new(Role::Group)
            .with_name("Contact list controls")
            .with_children(vec![
                AccessNode::new(Role::ComboBox)
                    .with_name(format!("Sort by {}", self.sort.label()))
                    .with_action_target("contacts.sort")
                    .with_bounds(self.hit.sort)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .expanded(matches!(self.popup, Some(Popup::Sort { .. }))),
                    ),
                AccessNode::new(Role::ComboBox)
                    .with_name("Group filter")
                    .with_value(match self.group_filter.as_deref() {
                        Some(FAVORITES_FILTER) => "Favorites",
                        Some(group) => group,
                        None => "All",
                    })
                    .with_action_target("contacts.group")
                    .with_bounds(self.hit.group)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .expanded(matches!(self.popup, Some(Popup::GroupFilter { .. }))),
                    ),
            ]);

        let list_is_visible = !self.narrow || self.push.pane() == PushPane::Master;
        let detail_is_visible = !self.narrow || self.push.pane() == PushPane::Detail;
        let mut children = vec![toolbar];
        if list_is_visible {
            children.extend([search, list_controls, list]);
        }
        if detail_is_visible {
            if self.narrow && self.hit.back.width() > 0.0 {
                children.push(
                    AccessNode::new(Role::Button)
                        .with_name("Back to contacts")
                        .with_action_target("contacts.back")
                        .with_bounds(self.hit.back)
                        .with_state(AccessState::new().focusable(true)),
                );
            }
            if self.mode == Mode::Edit {
                if let Some(form) = self.accessibility_form() {
                    children.push(form);
                }
            } else if let Some(detail) = self.accessibility_detail() {
                children.push(detail);
            }
        }
        if let Some((message, _)) = &self.toast {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(message.clone())
                    .with_hashed_key("contacts.status")
                    .with_live(ui::a11y::Politeness::Assertive),
            );
        }
        match self.popup {
            Some(Popup::TypeMenu { target, anchor }) if self.mode == Mode::Edit => {
                if let Some(id) = self.form.as_ref().map(|form| form.draft.id) {
                    children.push(self.accessibility_type_menu(id, target, anchor));
                }
            }
            Some(Popup::Sort { anchor }) => children.push(self.accessibility_sort_menu(anchor)),
            Some(Popup::GroupFilter { anchor }) => {
                children.push(self.accessibility_group_menu(anchor));
            }
            Some(Popup::ContextMenu { target, anchor }) => {
                children.push(self.accessibility_context_menu(target, anchor));
            }
            Some(Popup::Calendar {
                year,
                month,
                anchor,
            }) if self.mode == Mode::Edit => {
                if let Some(id) = self.form.as_ref().map(|form| form.draft.id) {
                    children.push(self.accessibility_calendar(id, year, month, anchor));
                }
            }
            _ => {}
        }

        Some(
            AccessNode::new(Role::Window)
                .with_name("Contacts")
                .with_children(children),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the semantic adapter deliberately keeps every painted Contacts control and its action route in one auditable dispatch table"
    )]
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        use ui::a11y::Action;
        let (state, _) = ui::doc::document(state, path);

        // List rows are hand-painted, but their standard AT-SPI context-menu
        // action opens precisely the same menu as a secondary pointer click.
        // This gives keyboard and screen-reader users access to the
        // per-contact export workflow instead of treating it as mouse-only.
        if matches!(action, Action::ShowContextMenu)
            && let Some(raw) = target.strip_prefix("contacts.select:")
            && let Ok(id) = raw.parse::<ContactId>()
            && let Some((_, row)) = self.list_rows.iter().find(|(row_id, _)| *row_id == id)
        {
            self.selected = Some(id);
            self.open_popup(Popup::ContextMenu {
                target: id,
                anchor: Rect::new(row.x0 + 40.0, row.center().y, row.x0 + 40.0, row.center().y),
            });
            self.dirty = true;
            return true;
        }

        // Menu items are exposed independently of the list row, with stable
        // targets.  Only honor them while their matching transient menu is
        // still open, preventing a stale assistive request from operating on
        // a contact after the user has dismissed or retargeted the menu.
        if matches!(action, Action::Activate)
            && let Some(Popup::ContextMenu {
                target: menu_target,
                ..
            }) = self.popup.as_ref()
            && let Some(context_action) = CONTEXT_ACTIONS
                .iter()
                .copied()
                .find(|candidate| target == context_action_target(*menu_target, *candidate))
        {
            let menu_target = *menu_target;
            self.popup = None;
            self.execute_context_action(state, menu_target, context_action);
            return true;
        }

        if matches!(action, Action::Activate) && self.duplicate_review.is_some() {
            if target == "contacts.duplicate.cancel" {
                self.duplicate_review = None;
                self.dirty = true;
                return true;
            }
            if target == "contacts.duplicate.merge" {
                self.merge_duplicate_review(state);
                return true;
            }
            if let Some(raw) = target.strip_prefix("contacts.duplicate.field:") {
                let (field_token, choice) = raw.split_once(':').unwrap_or((raw, "toggle"));
                let field = Self::duplicate_field_from_token(field_token);
                if let Some(field) = field
                    && let Some(review) = self.duplicate_review.as_mut()
                {
                    match choice {
                        "current" => {
                            review.take_from_source.remove(&field);
                        }
                        "other" => {
                            review.take_from_source.insert(field);
                        }
                        _ if !review.take_from_source.insert(field) => {
                            review.take_from_source.remove(&field);
                        }
                        _ => {}
                    }
                    self.dirty = true;
                    return true;
                }
            }
        }

        if matches!(action, Action::Activate)
            && let Some(conflict) = self.import_conflicts.first()
        {
            let keep = import_conflict_target(conflict.token, "keep-existing");
            let import_as_new = import_conflict_target(conflict.token, "import-as-new");
            if target == keep || target == import_as_new {
                self.pending_conflict_resolution =
                    Some((conflict.token.to_string(), target == import_as_new));
                self.dirty = true;
                return true;
            }
        }

        if self.export_dialog.is_open() {
            let (consumed, request) = self.export_dialog.perform_accessibility_action(
                target,
                action,
                self.rect,
                &self.store,
                self.sort,
            );
            if let Some(request) = request {
                self.request_export(request.ids, request.version);
            }
            if consumed {
                self.dirty = true;
                return true;
            }
        }

        if self.pending_switch.is_some() {
            let focus = match target {
                "contacts.unsaved.cancel" => Some(UnsavedFocus::Cancel),
                "contacts.unsaved.discard" => Some(UnsavedFocus::Discard),
                "contacts.unsaved.save" => Some(UnsavedFocus::Save),
                _ => None,
            };
            if let Some(focus) = focus {
                self.unsaved_focus = focus;
                if matches!(action, Action::Activate) {
                    self.resolve_unsaved_focus(state);
                }
                if matches!(action, Action::Focus | Action::Activate) {
                    self.dirty = true;
                    return true;
                }
            }
        }

        // Editable form fields are private draft state until Save. Semantic
        // input therefore follows the same local route as physical typing;
        // the eventual save below remains the one canonical CRDT action.
        if let Some(slot) = self.form.as_ref().and_then(|form| {
            form.inputs
                .iter()
                .find(|input| target == form_field_target(form.draft.id, input.slot))
                .map(|input| input.slot)
        }) {
            match action {
                Action::Focus | Action::Activate => self.focus_form_slot(slot),
                Action::SetValue(value) => {
                    self.focus_form_slot(slot);
                    if let Some(form) = &mut self.form
                        && let Some(input) = form.inputs.iter_mut().find(|input| input.slot == slot)
                    {
                        input.ti.set_text(value);
                    }
                }
                _ => return false,
            }
            self.dirty = true;
            return true;
        }

        if let Some(id) = self.form.as_ref().map(|form| form.draft.id) {
            if target == format!("contacts.form.favorite:{id}")
                && matches!(action, Action::Activate | Action::ToggleCheck)
            {
                if let Some(form) = &mut self.form {
                    form.draft.favorite = !form.draft.favorite;
                }
                self.dirty = true;
                return true;
            }
            if target == format!("contacts.form.save:{id}") && matches!(action, Action::Activate) {
                self.save_edit(state);
                return true;
            }
            if target == format!("contacts.form.cancel:{id}") && matches!(action, Action::Activate)
            {
                self.cancel_edit();
                return true;
            }
            if matches!(action, Action::Activate) {
                if let Some((row, anchor)) = self
                    .hit
                    .type_chips
                    .iter()
                    .find(|(row, _)| target == type_chip_target(id, *row))
                    .copied()
                {
                    self.open_popup(Popup::TypeMenu {
                        target: row,
                        anchor,
                    });
                    self.dirty = true;
                    return true;
                }
                if let Some(row) =
                    self.hit.remove_btns.iter().find_map(|(row, _)| {
                        (target == remove_type_target(id, *row)).then_some(*row)
                    })
                {
                    self.form_remove(row);
                    return true;
                }
                if target == calendar_target(id) {
                    let seed = self.form.as_ref().and_then(|form| {
                        form.inputs
                            .iter()
                            .find(|input| input.slot == Slot::Birthday)
                            .and_then(|input| Birthday::parse(input.ti.value()))
                            .or(form.draft.birthday)
                    });
                    let (year, month) = seed.map_or_else(
                        || {
                            let (year, month, _) = date::today();
                            (year, month)
                        },
                        |birthday| {
                            (
                                birthday.year().unwrap_or_else(|| date::today().0),
                                birthday.month(),
                            )
                        },
                    );
                    self.open_popup(Popup::Calendar {
                        year,
                        month,
                        anchor: self.hit.cal_btn,
                    });
                    self.dirty = true;
                    return true;
                }
                if target == format!("contacts.form.add-phone:{id}") {
                    self.form_add(|draft| draft.phones.push(Labeled::new("mobile", "")));
                    return true;
                }
                if target == format!("contacts.form.add-email:{id}") {
                    self.form_add(|draft| draft.emails.push(Labeled::new("home", "")));
                    return true;
                }
                if target == format!("contacts.form.add-address:{id}") {
                    self.form_add(|draft| {
                        draft.addresses.push(Address {
                            label: "home".into(),
                            ..Default::default()
                        });
                    });
                    return true;
                }
            }

            if matches!(action, Action::Activate | Action::ToggleCheck) {
                for (row, _) in &self.hit.type_chips {
                    for label in TYPE_OPTIONS {
                        if target == type_option_target(id, *row, label)
                            && matches!(
                                self.popup,
                                Some(Popup::TypeMenu { target: open, .. }) if open == *row
                            )
                        {
                            if let Some(form) = &mut self.form {
                                match row {
                                    TypeTarget::Phone(index) => form
                                        .draft
                                        .phones
                                        .get_mut(*index)
                                        .map(|item| item.label = (*label).into()),
                                    TypeTarget::Email(index) => form
                                        .draft
                                        .emails
                                        .get_mut(*index)
                                        .map(|item| item.label = (*label).into()),
                                    TypeTarget::Addr(index) => form
                                        .draft
                                        .addresses
                                        .get_mut(*index)
                                        .map(|item| item.label = (*label).into()),
                                };
                            }
                            self.popup = None;
                            self.dirty = true;
                            return true;
                        }
                    }
                }
            }
        }

        if let Some(id) = self.form.as_ref().map(|form| form.draft.id)
            && matches!(action, Action::Activate)
            && let Some(Popup::Calendar {
                year,
                month,
                anchor,
            }) = self.popup
        {
            if target == format!("contacts.form.calendar-prev:{id}") {
                let (year, month) = date::prev_month(year, month);
                self.popup = Some(Popup::Calendar {
                    year,
                    month,
                    anchor,
                });
                self.dirty = true;
                return true;
            }
            if target == format!("contacts.form.calendar-next:{id}") {
                let (year, month) = date::next_month(year, month);
                self.popup = Some(Popup::Calendar {
                    year,
                    month,
                    anchor,
                });
                self.dirty = true;
                return true;
            }
            for (day, _) in self.calendar_geometry(anchor, year, month).3 {
                if target == calendar_day_target(id, year, month, day) {
                    let iso = format!("{year:04}-{month:02}-{day:02}");
                    if let Some(form) = &mut self.form {
                        form.draft.birthday = Birthday::parse(&iso);
                        if let Some(input) = form
                            .inputs
                            .iter_mut()
                            .find(|input| input.slot == Slot::Birthday)
                        {
                            input.ti.set_text(&iso);
                        }
                    }
                    self.popup = None;
                    self.dirty = true;
                    return true;
                }
            }
        }

        if matches!(action, Action::Activate | Action::ToggleCheck) {
            if let Some(raw) = target.strip_prefix("contacts.detail.favorite:")
                && let Ok(id) = raw.parse::<ContactId>()
                && self.store.get(id).is_some()
            {
                self.toggle_favorite(state, id);
                return true;
            }
            if let Some(raw) = target.strip_prefix("contacts.detail.edit:")
                && let Ok(id) = raw.parse::<ContactId>()
                && let Some(contact) = self.store.get(id).cloned()
            {
                self.begin_edit(contact, false);
                return true;
            }
            if let Some(raw) = target.strip_prefix("contacts.detail.delete:")
                && let Ok(id) = raw.parse::<ContactId>()
                && self.store.get(id).is_some()
            {
                self.selected = Some(id);
                self.confirm_delete = Some(id);
                self.dirty = true;
                return true;
            }
            if let Some(raw) = target.strip_prefix("contacts.detail.website:")
                && let Ok(id) = raw.parse::<ContactId>()
                && self.selected == Some(id)
                && let Some(link) = self.website_link.as_mut()
            {
                link.activate();
                self.dirty = true;
                return true;
            }
        }

        if matches!(action, Action::Activate | Action::ToggleCheck) {
            let groups = self.group_filter_options();
            if let Some(group) = groups
                .iter()
                .find(|group| target == group_option_target(group))
                && matches!(self.popup, Some(Popup::GroupFilter { .. }))
            {
                self.popup = None;
                self.set_group_filter(match group.as_str() {
                    "All" => None,
                    "Favorites" => Some(FAVORITES_FILTER.into()),
                    _ => Some(group.clone()),
                });
                return true;
            }
            for (sort, _) in [(SortKey::Name, "Name"), (SortKey::Org, "Organization")] {
                if target == format!("contacts.sort.option:{}", sort.label())
                    && matches!(self.popup, Some(Popup::Sort { .. }))
                {
                    self.sort = sort;
                    self.list_scroll = 0.0;
                    self.popup = None;
                    self.relayout = true;
                    self.dirty = true;
                    return true;
                }
            }
        }

        match (target, action) {
            ("contacts.empty.new" | "contacts.empty.clear-filters", Action::Focus) => {
                self.empty_action_focused = true;
                self.search.set_focus(false);
                self.dirty = true;
                true
            }
            ("contacts.new" | "contacts.empty.new", Action::Activate) => {
                self.begin_new_contact();
                true
            }
            ("contacts.empty.clear-filters", Action::Activate) => self.activate_empty_list_action(),
            ("contacts.import", Action::Activate) => {
                self.request_import();
                true
            }
            ("contacts.export", Action::Activate) => {
                self.open_export_dialog();
                self.dirty = true;
                true
            }
            ("contacts.duplicates", Action::Activate) => {
                match self.invoke_duplicate_review_operation(state) {
                    Ok(true) => {
                        self.dirty = true;
                        self.relayout = true;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        self.show_toast(format!("Couldn't open duplicate review: {error}"));
                    }
                }
                true
            }
            ("contacts.sort", Action::Activate | Action::Expand) => {
                self.open_popup(Popup::Sort {
                    anchor: self.hit.sort,
                });
                self.dirty = true;
                true
            }
            ("contacts.sort", Action::Collapse) => {
                if matches!(self.popup, Some(Popup::Sort { .. })) {
                    self.popup = None;
                    self.dirty = true;
                }
                true
            }
            ("contacts.group", Action::Activate | Action::Expand) => {
                self.open_popup(Popup::GroupFilter {
                    anchor: self.hit.group,
                });
                self.dirty = true;
                true
            }
            ("contacts.group", Action::Collapse) => {
                if matches!(self.popup, Some(Popup::GroupFilter { .. })) {
                    self.popup = None;
                    self.dirty = true;
                }
                true
            }
            ("contacts.back", Action::Activate) if self.narrow => {
                self.push.show_master();
                self.relayout = true;
                self.dirty = true;
                true
            }
            ("contacts.search", Action::Activate | Action::Focus) => {
                self.set_only_focus_search();
                self.dirty = true;
                true
            }
            ("contacts.search", Action::SetValue(value)) => {
                self.set_only_focus_search();
                self.search.set_text(value);
                self.relayout = true;
                self.dirty = true;
                true
            }
            _ if target.starts_with("contacts.select:")
                && matches!(action, Action::Activate | Action::Focus) =>
            {
                let Some(raw) = target.strip_prefix("contacts.select:") else {
                    return false;
                };
                let Ok(id) = raw.parse::<ContactId>() else {
                    return false;
                };
                if self.store.get(id).is_none() {
                    return false;
                }
                self.reveal_contact(id);
                true
            }
            _ => false,
        }
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

    /// Contacts paints a compact toolbar itself, so expose its labels through
    /// the shared host tooltip path rather than leaving compact glyphs and
    /// dense contact controls unexplained.  The same operations are exposed
    /// by [`Self::accessibility`], while shortcuts here remain discoverable to
    /// pointer users too.
    fn tooltip(&self) -> Option<&str> {
        let point = self.last_mouse;
        if let Some(Popup::ContextMenu { target, anchor }) = self.popup {
            let (_panel, rows) = self.popup_menu_geometry(anchor, CONTEXT_ACTIONS.len(), 200.0);
            if let Some((index, _)) = rows.iter().enumerate().find(|(_, row)| row.contains(point))
                && let Some(action) = CONTEXT_ACTIONS.get(index)
            {
                return self
                    .store
                    .get(target)
                    .map(|contact| action.label(contact.favorite));
            }
        }
        if self.mode == Mode::Edit {
            if self.hit.save.contains(point) {
                return Some("Save changes — Ctrl+S");
            }
            if self.hit.cancel.contains(point) {
                return Some("Cancel editing — Esc");
            }
            if self.hit.fav_toggle.contains(point) {
                return Some("Toggle favorite");
            }
            if self.hit.add_phone.contains(point) {
                return Some("Add phone number");
            }
            if self.hit.add_email.contains(point) {
                return Some("Add email address");
            }
            if self.hit.add_address.contains(point) {
                return Some("Add postal address");
            }
            if self.hit.cal_btn.contains(point) {
                return Some("Choose birthday from calendar");
            }
            if self
                .hit
                .type_chips
                .iter()
                .any(|(_, rect)| rect.contains(point))
            {
                return Some("Change field type");
            }
            if self
                .hit
                .remove_btns
                .iter()
                .any(|(_, rect)| rect.contains(point))
            {
                return Some("Remove this field");
            }
        }
        if self.hit.import.contains(point) {
            Some("Import contacts")
        } else if !self.store.is_empty() && self.hit.export.contains(point) {
            Some("Export contacts")
        } else if self.has_duplicate_candidate() && self.hit.duplicates.contains(point) {
            Some("Review possible duplicate — nothing merges automatically")
        } else if !self.store.is_empty() && self.hit.new.contains(point) {
            Some("Create contact")
        } else if self.hit.search.contains(point) {
            Some("Search contacts — Ctrl+K or /")
        } else if self.hit.sort.contains(point) {
            Some("Choose contact sort order")
        } else if self.hit.group.contains(point) {
            Some("Filter contacts by group")
        } else if self.narrow && self.hit.back.contains(point) {
            Some("Back to contacts")
        } else if self.hit.fav.contains(point) {
            Some("Toggle favorite")
        } else if self.hit.edit.contains(point) {
            Some("Edit contact")
        } else if self.hit.delete.contains(point) {
            Some("Delete contact")
        } else if self.empty_action_rect.contains(point) {
            match self.empty_list_action {
                EmptyListAction::Create => Some("Create contact"),
                EmptyListAction::ClearFilters => Some("Clear search and group filter"),
                EmptyListAction::None => None,
            }
        } else if self.list_rows.iter().any(|(_, row)| row.contains(point)) {
            Some("Open contact — Menu or Shift+F10 for actions")
        } else if self
            .detail_field_hits
            .iter()
            .any(|(_, rect)| rect.contains(point))
        {
            Some("Double-click to edit this field")
        } else {
            None
        }
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

    fn set_motion_reduced(&mut self, reduced: bool) {
        if reduced {
            // `tick` owns the endpoint semantics; a zero-motion policy settles
            // the currently requested pane without waiting for another frame.
            let _ = self.push.tick(true);
            self.relayout = true;
            self.dirty = true;
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.relayout
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

impl ui::undo::UndoPoints for ContactsApp {
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
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        let state = <crate::resource::Contacts as ui::tilo_resource::Resource>::read(doc)
            .unwrap_or_default();
        let mut root = ui::tilo_resource::publish::<crate::resource::Contacts>(
            crate::resource::BOOK_RESOURCE_ID,
            "Contacts",
            &state,
        );
        root.children.extend(crate::resource::live_extras(self));
        vec![root]
    }
    /// Resolve contacts outside the contextual palette page. This is the
    /// other half of bounded discovery: a search hit for contact 65,000 still
    /// reaches the exact current resource/action catalogue, while a deleted
    /// contact has no resource for the broker to invoke.
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
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
    /// [`Self::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.take_committed()
    }
}

impl ContactsApp {
    #[allow(
        clippy::too_many_lines,
        reason = "all admitted Contacts view operations are centralized so resource routes cannot drift"
    )]
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if type_id != crate::actions::CONTACTS_OPERATION_TYPE {
            return Ok(false);
        }
        let operation: crate::actions::ContactsOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        match (kind, operation) {
            (tilo_actions::OperationKind::View, crate::actions::ContactsOperation::Open { id }) => {
                let id =
                    uuid::Uuid::parse_str(&id).map_err(|_| "invalid contact id".to_string())?;
                if self.store.get(id).is_none() {
                    return Err("contact no longer exists".into());
                }
                self.reveal_contact(id);
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::ContactsOperation::OpenGroup { name },
            ) => self.reveal_group(&name)?,
            (
                tilo_actions::OperationKind::View,
                crate::actions::ContactsOperation::OpenFavorites,
            ) => {
                self.set_group_filter(Some(FAVORITES_FILTER.into()));
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::ContactsOperation::OpenDuplicateReview {
                    survivor_id,
                    source_id,
                },
            ) => {
                let survivor = uuid::Uuid::parse_str(&survivor_id)
                    .map_err(|_| "invalid surviving contact id".to_string())?;
                let source = uuid::Uuid::parse_str(&source_id)
                    .map_err(|_| "invalid source contact id".to_string())?;
                self.open_duplicate_review(survivor, source)?;
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::ContactsOperation::ImportVcard { path },
            ) => {
                if path.is_some() {
                    return Err(
                        "direct filesystem paths are not accepted; use the system file picker"
                            .into(),
                    );
                }
                if self.import_rx.is_some() || self.import_parse_rx.is_some() {
                    return Err("a contact import is already in progress".into());
                }
                self.request_import();
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::ContactsOperation::ExportAllVcard { path },
            ) => {
                if path.is_some() {
                    return Err(
                        "direct filesystem paths are not accepted; use the system file picker"
                            .into(),
                    );
                }
                if self.export_rx.is_some() {
                    return Err("a contact export is already in progress".into());
                }
                let ids = self
                    .store
                    .contacts
                    .iter()
                    .map(|contact| contact.id)
                    .collect();
                if !self.request_export(ids, crate::vcard::VCardVersion::V4) {
                    return Err("there are no contacts to export".into());
                }
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::ContactsOperation::ExportContactVcard { id, path },
            ) => {
                if path.is_some() {
                    return Err(
                        "direct filesystem paths are not accepted; use the system file picker"
                            .into(),
                    );
                }
                let id =
                    uuid::Uuid::parse_str(&id).map_err(|_| "invalid contact id".to_string())?;
                self.open_contact_export(id)?;
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::ContactsOperation::KeepImportConflict { token },
            ) => {
                if !self
                    .import_conflicts
                    .iter()
                    .any(|conflict| conflict.token.to_string() == token)
                {
                    return Err("import conflict no longer exists".into());
                }
                self.pending_conflict_resolution = Some((token, false));
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::ContactsOperation::ImportConflictAsNew { token },
            ) => {
                if !self
                    .import_conflicts
                    .iter()
                    .any(|conflict| conflict.token.to_string() == token)
                {
                    return Err("import conflict no longer exists".into());
                }
                self.pending_conflict_resolution = Some((token, true));
            }
            _ => return Err("operation kind does not match Contacts operation".into()),
        }
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if type_id != crate::actions::CONTACTS_OPERATION_TYPE {
            return Ok(None);
        }
        let operation: crate::actions::ContactsOperation =
            serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        match operation {
            crate::actions::ContactsOperation::ImportVcard { path: file } => {
                if self.resource_invocation.is_some()
                    || self.import_rx.is_some()
                    || self.import_parse_rx.is_some()
                {
                    return Err("another contact file operation is running".into());
                }
                if file.is_some() {
                    return Err(
                        "direct filesystem paths are not accepted; use the system file picker"
                            .into(),
                    );
                }
                self.resource_invocation = Some(invocation);
                self.request_import();
                Ok(Some(ui::widget::ResourceOperationStart::Accepted))
            }
            crate::actions::ContactsOperation::ExportAllVcard { path: file } => {
                if self.store.contacts.is_empty() {
                    return Err("there are no contacts to export".into());
                }
                if self.resource_invocation.is_some() {
                    return Err("another contact file operation is running".into());
                }
                if file.is_some() {
                    return Err(
                        "direct filesystem paths are not accepted; use the system file picker"
                            .into(),
                    );
                }
                self.resource_invocation = Some(invocation);
                let ids = self.store.contacts.iter().map(|c| c.id).collect();
                if !self.request_export(ids, crate::vcard::VCardVersion::V4) {
                    self.resource_invocation = None;
                    return Err("contact export could not be started".into());
                }
                Ok(Some(ui::widget::ResourceOperationStart::Accepted))
            }
            crate::actions::ContactsOperation::ExportContactVcard { .. } => {
                // Decode/validate and open the exact same export dialog as a
                // context-menu activation. This operation completes once the
                // flow is shown; a portal job begins only after the person
                // chooses a vCard version and destination in that flow.
                self.answer_resource_operation(kind, type_id, payload, doc, path)
                    .map(|handled| {
                        handled.then(|| {
                            ui::widget::ResourceOperationStart::Completed(
                                tilo_actions::ActionOutput::default(),
                            )
                        })
                    })
            }
            crate::actions::ContactsOperation::Open { .. }
            | crate::actions::ContactsOperation::OpenGroup { .. }
            | crate::actions::ContactsOperation::OpenFavorites
            | crate::actions::ContactsOperation::OpenDuplicateReview { .. }
            | crate::actions::ContactsOperation::KeepImportConflict { .. }
            | crate::actions::ContactsOperation::ImportConflictAsNew { .. } => self
                .answer_resource_operation(kind, type_id, payload, doc, path)
                .map(|handled| {
                    handled.then(|| {
                        ui::widget::ResourceOperationStart::Completed(
                            tilo_actions::ActionOutput::default(),
                        )
                    })
                }),
        }
    }
}

impl ContactsApp {
    /// Blocking layers route every keyboard and pointer event before the app
    /// underneath can see it. Each dialog owns its own keyboard model (rather
    /// than letting global shortcuts leak through the scrim). Returns `true`
    /// when this method has fully consumed the event.
    fn handle_event_modals(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if self.duplicate_review.is_some() {
            self.route_duplicate_review_event(event, ctx.state, ctx.handled);
            return true;
        }
        if !self.import_conflicts.is_empty() {
            self.route_import_conflict_dialog_event(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                ctx.state,
                ctx.path,
                ctx.handled,
            );
            return true;
        }
        if self.export_dialog.is_open() {
            self.handle_export(event, *ctx.modifiers, ctx.handled);
            return true;
        }
        if self.pending_switch.is_some() {
            self.handle_unsaved(event, *ctx.modifiers, ctx.state, ctx.handled);
            return true;
        }
        if self.confirm_delete.is_some() {
            self.route_delete_dialog_event(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                ctx.state,
                ctx.path,
                ctx.handled,
            );
            return true;
        }
        if self.popup.is_some() {
            self.handle_popup(event, ctx.state, ctx.handled);
            // Popups consume clicks; let hover fall through harmlessly.
            if *ctx.handled {
                return true;
            }
        }
        false
    }

    #[allow(
        clippy::too_many_lines,
        reason = "pointer, keyboard, wheel, and modal dismissal share one duplicate-review event boundary"
    )]
    fn route_duplicate_review_event(
        &mut self,
        event: &WindowEvent,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        let Some(review) = self.duplicate_review.as_ref() else {
            return;
        };
        let geometry = self.duplicate_review_geometry(review);
        let field_count = geometry.rows.len();
        let mut toggle = None;
        let mut choose = None;
        let mut merge = false;
        let mut cancel = false;
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => cancel = true,
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::ArrowDown),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if let Some(review) = self.duplicate_review.as_mut() {
                    review.focus = (review.focus + 1) % (field_count + 2).max(1);
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::ArrowUp),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if let Some(review) = self.duplicate_review.as_mut() {
                    review.focus = (review.focus + field_count + 1) % (field_count + 2).max(1);
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Space),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if let Some(review) = self.duplicate_review.as_ref()
                    && review.focus < field_count
                {
                    toggle = geometry.rows.get(review.focus).map(|row| row.field);
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Enter),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if let Some(review) = self.duplicate_review.as_ref() {
                    match review.focus.cmp(&field_count) {
                        std::cmp::Ordering::Less => {
                            toggle = geometry.rows.get(review.focus).map(|row| row.field);
                        }
                        std::cmp::Ordering::Equal => merge = true,
                        std::cmp::Ordering::Greater => cancel = true,
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } if geometry.full_screen => {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(-y) * 48.0,
                    MouseScrollDelta::PixelDelta(position) => -position.y,
                };
                if let Some(review) = self.duplicate_review.as_mut() {
                    review.scroll = (review.scroll + amount).clamp(0.0, geometry.max_scroll);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if geometry.merge.contains(self.last_mouse) {
                    merge = true;
                } else if geometry.cancel.contains(self.last_mouse)
                    || !geometry.panel.contains(self.last_mouse)
                {
                    cancel = true;
                } else if let Some(row) = geometry
                    .rows
                    .iter()
                    .find(|row| row.card.contains(self.last_mouse))
                {
                    if geometry.full_screen {
                        if row.current.contains(self.last_mouse) {
                            choose = Some((row.field, false));
                        } else if row.other.contains(self.last_mouse) {
                            choose = Some((row.field, true));
                        }
                    } else {
                        toggle = Some(row.field);
                    }
                }
            }
            _ => {}
        }
        if let Some(field) = toggle
            && let Some(review) = self.duplicate_review.as_mut()
            && !review.take_from_source.insert(field)
        {
            review.take_from_source.remove(&field);
        }
        if let Some((field, take_source)) = choose
            && let Some(review) = self.duplicate_review.as_mut()
        {
            if take_source {
                review.take_from_source.insert(field);
            } else {
                review.take_from_source.remove(&field);
            }
        }
        if merge {
            self.merge_duplicate_review(state);
        } else if cancel {
            self.duplicate_review = None;
            self.dirty = true;
        }
        *handled = true;
    }

    /// Divider drag, primary/secondary click, mouse wheel scroll, and
    /// keyboard — the main per-`WindowEvent`-variant dispatch. Returns
    /// `true` if `event` was consumed here (the caller then returns without
    /// forwarding to inputs); `false` for a variant this function doesn't
    /// own, so the caller still forwards it as a pointer-move.
    fn handle_event_dispatch(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
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
                    *ctx.handled = true;
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
                    *ctx.handled = true;
                    return true;
                }
                self.handle_click(
                    ctx.state,
                    ctx.handled,
                    ctx.node_id,
                    ctx.taffy,
                    *ctx.modifiers,
                    ctx.path,
                );
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
                self.handle_right_click(self.last_mouse, ctx.handled);
                return true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.divider_drag {
                    self.divider_drag = false;
                    *ctx.handled = true;
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
                *ctx.handled = true;
                return true;
            }
            WindowEvent::KeyboardInput { event: ke, .. } => {
                self.handle_key(event, ctx, ke);
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

    fn forward_event_to_inputs(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let mut sink: Option<NodeId> = None;
        // Search box.
        self.search.handle_event(
            event,
            &mut ui::widget::EventCtx {
                focus_target: &mut sink,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        if let Some(form) = &mut self.form {
            for f in &mut form.inputs {
                let mut s2: Option<NodeId> = None;
                f.ti.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        focus_target: &mut s2,
                        ..ctx.reborrow(ctx.path, ctx.node_id)
                    },
                );
            }
        }
    }

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
        modifiers: winit::event::Modifiers,
        path: &ObjId,
    ) {
        let p = self.last_mouse;

        if self.empty_list_action != EmptyListAction::None && self.empty_action_rect.contains(p) {
            self.activate_empty_list_action();
            *handled = true;
            return;
        }

        if self.handle_click_toolbar_and_header(state, p) {
            *handled = true;
            return;
        }

        // Search field click → focus it.
        if self.hit.search.contains(p) {
            self.set_only_focus_search();
        } else if self.list_body.contains(p) {
            if self.empty_list_action != EmptyListAction::None && self.empty_action_rect.contains(p)
            {
                self.activate_empty_list_action();
                *handled = true;
                return;
            }
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
    fn handle_click_toolbar_and_header(&mut self, state: &AutoCommit, p: Point) -> bool {
        // Narrow single-pane push: the Back affordance returns to the list.
        // Only hittable while the detail pane is the visible (pushed) slab.
        if self.narrow
            && self.push.pane() == PushPane::Detail
            && self.hit.back.width() > 0.0
            && self.hit.back.contains(p)
        {
            self.push.show_master();
            self.dirty = true;
            self.relayout = true;
            return true;
        }

        // Toolbar.
        if self.hit.import.contains(p) {
            self.request_import();
            return true;
        }
        if !self.store.is_empty() && self.hit.export.contains(p) {
            self.open_export_dialog();
            self.dirty = true;
            return true;
        }
        if self.has_duplicate_candidate() && self.hit.duplicates.contains(p) {
            match self.invoke_duplicate_review_operation(state) {
                Ok(true) => {
                    self.dirty = true;
                    self.relayout = true;
                }
                Ok(false) => {}
                Err(error) => self.show_toast(format!("Couldn't open duplicate review: {error}")),
            }
            return true;
        }
        if !self.store.is_empty() && self.hit.new.contains(p) {
            self.begin_new_contact();
            return true;
        }

        // List header chips.
        if self.hit.sort.contains(p) {
            self.open_popup(Popup::Sort {
                anchor: self.hit.sort,
            });
            self.dirty = true;
            return true;
        }
        if self.hit.group.contains(p) {
            self.open_popup(Popup::GroupFilter {
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
                    self.toggle_favorite(state, id);
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
            self.open_popup(Popup::Calendar {
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
        if let Some((target, anchor)) = self
            .hit
            .type_chips
            .iter()
            .find(|(_, r)| r.contains(p))
            .copied()
        {
            self.open_popup(Popup::TypeMenu { target, anchor });
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
        self.empty_action_focused = false;
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
        let now = ui::time::now();
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
            Some(ci) => form
                .inputs
                .get(ci)
                .and_then(|f| best_field_in_direction(f.ti.get_rect(), dir, &rects, ci)),
            None => match dir {
                FieldDir::Down | FieldDir::Right => rects.first().map(|r| r.0),
                FieldDir::Up | FieldDir::Left => rects.last().map(|r| r.0),
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
        let body_top = self.form_body_top();
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

    fn forward_press_to_focused_input(
        &mut self,
        state: &mut AutoCommit,
        path: &ObjId,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
        modifiers: winit::event::Modifiers,
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
                &press,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &modifiers,
                    state,
                    path,
                    handled: &mut h,
                    focus_target: &mut sink,
                    node_id,
                    taffy,
                },
            );
        }
        if let Some(form) = &mut self.form {
            for f in &mut form.inputs {
                if f.ti.is_focused() {
                    let mut s2 = None;
                    f.ti.handle_event(
                        &press,
                        &mut ui::widget::EventCtx {
                            scale_factor: 1.0,
                            modifiers: &modifiers,
                            state,
                            path,
                            handled: &mut h,
                            focus_target: &mut s2,
                            node_id,
                            taffy,
                        },
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
            let base = form.base.clone();
            let mut draft = form.draft;
            // First fold current input values back in.
            let mut tmp = EditForm {
                base: form.base,
                draft: draft.clone(),
                is_new: form.is_new,
                inputs: form.inputs,
            };
            tmp.collect();
            draft = tmp.draft;
            f(&mut draft);
            self.form = Some(EditForm::with_base(base, draft, form.is_new));
            self.relayout = true;
            self.dirty = true;
        }
    }

    fn form_remove(&mut self, target: TypeTarget) {
        if let Some(form) = self.form.take() {
            let base = form.base.clone();
            let mut tmp = EditForm {
                base: form.base,
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
            self.form = Some(EditForm::with_base(base, draft, form.is_new));
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
        let Some(popup) = self.popup.take() else {
            return;
        };
        *handled = true;
        self.dirty = true;
        match popup {
            Popup::TypeMenu { target, anchor } => self.handle_popup_type_menu(target, anchor, p),
            Popup::Sort { anchor } => self.handle_popup_sort(anchor, p),
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
        if let Some(i) = rows.iter().position(|r| r.contains(p))
            && let Some(label) = TYPE_OPTIONS.get(i).map(|s| (*s).to_string())
            && let Some(form) = &mut self.form
        {
            {
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

    /// Sort-popup selection. The popup is presentation-only, while the sort
    /// preference remains local to this window.
    fn handle_popup_sort(&mut self, anchor: Rect, p: Point) {
        let options = [SortKey::Name, SortKey::Org];
        let (_panel, rows) = self.popup_menu_geometry(anchor, options.len(), 180.0);
        if let Some(index) = rows.iter().position(|row| row.contains(p))
            && let Some(sort) = options.get(index)
        {
            self.sort = *sort;
            self.list_scroll = 0.0;
        }
        self.relayout = true;
    }

    /// Group-filter selection: narrows the list to the picked group (or
    /// clears it for "All"), and keeps the selection valid.
    fn handle_popup_group_filter(&mut self, anchor: Rect, p: Point) {
        let opts = self.group_filter_options();
        let (_panel, rows) = self.popup_menu_geometry(anchor, opts.len(), 180.0);
        if let Some(i) = rows.iter().position(|r| r.contains(p)) {
            self.set_group_filter(match opts.get(i).map(String::as_str) {
                Some("All") => None,
                Some("Favorites") => Some(FAVORITES_FILTER.into()),
                Some(group) => Some(group.to_string()),
                None => return,
            });
        }
        self.relayout = true;
    }

    /// Apply a group-filter choice from either the pointer menu or its
    /// semantic radio options, and keep the presentation selection valid.
    fn set_group_filter(&mut self, group: Option<String>) {
        self.group_filter = group;
        self.list_scroll = 0.0;
        if let Some(selected) = self.selected
            && !self.visible_ids().contains(&selected)
        {
            self.selected = self.visible_ids().first().copied();
        }
        self.relayout = true;
        self.dirty = true;
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
        let Some(action) = CONTEXT_ACTIONS.get(i).copied() else {
            return;
        };
        self.execute_context_action(state, target, action);
    }

    /// Execute the single source of truth behind context-menu activation.
    /// Pointer, keyboard and assistive activation must not slowly drift into
    /// three subtly different implementations (especially for destructive
    /// Delete and the per-contact Export selection).
    fn execute_context_action(
        &mut self,
        state: &mut AutoCommit,
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
                // Toggle the favorite flag through the shared action
                // path (same action the view-mode star and Ctrl+K
                // dispatch), so the list star and the menu label flip
                // immediately and the change survives a reload.
                self.toggle_favorite(state, target);
                self.relayout = true;
            }
            ContextAction::Export => {
                if let Err(error) = self.invoke_contact_export_operation(state, target) {
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

    /// Route the open delete confirmation through `ModalDialog`. This retains
    /// the existing explicit-button gate (ordinary keys do not confirm), while
    /// shared handling supplies Escape, backdrop dismissal, and complete modal
    /// event capture.
    fn route_delete_dialog_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) {
        let Some(id) = self.confirm_delete else {
            return;
        };
        self.ensure_delete_dialog(id);
        if self.rect.width() > 0.0 && self.rect.height() > 0.0 {
            let theme = Theme::dark();
            let mut text_renderer = TextRenderer::new();
            if let Some(confirm) = self.delete_dialog.as_mut() {
                confirm
                    .dialog
                    .layout_in(self.rect, &theme, &mut text_renderer);
            }
        }
        let (confirm, cancel) = {
            let Some(dialog) = self.delete_dialog.as_mut() else {
                *handled = true;
                return;
            };
            *handled = dialog
                .dialog
                .handle_detached(event, scale_factor, modifiers, state, path);
            (dialog.confirm.replace(false), dialog.cancel.replace(false))
        };
        if confirm {
            self.delete_selected(state, id);
        } else if cancel {
            self.confirm_delete = None;
            self.delete_dialog = None;
        }
        self.dirty = true;
        *handled = true;
    }

    /// Route the imported-UID review modal. The modal itself is the explicit
    /// confirmation for the destructive "Import as new" choice; backdrop and
    /// Escape deliberately leave the pending conflict untouched.
    fn route_import_conflict_dialog_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) {
        self.ensure_import_conflict_dialog();
        if self.rect.width() > 0.0 && self.rect.height() > 0.0 {
            let theme = Theme::dark();
            let mut text_renderer = TextRenderer::new();
            if let Some(review) = self.import_conflict_dialog.as_mut() {
                review
                    .dialog
                    .layout_in(self.rect, &theme, &mut text_renderer);
            }
        }
        let (token, import_as_new, keep_existing) = {
            let Some(review) = self.import_conflict_dialog.as_mut() else {
                *handled = true;
                return;
            };
            *handled = review
                .dialog
                .handle_detached(event, scale_factor, modifiers, state, path);
            (
                review.token.to_string(),
                review.import_as_new.replace(false),
                review.keep_existing.replace(false),
            )
        };
        if import_as_new || keep_existing {
            self.pending_conflict_resolution = Some((token, import_as_new));
            // Do not permit a second physical click before the next sync
            // consumes the one explicit choice.
            self.import_conflict_dialog = None;
            self.dirty = true;
        }
        *handled = true;
    }

    /// Open the per-contact right-click context menu if `p` lands on a row.
    fn handle_right_click(&mut self, p: Point, handled: &mut bool) {
        // Only the list rows carry a context menu.
        if let Some((id, _)) = self.list_rows.iter().find(|(_, r)| r.contains(p)).copied() {
            self.selected = Some(id);
            let anchor = Rect::new(p.x, p.y, p.x, p.y);
            self.open_popup(Popup::ContextMenu { target: id, anchor });
            self.relayout = true;
            self.dirty = true;
            *handled = true;
        }
    }

    /// Drive the export-selection dialog: checkbox toggles, Select all/none,
    /// scroll, the two export actions, and dismissal.
    fn handle_export(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            self.dirty = true;
        }
        let (consumed, request) = if let WindowEvent::KeyboardInput { event: key, .. } = event {
            self.export_dialog.handle_key(
                key,
                modifiers.state().shift_key(),
                self.rect,
                &self.store,
                self.sort,
            )
        } else {
            self.export_dialog.handle_event(
                event,
                self.last_mouse,
                self.rect,
                &self.store,
                self.sort,
            )
        };
        *handled |= consumed;
        if consumed {
            self.dirty = true;
        }
        if let Some(request) = request {
            self.request_export(request.ids, request.version);
        }
    }

    /// Drive the "Save changes?" unsaved-changes guard modal: Save commits and
    /// switches, Discard drops edits and switches, Cancel keeps editing.
    fn handle_unsaved(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            self.dirty = true;
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event {
            if key.state != ElementState::Pressed {
                *handled = true;
                return;
            }
            match key.logical_key {
                Key::Named(NamedKey::Escape) => self.pending_switch = None,
                Key::Named(NamedKey::Tab) => {
                    self.unsaved_focus = match (self.unsaved_focus, modifiers.state().shift_key()) {
                        (UnsavedFocus::Cancel, false) | (UnsavedFocus::Discard, true) => {
                            UnsavedFocus::Discard
                        }
                        (UnsavedFocus::Discard, false) | (UnsavedFocus::Save, true) => {
                            UnsavedFocus::Save
                        }
                        (UnsavedFocus::Save, false) | (UnsavedFocus::Cancel, true) => {
                            UnsavedFocus::Cancel
                        }
                    };
                }
                Key::Named(NamedKey::Enter | NamedKey::Space) => self.resolve_unsaved_focus(state),
                _ => {}
            }
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
        let g = self.unsaved_geometry();
        if self.pending_switch.is_none() {
            return;
        }
        if g.save.contains(p) {
            self.unsaved_focus = UnsavedFocus::Save;
            self.resolve_unsaved_focus(state);
        } else if g.discard.contains(p) {
            self.unsaved_focus = UnsavedFocus::Discard;
            self.resolve_unsaved_focus(state);
        } else if g.cancel.contains(p) || !g.panel.contains(p) {
            // Cancel / scrim: stay in the edit form, drop the pending switch.
            self.unsaved_focus = UnsavedFocus::Cancel;
            self.pending_switch = None;
        }
    }

    fn resolve_unsaved_focus(&mut self, state: &mut AutoCommit) {
        let Some(target) = self.pending_switch else {
            return;
        };
        match self.unsaved_focus {
            UnsavedFocus::Save => {
                self.save_edit(state);
                self.pending_switch = None;
                self.commit_select(target);
            }
            UnsavedFocus::Discard => {
                self.pending_switch = None;
                self.commit_select(target);
            }
            UnsavedFocus::Cancel => self.pending_switch = None,
        }
    }

    /// Route a keypress. Split into per-concern helpers purely for length;
    /// the guard order (which chord wins) is unchanged from one flat
    /// function.
    fn handle_key(
        &mut self,
        full_event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        ke: &KeyEvent,
    ) {
        if ke.state != ElementState::Pressed {
            // Route key-release to inputs too (harmless) then return.
            self.forward_event_to_inputs(full_event, ctx);
            return;
        }
        let ctrl = ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key();

        // Ctrl/Cmd+K and Ctrl/Cmd+/ focus the list's search field — the
        // familiar "jump to search" chord, wired to this app's real search box
        // (bare `/` does the same in the no-field-focused branch below).
        if is_focus_search_chord(ctrl, &ke.logical_key) {
            self.set_only_focus_search();
            *ctx.handled = true;
            self.dirty = true;
            return;
        }

        // Escape closes popups / dialogs / cancels edit / returns from a
        // narrow pushed detail to the list / clears search focus.
        if matches!(ke.logical_key, Key::Named(NamedKey::Escape)) {
            self.handle_key_escape();
            *ctx.handled = true;
            self.dirty = true;
            return;
        }

        // Ctrl+S saves in edit mode.
        if ctrl
            && let Key::Character(c) = &ke.logical_key
            && c.eq_ignore_ascii_case("s")
            && self.mode == Mode::Edit
        {
            self.save_edit(ctx.state);
            *ctx.handled = true;
            return;
        }

        // Menus and the birthday picker are fully keyboard-operable.  They
        // are local presentation/draft controls, but keyboard selection must
        // reach the exact same handlers as pointer selection.  This comes
        // before form navigation so an open calendar owns its arrow keys.
        if self.popup.is_some() && self.handle_key_popup(ke, ctx.state, *ctx.modifiers) {
            *ctx.handled = true;
            self.dirty = true;
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
            && self.handle_key_edit_form_nav(ke, ctx.state, *ctx.modifiers)
        {
            *ctx.handled = true;
            self.dirty = true;
            return;
        }

        // If a text field is focused, route typing to it. In edit mode the
        // keystroke lands in the field's *local* (non-CRDT) buffer only — the
        // draft is folded into the store + CRDT solely by Save, so Cancel can
        // genuinely discard and the unsaved-changes guard can see real dirt.
        if self.any_text_focused() {
            self.forward_event_to_inputs(full_event, ctx);
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
        self.handle_key_list_shortcuts(ke, ctx.modifiers.state(), ctx.handled);
    }

    /// The Escape-key state transition: closes whichever dialog/popup/edit is
    /// topmost, in priority order, or (with nothing open) clears search/form
    /// focus. The caller sets `handled`/`dirty` uniformly afterward.
    fn handle_key_escape(&mut self) {
        if self.export_dialog.is_open() {
            self.export_dialog.close();
        } else if self.pending_switch.is_some() {
            self.pending_switch = None;
        } else if self.confirm_delete.is_some() {
            self.confirm_delete = None;
            self.delete_dialog = None;
        } else if self.popup.is_some() {
            self.popup = None;
        } else if self.mode == Mode::Edit {
            self.cancel_edit();
            // On a narrow window, leaving the edit form returns to the
            // list (the form lived in the pushed detail pane).
            if self.narrow {
                self.push.show_master();
                self.relayout = true;
            }
        } else if self.narrow && self.push.pane() == PushPane::Detail {
            // Back out of the pushed detail pane to the list.
            self.push.show_master();
            self.relayout = true;
        } else {
            self.search.set_focus(false);
            self.empty_action_focused = false;
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
        modifiers: winit::event::Modifiers,
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

    fn move_popup_focus(&mut self, count: usize, delta: isize) {
        if count > 0 {
            self.popup_focus =
                (self.popup_focus as isize + delta).rem_euclid(count as isize) as usize;
        }
    }

    /// Arrow/Tab navigation and activation for the custom painted popup
    /// surfaces.  Pointer interaction still routes through `handle_popup`;
    /// keeping this adapter alongside it ensures the two paths mutate the
    /// same local draft/action state.
    fn handle_key_popup(
        &mut self,
        ke: &KeyEvent,
        state: &mut AutoCommit,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let Some(popup) = self.popup.as_ref() else {
            return false;
        };
        let count = match popup {
            Popup::TypeMenu { .. } => TYPE_OPTIONS.len(),
            Popup::Sort { .. } => 2,
            Popup::GroupFilter { .. } => self.group_filter_options().len(),
            Popup::ContextMenu { .. } => CONTEXT_ACTIONS.len(),
            Popup::Calendar { year, month, .. } => date::days_in_month(*year, *month) as usize + 2,
        };
        if count == 0 {
            return false;
        }
        self.popup_focus = self.popup_focus.min(count - 1);
        match &ke.logical_key {
            Key::Named(NamedKey::Tab | NamedKey::ArrowDown) => {
                self.move_popup_focus(count, if modifiers.state().shift_key() { -1 } else { 1 });
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_popup_focus(count, -1);
                true
            }
            Key::Named(NamedKey::Home) => {
                self.popup_focus = 0;
                true
            }
            Key::Named(NamedKey::End) => {
                self.popup_focus = count - 1;
                true
            }
            Key::Named(NamedKey::ArrowLeft) if matches!(popup, Popup::Calendar { .. }) => {
                self.move_popup_focus(count, -1);
                true
            }
            Key::Named(NamedKey::ArrowRight) if matches!(popup, Popup::Calendar { .. }) => {
                self.move_popup_focus(count, 1);
                true
            }
            Key::Named(NamedKey::PageUp) if matches!(popup, Popup::Calendar { .. }) => {
                self.activate_popup_calendar_month(false);
                true
            }
            Key::Named(NamedKey::PageDown) if matches!(popup, Popup::Calendar { .. }) => {
                self.activate_popup_calendar_month(true);
                true
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                self.activate_popup_focus(state);
                true
            }
            _ => false,
        }
    }

    fn activate_popup_calendar_month(&mut self, next: bool) {
        let Some(Popup::Calendar {
            year,
            month,
            anchor,
        }) = self.popup
        else {
            return;
        };
        let (year, month) = if next {
            date::next_month(year, month)
        } else {
            date::prev_month(year, month)
        };
        self.popup = Some(Popup::Calendar {
            year,
            month,
            anchor,
        });
        self.popup_focus = 2;
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one focused popup action table keeps keyboard activation aligned with every local popup variant"
    )]
    fn activate_popup_focus(&mut self, state: &mut AutoCommit) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        match popup {
            Popup::TypeMenu { target, .. } => {
                if let Some(label) = TYPE_OPTIONS.get(self.popup_focus)
                    && let Some(form) = &mut self.form
                {
                    match target {
                        TypeTarget::Phone(index) => form
                            .draft
                            .phones
                            .get_mut(index)
                            .map(|row| row.label = (*label).into()),
                        TypeTarget::Email(index) => form
                            .draft
                            .emails
                            .get_mut(index)
                            .map(|row| row.label = (*label).into()),
                        TypeTarget::Addr(index) => form
                            .draft
                            .addresses
                            .get_mut(index)
                            .map(|row| row.label = (*label).into()),
                    };
                }
            }
            Popup::Sort { .. } => {
                self.sort = if self.popup_focus == 0 {
                    SortKey::Name
                } else {
                    SortKey::Org
                };
                self.list_scroll = 0.0;
                self.relayout = true;
            }
            Popup::GroupFilter { .. } => {
                let options = self.group_filter_options();
                self.set_group_filter(match options.get(self.popup_focus).map(String::as_str) {
                    Some("All") | None => None,
                    Some("Favorites") => Some(FAVORITES_FILTER.into()),
                    Some(group) => Some(group.to_string()),
                });
            }
            Popup::ContextMenu { target, .. } => {
                let Some(action) = CONTEXT_ACTIONS.get(self.popup_focus).copied() else {
                    return;
                };
                self.execute_context_action(state, target, action);
            }
            Popup::Calendar {
                year,
                month,
                anchor,
            } => match self.popup_focus {
                0 => {
                    let (year, month) = date::prev_month(year, month);
                    self.popup = Some(Popup::Calendar {
                        year,
                        month,
                        anchor,
                    });
                    self.popup_focus = 2;
                }
                1 => {
                    let (year, month) = date::next_month(year, month);
                    self.popup = Some(Popup::Calendar {
                        year,
                        month,
                        anchor,
                    });
                    self.popup_focus = 2;
                }
                focused => {
                    let day = (focused - 1) as u32;
                    if day <= date::days_in_month(year, month) {
                        let iso = format!("{year:04}-{month:02}-{day:02}");
                        if let Some(form) = &mut self.form {
                            form.draft.birthday = Birthday::parse(&iso);
                            if let Some(input) = form
                                .inputs
                                .iter_mut()
                                .find(|input| input.slot == Slot::Birthday)
                            {
                                input.ti.set_text(&iso);
                            }
                        }
                    }
                }
            },
        }
    }

    /// Global list shortcuts: arrows select, Enter opens, Menu / Shift+F10
    /// opens the selected contact's actions, Delete/Backspace opens the
    /// confirmation dialog, `n` starts a new contact, `e` edits the
    /// selection, and `/` focuses search. Only reached in view mode with no
    /// popup open (see the call site's guard).
    fn handle_key_list_shortcuts(
        &mut self,
        ke: &KeyEvent,
        modifiers: winit::keyboard::ModifiersState,
        handled: &mut bool,
    ) {
        if is_context_menu_chord(modifiers.shift_key(), &ke.logical_key) {
            *handled = self.open_selected_context_menu();
            return;
        }
        match &ke.logical_key {
            Key::Named(NamedKey::Tab) if self.empty_list_action != EmptyListAction::None => {
                self.search.set_focus(false);
                self.empty_action_focused = true;
                *handled = true;
                self.dirty = true;
            }
            Key::Named(NamedKey::Enter | NamedKey::Space)
                if self.empty_list_action != EmptyListAction::None && self.empty_action_focused =>
            {
                self.activate_empty_list_action();
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.move_selection(1);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_selection(-1);
                *handled = true;
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(id) = self.selected {
                    self.request_select(id);
                    *handled = true;
                }
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
                        self.begin_new_contact();
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

/// Size and kind of a file a chooser named.
fn file_metadata(path: &std::path::Path) -> Result<tilo_files::Meta, String> {
    tilo_files::file_at(path)
        .and_then(|(dir, name)| tilo_files::blocking::metadata(&dir, &name))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
#[path = "app/tests.rs"]
mod tests;
