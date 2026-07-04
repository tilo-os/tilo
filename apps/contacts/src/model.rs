//! Pure data model for the Contacts app.
//!
//! The in-memory [`ContactStore`] is the working set the UI reads and
//! mutates; it is mirrored into the Automerge document (see
//! [`crate::doc`]) so two peers editing the same address book converge.
//! Nothing here touches the CRDT, the renderer, or the filesystem — it
//! is plain, unit-testable logic.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier for a single contact record. Backs every
/// cross-reference and the CRDT map key.
pub type ContactId = Uuid;

/// A typed, repeatable single-value field — a phone number or an email
/// address. `label` is a free-form type tag (home / work / mobile / …);
/// `value` is the actual number or address.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Labeled {
    pub label: String,
    pub value: String,
}

impl Labeled {
    pub fn new(label: &str, value: &str) -> Self {
        Self { label: label.to_string(), value: value.to_string() }
    }
}

/// A typed postal address. Components map onto the vCard `ADR` structured
/// value (`PO;EXT;STREET;CITY;REGION;POSTAL;COUNTRY`) — we keep the four
/// fields that matter for a daily address book plus a free type label.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    pub label: String,
    pub street: String,
    pub city: String,
    pub region: String,
    pub postal: String,
    pub country: String,
}

impl Address {
    /// A single-line rendering for compact list / summary contexts.
    pub fn one_line(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for p in [&self.street, &self.city, &self.region, &self.postal, &self.country] {
            if !p.is_empty() {
                parts.push(p.as_str());
            }
        }
        parts.join(", ")
    }

    pub fn is_empty(&self) -> bool {
        self.street.is_empty()
            && self.city.is_empty()
            && self.region.is_empty()
            && self.postal.is_empty()
            && self.country.is_empty()
    }
}

/// One person or organization. Text fields are plain strings here; the
/// CRDT layer stores the collaborative ones as Automerge `Text` objects
/// so concurrent edits merge.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Contact {
    pub id: ContactId,
    pub first: String,
    pub last: String,
    pub org: String,
    pub title: String,
    pub website: String,
    /// ISO `YYYY-MM-DD`. Empty when unset.
    pub birthday: String,
    pub notes: String,
    pub phones: Vec<Labeled>,
    pub emails: Vec<Labeled>,
    pub addresses: Vec<Address>,
    pub groups: Vec<String>,
    pub favorite: bool,
}

impl Contact {
    pub fn new() -> Self {
        Self { id: Uuid::new_v4(), ..Default::default() }
    }

    /// Best-effort display name: "First Last", falling back to the
    /// organization, then to a placeholder so the row is never blank.
    pub fn display_name(&self) -> String {
        let full = format!("{} {}", self.first.trim(), self.last.trim());
        let full = full.trim().to_string();
        if !full.is_empty() {
            full
        } else if !self.org.trim().is_empty() {
            self.org.trim().to_string()
        } else {
            "No Name".to_string()
        }
    }

    /// One- or two-letter initials for the avatar chip.
    pub fn initials(&self) -> String {
        let f = self.first.trim().chars().next();
        let l = self.last.trim().chars().next();
        match (f, l) {
            (Some(a), Some(b)) => format!("{a}{b}"),
            (Some(a), None) => a.to_string(),
            (None, Some(b)) => b.to_string(),
            (None, None) => self
                .org
                .trim()
                .chars()
                .next()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".to_string()),
        }
        .to_uppercase()
    }

    /// Secondary line shown under the name in the list: org + title.
    pub fn subtitle(&self) -> String {
        match (self.org.trim().is_empty(), self.title.trim().is_empty()) {
            (false, false) => format!("{} · {}", self.title.trim(), self.org.trim()),
            (false, true) => self.org.trim().to_string(),
            (true, false) => self.title.trim().to_string(),
            (true, true) => String::new(),
        }
    }

