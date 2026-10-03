//! Automerge document model for the address book.
//!
//! Mirrors the in-memory [`ContactStore`](crate::model::ContactStore)
//! into a live CRDT structure so two peers editing the same book
//! converge. Layout:
//!
//! ```text
//! root
//! └── "contacts" (Map, keyed by ContactId string)
//!     └── <id> (Map)
//!         ├── "first" / "last" / "org" / "title" / "website" / "notes" (Text)
//!         ├── "birthday" (Str)   "favorite" / "deleted" (Bool)
//!         ├── "groups" (List<Map{id:Uuid, value:Str}>)
//!         ├── "phones" / "emails" (List<Map{id:Uuid, label:Str, value:Text}>)
//!         └── "addresses" (List<Map{id:Uuid, label,street,city,region,postal,country:Str}>)
//! ```
//!
//! Collaborative text fields (name, org, notes, …) are `Text` objects so
//! concurrent keystrokes merge; the spliced-diff write keeps churn low.
//! Repeated fields are changed entry-by-entry. Deleted contacts remain as
//! tombstones, so a delayed patch can never recreate a record another peer
//! deleted.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::doc::{read_bool, read_str, read_str_or_text};

use crate::model::{Address, Birthday, Contact, ContactId, GroupEntry, Labeled};
use uuid::Uuid;

/// Stateless namespace for the contacts document helpers.
pub struct ContactsDoc;

