//! Editor of the person's address book; no collaborative actions or password
//! fields.
use crate::{
    autofill::{AddressBook, AddressProfile, AutofillStore},
    browsing_mode::BrowsingMode,
};
use ui::framework::PersonRecord;
use ui::{
    LocalState,
    icons::Icon,
    tokens::TextRole,
    webview::AddressFields,
    widget::{
        Actions, Button, ButtonRole, Column, Field, Form, FormSection, ListRow, Section, Text,
        TextInput, Widget,
    },
};
#[derive(Clone, Debug)]
enum Action {
    Enable(bool),
    New,
    Edit(String),
    Select(String),
    Save,
    Cancel,
    Delete(String),
    ConfirmDelete,
    CancelDelete,
}
pub(crate) struct AddressControls {
    private: bool,
    book: Option<PersonRecord<AddressBook>>,
    enabled: bool,
    profiles: Vec<AddressProfile>,
    selected: Option<String>,
    editing: Option<String>,
    fields: [LocalState<String>; 9],
    delete: Option<String>,
    action: LocalState<Option<Action>>,
    notice: String,
}
impl AddressControls {
    pub fn new(private: bool) -> Self {
        Self {
            private,
            book: None,
            enabled: false,
            profiles: vec![],
            selected: None,
            editing: None,
            fields: std::array::from_fn(|_| LocalState::new(String::new())),
            delete: None,
            action: LocalState::new(None),
            notice: String::new(),
        }
    }
    /// Edit the person's address book.
    pub fn attach(&mut self, book: PersonRecord<AddressBook>) {
        if !self.private {
            self.book = Some(book);
        }
    }
    fn store(&self) -> Result<AutofillStore, String> {
        if self.private {
            return Err("Address autofill is unavailable in private windows.".into());
        }
        let book = self.book.clone().ok_or("Address storage is unavailable.")?;
        Ok(AutofillStore::open(BrowsingMode::Normal, Some(book)))
    }
    pub fn clear(&mut self) {
        self.profiles.clear();
        self.selected = None;
        self.editing = None;
        self.delete = None;
        self.action.set(None);
        for field in &self.fields {
            field.set(String::new());
        }
    }
    pub fn refresh(&mut self) {
        match self.store() {
            Ok(store) => {
                self.enabled = store.enabled();
                self.profiles = store.profiles().to_vec();
                if !self
                    .profiles
                    .iter()
                    .any(|p| Some(&p.id) == self.selected.as_ref())
                {
                    self.selected = None;
                }
            }
            Err(error) => {
                self.enabled = false;
                self.profiles.clear();
                self.selected = None;
                self.notice = error;
            }
        }
    }
    pub fn selected_fields(&mut self) -> Option<AddressFields> {
        // Reload before fill so another tab disabling or deleting a profile takes effect.
        let store = match self.store() {
            Ok(store) => store,
            Err(error) => {
                self.notice = error;
                return None;
            }
        };
        let profile = store.profile_for_fill(self.selected.as_deref()?)?;
        Some(AddressFields {
            full_name: profile.full_name.clone(),
            given_name: profile.given_name.clone(),
            family_name: profile.family_name.clone(),
            street: profile.street.clone(),
            city: profile.city.clone(),
            region: profile.region.clone(),
            postal_code: profile.postal_code.clone(),
            country: profile.country.clone(),
        })
    }
    /// Review fixture: autofill on, with two saved profiles, one chosen.
    pub(crate) fn stage_for_review(&mut self) {
        self.private = false;
        self.enabled = true;
        self.profiles = vec![
            AddressProfile {
                id: "home".into(),
                label: "Home".into(),
                full_name: "Alex Morgan".into(),
                given_name: "Alex".into(),
                family_name: "Morgan".into(),
                street: "14 Elm Street".into(),
                city: "Portland".into(),
                region: "Oregon".into(),
                postal_code: "97201".into(),
                country: "United States".into(),
            },
            AddressProfile {
                id: "work".into(),
                label: "Work".into(),
                full_name: "Alex Morgan".into(),
                given_name: "Alex".into(),
                family_name: "Morgan".into(),
                street: "300 Pine Avenue".into(),
                city: "Seattle".into(),
                region: "Washington".into(),
                postal_code: "98101".into(),
                country: "United States".into(),
            },
        ];
        self.selected = Some("home".into());
    }
    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }
    pub fn has_selection(&self) -> bool {
        self.enabled && self.selected.is_some() && self.editing.is_none()
    }
    fn edit(&mut self, profile: AddressProfile) {
        self.editing = Some(profile.id);
        self.delete = None;
        for (state, value) in self.fields.iter().zip([
            profile.label,
            profile.full_name,
            profile.given_name,
            profile.family_name,
            profile.street,
            profile.city,
            profile.region,
            profile.postal_code,
            profile.country,
        ]) {
            state.set(value);
        }
    }
    fn act(&mut self, action: Action) {
        if self.private {
            return;
        }
        self.notice.clear();
        let result: Result<(), String> = (|| {
            match action {
                Action::Enable(enabled) => {
                    let mut store = self.store()?;
                    store.set_enabled(enabled)?;
                    self.editing = None;
                    for field in &self.fields {
                        field.set(String::new());
                    }
                }
                Action::New => {
                    if self.store()?.enabled() {
                        self.edit(AddressProfile {
                            id: uuid::Uuid::new_v4().to_string(),
                            label: String::new(),
                            full_name: String::new(),
                            given_name: String::new(),
                            family_name: String::new(),
                            street: String::new(),
                            city: String::new(),
                            region: String::new(),
                            postal_code: String::new(),
                            country: String::new(),
                        });
                    }
                }
                Action::Edit(id) => {
                    let store = self.store()?;
                    if let Some(profile) = store.profile_for_fill(&id) {
                        self.edit(profile.clone());
                    }
                }
                Action::Select(id) => {
                    let store = self.store()?;
                    if store.profile_for_fill(&id).is_some() {
                        self.selected = Some(id);
                        self.delete = None;
                    }
                }
                Action::Save => {
                    if let Some(id) = &self.editing {
                        let mut store = self.store()?;
                        store.put(AddressProfile {
                            id: id.clone(),
                            label: self.fields[0].get(),
                            full_name: self.fields[1].get(),
                            given_name: self.fields[2].get(),
                            family_name: self.fields[3].get(),
                            street: self.fields[4].get(),
                            city: self.fields[5].get(),
                            region: self.fields[6].get(),
                            postal_code: self.fields[7].get(),
                            country: self.fields[8].get(),
                        })?;
                        self.selected = Some(id.clone());
                        self.editing = None;
                        for field in &self.fields {
                            field.set(String::new());
                        }
                        self.notice = "Address saved on this device.".into();
                    }
                }
                Action::Cancel => {
                    self.editing = None;
                    for field in &self.fields {
                        field.set(String::new());
                    }
                }
                Action::Delete(id) => {
                    if self.store()?.profiles().iter().any(|p| p.id == id) {
                        self.delete = Some(id);
                    }
                }
                Action::ConfirmDelete => {
                    if let Some(id) = self.delete.take() {
                        self.store()?.delete(&id)?;
                    }
                }
                Action::CancelDelete => self.delete = None,
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.notice = error;
        }
        self.refresh();
    }
    pub fn sync(&mut self) -> bool {
        if let Some(action) = self.action.get() {
            self.action.set(None);
            self.act(action);
            true
        } else {
            false
        }
    }
    fn button(&self, id: &str, label: &str, action: Action) -> Button {
        Button::new(id, label).on_effect(self.command(action))
    }
    fn command(&self, action: Action) -> impl FnMut() + 'static {
        let pending = self.action.clone();
        move || pending.set(Some(action.clone()))
    }
    pub fn panel(&self) -> Box<dyn Widget> {
        let mut column = Column::new().with_no_padding().with_intrinsic_height();
        if self.private {
            return Box::new(
                column.push(line("Address autofill is unavailable in private windows.")),
            );
        }
        if !self.notice.is_empty() {
            column = column.push(line(&self.notice));
        }
        if self.editing.is_none() {
            column = column.push(Box::new(
                self.button(
                    "browser.address.enable",
                    if self.enabled {
                        "Turn off address autofill"
                    } else {
                        "Turn on address autofill"
                    },
                    Action::Enable(!self.enabled),
                )
                .role(if self.enabled {
                    ButtonRole::Secondary
                } else {
                    ButtonRole::Primary
                }),
            ));
        }
        column = self.profile_controls(column);
        if let Some(id) = &self.delete {
            let label = self
                .profiles
                .iter()
                .find(|p| &p.id == id)
                .map_or("the selected address", |p| p.label.as_str());
            column = column
                .push(line(&format!("Delete {label}?")))
                .push(Box::new(
                    Actions::new(
                        self.button(
                            "browser.address.delete.confirm",
                            "Delete address",
                            Action::ConfirmDelete,
                        )
                        .role(ButtonRole::Destructive),
                    )
                    .secondary(self.button(
                        "browser.address.delete.cancel",
                        "Keep address",
                        Action::CancelDelete,
                    )),
                ));
        }
        Box::new(column)
    }
    fn profile_controls(&self, mut column: Column) -> Column {
        if self.editing.is_some() {
            let mut section = FormSection::new("Address");
            for (i, (label, field)) in [
                "Profile label",
                "Full name",
                "Given name",
                "Family name",
                "Street address",
                "City",
                "Region or state",
                "Postal code",
                "Country",
            ]
            .iter()
            .zip(&self.fields)
            .enumerate()
            {
                section = section.field(Field::new(
                    *label,
                    TextInput::from_state(field).with_id(&format!("browser.address.field.{i}")),
                ));
            }
            column = column.push(Box::new(
                Form::new().section(section).with_actions(
                    Actions::new(
                        self.button("browser.address.save", "Save address", Action::Save)
                            .role(ButtonRole::Primary),
                    )
                    .secondary(self.button(
                        "browser.address.cancel",
                        "Discard edits",
                        Action::Cancel,
                    )),
                ),
            ));
        } else {
            let rows: Vec<Box<dyn Widget>> = self
                .profiles
                .iter()
                .enumerate()
                .map(|(i, profile)| {
                    let mut row =
                        ListRow::new(format!("browser.address.select.{i}"), &profile.label)
                            .selected(self.selected.as_ref() == Some(&profile.id));
                    if self.enabled {
                        row = row
                            .on_activate(self.command(Action::Select(profile.id.clone())))
                            .action(
                                Icon::Pencil,
                                "Edit address",
                                self.command(Action::Edit(profile.id.clone())),
                            );
                    }
                    row = row.action(
                        Icon::Trash2,
                        "Delete address",
                        self.command(Action::Delete(profile.id.clone())),
                    );
                    Box::new(row) as Box<dyn Widget>
                })
                .collect();
            if !rows.is_empty() {
                column = column.push(Box::new(Section::new(rows)));
            }
            if self.enabled {
                column = column.push(Box::new(self.button(
                    "browser.address.new",
                    "Add address",
                    Action::New,
                )));
            }
        }
        column
    }
}

