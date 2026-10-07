//! Opt-in address profiles: the person's own, kept in their own room
//! ([`AddressBook`]). They follow the person to their other devices and never
//! reach a collaborator; a private window has none.

use crate::browsing_mode::BrowsingMode;
use serde::{Deserialize, Serialize};
use ui::framework::PersonRecord;

const MAX_PROFILES: usize = 16;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AddressProfile {
    pub id: String,
    pub label: String,
    pub full_name: String,
    pub given_name: String,
    pub family_name: String,
    pub street: String,
    pub city: String,
    pub region: String,
    pub postal_code: String,
    pub country: String,
}
impl AddressProfile {
    fn valid(&self) -> bool {
        !self.id.is_empty()
            && self.id.len() <= 64
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && !self.label.trim().is_empty()
            && [
                (&self.label, 128),
                (&self.full_name, 512),
                (&self.given_name, 256),
                (&self.family_name, 256),
                (&self.street, 1024),
                (&self.city, 256),
                (&self.region, 256),
                (&self.postal_code, 64),
                (&self.country, 64),
            ]
            .into_iter()
            .all(|(value, limit)| {
                value.len() <= limit && !value.chars().any(|c| c.is_control() && c != '\n')
            })
    }
}

/// The person's address book as their record holds it: whether autofill is
/// on, and the saved profiles.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SavedProfiles {
    enabled: bool,
    profiles: Vec<AddressProfile>,
}

/// The person's address book, a resource of their own room.
pub struct AddressBook;

/// The edits of [`AddressBook`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AddressAction {
    /// Turn address autofill on or off.
    SetEnabled { enabled: bool },
    /// Save a profile, replacing the one with its id.
    Put { profile: Box<AddressProfile> },
    /// Delete the profile with this id.
    Delete { id: String },
}

/// `std::fmt::Debug` names the edit, never the personal data it carries.
impl std::fmt::Debug for AddressAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SetEnabled { enabled } => write!(f, "SetEnabled({enabled})"),
            Self::Put { profile } => write!(f, "Put({})", profile.id),
            Self::Delete { id } => write!(f, "Delete({id})"),
        }
    }
}

const ENABLED: &str = "enabled";
const PROFILES: &str = "profiles";

fn profiles_map(doc: &tilo_actions::Doc) -> Option<automerge::ObjId> {
    use automerge::ReadDoc;
    match doc.get(&automerge::ROOT, PROFILES).ok().flatten() {
        Some((automerge::Value::Object(automerge::ObjType::Map), id)) => Some(id),
        _ => None,
    }
}

fn read_book(doc: &tilo_actions::Doc) -> SavedProfiles {
    use automerge::ReadDoc;
    let enabled = ui::doc::read_bool(doc, &automerge::ROOT, ENABLED).unwrap_or(false);
    let mut profiles: Vec<AddressProfile> = profiles_map(doc)
        .map(|map| {
            doc.keys(&map)
                .filter_map(|id| {
                    ui::doc::read_text(doc, &map, id.as_str()).or_else(|| {
                        doc.get(&map, id.as_str())
                            .ok()
                            .flatten()
                            .and_then(|(value, _)| value.into_scalar().ok())
                            .and_then(|value| value.as_str().map(str::to_owned))
                    })
                })
                .filter_map(|json| serde_json::from_str::<AddressProfile>(&json).ok())
                .filter(AddressProfile::valid)
                .collect()
        })
        .unwrap_or_default();
    profiles.sort_by(|a, b| a.id.cmp(&b.id));
    profiles.truncate(MAX_PROFILES);
    SavedProfiles { enabled, profiles }
}