impl ContactsDoc {
    /// Get-or-create the top-level `contacts` Map.
    ///
    /// Returns `None` only if the document rejects the write (e.g. the root
    /// was somehow replaced by a non-map); callers treat that as a no-op
    /// rather than crashing.
    pub fn contacts_map(doc: &mut AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, "contacts") {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => doc.put_object(&ROOT, "contacts", ObjType::Map).ok(),
        }
    }

    /// Locate an active contact without ever recreating it. `None` covers a
    /// missing record, a malformed document shape, and a deletion tombstone.
    fn active_contact(doc: &AutoCommit, id: ContactId) -> Option<ObjId> {
        let contacts = doc
            .get(&ROOT, "contacts")
            .ok()
            .flatten()
            .and_then(|(value, object)| {
                matches!(value, Value::Object(ObjType::Map)).then_some(object)
            })?;
        let key = id.to_string();
        let (value, contact) = doc.get(&contacts, key.as_str()).ok().flatten()?;
        matches!(value, Value::Object(ObjType::Map))
            .then_some(contact)
            .filter(|contact| !read_bool(doc, contact, "deleted").unwrap_or(false))
    }

    /// Whether an action may still address this contact. Kept read-only so a
    /// patch can validate every row target before it makes its first write.
    pub fn contact_is_active(doc: &AutoCommit, id: ContactId) -> bool {
        Self::active_contact(doc, id).is_some()
    }

    /// Whether a group entry id exists on an active contact.
    pub fn group_row_exists(doc: &AutoCommit, id: ContactId, entry_id: Uuid) -> bool {
        Self::active_contact(doc, id)
            .is_some_and(|contact| list_row(doc, &contact, "groups", entry_id).is_some())
    }

    /// Whether a phone/email row id exists on an active contact.
    pub fn labeled_row_exists(doc: &AutoCommit, id: ContactId, key: &str, entry_id: Uuid) -> bool {
        Self::active_contact(doc, id)
            .is_some_and(|contact| list_row(doc, &contact, key, entry_id).is_some())
    }

    /// Whether an address row id exists on an active contact.
    pub fn address_row_exists(doc: &AutoCommit, id: ContactId, entry_id: Uuid) -> bool {
        Self::active_contact(doc, id)
            .is_some_and(|contact| list_row(doc, &contact, "addresses", entry_id).is_some())
    }

    /// Create a previously unseen contact. An existing active record or a
    /// tombstone refuses creation: ids are stable, and a stale create must not
    /// become a resurrection path.
    pub fn create_contact(doc: &mut AutoCommit, c: &Contact) -> bool {
        let Some(contacts) = Self::contacts_map(doc) else {
            return false;
        };
        let key = c.id.to_string();
        if doc.get(&contacts, key.as_str()).ok().flatten().is_some() {
            return false;
        }
        let Ok(contact) = doc.put_object(&contacts, key.as_str(), ObjType::Map) else {
            return false;
        };
        ui::doc::put_if_changed(doc, &contact, "deleted", false);
        Self::write_contact(doc, &contact, c);
        true
    }

    /// Insert or update a contact record, writing only the fields that
    /// actually changed so concurrent edits don't thrash.
    pub fn upsert_contact(doc: &mut AutoCommit, c: &Contact) {
        if Self::create_contact(doc, c) {
            return;
        }
        let Some(m) = Self::active_contact(doc, c.id) else {
            return;
        };
        Self::write_contact(doc, &m, c);
    }

    fn write_contact(doc: &mut AutoCommit, m: &ObjId, c: &Contact) {
        sync_text(doc, m, "first", &c.first);
        sync_text(doc, m, "last", &c.last);
        sync_text(doc, m, "org", &c.org);
        sync_text(doc, m, "title", &c.title);
        sync_text(doc, m, "website", &c.website);
        sync_text(doc, m, "notes", &c.notes);
        // Persist the birthday as its canonical string (empty when unset); the
        // structured value is reconstructed by `Birthday::parse` on read.
        ui::doc::put_if_changed(
            doc,
            m,
            "birthday",
            c.birthday.map(|b| b.iso()).unwrap_or_default(),
        );
        ui::doc::put_if_changed(doc, m, "favorite", c.favorite);
        sync_group_list(doc, m, &c.groups);
        sync_labeled_list(doc, m, "phones", &c.phones);
        sync_labeled_list(doc, m, "emails", &c.emails);
        sync_address_list(doc, m, "addresses", &c.addresses);
    }

    /// Tombstone a contact. The map is intentionally retained so stale
    /// actions cannot recreate it after a peer has deleted it.
    pub fn delete_contact(doc: &mut AutoCommit, id: ContactId) {
        let Some(contact) = Self::active_contact(doc, id) else {
            return;
        };
        ui::doc::put_if_changed(doc, &contact, "deleted", true);
    }

    /// Set just the `favorite` flag on an existing contact, leaving every other
    /// field untouched. A no-op when the contact is absent, so a stale replay
    /// can't resurrect a deleted record. The guarded put makes an unchanged
    /// re-store free.
    pub fn set_favorite(doc: &mut AutoCommit, id: ContactId, favorite: bool) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        ui::doc::put_if_changed(doc, &contact, "favorite", favorite);
        true
    }

    /// Patch one collaborative scalar field on an active record. Each field
    /// lives in its own Automerge object, so independent peer edits merge.
    pub fn set_text_field(doc: &mut AutoCommit, id: ContactId, field: &str, value: &str) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        sync_text(doc, &contact, field, value);
        true
    }

    /// Patch the structured birthday string on an active record.
    pub fn set_birthday(doc: &mut AutoCommit, id: ContactId, birthday: &str) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        ui::doc::put_if_changed(doc, &contact, "birthday", birthday);
        true
    }

    /// Add a fresh group row. The caller supplies its durable entry identity.
    pub fn add_group(doc: &mut AutoCommit, id: ContactId, value: &GroupEntry) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        add_group_row(doc, &contact, value)
    }

    /// Update exactly one group row by identity.
    pub fn update_group(doc: &mut AutoCommit, id: ContactId, value: &GroupEntry) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        update_group_row(doc, &contact, value)
    }

    /// Remove exactly one group row by identity.
    pub fn remove_group(doc: &mut AutoCommit, id: ContactId, entry_id: Uuid) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        remove_row(doc, &contact, "groups", entry_id)
    }

    /// Add a fresh phone/email row by durable entry identity.
    pub fn add_labeled(doc: &mut AutoCommit, id: ContactId, key: &str, value: &Labeled) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        add_labeled_row(doc, &contact, key, value)
    }

    /// Update exactly one phone/email row by durable entry identity.
    pub fn update_labeled(doc: &mut AutoCommit, id: ContactId, key: &str, value: &Labeled) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        update_labeled_row(doc, &contact, key, value)
    }

    /// Remove exactly one phone/email row by durable entry identity.
    pub fn remove_labeled(doc: &mut AutoCommit, id: ContactId, key: &str, entry_id: Uuid) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        remove_row(doc, &contact, key, entry_id)
    }

    /// Add a fresh postal-address row by durable entry identity.
    pub fn add_address(doc: &mut AutoCommit, id: ContactId, value: &Address) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        add_address_row(doc, &contact, value)
    }

    /// Update exactly one postal-address row by durable entry identity.
    pub fn update_address(doc: &mut AutoCommit, id: ContactId, value: &Address) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        update_address_row(doc, &contact, value)
    }

    /// Remove exactly one postal-address row by durable entry identity.
    pub fn remove_address(doc: &mut AutoCommit, id: ContactId, entry_id: Uuid) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        remove_row(doc, &contact, "addresses", entry_id)
    }

    /// Read every contact out of the document. Records with an
    /// unparseable id key are skipped.
    pub fn list_contacts(doc: &AutoCommit) -> Vec<Contact> {
        let Ok(Some((Value::Object(ObjType::Map), contacts))) = doc.get(&ROOT, "contacts") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for key in doc.keys(&contacts) {
            let Ok(id) = key.parse::<ContactId>() else {
                continue;
            };
            let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&contacts, key.as_str())
            else {
                continue;
            };
            if read_bool(doc, &m, "deleted").unwrap_or(false) {
                continue;
            }
            out.push(Contact {
                id,
                first: read_str_or_text(doc, &m, "first").unwrap_or_default(),
                last: read_str_or_text(doc, &m, "last").unwrap_or_default(),
                org: read_str_or_text(doc, &m, "org").unwrap_or_default(),
                title: read_str_or_text(doc, &m, "title").unwrap_or_default(),
                website: read_str_or_text(doc, &m, "website").unwrap_or_default(),
                notes: read_str_or_text(doc, &m, "notes").unwrap_or_default(),
                birthday: Birthday::parse(&read_str(doc, &m, "birthday").unwrap_or_default()),
                favorite: read_bool(doc, &m, "favorite").unwrap_or(false),
                groups: read_group_list(doc, &m),
                phones: read_labeled_list(doc, &m, "phones"),
                emails: read_labeled_list(doc, &m, "emails"),
                addresses: read_address_list(doc, &m, "addresses"),
            });
        }
        out
    }
}

