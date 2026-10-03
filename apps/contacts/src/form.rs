//! The edit form: a draft contact and the form that edits it.
//!
//! Every field's text lives in a [`LocalState`] that its [`TextInput`] is
//! bound to, so the app reads and rebases values without reaching into the
//! widget tree, and the form is ordinary retained widgets: the window's
//! focus order, Tab, the caret and the semantics are the toolkit's. Nothing
//! reaches the shared document until Save.

use ui::icons::Icon;
use ui::widget::{
    Button, ButtonRole, Column, ControlSize, DatePicker, Dropdown, Element, Field, Form,
    FormSection, Row, Scrollable, Text, TextInput, TextInputKind,
};
use ui::{LocalState, TextRole};

use crate::model::{Address, Birthday, Contact, GroupEntry, Labeled, group_names_equal};
use crate::view::{Readable, Request, Shared, queue};

/// The kinds of a phone number, an email or an address, in menu order:
/// the stored vCard type and the label the menu shows.
pub(crate) const TYPE_OPTIONS: &[(&str, &str)] = &[
    ("mobile", "Mobile"),
    ("home", "Home"),
    ("work", "Work"),
    ("main", "Main"),
    ("other", "Other"),
];

/// Kinds other apps write that the menu does not offer, and how they read.
pub(crate) const OTHER_KINDS: &[(&str, &str)] = &[
    ("cell", "Mobile"),
    ("voice", "Phone"),
    ("fax", "Fax"),
    ("pager", "Pager"),
    ("text", "Text"),
    ("video", "Video"),
    ("textphone", "Textphone"),
    ("internet", "Email"),
    ("personal", "Personal"),
    ("pref", "Preferred"),
    ("x400", "X.400"),
];

/// A repeated row of the form.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RowKind {
    Phone,
    Email,
    Address,
}

/// One repeated row: its kind and its index in the draft.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TypeTarget {
    Phone(usize),
    Email(usize),
    Addr(usize),
}

impl TypeTarget {
    fn noun(self) -> (&'static str, usize) {
        match self {
            TypeTarget::Phone(index) => ("phone", index),
            TypeTarget::Email(index) => ("email", index),
            TypeTarget::Addr(index) => ("address", index),
        }
    }

    /// "Phone 2", as screen readers and the remove button name the row.
    pub(crate) fn spoken(self) -> String {
        let (noun, index) = match self {
            TypeTarget::Phone(index) => ("Phone", index),
            TypeTarget::Email(index) => ("Email", index),
            TypeTarget::Addr(index) => ("Address", index),
        };
        format!("{noun} {}", index + 1)
    }

    fn key(self) -> String {
        let (noun, index) = self.noun();
        format!("{noun}-{index}")
    }
}

/// Identifies which draft field a text input edits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Slot {
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

/// The human-facing name of a field: its visible label, or for a field
/// inside a repeated row, what assistive technology calls it.
pub(crate) fn field_name(slot: Slot) -> String {
    match slot {
        Slot::First => "First name".into(),
        Slot::Last => "Last name".into(),
        Slot::Org => "Organization".into(),
        Slot::Title => "Job title".into(),
        Slot::Website => "Website".into(),
        Slot::Birthday => "Birthday".into(),
        Slot::Notes => "Notes".into(),
        Slot::Groups => "Groups".into(),
        Slot::Phone(index) => format!("Phone {}", index + 1),
        Slot::Email(index) => format!("Email {}", index + 1),
        Slot::AddrStreet(index) => format!("Address {} street", index + 1),
        Slot::AddrCity(index) => format!("Address {} city", index + 1),
        Slot::AddrRegion(index) => format!("Address {} region", index + 1),
        Slot::AddrPostal(index) => format!("Address {} postal code", index + 1),
        Slot::AddrCountry(index) => format!("Address {} country", index + 1),
    }
}

/// The hint an empty field shows, where the label alone does not say what
/// goes in it.
fn placeholder(slot: Slot) -> Option<&'static str> {
    match slot {
        Slot::Website => Some("https://example.com"),
        Slot::Birthday => Some("YYYY-MM-DD"),
        Slot::Groups => Some("Family, Work"),
        Slot::Phone(_) => Some("Phone number"),
        Slot::Email(_) => Some("name@example.com"),
        Slot::AddrStreet(_) => Some("Street"),
        Slot::AddrCity(_) => Some("City"),
        Slot::AddrRegion(_) => Some("State or region"),
        Slot::AddrPostal(_) => Some("Postal code"),
        Slot::AddrCountry(_) => Some("Country"),
        Slot::First | Slot::Last | Slot::Org | Slot::Title | Slot::Notes => None,
    }
}

