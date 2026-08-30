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
//!         ├── "birthday" (Str)   "favorite" (Bool)
//!         ├── "groups" (List<Str>)
//!         ├── "phones" / "emails" (List<Map{label:Str, value:Text}>)
//!         └── "addresses" (List<Map{label,street,city,region,postal,country:Str}>)
//! ```
//!
//! Collaborative text fields (name, org, notes, …) are `Text` objects so
//! concurrent keystrokes merge; the spliced-diff write keeps churn low.
//! Repeated fields are rebuilt only when their content actually changed.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::{Address, Birthday, Contact, ContactId, Labeled};

/// Stateless namespace for the contacts document helpers.
pub struct ContactsDoc;

impl ContactsDoc {
    /// Get-or-create the top-level `contacts` Map.
    pub fn contacts_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "contacts") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "contacts", ObjType::Map)
                .expect("put_object contacts on root never fails"),
        }
    }

    /// Get-or-create the per-contact Map for `id`.
    fn ensure_contact(doc: &mut AutoCommit, id: ContactId) -> ObjId {
        let contacts = Self::contacts_map(doc);
        let key = id.to_string();
        match doc.get(&contacts, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), cid))) => cid,
            _ => doc
                .put_object(&contacts, key.as_str(), ObjType::Map)
                .expect("put_object contact map never fails"),
        }
    }

    /// Insert or update a contact record, writing only the fields that
    /// actually changed so concurrent edits don't thrash.
    pub fn upsert_contact(doc: &mut AutoCommit, c: &Contact) {
        let m = Self::ensure_contact(doc, c.id);
        sync_text(doc, &m, "first", &c.first);
        sync_text(doc, &m, "last", &c.last);
        sync_text(doc, &m, "org", &c.org);
        sync_text(doc, &m, "title", &c.title);
        sync_text(doc, &m, "website", &c.website);
        sync_text(doc, &m, "notes", &c.notes);
        // Persist the birthday as its canonical string (empty when unset); the
        // structured value is reconstructed by `Birthday::parse` on read.
        ui::doc::put_if_changed(
            doc,
            &m,
            "birthday",
            c.birthday.map(|b| b.iso()).unwrap_or_default(),
        );
        ui::doc::put_if_changed(doc, &m, "favorite", c.favorite);
        sync_str_list(doc, &m, "groups", &c.groups);
        sync_labeled_list(doc, &m, "phones", &c.phones);
        sync_labeled_list(doc, &m, "emails", &c.emails);
        sync_address_list(doc, &m, "addresses", &c.addresses);
    }

    /// Remove a contact from the document.
    pub fn delete_contact(doc: &mut AutoCommit, id: ContactId) {
        let contacts = Self::contacts_map(doc);
        let _ = doc.delete(&contacts, id.to_string().as_str());
    }

    /// Set just the `favorite` flag on an existing contact, leaving every other
    /// field untouched. A no-op when the contact is absent, so a stale replay
    /// can't resurrect a deleted record. The guarded put makes an unchanged
    /// re-store free.
    pub fn set_favorite(doc: &mut AutoCommit, id: ContactId, favorite: bool) {
        let contacts = Self::contacts_map(doc);
        let key = id.to_string();
        if let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&contacts, key.as_str()) {
            ui::doc::put_if_changed(doc, &m, "favorite", favorite);
        }
    }

    /// Read every contact out of the document. Records with an
    /// unparseable id key are skipped.
    pub fn list_contacts(doc: &AutoCommit) -> Vec<Contact> {
        let contacts = match doc.get(&ROOT, "contacts") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return Vec::new(),
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
                groups: read_str_list(doc, &m, "groups"),
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

fn text_obj(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::Text), id))) => id,
        _ => doc
            .put_object(parent, key, ObjType::Text)
            .expect("put_object text field never fails"),
    }
}

fn sync_text(doc: &mut AutoCommit, parent: &ObjId, key: &str, value: &str) {
    let tid = text_obj(doc, parent, key);
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
fn list_obj(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::List), id))) => id,
        _ => doc
            .put_object(parent, key, ObjType::List)
            .expect("put_object list never fails"),
    }
}

/// Delete every element of a list so it can be rebuilt.
fn clear_list(doc: &mut AutoCommit, list: &ObjId) {
    let n = doc.length(list);
    for _ in 0..n {
        let _ = doc.delete(list, 0);
    }
}

fn read_str_list(doc: &AutoCommit, parent: &ObjId, key: &str) -> Vec<String> {
    let list = match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::List), id))) => id,
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for i in 0..doc.length(&list) {
        if let Ok(Some((Value::Scalar(v), _))) = doc.get(&list, i)
            && let ScalarValue::Str(s) = v.as_ref()
        {
            out.push(s.to_string());
        }
    }
    out
}

fn sync_str_list(doc: &mut AutoCommit, parent: &ObjId, key: &str, values: &[String]) {
    if read_str_list(doc, parent, key) == values {
        return;
    }
    let list = list_obj(doc, parent, key);
    clear_list(doc, &list);
    for (i, v) in values.iter().enumerate() {
        let _ = doc.insert(&list, i, v.clone());
    }
}

fn read_labeled_list(doc: &AutoCommit, parent: &ObjId, key: &str) -> Vec<Labeled> {
    let list = match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::List), id))) => id,
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for i in 0..doc.length(&list) {
        if let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) {
            out.push(Labeled {
                label: read_str(doc, &m, "label"),
                value: read_text(doc, &m, "value"),
            });
        }
    }
    out
}

fn sync_labeled_list(doc: &mut AutoCommit, parent: &ObjId, key: &str, values: &[Labeled]) {
    if read_labeled_list(doc, parent, key) == values {
        return;
    }
    let list = list_obj(doc, parent, key);
    clear_list(doc, &list);
    for (i, v) in values.iter().enumerate() {
        if let Ok(m) = doc.insert_object(&list, i, ObjType::Map) {
            let _ = doc.put(&m, "label", v.label.clone());
            let vid = doc
                .put_object(&m, "value", ObjType::Text)
                .expect("put value text never fails");
            if !v.value.is_empty() {
                let _ = doc.splice_text(&vid, 0, 0, &v.value);
            }
        }
    }
}

fn read_address_list(doc: &AutoCommit, parent: &ObjId, key: &str) -> Vec<Address> {
    let list = match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::List), id))) => id,
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for i in 0..doc.length(&list) {
        if let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) {
            out.push(Address {
                label: read_str(doc, &m, "label"),
                street: read_str(doc, &m, "street"),
                city: read_str(doc, &m, "city"),
                region: read_str(doc, &m, "region"),
                postal: read_str(doc, &m, "postal"),
                country: read_str(doc, &m, "country"),
            });
        }
    }
    out
}

fn sync_address_list(doc: &mut AutoCommit, parent: &ObjId, key: &str, values: &[Address]) {
    if read_address_list(doc, parent, key) == values {
        return;
    }
    let list = list_obj(doc, parent, key);
    clear_list(doc, &list);
    for (i, a) in values.iter().enumerate() {
        if let Ok(m) = doc.insert_object(&list, i, ObjType::Map) {
            let _ = doc.put(&m, "label", a.label.clone());
            let _ = doc.put(&m, "street", a.street.clone());
            let _ = doc.put(&m, "city", a.city.clone());
            let _ = doc.put(&m, "region", a.region.clone());
            let _ = doc.put(&m, "postal", a.postal.clone());
            let _ = doc.put(&m, "country", a.country.clone());
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
}