// ---------------------------------------------------------------------
// Field helpers
// ---------------------------------------------------------------------

fn text_obj(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::Text), id))) => Some(id),
        _ => doc.put_object(parent, key, ObjType::Text).ok(),
    }
}

fn sync_text(doc: &mut AutoCommit, parent: &ObjId, key: &str, value: &str) {
    let Some(tid) = text_obj(doc, parent, key) else {
        return;
    };
    ui::doc::splice_text_diffed(doc, &tid, value);
}

/// Get-or-create a List object at `parent[key]`.
fn list_obj(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
        _ => doc.put_object(parent, key, ObjType::List).ok(),
    }
}

fn row_id(doc: &AutoCommit, row: &ObjId) -> Option<Uuid> {
    read_str(doc, row, "id")?
        .parse()
        .ok()
        .filter(|id: &Uuid| !id.is_nil())
}

fn list_row(
    doc: &AutoCommit,
    parent: &ObjId,
    key: &str,
    entry_id: Uuid,
) -> Option<(ObjId, usize, ObjId)> {
    let (Value::Object(ObjType::List), list) = doc.get(parent, key).ok().flatten()? else {
        return None;
    };
    for index in 0..doc.length(&list) {
        let Some((Value::Object(ObjType::Map), row)) = doc.get(&list, index).ok().flatten() else {
            continue;
        };
        if row_id(doc, &row) == Some(entry_id) {
            return Some((list, index, row));
        }
    }
    None
}

