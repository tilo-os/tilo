//! Opt-in address profiles: the person's own, kept in their own room
//! ([`AddressBook`]). They follow the person to their other devices and never
//! reach a collaborator; a private window has none.

use crate::browsing_mode::BrowsingMode;
use serde::{Deserialize, Serialize};
use ui::framework::PersonRecord;
use ui::tilo_resource::{self, Id, Identified, Invoke, Keyed, Model, Refusal, resource};

const MAX_PROFILES: usize = 16;

/// One saved address, an item of the person's [`AddressBook`].
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Model)]
#[serde(deny_unknown_fields)]
pub struct AddressProfile {
    /// The profile's id: letters, digits, `-` and `_`.
    pub id: String,
    /// Label
    #[model(label)]
    pub label: String,
    /// Full name
    pub full_name: String,
    /// Given name
    pub given_name: String,
    /// Family name
    pub family_name: String,
    /// Street
    pub street: String,
    /// City
    pub city: String,
    /// Region
    pub region: String,
    /// Postal code
    pub postal_code: String,
    /// Country
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

impl Identified for AddressProfile {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

/// `std::fmt::Debug` names the profile, never the personal data it holds.
impl std::fmt::Debug for AddressProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AddressProfile({})", self.id)
    }
}

/// The person's address book, a resource of their own room: whether
/// autofill is on, and the saved profiles. Every way in keeps each profile
/// valid and the book within its limit.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = AddressBook::valid)]
pub struct AddressBook {
    /// Address autofill
    pub enabled: bool,
    /// Saved addresses, by id
    pub profiles: Keyed<AddressProfile>,
}

#[resource(kind = "browser.address-book", label = "Address book")]
impl AddressBook {}

impl AddressBook {
    fn valid(&self) -> Result<(), Refusal> {
        if !self.profiles.iter().all(AddressProfile::valid) {
            return Err(Refusal::new(
                "Address profile fields are invalid or too long",
            ));
        }
        if self.profiles.len() > MAX_PROFILES {
            return Err(Refusal::new("Address profile limit reached"));
        }
        Ok(())
    }
}

/// The address book a window fills forms from: the person's record, or
/// nothing in a private window.
///
/// Contains personal data: deliberately has no Debug implementation.
pub struct AutofillStore {
    record: Option<PersonRecord<AddressBook>>,
    saved: AddressBook,
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
        self.edit(&AddressBook::paths().enabled.set(enabled))
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
        // A saved profile changes field by field, so a concurrent edit of
        // another of its fields is kept.
        let mut after = self.saved.profiles.clone();
        match after.find_mut(&profile.id()) {
            Some(saved) => *saved = profile,
            None => after.push(profile),
        }
        let edits = AddressBook::paths()
            .profiles
            .edits(&self.saved.profiles, &after);
        self.edit_all(&edits)
    }
    /// Delete a saved address profile by ID.
    ///
    /// # Errors
    /// Returns an error in a private window or when the record refuses it.
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        self.edit(
            &AddressBook::paths()
                .profiles
                .remove(&Id::from_raw(id.to_owned())),
        )
    }
    fn edit(&mut self, edit: &tilo_resource::Edit<AddressBook>) -> Result<(), String> {
        self.edit_all(std::slice::from_ref(edit))
    }
    fn edit_all(&mut self, edits: &[tilo_resource::Edit<AddressBook>]) -> Result<(), String> {
        if edits.is_empty() {
            return Ok(());
        }
        let record = self
            .record
            .as_ref()
            .ok_or("Address profiles are unavailable in private windows")?;
        let members = edits
            .iter()
            .map(Invoke::edit)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let invoke = match <[_; 1]>::try_from(members) {
            Ok([edit]) => edit,
            Err(members) => Invoke::group(members),
        };
        record.invoke(&invoke).map_err(|error| error.to_string())?;
        self.saved = (*record.get()).clone();
        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::browsing_mode::BrowsingMode;

    pub fn profile(id: &str) -> AddressProfile {
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
    pub fn address_book() -> (
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
                .invoke(&Invoke::edit(&AddressBook::paths().profiles.insert(&invalid)).unwrap())
                .is_err()
        );
        assert!(record.with(|book| book.profiles.is_empty()));
        let mut data = serde_json::to_value(profile("one")).unwrap();
        data["password"] = "must never enter this store".into();
        assert!(serde_json::from_value::<AddressProfile>(data).is_err());
    }
}