/// The menu a type dropdown offers: the stock kinds, plus the row's own
/// kind when an imported card used one the menu does not know.
fn type_options(current: &str) -> Vec<(String, String)> {
    let mut options: Vec<(String, String)> = TYPE_OPTIONS
        .iter()
        .map(|(value, label)| ((*value).to_owned(), (*label).to_owned()))
        .collect();
    let current = current.trim();
    if !current.is_empty() && !options.iter().any(|(value, _)| value == current) {
        options.push((current.to_owned(), crate::detail::kind_label(current)));
    }
    options
}

/// The live editing state: the record when the editor opened, the draft
/// with its repeated rows, and the text of every field.
pub(crate) struct EditForm {
    /// Contact value when the editor opened. Save derives a granular patch
    /// against this stable base, rather than overwriting peer changes that
    /// arrived while the form was open.
    pub base: Contact,
    pub draft: Contact,
    pub is_new: bool,
    pub fields: Vec<(Slot, LocalState<String>)>,
    pub types: Vec<(TypeTarget, LocalState<String>)>,
    /// The birthday calendar is shown under the field.
    pub calendar_open: bool,
    /// Where the form is scrolled to; it survives the form being rebuilt
    /// when a row is added or removed.
    pub scroll: std::rc::Rc<std::cell::Cell<(f32, f32)>>,
}