    /// True if `query` (already lower-cased) matches anywhere in the
    /// record — name, org, title, any phone / email / address, notes or
    /// group. Powers the cross-field search box.
    pub fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let mut hay = String::new();
        hay.push_str(&self.first);
        hay.push(' ');
        hay.push_str(&self.last);
        hay.push(' ');
        hay.push_str(&self.org);
        hay.push(' ');
        hay.push_str(&self.title);
        hay.push(' ');
        hay.push_str(&self.website);
        hay.push(' ');
        hay.push_str(&self.notes);
        for p in &self.phones {
            hay.push(' ');
            hay.push_str(&p.value);
        }
        for e in &self.emails {
            hay.push(' ');
            hay.push_str(&e.value);
        }
        for a in &self.addresses {
            hay.push(' ');
            hay.push_str(&a.one_line());
        }
        for g in &self.groups {
            hay.push(' ');
            hay.push_str(g);
        }
        hay.to_lowercase().contains(query)
    }
}

/// How the master list is ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    /// Surname, then given name, then display name.
    Name,
    /// Organization, then display name.
    Org,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Name => "Name",
            SortKey::Org => "Org",
        }
    }
}

/// The complete address book held in memory. Order in `contacts` is not
/// significant — every read path sorts on demand.
#[derive(Clone, Debug, Default)]
pub struct ContactStore {
    pub contacts: Vec<Contact>,
}

impl ContactStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: ContactId) -> Option<&Contact> {
        self.contacts.iter().find(|c| c.id == id)
    }

    pub fn get_mut(&mut self, id: ContactId) -> Option<&mut Contact> {
        self.contacts.iter_mut().find(|c| c.id == id)
    }

    /// Insert a new contact or replace the existing one with the same id.
    /// Returns the id for convenience.
    pub fn upsert(&mut self, contact: Contact) -> ContactId {
        let id = contact.id;
        if let Some(existing) = self.contacts.iter_mut().find(|c| c.id == id) {
            *existing = contact;
        } else {
            self.contacts.push(contact);
        }
        id
    }

    /// Remove a contact. Returns true if it existed.
    pub fn delete(&mut self, id: ContactId) -> bool {
        let before = self.contacts.len();
        self.contacts.retain(|c| c.id != id);
        self.contacts.len() != before
    }

    /// Every distinct group across all contacts, sorted alphabetically.
    pub fn all_groups(&self) -> Vec<String> {
        let mut set: Vec<String> = Vec::new();
        for c in &self.contacts {
            for g in &c.groups {
                let g = g.trim();
                if !g.is_empty() && !set.iter().any(|x| x.eq_ignore_ascii_case(g)) {
                    set.push(g.to_string());
                }
            }
        }
        set.sort_by_key(|s| s.to_lowercase());
        set
    }

    /// Ids of the contacts that pass `query` (cross-field) and the
    /// optional `group` filter, ordered by `sort`. This is the single
    /// source of truth for what the list pane renders.
    pub fn filtered_sorted(
        &self,
        query: &str,
        group: Option<&str>,
        sort: SortKey,
    ) -> Vec<ContactId> {
        let q = query.trim().to_lowercase();
        let mut out: Vec<&Contact> = self
            .contacts
            .iter()
            .filter(|c| c.matches(&q))
            .filter(|c| match group {
                None => true,
                Some(g) => c.groups.iter().any(|x| x.eq_ignore_ascii_case(g)),
            })
            .collect();
        out.sort_by(|a, b| sort_cmp(a, b, sort));
        out.into_iter().map(|c| c.id).collect()
    }

    pub fn len(&self) -> usize {
        self.contacts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.contacts.is_empty()
    }
}

