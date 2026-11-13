//! The pictures beside senders: the person's own (their identity's photo,
//! for mail from their accounts) and the pictures Contacts keeps for the
//! people in their book. Nothing is looked up anywhere else, so writing to
//! someone never tells a picture service who they write to; a sender without
//! a picture shows their coloured initials.
//!
//! Pictures are held as the data they arrive in and decoded off the UI thread
//! by an image store, once each; the address index is rebuilt only when the
//! contact directory changes.

use std::collections::HashMap;

use ui::held_images::HeldImages;
use vello::peniko::ImageData;

pub struct Avatars {
    pictures: HeldImages,
    store: ui::ImageStore,
    by_address: HashMap<String, String>,
    own: Option<(std::collections::HashSet<String>, ImageData)>,
    directory_revision: Option<u64>,
}

impl Default for Avatars {
    fn default() -> Self {
        Self::new()
    }
}

impl Avatars {
    #[must_use]
    pub fn new() -> Self {
        let pictures = HeldImages::default();
        let store = pictures.store();
        Self {
            pictures,
            store,
            by_address: HashMap::new(),
            own: None,
            directory_revision: None,
        }
    }

    #[must_use]
    pub fn image(&self, address: &str) -> Option<ImageData> {
        let address = address.trim().to_ascii_lowercase();
        if let Some((addresses, photo)) = &self.own
            && addresses.contains(&address)
        {
            return Some(photo.clone());
        }
        let held = self.by_address.get(&address)?;
        self.store.get(held)
    }

    #[must_use]
    pub fn follows(&self, revision: u64) -> bool {
        self.directory_revision == Some(revision)
    }

    pub fn follow(
        &mut self,
        directory: Option<(&tilo_contact_directory::ContactDirectory, u64)>,
        own: &[String],
        own_photo: Option<ImageData>,
    ) {
        let mut by_address: HashMap<String, String> = HashMap::new();
        let mut shared: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some((directory, _)) = directory {
            for person in directory.people.values() {
                if person.removed || person.photo.is_empty() {
                    continue;
                }
                let held = self.pictures.hold_text(&person.photo);
                for email in &person.emails {
                    let key = email.trim().to_ascii_lowercase();
                    if let Some(previous) = by_address.insert(key.clone(), held.clone())
                        && previous != held
                    {
                        shared.insert(key);
                    }
                }
            }
        }
        for key in shared {
            by_address.remove(&key);
        }
        self.own = own_photo.map(|photo| {
            let addresses = own
                .iter()
                .map(|email| email.trim().to_ascii_lowercase())
                .collect();
            (addresses, photo)
        });
        let shown: std::collections::HashSet<String> = by_address.values().cloned().collect();
        self.pictures.retain(&shown);
        self.by_address = by_address;
        self.directory_revision = directory.map(|(_, revision)| revision);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_contact_directory::{ContactDirectory, Projected};
    use ui::tilo_resource::{Id, Replica};

    fn directory(people: &[(&str, &str, &str)]) -> ContactDirectory {
        let mut record = Replica::<ContactDirectory>::new();
        let people = people
            .iter()
            .map(|&(id, email, photo)| Projected {
                person: Id::from_raw(id),
                label: email.into(),
                phones: Vec::new(),
                emails: vec![email.into()],
                photo: photo.into(),
            })
            .collect();
        record
            .apply(ContactDirectory::PROJECT_CONTACTS.with((people, Vec::new())))
            .unwrap();
        record.state().unwrap()
    }

    #[test]
    fn sender_pictures_come_from_contacts_and_the_own_identity_only() {
        let book = directory(&[
            (
                "00000000-0000-4000-8000-000000000001",
                "Maya@Northwind.example",
                "QUJD",
            ),
            (
                "00000000-0000-4000-8000-000000000002",
                "team@northwind.example",
                "REVG",
            ),
            (
                "00000000-0000-4000-8000-000000000003",
                "team@northwind.example",
                "R0hJ",
            ),
            (
                "00000000-0000-4000-8000-000000000004",
                "tom@northwind.example",
                "",
            ),
        ]);
        let mut avatars = Avatars::new();
        let me = ImageData {
            data: vello::peniko::Blob::new(std::sync::Arc::new(vec![1, 2, 3, 255])),
            format: vello::peniko::ImageFormat::Rgba8,
            alpha_type: vello::peniko::ImageAlphaType::Alpha,
            width: 1,
            height: 1,
        };
        avatars.follow(Some((&book, 7)), &["Sam@Rivera.example".into()], Some(me));
        assert!(avatars.follows(7));
        assert!(avatars.by_address.contains_key("maya@northwind.example"));
        assert!(!avatars.by_address.contains_key("team@northwind.example"));
        assert!(!avatars.by_address.contains_key("tom@northwind.example"));
        assert!(
            avatars.image("sam@rivera.example").is_some(),
            "the person's own picture"
        );
        for held in avatars.by_address.values() {
            assert!(held.starts_with("held"), "{held}");
            assert!(avatars.pictures.holds(held));
        }
        assert!(avatars.image("stranger@example.com").is_none());
    }
}
