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
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

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
            .filter(|contact| !read_bool(doc, contact, "deleted"))
    }

    /// Whether an action may still address this contact. Kept read-only so a
    /// patch can validate every row target before it makes its first write.
    pub fn contact_is_active(doc: &AutoCommit, id: ContactId) -> bool {
        Self::active_contact(doc, id).is_some()
    }

    /// Whether a group entry id exists on an active contact. This also
    /// recognizes the deterministic id assigned to an unmigrated legacy
    /// string row, so validation and migration agree exactly.
    pub fn group_row_exists(doc: &AutoCommit, id: ContactId, entry_id: Uuid) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        if list_row(doc, &contact, id, "groups", entry_id, group_fingerprint).is_some() {
            return true;
        }
        let Some(list) = list_obj_read(doc, &contact, "groups") else {
            return false;
        };
        (0..doc.length(&list)).any(|index| {
            let Some((Value::Scalar(scalar), _)) = doc.get(&list, index).ok().flatten() else {
                return false;
            };
            let ScalarValue::Str(value) = scalar.as_ref() else {
                return false;
            };
            legacy_row_id(id, "groups", index, value) == entry_id
        })
    }

    /// Whether a phone/email row id exists on an active contact.
    pub fn labeled_row_exists(doc: &AutoCommit, id: ContactId, key: &str, entry_id: Uuid) -> bool {
        Self::active_contact(doc, id).is_some_and(|contact| {
            list_row(doc, &contact, id, key, entry_id, labeled_fingerprint).is_some()
        })
    }

    /// Whether an address row id exists on an active contact.
    pub fn address_row_exists(doc: &AutoCommit, id: ContactId, entry_id: Uuid) -> bool {
        Self::active_contact(doc, id).is_some_and(|contact| {
            list_row(
                doc,
                &contact,
                id,
                "addresses",
                entry_id,
                address_fingerprint,
            )
            .is_some()
        })
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
        sync_group_list(doc, m, c.id, &c.groups);
        sync_labeled_list(doc, m, c.id, "phones", &c.phones);
        sync_labeled_list(doc, m, c.id, "emails", &c.emails);
        sync_address_list(doc, m, c.id, "addresses", &c.addresses);
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
        add_group_row(doc, &contact, id, value)
    }

    /// Update exactly one group row by identity.
    pub fn update_group(doc: &mut AutoCommit, id: ContactId, value: &GroupEntry) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        update_group_row(doc, &contact, id, value)
    }

    /// Remove exactly one group row by identity.
    pub fn remove_group(doc: &mut AutoCommit, id: ContactId, entry_id: Uuid) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        if remove_row(doc, &contact, id, "groups", entry_id, group_fingerprint) {
            return true;
        }
        let Some(list) = list_obj(doc, &contact, "groups") else {
            return false;
        };
        for index in 0..doc.length(&list) {
            let Some((Value::Scalar(scalar), _)) = doc.get(&list, index).ok().flatten() else {
                continue;
            };
            let ScalarValue::Str(value) = scalar.as_ref() else {
                continue;
            };
            if legacy_row_id(id, "groups", index, value) == entry_id {
                return doc.delete(&list, index).is_ok();
            }
        }
        false
    }

    /// Add a fresh phone/email row by durable entry identity.
    pub fn add_labeled(doc: &mut AutoCommit, id: ContactId, key: &str, value: &Labeled) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        add_labeled_row(doc, &contact, id, key, value)
    }

    /// Update exactly one phone/email row by durable entry identity.
    pub fn update_labeled(doc: &mut AutoCommit, id: ContactId, key: &str, value: &Labeled) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        update_labeled_row(doc, &contact, id, key, value)
    }

    /// Remove exactly one phone/email row by durable entry identity.
    pub fn remove_labeled(doc: &mut AutoCommit, id: ContactId, key: &str, entry_id: Uuid) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        remove_row(doc, &contact, id, key, entry_id, labeled_fingerprint)
    }

    /// Add a fresh postal-address row by durable entry identity.
    pub fn add_address(doc: &mut AutoCommit, id: ContactId, value: &Address) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        add_address_row(doc, &contact, id, value)
    }

    /// Update exactly one postal-address row by durable entry identity.
    pub fn update_address(doc: &mut AutoCommit, id: ContactId, value: &Address) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        update_address_row(doc, &contact, id, value)
    }

    /// Remove exactly one postal-address row by durable entry identity.
    pub fn remove_address(doc: &mut AutoCommit, id: ContactId, entry_id: Uuid) -> bool {
        let Some(contact) = Self::active_contact(doc, id) else {
            return false;
        };
        remove_row(
            doc,
            &contact,
            id,
            "addresses",
            entry_id,
            address_fingerprint,
        )
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
            if read_bool(doc, &m, "deleted") {
                continue;
            }
            out.push(Contact {
                id,
                first: read_text(doc, &m, "first"),
                last: read_text(doc, &m, "last"),
                org: read_text(doc, &m, "org"),
                title: read_text(doc, &m, "title"),
                website: read_text(doc, &m, "website"),
                notes: read_text(doc, &m, "notes"),
                birthday: Birthday::parse(&read_str(doc, &m, "birthday")),
                favorite: read_bool(doc, &m, "favorite"),
                groups: read_group_list(doc, &m, id),
                phones: read_labeled_list(doc, &m, id, "phones"),
                emails: read_labeled_list(doc, &m, id, "emails"),
                addresses: read_address_list(doc, &m, id, "addresses"),
            });
        }
        out
    }

    /// Persist canonical row maps for legacy repeated fields without changing
    /// their values. Reads expose deterministic IDs before this runs; the app
    /// calls it during synchronization so later actions never depend on a
    /// shifting legacy list index.
    pub fn migrate_legacy_rows(doc: &mut AutoCommit) {
        for contact in Self::list_contacts(doc) {
            let Some(row) = Self::active_contact(doc, contact.id) else {
                continue;
            };
            sync_group_list(doc, &row, contact.id, &contact.groups);
            sync_labeled_list(doc, &row, contact.id, "phones", &contact.phones);
            sync_labeled_list(doc, &row, contact.id, "emails", &contact.emails);
            sync_address_list(doc, &row, contact.id, "addresses", &contact.addresses);
        }
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

fn read_text(doc: &AutoCommit, parent: &ObjId, key: &str) -> String {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::Text), id))) => doc.text(&id).unwrap_or_default(),
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Str(s) => s.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