/// Case-insensitive comparison used by [`ContactStore::filtered_sorted`].
fn sort_cmp(a: &Contact, b: &Contact, sort: SortKey) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let key = |c: &Contact| -> (String, String) {
        match sort {
            SortKey::Name => (
                c.last.trim().to_lowercase(),
                c.first.trim().to_lowercase(),
            ),
            SortKey::Org => (
                c.org.trim().to_lowercase(),
                c.display_name().to_lowercase(),
            ),
        }
    };
    let (ka1, ka2) = key(a);
    let (kb1, kb2) = key(b);
    // Empty primary keys sort to the bottom so unnamed / org-less rows
    // don't crowd the top of the list.
    match (ka1.is_empty(), kb1.is_empty()) {
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        _ => {}
    }
    match ka1.cmp(&kb1) {
        Ordering::Equal => match ka2.cmp(&kb2) {
            Ordering::Equal => a.display_name().to_lowercase().cmp(&b.display_name().to_lowercase()),
            other => other,
        },
        other => other,
    }
}

/// Seed an empty store with a varied sample address book so a fresh
/// launch shows a populated list and a fully-filled detail pane.
pub fn sample_contacts() -> Vec<Contact> {
    let mk = |first: &str,
              last: &str,
              org: &str,
              title: &str,
              phones: &[(&str, &str)],
              emails: &[(&str, &str)],
              addresses: Vec<Address>,
              birthday: &str,
              website: &str,
              notes: &str,
              groups: &[&str],
              favorite: bool|
     -> Contact {
        Contact {
            id: Uuid::new_v4(),
            first: first.to_string(),
            last: last.to_string(),
            org: org.to_string(),
            title: title.to_string(),
            website: website.to_string(),
            birthday: birthday.to_string(),
            notes: notes.to_string(),
            phones: phones.iter().map(|(l, v)| Labeled::new(l, v)).collect(),
            emails: emails.iter().map(|(l, v)| Labeled::new(l, v)).collect(),
            addresses,
            groups: groups.iter().map(|s| s.to_string()).collect(),
            favorite,
        }
    };

    vec![
        mk(
            "Ada",
            "Lovelace",
            "Analytical Engines",
            "Chief Mathematician",
            &[("mobile", "+44 20 7946 0101"), ("work", "+44 20 7946 0199")],
            &[("work", "ada@analyticalengines.co.uk"), ("home", "ada.lovelace@example.com")],
            vec![Address {
                label: "home".into(),
                street: "12 St James's Square".into(),
                city: "London".into(),
                region: "".into(),
                postal: "SW1Y 4LE".into(),
                country: "United Kingdom".into(),
            }],
            "1815-12-10",
            "https://analyticalengines.co.uk",
            "First programmer. Prefers correspondence by letter. Working on Note G.",
            &["Work", "Favorites"],
            true,
        ),
        mk(
            "Grace",
            "Hopper",
            "US Navy",
            "Rear Admiral",
            &[("work", "+1 202 555 0143")],
            &[("work", "grace.hopper@navy.mil")],
            vec![Address {
                label: "work".into(),
                street: "9620 Maryland Ave".into(),
                city: "Arlington".into(),
                region: "VA".into(),
                postal: "22203".into(),
                country: "USA".into(),
            }],
            "1906-12-09",
            "",
            "Coined the term \"debugging\". Keeps a nanosecond on her desk.",
            &["Work"],
            true,
        ),
        mk(
            "Alan",
            "Turing",
            "Bletchley Park",
            "Cryptanalyst",
            &[("home", "+44 1908 555 0117")],
            &[("home", "alan.turing@example.com")],
            vec![],
            "1912-06-23",
            "https://turing.org.uk",
            "Morning runner. Reach out about the Bombe redesign.",
            &["Friends"],
            false,
        ),
        mk(
            "Katherine",
            "Johnson",
            "NASA",
            "Research Mathematician",
            &[("work", "+1 757 555 0188"), ("mobile", "+1 757 555 0042")],
            &[("work", "k.johnson@nasa.gov")],
            vec![Address {
                label: "work".into(),
                street: "100 NASA Rd".into(),
                city: "Hampton".into(),
                region: "VA".into(),
                postal: "23666".into(),
                country: "USA".into(),
            }],
            "1918-08-26",
            "",
            "Orbital mechanics. Double-checks every trajectory by hand.",
            &["Work", "Favorites"],
            true,
        ),
        mk(
            "Tim",
            "Berners-Lee",
            "W3C",
            "Director",
            &[("mobile", "+41 22 555 0190")],
            &[("work", "timbl@w3.org")],
            vec![],
            "1955-06-08",
            "https://w3.org",
            "Invented the Web. Ask about standards, not implementations.",
            &["Work"],
            false,
        ),
        mk(
            "Radia",
            "Perlman",
            "",
            "Network Engineer",
            &[("mobile", "+1 408 555 0173")],
            &[("home", "radia@example.com")],
            vec![],
            "1951-12-18",
            "",
            "Mother of the spanning tree protocol.",
            &["Friends"],
            false,
        ),
        mk(
            "Margaret",
            "Hamilton",
            "MIT",
            "Lead Software Engineer",
            &[("work", "+1 617 555 0156")],
            &[("work", "mhamilton@mit.edu")],
            vec![Address {
                label: "work".into(),
                street: "77 Massachusetts Ave".into(),
                city: "Cambridge".into(),
                region: "MA".into(),
                postal: "02139".into(),
                country: "USA".into(),
            }],
            "1936-08-17",
            "",
            "Apollo guidance software. Coined \"software engineering\".",
            &["Work"],
            false,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_falls_back_to_org_then_placeholder() {
        let mut c = Contact::new();
        assert_eq!(c.display_name(), "No Name");
        c.org = "ACME".into();
        assert_eq!(c.display_name(), "ACME");
        c.first = "Jane".into();
        c.last = "Doe".into();
        assert_eq!(c.display_name(), "Jane Doe");
    }

    #[test]
    fn initials_uppercase_first_and_last() {
        let mut c = Contact::new();
        c.first = "ada".into();
        c.last = "lovelace".into();
        assert_eq!(c.initials(), "AL");
    }

    #[test]
    fn matches_searches_across_fields() {
        let mut c = Contact::new();
        c.first = "Grace".into();
        c.emails.push(Labeled::new("work", "grace@navy.mil"));
        assert!(c.matches(&"navy".to_lowercase()));
        assert!(c.matches(&"grace".to_lowercase()));
        assert!(!c.matches(&"zzz".to_lowercase()));
    }

    #[test]
    fn filtered_sorted_by_name_and_group() {
        let mut store = ContactStore::new();
        for c in sample_contacts() {
            store.upsert(c);
        }
        // Name sort puts "Berners-Lee" before "Hopper" before "Lovelace".
        let ids = store.filtered_sorted("", None, SortKey::Name);
        let names: Vec<String> =
            ids.iter().map(|id| store.get(*id).unwrap().last.clone()).collect();
        let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
        assert!(pos("Berners-Lee") < pos("Hopper"));
        assert!(pos("Hopper") < pos("Lovelace"));

        // Group filter narrows to Friends only.
        let friends = store.filtered_sorted("", Some("Friends"), SortKey::Name);
        assert!(friends.iter().all(|id| store
            .get(*id)
            .unwrap()
            .groups
            .iter()
            .any(|g| g.eq_ignore_ascii_case("Friends"))));
        assert!(!friends.is_empty());
    }

    #[test]
    fn all_groups_dedupes_case_insensitively() {
        let mut store = ContactStore::new();
        let mut a = Contact::new();
        a.groups = vec!["Work".into(), "work".into(), "Family".into()];
        store.upsert(a);
        let groups = store.all_groups();
        assert_eq!(groups.len(), 2, "Work/work collapse: {groups:?}");
    }

    #[test]
    fn upsert_replaces_in_place() {
        let mut store = ContactStore::new();
        let mut a = Contact::new();
        a.first = "A".into();
        let id = store.upsert(a.clone());
        a.first = "B".into();
        store.upsert(a);
        assert_eq!(store.len(), 1);
        assert_eq!(store.get(id).unwrap().first, "B");
    }
}