fn remove_row(doc: &mut AutoCommit, parent: &ObjId, key: &str, entry_id: Uuid) -> bool {
    list_row(doc, parent, key, entry_id)
        .is_some_and(|(list, index, _)| doc.delete(&list, index).is_ok())
}

fn read_group_list(doc: &AutoCommit, parent: &ObjId) -> Vec<GroupEntry> {
    let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(parent, "groups") else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|index| match doc.get(&list, index).ok().flatten() {
            Some((Value::Object(ObjType::Map), row)) => Some(GroupEntry {
                id: row_id(doc, &row)?,
                value: read_str(doc, &row, "value").unwrap_or_default(),
            }),
            _ => None,
        })
        .collect()
}

fn write_group_row(doc: &mut AutoCommit, row: &ObjId, value: &GroupEntry) {
    ui::doc::put_if_changed(doc, row, "id", value.id.to_string());
    ui::doc::put_if_changed(doc, row, "value", value.value.clone());
}

fn add_group_row(doc: &mut AutoCommit, parent: &ObjId, value: &GroupEntry) -> bool {
    if list_row(doc, parent, "groups", value.id).is_some() {
        return false;
    }
    let Some(list) = list_obj(doc, parent, "groups") else {
        return false;
    };
    let Ok(row) = doc.insert_object(&list, doc.length(&list), ObjType::Map) else {
        return false;
    };
    write_group_row(doc, &row, value);
    true
}

fn update_group_row(doc: &mut AutoCommit, parent: &ObjId, value: &GroupEntry) -> bool {
    let Some((_, _, row)) = list_row(doc, parent, "groups", value.id) else {
        return false;
    };
    write_group_row(doc, &row, value);
    true
}

fn sync_group_list(doc: &mut AutoCommit, parent: &ObjId, values: &[GroupEntry]) {
    let current = read_group_list(doc, parent);
    for value in &current {
        if !values.iter().any(|next| next.id == value.id) {
            let _ = remove_row(doc, parent, "groups", value.id);
        }
    }
    for value in values {
        if current.iter().any(|old| old.id == value.id) {
            let _ = update_group_row(doc, parent, value);
        } else {
            let _ = add_group_row(doc, parent, value);
        }
    }
}

fn read_labeled_list(doc: &AutoCommit, parent: &ObjId, key: &str) -> Vec<Labeled> {
    let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(parent, key) else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|index| match doc.get(&list, index) {
            Ok(Some((Value::Object(ObjType::Map), row))) => Some(Labeled {
                id: row_id(doc, &row)?,
                label: read_str(doc, &row, "label").unwrap_or_default(),
                value: read_str_or_text(doc, &row, "value").unwrap_or_default(),
            }),
            _ => None,
        })
        .collect()
}

fn write_labeled_row(doc: &mut AutoCommit, row: &ObjId, value: &Labeled) {
    ui::doc::put_if_changed(doc, row, "id", value.id.to_string());
    ui::doc::put_if_changed(doc, row, "label", value.label.clone());
    sync_text(doc, row, "value", &value.value);
}

fn add_labeled_row(doc: &mut AutoCommit, parent: &ObjId, key: &str, value: &Labeled) -> bool {
    if list_row(doc, parent, key, value.id).is_some() {
        return false;
    }
    let Some(list) = list_obj(doc, parent, key) else {
        return false;
    };
    let Ok(row) = doc.insert_object(&list, doc.length(&list), ObjType::Map) else {
        return false;
    };
    write_labeled_row(doc, &row, value);
    true
}