fn line(text: &str) -> Box<dyn Widget> {
    Box::new(Text::new(text).with_role(TextRole::Body))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Address editor opt-in, cross-tab disable and private-mode isolation.
    #[test]
    fn address_editor_requires_opt_in_and_isolates_tabs_and_private_mode() {
        // Phase `editor_requires_opt_in_saves_edits_and_confirms_deletion`.
        {
            let (_app, book) = crate::autofill::tests::address_book();
            let mut editor = AddressControls::new(false);
            editor.attach(book);
            editor.refresh();
            assert!(editor.profiles.is_empty());
            editor.act(Action::New);
            assert!(editor.editing.is_none());
            editor.act(Action::Enable(true));
            editor.act(Action::New);
            editor.fields[0].set("Home".into());
            editor.fields[1].set("Test Person".into());
            editor.fields[2].set("Test".into());
            editor.fields[3].set("Person".into());
            editor.act(Action::Save);
            let id = editor.profiles[0].id.clone();
            editor.act(Action::Select(id.clone()));
            let profile = editor.selected_fields().unwrap();
            assert_eq!(profile.given_name, "Test");
            assert_eq!(profile.family_name, "Person");
            editor.act(Action::Edit(id.clone()));
            editor.fields[5].set("Changed City".into());
            editor.act(Action::Save);
            assert_eq!(editor.selected_fields().unwrap().city, "Changed City");
            editor.act(Action::Delete(id));
            assert_eq!(editor.profiles.len(), 1);
            editor.act(Action::ConfirmDelete);
            assert!(editor.profiles.is_empty());
            editor.refresh();
            assert!(editor.profiles.is_empty());
        }

        // Phase `another_tab_disabling_profiles_prevents_fill_from_cached_selection`.
        {
            let (_app, book) = crate::autofill::tests::address_book();
            let mut editor = AddressControls::new(false);
            editor.attach(book.clone());
            editor.act(Action::Enable(true));
            editor.act(Action::New);
            editor.fields[0].set("Home".into());
            editor.act(Action::Save);
            assert!(editor.selected_fields().is_some());
            AutofillStore::open(BrowsingMode::Normal, Some(book))
                .set_enabled(false)
                .unwrap();
            assert!(editor.selected_fields().is_none());
        }

        // Phase `private_editor_never_reads_or_writes_the_persons_book`.
        {
            let (_app, book) = crate::autofill::tests::address_book();
            let mut editor = AddressControls::new(true);
            editor.attach(book.clone());
            editor.refresh();
            editor.act(Action::Enable(true));
            editor.act(Action::New);
            assert!(editor.profiles.is_empty());
            assert!(editor.editing.is_none());
            assert!(book.with(|saved| *saved == crate::autofill::AddressBook::default()));
        }
    }
}
