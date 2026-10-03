//! Local-only address editor; no collaborative actions or password fields.
use crate::{
    autofill::{AddressProfile, AutofillStore},
    browsing_mode::BrowsingMode,
};
use std::path::PathBuf;
use ui::{
    LocalState,
    webview::AddressFields,
    widget::{Button, Column, Text, TextInput, Widget},
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
    path: Option<PathBuf>,
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
    pub fn new(private: bool, path: Option<PathBuf>) -> Self {
        Self {
            private,
            path: if private { None } else { path },
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
    fn store(&self) -> Result<AutofillStore, String> {
        if self.private {
            return Err("Address autofill is unavailable in private windows.".into());
        }
        AutofillStore::open(
            BrowsingMode::Normal,
            self.path
                .as_deref()
                .ok_or("Address storage is unavailable.")?,
        )
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
        let pending = self.action.clone();
        Button::new(id, label, None)
            .with_size(210.0, 32.0)
            .on_effect(move || pending.set(Some(action.clone())))
    }
    pub fn panel(&self) -> Box<dyn Widget> {
        let mut column=Column::new().with_intrinsic_height().push(line("Address profiles stay on this device. Filling requires your selection and confirmation."));
        if self.private {
            return Box::new(
                column.push(line("Address autofill is unavailable in private windows.")),
            );
        }
        column = column.push(line(&self.notice)).push(Box::new(self.button(
            "browser.address.enable",
            if self.enabled {
                "Disable address autofill"
            } else {
                "Enable address autofill"
            },
            Action::Enable(!self.enabled),
        )));
        column = self.profile_controls(column);
        if let Some(id) = &self.delete {
            let label = self
                .profiles
                .iter()
                .find(|p| &p.id == id)
                .map_or("selected address", |p| p.label.as_str());
            column = column
                .push(line(&format!("Delete {label}?")))
                .push(Box::new(self.button(
                    "browser.address.delete.confirm",
                    "Confirm delete",
                    Action::ConfirmDelete,
                )))
                .push(Box::new(self.button(
                    "browser.address.delete.cancel",
                    "Keep address",
                    Action::CancelDelete,
                )));
        }
        Box::new(column)
    }
    fn profile_controls(&self, mut column: Column) -> Column {
        if self.editing.is_some() {
            for (i, (label, field)) in [
                "Profile label",
                "Full name",
                "Given name",
                "Family name",
                "Street address",
                "City",
                "Region / state",
                "Postal code",
                "Country",
            ]
            .iter()
            .zip(&self.fields)
            .enumerate()
            {
                column = column.push(line(label)).push(Box::new(
                    TextInput::from_state(field)
                        .with_id(&format!("browser.address.field.{i}"))
                        .with_accessible_name(*label),
                ));
            }
            column = column
                .push(Box::new(self.button(
                    "browser.address.save",
                    "Save address",
                    Action::Save,
                )))
                .push(Box::new(self.button(
                    "browser.address.cancel",
                    "Discard edits",
                    Action::Cancel,
                )));
        } else {
            for (i, profile) in self.profiles.iter().enumerate() {
                let label = format!(
                    "{}{}",
                    if self.selected.as_ref() == Some(&profile.id) {
                        "✓ "
                    } else {
                        ""
                    },
                    profile.label
                );
                column = column
                    .push(Box::new(
                        self.button(
                            &format!("browser.address.select.{i}"),
                            &label,
                            Action::Select(profile.id.clone()),
                        )
                        .with_enabled(self.enabled),
                    ))
                    .push(Box::new(
                        self.button(
                            &format!("browser.address.edit.{i}"),
                            "Edit address",
                            Action::Edit(profile.id.clone()),
                        )
                        .with_enabled(self.enabled),
                    ))
                    .push(Box::new(self.button(
                        &format!("browser.address.delete.{i}"),
                        "Delete address…",
                        Action::Delete(profile.id.clone()),
                    )));
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
    Box::new(Text::new(text).with_size(13.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Address editor opt-in, cross-tab disable and private-mode isolation.
    #[test]
    fn address_editor_requires_opt_in_and_isolates_tabs_and_private_mode() {
        // Phase `editor_requires_opt_in_saves_edits_and_confirms_deletion`.
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("addresses.json");
            let mut editor = AddressControls::new(false, Some(path));
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
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("addresses.json");
            let mut editor = AddressControls::new(false, Some(path.clone()));
            editor.act(Action::Enable(true));
            editor.act(Action::New);
            editor.fields[0].set("Home".into());
            editor.act(Action::Save);
            assert!(editor.selected_fields().is_some());
            AutofillStore::open(BrowsingMode::Normal, &path)
                .unwrap()
                .set_enabled(false)
                .unwrap();
            assert!(editor.selected_fields().is_none());
        }

        // Phase `private_editor_never_reads_or_writes_profile_path`.
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("addresses.json");
            std::fs::write(&path, "invalid normal data").unwrap();
            let mut editor = AddressControls::new(true, Some(path.clone()));
            editor.refresh();
            editor.act(Action::Enable(true));
            editor.act(Action::New);
            assert!(editor.profiles.is_empty());
            assert!(editor.editing.is_none());
            assert_eq!(
                std::fs::read_to_string(path).unwrap(),
                "invalid normal data"
            );
        }
    }
}