fn update_labeled_row(doc: &mut AutoCommit, parent: &ObjId, key: &str, value: &Labeled) -> bool {
    let Some((_, _, row)) = list_row(doc, parent, key, value.id) else {
        return false;
    };
    write_labeled_row(doc, &row, value);
    true
}

fn sync_labeled_list(doc: &mut AutoCommit, parent: &ObjId, key: &str, values: &[Labeled]) {
    let current = read_labeled_list(doc, parent, key);
    for value in &current {
        if !values.iter().any(|next| next.id == value.id) {
            let _ = remove_row(doc, parent, key, value.id);
        }
    }
    for value in values {
        if current.iter().any(|old| old.id == value.id) {
            let _ = update_labeled_row(doc, parent, key, value);
        } else {
            let _ = add_labeled_row(doc, parent, key, value);
        }
    }
}

fn read_address_list(doc: &AutoCommit, parent: &ObjId, key: &str) -> Vec<Address> {
    let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(parent, key) else {
        return Vec::new();
    };
    (0..doc.length(&list))
        .filter_map(|index| match doc.get(&list, index) {
            Ok(Some((Value::Object(ObjType::Map), row))) => Some(Address {
                id: row_id(doc, &row)?,
                label: read_str(doc, &row, "label").unwrap_or_default(),
                street: read_str(doc, &row, "street").unwrap_or_default(),
                city: read_str(doc, &row, "city").unwrap_or_default(),
                region: read_str(doc, &row, "region").unwrap_or_default(),
                postal: read_str(doc, &row, "postal").unwrap_or_default(),
                country: read_str(doc, &row, "country").unwrap_or_default(),
            }),
            _ => None,
        })
        .collect()
}

fn write_address_row(doc: &mut AutoCommit, row: &ObjId, value: &Address) {
    ui::doc::put_if_changed(doc, row, "id", value.id.to_string());
    for (key, field) in [
        ("label", &value.label),
        ("street", &value.street),
        ("city", &value.city),
        ("region", &value.region),
        ("postal", &value.postal),
        ("country", &value.country),
    ] {
        ui::doc::put_if_changed(doc, row, key, field.clone());
    }
}

fn add_address_row(doc: &mut AutoCommit, parent: &ObjId, value: &Address) -> bool {
    if list_row(doc, parent, "addresses", value.id).is_some() {
        return false;
    }
    let Some(list) = list_obj(doc, parent, "addresses") else {
        return false;
    };
    let Ok(row) = doc.insert_object(&list, doc.length(&list), ObjType::Map) else {
        return false;
    };
    write_address_row(doc, &row, value);
    true
}

fn update_address_row(doc: &mut AutoCommit, parent: &ObjId, value: &Address) -> bool {
    let Some((_, _, row)) = list_row(doc, parent, "addresses", value.id) else {
        return false;
    };
    write_address_row(doc, &row, value);
    true
}