fn slots_of(draft: &Contact) -> Vec<(Slot, String)> {
    let mut slots = vec![
        (Slot::First, draft.first.clone()),
        (Slot::Last, draft.last.clone()),
        (Slot::Org, draft.org.clone()),
        (Slot::Title, draft.title.clone()),
    ];
    for (i, p) in draft.phones.iter().enumerate() {
        slots.push((Slot::Phone(i), p.value.clone()));
    }
    for (i, e) in draft.emails.iter().enumerate() {
        slots.push((Slot::Email(i), e.value.clone()));
    }
    for (i, a) in draft.addresses.iter().enumerate() {
        slots.push((Slot::AddrStreet(i), a.street.clone()));
        slots.push((Slot::AddrCity(i), a.city.clone()));
        slots.push((Slot::AddrRegion(i), a.region.clone()));
        slots.push((Slot::AddrPostal(i), a.postal.clone()));
        slots.push((Slot::AddrCountry(i), a.country.clone()));
    }
    slots.push((
        Slot::Birthday,
        draft.birthday.map(|b| b.iso()).unwrap_or_default(),
    ));
    slots.push((Slot::Website, draft.website.clone()));
    slots.push((Slot::Notes, draft.notes.clone()));
    let groups = draft
        .groups
        .iter()
        .map(|group| group.value.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    slots.push((Slot::Groups, groups));
    slots
}

fn types_of(draft: &Contact) -> Vec<(TypeTarget, String)> {
    let mut types = Vec::new();
    for (i, p) in draft.phones.iter().enumerate() {
        types.push((TypeTarget::Phone(i), p.label.clone()));
    }
    for (i, e) in draft.emails.iter().enumerate() {
        types.push((TypeTarget::Email(i), e.label.clone()));
    }
    for (i, a) in draft.addresses.iter().enumerate() {
        types.push((TypeTarget::Addr(i), a.label.clone()));
    }
    types
}

fn read(state: &LocalState<String>) -> String {
    state.with(Clone::clone)
}

/// Write `value` only when it differs, so a bound field keeps its caret.
fn write(state: &LocalState<String>, value: &str) {
    if state.with(|current| current != value) {
        state.set(value.to_owned());
    }
}

impl EditForm {
    pub(crate) fn new(draft: Contact, is_new: bool) -> Self {
        let fields = slots_of(&draft)
            .into_iter()
            .map(|(slot, value)| (slot, LocalState::new(value)))
            .collect();
        let types = types_of(&draft)
            .into_iter()
            .map(|(target, value)| (target, LocalState::new(value)))
            .collect();
        Self {
            base: draft.clone(),
            draft,
            is_new,
            fields,
            types,
            calendar_open: false,
            scroll: std::rc::Rc::default(),
        }
    }

    pub(crate) fn with_base(base: Contact, draft: Contact, is_new: bool) -> Self {
        let mut form = Self::new(draft, is_new);
        form.base = base;
        form
    }

    pub(crate) fn state(&self, slot: Slot) -> Option<&LocalState<String>> {
        self.fields
            .iter()
            .find(|(candidate, _)| *candidate == slot)
            .map(|(_, state)| state)
    }

    /// The text a field holds now.
    pub(crate) fn value(&self, slot: Slot) -> String {
        self.state(slot).map(read).unwrap_or_default()
    }

    /// Replace a field's text, as typing would.
    pub(crate) fn set_value(&self, slot: Slot, value: &str) {
        if let Some(state) = self.state(slot) {
            write(state, value);
        }
    }

    /// The kind a repeated row holds now.
    #[cfg(test)]
    pub(crate) fn kind(&self, target: TypeTarget) -> String {
        self.types
            .iter()
            .find(|(candidate, _)| *candidate == target)
            .map(|(_, state)| read(state))
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn set_kind(&self, target: TypeTarget, value: &str) {
        if let Some((_, state)) = self
            .types
            .iter()
            .find(|(candidate, _)| *candidate == target)
        {
            write(state, value);
        }
    }

    /// Fold every field's text and every row's kind into the draft.
    pub(crate) fn collect(&mut self) {
        let values: Vec<(Slot, String)> = self
            .fields
            .iter()
            .map(|(slot, state)| (*slot, read(state)))
            .collect();
        for (slot, v) in values {
            match slot {
                Slot::First => self.draft.first = v,
                Slot::Last => self.draft.last = v,
                Slot::Org => self.draft.org = v,
                Slot::Title => self.draft.title = v,
                Slot::Website => self.draft.website = v,
                // Unparseable or empty text clears the date rather than
                // storing junk.
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
        let kinds: Vec<(TypeTarget, String)> = self
            .types
            .iter()
            .map(|(target, state)| (*target, read(state)))
            .collect();
        for (target, label) in kinds {
            match target {
                TypeTarget::Phone(i) => {
                    if let Some(row) = self.draft.phones.get_mut(i) {
                        row.label = label;
                    }
                }
                TypeTarget::Email(i) => {
                    if let Some(row) = self.draft.emails.get_mut(i) {
                        row.label = label;
                    }
                }
                TypeTarget::Addr(i) => {
                    if let Some(row) = self.draft.addresses.get_mut(i) {
                        row.label = label;
                    }
                }
            }
        }
    }

    /// The fields and rows in order, by stable identity: two forms with the
    /// same layout show the same widgets.
    pub(crate) fn layout_key(&self) -> Vec<String> {
        self.fields
            .iter()
            .map(|(slot, _)| form_input_key(&self.draft, *slot))
            .collect()
    }

    /// Rebase only local differences onto the current canonical contact.
    /// Unchanged fields adopt restored content; genuinely edited values
    /// (including temporarily invalid birthday text) remain unsaved.
    /// Returns whether the form's rows changed, so the caller rebuilds it.
    pub(crate) fn reconcile(&mut self, canonical: &Contact) -> bool {
        if self.is_new || self.base == *canonical {
            return false;
        }
        let baseline: Vec<(String, String)> = slots_of(&self.base)
            .into_iter()
            .map(|(slot, value)| (form_input_key(&self.base, slot), value))
            .collect();
        let before = self.layout_key();
        self.collect();
        let old_draft = self.draft.clone();
        let draft = rebase_contact(&self.base, &self.draft, canonical);
        let mut previous: Vec<(String, String, LocalState<String>)> =
            std::mem::take(&mut self.fields)
                .into_iter()
                .map(|(slot, state)| (form_input_key(&old_draft, slot), read(&state), state))
                .collect();
        let mut fields = Vec::new();
        for (slot, value) in slots_of(&draft) {
            let key = form_input_key(&draft, slot);
            let state = match previous.iter().position(|(old, ..)| *old == key) {
                Some(index) => {
                    let (_, old_value, state) = previous.remove(index);
                    let untouched = baseline
                        .iter()
                        .find(|(base_key, _)| *base_key == key)
                        .is_some_and(|(_, base_value)| *base_value == old_value);
                    if untouched {
                        write(&state, &value);
                    }
                    state
                }
                None => LocalState::new(value),
            };
            fields.push((slot, state));
        }
        let mut old_types: Vec<(String, LocalState<String>)> = std::mem::take(&mut self.types)
            .into_iter()
            .map(|(target, state)| (type_key(&old_draft, target), state))
            .collect();
        let mut types = Vec::new();
        for (target, value) in types_of(&draft) {
            let key = type_key(&draft, target);
            let state = match old_types.iter().position(|(old, _)| *old == key) {
                Some(index) => {
                    let (_, state) = old_types.remove(index);
                    write(&state, &value);
                    state
                }
                None => LocalState::new(value),
            };
            types.push((target, state));
        }
        self.fields = fields;
        self.types = types;
        self.draft = draft;
        self.base = canonical.clone();
        self.layout_key() != before
    }

    /// Whether any field differs from the record the editor opened on.
    pub(crate) fn has_local_edits(&mut self) -> bool {
        let baseline: Vec<(String, String)> = slots_of(&self.base)
            .into_iter()
            .map(|(slot, value)| (form_input_key(&self.base, slot), value))
            .collect();
        let raw_edit = self.fields.iter().any(|(slot, state)| {
            let key = form_input_key(&self.draft, *slot);
            baseline
                .iter()
                .find(|(base_key, _)| *base_key == key)
                .is_none_or(|(_, value)| *value != read(state))
        });
        self.collect();
        raw_edit || self.draft != self.base
    }

    /// Append an empty row of `kind`.
    pub(crate) fn add(&mut self, kind: RowKind) {
        self.collect();
        match kind {
            RowKind::Phone => self.draft.phones.push(Labeled::new("mobile", "")),
            RowKind::Email => self.draft.emails.push(Labeled::new("home", "")),
            RowKind::Address => self.draft.addresses.push(Address {
                label: "home".into(),
                ..Default::default()
            }),
        }
        self.restructure();
    }

    /// Remove one repeated row; the rows after it keep their text.
    pub(crate) fn remove(&mut self, target: TypeTarget) {
        self.collect();
        match target {
            TypeTarget::Phone(i) if i < self.draft.phones.len() => {
                self.draft.phones.remove(i);
            }
            TypeTarget::Email(i) if i < self.draft.emails.len() => {
                self.draft.emails.remove(i);
            }
            TypeTarget::Addr(i) if i < self.draft.addresses.len() => {
                self.draft.addresses.remove(i);
            }
            _ => return,
        }
        self.restructure();
    }

    /// Rebuild the field states after the draft's rows changed, keeping
    /// every surviving field's text.
    fn restructure(&mut self) {
        let birthday_text = self.value(Slot::Birthday);
        let calendar_open = self.calendar_open;
        let scroll = std::rc::Rc::clone(&self.scroll);
        let mut next = Self::with_base(self.base.clone(), self.draft.clone(), self.is_new);
        next.scroll = scroll;
        // An unparseable birthday is still the person's text: keep it.
        next.set_value(Slot::Birthday, &birthday_text);
        next.calendar_open = calendar_open;
        *self = next;
    }

    /// Whether the draft holds anything worth keeping (a new contact's
    /// seeded empty rows do not count).
    pub(crate) fn has_content(&mut self) -> bool {
        self.collect();
        let draft = &self.draft;
        !draft.first.trim().is_empty()
            || !draft.last.trim().is_empty()
            || !draft.org.trim().is_empty()
            || !draft.title.trim().is_empty()
            || !draft.website.trim().is_empty()
            || draft.birthday.is_some()
            || !draft.notes.trim().is_empty()
            || !draft.groups.is_empty()
            || draft.phones.iter().any(|p| !p.value.trim().is_empty())
            || draft.emails.iter().any(|e| !e.value.trim().is_empty())
            || draft.addresses.iter().any(|a| !a.is_empty())
    }

    fn input(&self, slot: Slot) -> TextInput {
        let mut input = self
            .state(slot)
            .map_or_else(TextInput::local, TextInput::from_state)
            .with_id(&format!("contacts.form.{}", slot_key(slot)))
            .with_accessible_name(field_name(slot));
        if let Some(hint) = placeholder(slot) {
            input = input.with_placeholder(hint);
        }
        match slot {
            Slot::First | Slot::Last => input = input.with_kind(TextInputKind::Name),
            Slot::Website => input = input.with_kind(TextInputKind::Url),
            Slot::Email(_) => input = input.with_kind(TextInputKind::Email),
            Slot::Phone(_) => input = input.with_kind(TextInputKind::Number),
            Slot::Notes => input.set_multiline(true),
            _ => {}
        }
        input
    }

    fn type_menu(&self, target: TypeTarget) -> Option<Chooser> {
        let (_, state) = self
            .types
            .iter()
            .find(|(candidate, _)| *candidate == target)?;
        let current = read(state);
        let options = type_options(&current);
        let labels: Vec<String> = options.iter().map(|(_, label)| label.clone()).collect();
        let mut menu = Dropdown::from_state(labels.clone(), state)
            .with_id(&format!("contacts.form.type.{}", target.key()))
            .with_accessible_name(format!("{} type", target.spoken()))
            .control_size(ControlSize::Large);
        menu.set_value_options(options);
        Some(Chooser { menu, labels })
    }

    /// `[kind][value][remove]`: one phone, email or the first line of an
    /// address.
    fn typed_row(&self, target: TypeTarget, slot: Slot, shared: &Shared) -> Element {
        let mut row = Row::new().no_wrap();
        if let Some(menu) = self.type_menu(target) {
            row = row.push(Box::new(menu));
        }
        Box::new(
            row.push(Box::new(self.input(slot))).push(Box::new(
                Button::icon_only(
                    &format!("contacts.form.remove.{}", target.key()),
                    Icon::X,
                    &format!("Remove {}", target.spoken().to_lowercase()),
                )
                .on_view(queue(shared, Request::Remove(target))),
            )),
        )
    }

    fn pair(&self, left: Slot, right: Slot) -> Element {
        Box::new(
            Row::new()
                .no_wrap()
                .push(Box::new(self.input(left)))
                .push(Box::new(self.input(right))),
        )
    }

    fn add_button(label: &str, kind: RowKind, shared: &Shared) -> Element {
        let id = format!(
            "contacts.form.add.{}",
            label.to_lowercase().replace(' ', "-")
        );
        Box::new(
            Button::new(&id, label, None)
                .with_icon(Icon::Plus)
                .role(ButtonRole::Plain)
                .on_view(queue(shared, Request::Add(kind))),
        )
    }

    /// The form as widgets, in a scrolling page bounded to a readable
    /// measure.
    pub(crate) fn element(&self, shared: &Shared) -> Element {
        let name = FormSection::new("Name")
            .field(Field::new("First name", self.input(Slot::First)))
            .field(Field::new("Last name", self.input(Slot::Last)))
            .field(Field::new("Organization", self.input(Slot::Org)))
            .field(Field::new("Job title", self.input(Slot::Title)));

        let mut phones = FormSection::new("Phone");
        for i in 0..self.draft.phones.len() {
            phones = phones.content(self.typed_row(TypeTarget::Phone(i), Slot::Phone(i), shared));
        }
        phones = phones.content(Self::add_button("Add phone", RowKind::Phone, shared));

        let mut emails = FormSection::new("Email");
        for i in 0..self.draft.emails.len() {
            emails = emails.content(self.typed_row(TypeTarget::Email(i), Slot::Email(i), shared));
        }
        emails = emails.content(Self::add_button("Add email", RowKind::Email, shared));

        let mut addresses = FormSection::new("Address");
        for i in 0..self.draft.addresses.len() {
            addresses = addresses
                .content(self.typed_row(TypeTarget::Addr(i), Slot::AddrStreet(i), shared))
                .content(self.pair(Slot::AddrCity(i), Slot::AddrRegion(i)))
                .content(self.pair(Slot::AddrPostal(i), Slot::AddrCountry(i)));
        }
        addresses = addresses.content(Self::add_button("Add address", RowKind::Address, shared));

        let calendar_label = if self.calendar_open {
            "Hide calendar"
        } else {
            "Choose from calendar"
        };
        let mut details = FormSection::new("Details")
            .field(Field::new("Birthday", self.input(Slot::Birthday)))
            .content(Box::new(
                Button::new("contacts.form.calendar", calendar_label, None)
                    .with_icon(Icon::Calendar)
                    .role(ButtonRole::Plain)
                    .on_view(queue(shared, Request::ToggleCalendar)),
            ));
        if self.calendar_open {
            let shown = self.value(Slot::Birthday);
            let today = ui::clock::today();
            let initial = Birthday::parse(&shown)
                .and_then(|birthday| birthday.year().map(|_| birthday.iso()))
                .and_then(|iso| iso.parse().ok())
                .unwrap_or(today);
            let picker_shared = std::rc::Rc::clone(shared);
            details = details.content(Box::new(
                DatePicker::new(initial)
                    .with_accessible_name("Birthday calendar")
                    .on_change(move |date| {
                        picker_shared
                            .borrow_mut()
                            .push(Request::PickDate(date.to_string()));
                    }),
            ));
        }
        details = details
            .field(Field::new("Website", self.input(Slot::Website)))
            .field(
                Field::new("Groups", self.input(Slot::Groups))
                    .with_help("Separate groups with commas"),
            )
            .field(Field::new("Notes", self.input(Slot::Notes)));

        let form = Form::new()
            .section(name)
            .section(phones)
            .section(emails)
            .section(addresses)
            .section(details);
        Box::new(
            Scrollable::new_vertical()
                .with_semantic_key("contacts.form")
                .with_persisted_offset(std::rc::Rc::clone(&self.scroll))
                .with_child(Box::new(Readable::of_groups(
                    Column::new()
                        .with_intrinsic_height()
                        .with_id("contacts.form")
                        .push(Box::new(
                            Text::new(if self.is_new {
                                "New contact"
                            } else {
                                "Edit contact"
                            })
                            .with_role(TextRole::Title)
                            .with_heading_level(1),
                        ))
                        .push(Box::new(form)),
                ))),
        )
    }
}

/// The kind of a phone, email or address beside its value: as wide as its
/// widest choice instead of an equal share of the row.
struct Chooser {
    menu: Dropdown,
    labels: Vec<String>,
}

impl ui::widget::Decorator for Chooser {
    type Inner = Dropdown;

    fn inner(&self) -> &Dropdown {
        &self.menu
    }

    fn inner_mut(&mut self) -> &mut Dropdown {
        &mut self.menu
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> taffy::NodeId {
        let node = ui::widget::Widget::build_layout(&mut self.menu, taffy, theme, node_map);
        let style = theme.text(TextRole::Body);
        let widest = ui::text_render::with_layout_renderer(theme.text_scale, |text| {
            self.labels
                .iter()
                .map(|label| text.measure_style(label, None, &style).0)
                .fold(0.0_f32, f32::max)
        });
        // The label, the chevron's pointer-sized slot and the field's
        // insets on both sides.
        let width =
            widest + theme.tokens.metrics.min_pointer_target + 2.0 * theme.tokens.spacing.s3;
        if let Ok(current) = taffy.style(node) {
            let mut layout = current.clone();
            layout.size.width = taffy::style_helpers::length(width.ceil());
            layout.flex_shrink = 0.0;
            let _ = taffy.set_style(node, layout);
        }
        node
    }
}

fn slot_key(slot: Slot) -> String {
    match slot {
        Slot::First => "first".into(),
        Slot::Last => "last".into(),
        Slot::Org => "organization".into(),
        Slot::Title => "title".into(),
        Slot::Website => "website".into(),
        Slot::Birthday => "birthday".into(),
        Slot::Notes => "notes".into(),
        Slot::Groups => "groups".into(),
        Slot::Phone(index) => format!("phone-{index}"),
        Slot::Email(index) => format!("email-{index}"),
        Slot::AddrStreet(index) => format!("address-{index}-street"),
        Slot::AddrCity(index) => format!("address-{index}-city"),
        Slot::AddrRegion(index) => format!("address-{index}-region"),
        Slot::AddrPostal(index) => format!("address-{index}-postal"),
        Slot::AddrCountry(index) => format!("address-{index}-country"),
    }
}

/// Stable identity across reordered repeated fields; indices are layout slots.
pub(crate) fn form_input_key(contact: &Contact, slot: Slot) -> String {
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

fn type_key(contact: &Contact, target: TypeTarget) -> String {
    match target {
        TypeTarget::Phone(i) => format!("phone:{:?}", contact.phones.get(i).map(|row| row.id)),
        TypeTarget::Email(i) => format!("email:{:?}", contact.emails.get(i).map(|row| row.id)),
        TypeTarget::Addr(i) => {
            format!("address:{:?}", contact.addresses.get(i).map(|row| row.id))
        }
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

pub(crate) fn rebase_contact(base: &Contact, local: &Contact, canonical: &Contact) -> Contact {
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