impl tilo_actions::Action for AddressAction {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        use automerge::transaction::Transactable;
        let refused = |reason: &str| tilo_actions::ActionError::Refused(reason.into());
        match self {
            Self::SetEnabled { enabled } => doc.put(automerge::ROOT, ENABLED, *enabled)?,
            Self::Put { profile } => {
                if !profile.valid() {
                    return Err(refused("Address profile fields are invalid or too long"));
                }
                let book = read_book(doc);
                if !book.profiles.iter().any(|saved| saved.id == profile.id)
                    && book.profiles.len() >= MAX_PROFILES
                {
                    return Err(refused("Address profile limit reached"));
                }
                let map = match profiles_map(doc) {
                    Some(map) => map,
                    None => doc.put_object(automerge::ROOT, PROFILES, automerge::ObjType::Map)?,
                };
                let json = serde_json::to_string(profile.as_ref())
                    .map_err(|_| refused("Cannot encode the address profile"))?;
                doc.put(&map, profile.id.as_str(), json)?;
            }
            Self::Delete { id } => {
                if let Some(map) = profiles_map(doc)
                    && automerge::ReadDoc::get(doc, &map, id.as_str())?.is_some()
                {
                    doc.delete(&map, id.as_str())?;
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> tilo_actions::ExecutionLocus {
        tilo_actions::ExecutionLocus::ClientLocal
    }
}

impl ui::tilo_resource::Resource for AddressBook {
    const KIND: &'static str = "browser.address-book";
    const LABEL: &'static str = "Address book";
    const ACTION_TYPE: &'static str = "browser.address-book.action";

    type State = SavedProfiles;
    type Action = AddressAction;

    fn read(doc: &ui::tilo_resource::Doc) -> Result<SavedProfiles, ui::tilo_resource::ModelError> {
        Ok(read_book(doc))
    }
}

/// The address book a window fills forms from: the person's record, or
/// nothing in a private window.
///
/// Contains personal data: deliberately has no Debug implementation.
pub struct AutofillStore {
    record: Option<PersonRecord<AddressBook>>,
    saved: SavedProfiles,
}
impl AutofillStore {
    /// The person's address book, or an empty one nothing is saved to in a
    /// private window or without the person's record.
    pub fn open(mode: BrowsingMode, record: Option<PersonRecord<AddressBook>>) -> Self {
        let record = record.filter(|_| !mode.is_private());
        let saved = record
            .as_ref()
            .map(|record| (*record.get()).clone())
            .unwrap_or_default();
        Self { record, saved }
    }
    pub fn enabled(&self) -> bool {
        self.record.is_some() && self.saved.enabled
    }
    pub fn profiles(&self) -> &[AddressProfile] {
        &self.saved.profiles
    }
    pub fn profile_for_fill(&self, id: &str) -> Option<&AddressProfile> {
        self.enabled()
            .then(|| self.saved.profiles.iter().find(|profile| profile.id == id))
            .flatten()
    }
    /// Turn address autofill on or off.
    ///
    /// # Errors
    /// Returns an error in a private window or when the record refuses it.
    pub fn set_enabled(&mut self, enabled: bool) -> Result<(), String> {
        self.edit(&AddressAction::SetEnabled { enabled })
    }
    /// Save a validated address profile.
    ///
    /// # Errors
    /// Returns an error when autofill is disabled, fields or profile count exceed limits, or the record refuses it.
    pub fn put(&mut self, profile: AddressProfile) -> Result<(), String> {
        if !self.enabled() {
            return Err("Enable address autofill before adding profiles".into());
        }
        if !profile.valid() {
            return Err("Address profile fields are invalid or too long".into());
        }
        self.edit(&AddressAction::Put {
            profile: Box::new(profile),
        })
    }
    /// Delete a saved address profile by ID.
    ///
    /// # Errors
    /// Returns an error in a private window or when the record refuses it.
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        self.edit(&AddressAction::Delete { id: id.to_owned() })
    }
    fn edit(&mut self, action: &AddressAction) -> Result<(), String> {
        let record = self
            .record
            .as_ref()
            .ok_or("Address profiles are unavailable in private windows")?;
        record.apply(action).map_err(|error| error.to_string())?;
        self.saved = (*record.get()).clone();
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::browsing_mode::BrowsingMode;

    pub(crate) fn profile(id: &str) -> AddressProfile {
        AddressProfile {
            id: id.into(),
            label: "Home".into(),
            full_name: "Synthetic Person".into(),
            given_name: "Synthetic".into(),
            family_name: "Person".into(),
            street: "123 Test Street".into(),
            city: "Example".into(),
            region: "CA".into(),
            postal_code: "00000".into(),
            country: "US".into(),
        }
    }

    /// The person's address book, as a capture's profile keeps it.
    pub(crate) fn address_book() -> (
        ui::framework::testing::TestApp<crate::BrowserShell>,
        PersonRecord<AddressBook>,
    ) {
        let mut app =
            ui::framework::testing::TestApp::<crate::BrowserShell>::capture(None).unwrap();
        let record = app
            .person::<AddressBook>()
            .expect("a test profile keeps person records");
        (app, record)
    }

    /// Profile lifecycle: opt-in, private isolation and capacity.
    #[test]
    fn address_profiles_opt_in_edit_capacity_and_private_isolation() {
        // Phase `address_profiles_require_opt_in_and_survive_explicit_edits`.
        {
            let (_app, record) = address_book();
            let mut store = AutofillStore::open(BrowsingMode::Normal, Some(record.clone()));
            assert!(!store.enabled());
            assert!(store.put(profile("one")).is_err());
            store.set_enabled(true).unwrap();
            store.put(profile("one")).unwrap();
            store.put(profile("two")).unwrap();
            let mut edited = profile("one");
            edited.city = "Changed".into();
            store.put(edited).unwrap();
            let mut reopened = AutofillStore::open(BrowsingMode::Normal, Some(record.clone()));
            assert!(reopened.enabled());
            assert_eq!(reopened.profiles().len(), 2);
            assert_eq!(reopened.profiles()[0].city, "Changed");
            reopened.delete("one").unwrap();
            assert_eq!(
                AutofillStore::open(BrowsingMode::Normal, Some(record)).profiles()[0].id,
                "two"
            );
            reopened.set_enabled(false).unwrap();
            assert!(reopened.profile_for_fill("two").is_none());
        }

        // Phase `private_mode_never_reads_or_changes_the_persons_profiles`.
        {
            let (_app, record) = address_book();
            let mut normal = AutofillStore::open(BrowsingMode::Normal, Some(record.clone()));
            normal.set_enabled(true).unwrap();
            normal.put(profile("kept")).unwrap();
            let mut private = AutofillStore::open(BrowsingMode::Private, Some(record.clone()));
            assert!(private.profiles().is_empty());
            assert!(private.set_enabled(false).is_err());
            assert!(private.put(profile("one")).is_err());
            assert!(private.delete("kept").is_err());
            assert_eq!(record.with(|book| book.profiles.len()), 1);
        }

        // Phase `profile_capacity_still_allows_editing_and_deleting_existing_entries`.
        {
            let (_app, record) = address_book();
            let mut store = AutofillStore::open(BrowsingMode::Normal, Some(record));
            store.set_enabled(true).unwrap();
            for index in 0..16 {
                store.put(profile(&format!("profile-{index}"))).unwrap();
            }
            assert!(store.put(profile("overflow")).is_err());
            store.put(profile("profile-0")).unwrap();
            store.delete("profile-0").unwrap();
            store.put(profile("replacement")).unwrap();
            assert_eq!(store.profiles().len(), 16);
        }
    }

    /// Rejected input never reaches the record.
    #[test]
    fn invalid_fields_leave_the_book_untouched() {
        let (_app, record) = address_book();
        let mut store = AutofillStore::open(BrowsingMode::Normal, Some(record.clone()));
        store.set_enabled(true).unwrap();
        let mut invalid = profile("one");
        invalid.street = "x".repeat(1025);
        assert!(store.put(invalid.clone()).is_err());
        assert!(
            record
                .apply(&AddressAction::Put {
                    profile: Box::new(invalid)
                })
                .is_err()
        );
        assert!(record.with(|book| book.profiles.is_empty()));
        let mut data = serde_json::to_value(profile("one")).unwrap();
        data["password"] = "must never enter this store".into();
        assert!(serde_json::from_value::<AddressProfile>(data).is_err());
    }
}