fn sync_address_list(doc: &mut AutoCommit, parent: &ObjId, key: &str, values: &[Address]) {
    let current = read_address_list(doc, parent, key);
    for value in &current {
        if !values.iter().any(|next| next.id == value.id) {
            let _ = remove_row(doc, parent, key, value.id);
        }
    }
    for value in values {
        if current.iter().any(|old| old.id == value.id) {
            let _ = update_address_row(doc, parent, value);
        } else {
            let _ = add_address_row(doc, parent, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::sample_contacts;
    use tilo_automerge::ApplyIncoming;

    /// Records upsert in place, list and delete.
    #[test]
    fn contact_records_round_trip() {
        // --- phase: upsert_then_list_round_trips ---
        {
            let mut doc = AutoCommit::new();
            let c = sample_contacts().into_iter().next().unwrap();
            ContactsDoc::upsert_contact(&mut doc, &c);
            let back = ContactsDoc::list_contacts(&doc);
            assert_eq!(back.len(), 1);
            let r = &back[0];
            assert_eq!(r.id, c.id);
            assert_eq!(r.first, c.first);
            assert_eq!(r.last, c.last);
            assert_eq!(r.org, c.org);
            assert_eq!(r.phones, c.phones);
            assert_eq!(r.emails, c.emails);
            assert_eq!(r.addresses, c.addresses);
            assert_eq!(r.groups, c.groups);
            assert_eq!(r.favorite, c.favorite);
            assert_eq!(r.birthday, c.birthday);
        }

        // --- phase: upsert_updates_in_place_no_duplicates ---
        {
            let mut doc = AutoCommit::new();
            let mut c = sample_contacts().into_iter().next().unwrap();
            ContactsDoc::upsert_contact(&mut doc, &c);
            c.title = "Updated Title".into();
            c.phones.push(Labeled::new("home", "+1 555 0000"));
            ContactsDoc::upsert_contact(&mut doc, &c);
            let back = ContactsDoc::list_contacts(&doc);
            assert_eq!(back.len(), 1, "update must not create a second record");
            assert_eq!(back[0].title, "Updated Title");
            assert_eq!(back[0].phones.len(), c.phones.len());
        }

        // --- phase: delete_removes_record ---
        {
            let mut doc = AutoCommit::new();
            let c = sample_contacts().into_iter().next().unwrap();
            ContactsDoc::upsert_contact(&mut doc, &c);
            ContactsDoc::delete_contact(&mut doc, c.id);
            assert!(ContactsDoc::list_contacts(&doc).is_empty());
        }
    }

    /// Two documents merge contacts, and text sync and repeated upserts keep the document bounded.
    #[test]
    fn contact_doc_merges_and_stays_bounded() {
        // --- phase: two_docs_merge_contacts ---
        {
            // Peer A seeds; peer B joins from A's state, both add a contact,
            // and the merged document carries both.
            let mut a = AutoCommit::new();
            let ca = sample_contacts().into_iter().next().unwrap();
            ContactsDoc::upsert_contact(&mut a, &ca);

            let mut b = AutoCommit::load(&a.save()).unwrap();
            let mut cb = Contact::new();
            cb.first = "Grace".into();
            cb.last = "Hopper".into();
            ContactsDoc::upsert_contact(&mut b, &cb);

            let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b_loaded).unwrap();

            let names: Vec<String> = ContactsDoc::list_contacts(&a)
                .into_iter()
                .map(|c| c.display_name())
                .collect();
            assert!(names.iter().any(|n| n.contains("Ada")), "{names:?}");
            assert!(names.iter().any(|n| n.contains("Grace")), "{names:?}");
        }

        // --- phase: sync_text_append_one_char_is_bounded ---
        {
            // Anti-churn on the collaborative text fields: appending one char to a
            // long field must splice only the new char, not rewrite the value.
            let mut doc = AutoCommit::new();
            let long = "a".repeat(300);
            sync_text(&mut doc, &ROOT, "notes", &long);
            let before = doc.save().len();
            let mut longer = long.clone();
            longer.push('!');
            sync_text(&mut doc, &ROOT, "notes", &longer);
            let grew = doc.save().len() - before;
            assert_eq!(
                read_str_or_text(&doc, &ROOT, "notes").unwrap_or_default(),
                longer,
                "value round-trips"
            );
            assert!(grew < 40, "one-char text edit grew doc by {grew} bytes");
        }

        // --- phase: repeated_upsert_keeps_doc_bounded ---
        {
            // Full store-path idempotency: with diff-spliced text, guarded scalar
            // puts, and change-gated list rebuilds, re-upserting an unchanged
            // contact must not grow the document at all.
            let mut doc = AutoCommit::new();
            let c = sample_contacts().into_iter().next().unwrap();
            ContactsDoc::upsert_contact(&mut doc, &c);
            let a = doc.save().len();
            for _ in 0..50 {
                ContactsDoc::upsert_contact(&mut doc, &c);
            }
            let b = doc.save().len();
            assert_eq!(
                a, b,
                "re-upserting an unchanged contact grew the document {a} -> {b}"
            );
        }
    }
}