fn read_str(doc: &AutoCommit, parent: &ObjId, key: &str) -> String {
    match doc.get(parent, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Str(s) => s.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

fn read_bool(doc: &AutoCommit, parent: &ObjId, key: &str) -> bool {
    match doc.get(parent, key) {
        Ok(Some((Value::Scalar(v), _))) => matches!(v.as_ref(), ScalarValue::Boolean(true)),
        _ => false,
    }
}

/// Get-or-create a List object at `parent[key]`.
fn list_obj(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
        _ => doc.put_object(parent, key, ObjType::List).ok(),
    }
}

fn list_obj_read(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    doc.get(parent, key)
        .ok()
        .flatten()
        .and_then(|(value, object)| matches!(value, Value::Object(ObjType::List)).then_some(object))
}

// Old documents represented groups as strings and the other repeated fields
// as maps without an `id`. Their identity is derived deterministically from
// the immutable contact id, field kind, position, and value. This produces the
// same row identity on every peer until the next write persists the UUID.
fn legacy_row_id(contact: ContactId, key: &str, index: usize, fingerprint: &str) -> Uuid {
    let mut first = 0xcbf2_9ce4_8422_2325_u64;
    let mut second = 0x8422_2325_cbf2_9ce4_u64;
    for byte in format!("{contact}:{key}:{index}:{fingerprint}").bytes() {
        first = (first ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        second = (second ^ u64::from(byte)).wrapping_mul(0x9e37_79b1_85eb_ca87);
    }
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&first.to_be_bytes());
    bytes[8..].copy_from_slice(&second.to_be_bytes());
    // Mark this deterministic, non-nil UUID as RFC 4122 variant/version 5.
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn row_id(
    doc: &AutoCommit,
    row: &ObjId,
    contact: ContactId,
    key: &str,
    index: usize,
    fingerprint: &str,
) -> Uuid {
    read_str(doc, row, "id")
        .parse()
        .ok()
        .filter(|id: &Uuid| !id.is_nil())
        .unwrap_or_else(|| legacy_row_id(contact, key, index, fingerprint))
}

fn group_fingerprint(doc: &AutoCommit, row: &ObjId) -> String {
    read_str(doc, row, "value")
}

fn labeled_fingerprint(doc: &AutoCommit, row: &ObjId) -> String {
    format!(
        "{}\u{0}{}",
        read_str(doc, row, "label"),
        read_text(doc, row, "value")
    )
}

fn address_fingerprint(doc: &AutoCommit, row: &ObjId) -> String {
    ["label", "street", "city", "region", "postal", "country"]
        .into_iter()
        .map(|key| read_str(doc, row, key))
        .collect::<Vec<_>>()
        .join("\u{0}")
}

fn list_row(
    doc: &AutoCommit,
    parent: &ObjId,
    contact: ContactId,
    key: &str,
    entry_id: Uuid,
    fingerprint: fn(&AutoCommit, &ObjId) -> String,
) -> Option<(ObjId, usize, ObjId)> {
    let (Value::Object(ObjType::List), list) = doc.get(parent, key).ok().flatten()? else {
        return None;
    };
    for index in 0..doc.length(&list) {
        let Some((Value::Object(ObjType::Map), row)) = doc.get(&list, index).ok().flatten() else {
            continue;
        };
        if row_id(doc, &row, contact, key, index, &fingerprint(doc, &row)) == entry_id {
            return Some((list, index, row));
        }
    }
    None
}

fn remove_row(
    doc: &mut AutoCommit,
    parent: &ObjId,
    contact: ContactId,
    key: &str,
    entry_id: Uuid,
    fingerprint: fn(&AutoCommit, &ObjId) -> String,
) -> bool {
    list_row(doc, parent, contact, key, entry_id, fingerprint)
        .is_some_and(|(list, index, _)| doc.delete(&list, index).is_ok())
}

fn read_group_list(doc: &AutoCommit, parent: &ObjId, contact: ContactId) -> Vec<GroupEntry> {
    let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(parent, "groups") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for index in 0..doc.length(&list) {
        match doc.get(&list, index).ok().flatten() {
            Some((Value::Object(ObjType::Map), row)) => {
                let value = read_str(doc, &row, "value");
                out.push(GroupEntry {
                    id: row_id(doc, &row, contact, "groups", index, &value),
                    value,
                });
            }
            Some((Value::Scalar(value), _)) => {
                if let ScalarValue::Str(value) = value.as_ref() {
                    out.push(GroupEntry {
                        id: legacy_row_id(contact, "groups", index, value),
                        value: value.to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

fn write_group_row(doc: &mut AutoCommit, row: &ObjId, value: &GroupEntry) {
    ui::doc::put_if_changed(doc, row, "id", value.id.to_string());
    ui::doc::put_if_changed(doc, row, "value", value.value.clone());
}

fn add_group_row(
    doc: &mut AutoCommit,
    parent: &ObjId,
    contact: ContactId,
    value: &GroupEntry,
) -> bool {
    if list_row(doc, parent, contact, "groups", value.id, group_fingerprint).is_some() {
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

fn update_group_row(
    doc: &mut AutoCommit,
    parent: &ObjId,
    contact: ContactId,
    value: &GroupEntry,
) -> bool {
    if let Some((_, _, row)) = list_row(doc, parent, contact, "groups", value.id, group_fingerprint)
    {
        write_group_row(doc, &row, value);
        return true;
    }
    // A legacy group was a scalar list item. Locate it using the same
    // deterministic id exposed to readers, then replace only that one item
    // with its canonical map representation.
    let Some(list) = list_obj(doc, parent, "groups") else {
        return false;
    };
    for index in 0..doc.length(&list) {
        let Some((Value::Scalar(scalar), _)) = doc.get(&list, index).ok().flatten() else {
            continue;
        };
        let ScalarValue::Str(text) = scalar.as_ref() else {
            continue;
        };
        if legacy_row_id(contact, "groups", index, text) == value.id {
            if doc.delete(&list, index).is_err() {
                return false;
            }
            let Ok(row) = doc.insert_object(&list, doc.length(&list), ObjType::Map) else {
                return false;
            };
            write_group_row(doc, &row, value);
            return true;
        }
    }
    false
}

fn sync_group_list(
    doc: &mut AutoCommit,
    parent: &ObjId,
    contact: ContactId,
    values: &[GroupEntry],
) {
    let current = read_group_list(doc, parent, contact);
    for value in &current {
        if !values.iter().any(|next| next.id == value.id) {
            let _ = remove_row(doc, parent, contact, "groups", value.id, group_fingerprint);
        }
    }
    for value in values {
        if current.iter().any(|old| old.id == value.id) {
            let _ = update_group_row(doc, parent, contact, value);
        } else {
            let _ = add_group_row(doc, parent, contact, value);
        }
    }
}

fn read_labeled_list(
    doc: &AutoCommit,
    parent: &ObjId,
    contact: ContactId,
    key: &str,
) -> Vec<Labeled> {
    let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(parent, key) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for index in 0..doc.length(&list) {
        if let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&list, index) {
            let label = read_str(doc, &row, "label");
            let value = read_text(doc, &row, "value");
            let fingerprint = format!("{label}\u{0}{value}");
            out.push(Labeled {
                id: row_id(doc, &row, contact, key, index, &fingerprint),
                label,
                value,
            });
        }
    }
    out
}

fn write_labeled_row(doc: &mut AutoCommit, row: &ObjId, value: &Labeled) {
    ui::doc::put_if_changed(doc, row, "id", value.id.to_string());
    ui::doc::put_if_changed(doc, row, "label", value.label.clone());
    sync_text(doc, row, "value", &value.value);
}

fn add_labeled_row(
    doc: &mut AutoCommit,
    parent: &ObjId,
    contact: ContactId,
    key: &str,
    value: &Labeled,
) -> bool {
    if list_row(doc, parent, contact, key, value.id, labeled_fingerprint).is_some() {
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

fn update_labeled_row(
    doc: &mut AutoCommit,
    parent: &ObjId,
    contact: ContactId,
    key: &str,
    value: &Labeled,
) -> bool {
    let Some((_, _, row)) = list_row(doc, parent, contact, key, value.id, labeled_fingerprint)
    else {
        return false;
    };
    write_labeled_row(doc, &row, value);
    true
}

fn sync_labeled_list(
    doc: &mut AutoCommit,
    parent: &ObjId,
    contact: ContactId,
    key: &str,
    values: &[Labeled],
) {
    let current = read_labeled_list(doc, parent, contact, key);
    for value in &current {
        if !values.iter().any(|next| next.id == value.id) {
            let _ = remove_row(doc, parent, contact, key, value.id, labeled_fingerprint);
        }
    }
    for value in values {
        if current.iter().any(|old| old.id == value.id) {
            let _ = update_labeled_row(doc, parent, contact, key, value);
        } else {
            let _ = add_labeled_row(doc, parent, contact, key, value);
        }
    }
}

fn read_address_list(
    doc: &AutoCommit,
    parent: &ObjId,
    contact: ContactId,
    key: &str,
) -> Vec<Address> {
    let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(parent, key) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for index in 0..doc.length(&list) {
        if let Ok(Some((Value::Object(ObjType::Map), row))) = doc.get(&list, index) {
            let address = Address {
                id: Uuid::nil(),
                label: read_str(doc, &row, "label"),
                street: read_str(doc, &row, "street"),
                city: read_str(doc, &row, "city"),
                region: read_str(doc, &row, "region"),
                postal: read_str(doc, &row, "postal"),
                country: read_str(doc, &row, "country"),
            };
            out.push(Address {
                id: row_id(
                    doc,
                    &row,
                    contact,
                    key,
                    index,
                    &address_fingerprint(doc, &row),
                ),
                ..address
            });
        }
    }
    out
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

fn add_address_row(
    doc: &mut AutoCommit,
    parent: &ObjId,
    contact: ContactId,
    value: &Address,
) -> bool {
    if list_row(
        doc,
        parent,
        contact,
        "addresses",
        value.id,
        address_fingerprint,
    )
    .is_some()
    {
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

fn update_address_row(
    doc: &mut AutoCommit,
    parent: &ObjId,
    contact: ContactId,
    value: &Address,
) -> bool {
    let Some((_, _, row)) = list_row(
        doc,
        parent,
        contact,
        "addresses",
        value.id,
        address_fingerprint,
    ) else {
        return false;
    };
    write_address_row(doc, &row, value);
    true
}

fn sync_address_list(
    doc: &mut AutoCommit,
    parent: &ObjId,
    contact: ContactId,
    key: &str,
    values: &[Address],
) {
    let current = read_address_list(doc, parent, contact, key);
    for value in &current {
        if !values.iter().any(|next| next.id == value.id) {
            let _ = remove_row(doc, parent, contact, key, value.id, address_fingerprint);
        }
    }
    for value in values {
        if current.iter().any(|old| old.id == value.id) {
            let _ = update_address_row(doc, parent, contact, value);
        } else {
            let _ = add_address_row(doc, parent, contact, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::sample_contacts;

    #[test]
    fn upsert_then_list_round_trips() {
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

    #[test]
    fn upsert_updates_in_place_no_duplicates() {
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

    #[test]
    fn delete_removes_record() {
        let mut doc = AutoCommit::new();
        let c = sample_contacts().into_iter().next().unwrap();
        ContactsDoc::upsert_contact(&mut doc, &c);
        ContactsDoc::delete_contact(&mut doc, c.id);
        assert!(ContactsDoc::list_contacts(&doc).is_empty());
    }

    #[test]
    fn two_docs_merge_contacts() {
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
        a.merge(&mut b_loaded).unwrap();

        let names: Vec<String> = ContactsDoc::list_contacts(&a)
            .into_iter()
            .map(|c| c.display_name())
            .collect();
        assert!(names.iter().any(|n| n.contains("Ada")), "{names:?}");
        assert!(names.iter().any(|n| n.contains("Grace")), "{names:?}");
    }

    #[test]
    fn sync_text_append_one_char_is_bounded() {
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
        assert_eq!(read_text(&doc, &ROOT, "notes"), longer, "value round-trips");
        assert!(grew < 40, "one-char text edit grew doc by {grew} bytes");
    }

    #[test]
    fn repeated_upsert_keeps_doc_bounded() {
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

    #[test]
    fn legacy_rows_get_deterministic_ids_then_migrate_to_persisted_maps() {
        let mut doc = AutoCommit::new();
        let mut contact = Contact::new();
        contact.first = "Legacy".into();
        ContactsDoc::upsert_contact(&mut doc, &contact);

        let contacts = doc.get(&ROOT, "contacts").unwrap().unwrap().1;
        let row = doc
            .get(&contacts, contact.id.to_string().as_str())
            .unwrap()
            .unwrap()
            .1;
        let groups = doc.put_object(&row, "groups", ObjType::List).unwrap();
        doc.insert(&groups, 0, "Work").unwrap();

        let first_read = ContactsDoc::list_contacts(&doc).remove(0);
        let mut peer = AutoCommit::load(&doc.save()).unwrap();
        let second_read = ContactsDoc::list_contacts(&peer).remove(0);
        assert_eq!(first_read.groups[0].id, second_read.groups[0].id);
        assert_eq!(first_read.groups[0].value, "Work");

        ContactsDoc::upsert_contact(&mut peer, &second_read);
        let persisted_list = peer.get(&row, "groups").unwrap().unwrap().1;
        let persisted_row = peer.get(&persisted_list, 0).unwrap().unwrap();
        assert!(matches!(persisted_row.0, Value::Object(ObjType::Map)));
        let persisted_id = read_str(&peer, &persisted_row.1, "id");
        assert_eq!(persisted_id, second_read.groups[0].id.to_string());
    }
}
