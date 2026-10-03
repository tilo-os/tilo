//! Pure data model for the Contacts app.
//!
//! The in-memory [`ContactStore`] is the working set the UI reads and
//! mutates; it is mirrored into the Automerge document (see
//! [`crate::doc`]) so two peers editing the same address book converge.
//! Nothing here touches the CRDT, the renderer, or the filesystem — it
//! is plain, unit-testable logic.

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

/// Canonical, Unicode-safe text projection used for search, group matching,
/// and deterministic list ordering. NFC first makes canonically equivalent
/// user input compare alike; lowercasing is deliberately scalar-based, never
/// byte-indexed, so names in every script remain valid UTF-8 throughout.
#[must_use]
pub fn normalize_text(value: &str) -> String {
    value.nfc().flat_map(char::to_lowercase).collect()
}

/// Whether two user-visible group names designate the same group.
#[must_use]
pub fn group_names_equal(left: &str, right: &str) -> bool {
    normalize_text(left.trim()) == normalize_text(right.trim())
}

/// Stable identifier for a single contact record. Backs every
/// cross-reference and the CRDT map key.
pub type ContactId = Uuid;

/// A typed, repeatable single-value field — a phone number or an email
/// address. `label` is a free-form type tag (home / work / mobile / …);
/// `value` is the actual number or address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Labeled {
    pub id: Uuid,
    pub label: String,
    pub value: String,
}

impl Labeled {
    #[must_use]
    pub fn new(label: &str, value: &str) -> Self {
        Self {
            id: Uuid::new_v4(),
            label: label.to_string(),
            value: value.to_string(),
        }
    }
}

impl Default for Labeled {
    fn default() -> Self {
        Self::new("", "")
    }
}

/// A typed postal address. Components map onto the vCard `ADR` structured
/// value (`PO;EXT;STREET;CITY;REGION;POSTAL;COUNTRY`) — we keep the four
/// fields that matter for a daily address book plus a free type label.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    pub id: Uuid,
    pub label: String,
    pub street: String,
    pub city: String,
    pub region: String,
    pub postal: String,
    pub country: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupEntry {
    pub id: Uuid,
    pub value: String,
}

impl GroupEntry {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            value: value.into(),
        }
    }
}

impl Default for Address {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            label: String::new(),
            street: String::new(),
            city: String::new(),
            region: String::new(),
            postal: String::new(),
            country: String::new(),
        }
    }
}

impl Address {
    /// A single-line rendering for compact list / summary contexts.
    pub fn one_line(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for p in [
            &self.street,
            &self.city,
            &self.region,
            &self.postal,
            &self.country,
        ] {
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

/// A birthday as a real calendar value — not a free-text string — so it can
/// be surfaced in Calendar and reasoned about by day/month/year. Either a
/// full date, or a month/day with the year unknown (people often record a
/// birthday without the year; the vCard spec models this as `--MMDD`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Birthday {
    Full { year: i32, month: u32, day: u32 },
    MonthDay { month: u32, day: u32 },
}

impl Birthday {
    /// The year, or `None` for a year-less birthday.
    pub fn year(&self) -> Option<i32> {
        match self {
            Birthday::Full { year, .. } => Some(*year),
            Birthday::MonthDay { .. } => None,
        }
    }

    /// Month, `1..=12`.
    pub fn month(&self) -> u32 {
        match self {
            Birthday::Full { month, .. } | Birthday::MonthDay { month, .. } => *month,
        }
    }

    /// Day of month, `1..=31`.
    pub fn day(&self) -> u32 {
        match self {
            Birthday::Full { day, .. } | Birthday::MonthDay { day, .. } => *day,
        }
    }

    /// Build a full date, validating the components against the calendar.
    fn full(year: i32, month: u32, day: u32) -> Option<Birthday> {
        if (1..=12).contains(&month) && (1..=crate::date::days_in_month(year, month)).contains(&day)
        {
            Some(Birthday::Full { year, month, day })
        } else {
            None
        }
    }

    /// Build a year-less date. Validated against a leap year so `--02-29`
    /// (a Feb-29 birthday whose year is unknown) is accepted.
    fn month_day(month: u32, day: u32) -> Option<Birthday> {
        if (1..=12).contains(&month) && (1..=crate::date::days_in_month(2000, month)).contains(&day)
        {
            Some(Birthday::MonthDay { month, day })
        } else {
            None
        }
    }

    /// Parse typed input or a vCard `BDAY` value into a structured date.
    /// Accepts:
    /// * ISO `YYYY-MM-DD` (and `YYYY/MM/DD`, `YYYY.MM.DD`), basic `YYYYMMDD`
    /// * US `MM/DD/YYYY`, European `DD.MM.YYYY`
    /// * year-less `MM-DD` / `MM/DD` (US), `DD.MM` (EU), and the vCard
    ///   truncated forms `--MMDD` / `--MM-DD`
    ///
    /// Returns `None` for empty or unrecognizable input. Where the day/month
    /// order is ambiguous, a swap that yields a valid date is accepted (so
    /// `25/12/1990` reads as 25 December).
    pub fn parse(s: &str) -> Option<Birthday> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }

        // vCard truncated "year unknown" forms: --MMDD or --MM-DD.
        if let Some(rest) = s.strip_prefix("--") {
            let digits: String = rest.chars().filter(char::is_ascii_digit).collect();
            if digits.len() == 4 {
                let month = digits.get(0..2)?.parse().ok()?;
                let day = digits.get(2..4)?.parse().ok()?;
                return Self::month_day(month, day);
            }
            return None;
        }

        // Basic 8-digit ISO with no separators (vCard 4.0): YYYYMMDD.
        if s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()) {
            let year = s.get(0..4)?.parse().ok()?;
            let month = s.get(4..6)?.parse().ok()?;
            let day = s.get(6..8)?.parse().ok()?;
            return Self::full(year, month, day);
        }

        let sep = s.chars().find(|c| matches!(c, '-' | '/' | '.'))?;
        let parts: Vec<&str> = s
            .split(sep)
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        match parts.as_slice() {
            [a, b, c] => {
                let (a, b, c): (i64, i64, i64) =
                    (a.parse().ok()?, b.parse().ok()?, c.parse().ok()?);
                if a >= 1000 {
                    // Year first: Y-M-D.
                    Self::full(a as i32, u32::try_from(b).ok()?, u32::try_from(c).ok()?)
                } else if c >= 1000 {
                    // Year last. `.` is the European D.M.Y convention; `/` / `-`
                    // read US-style M/D/Y. Fall back to a swap when the first
                    // reading is out of range.
                    let (month, day) = if sep == '.' { (b, a) } else { (a, b) };
                    let (month, day) = (u32::try_from(month).ok()?, u32::try_from(day).ok()?);
                    Self::full(c as i32, month, day).or_else(|| Self::full(c as i32, day, month))
                } else {
                    None
                }
            }
            [a, b] => {
                let (a, b): (i64, i64) = (a.parse().ok()?, b.parse().ok()?);
                // Year-less. `.` is European D.M; otherwise US M/D.
                let (month, day) = if sep == '.' { (b, a) } else { (a, b) };
                let (month, day) = (u32::try_from(month).ok()?, u32::try_from(day).ok()?);
                Self::month_day(month, day).or_else(|| Self::month_day(day, month))
            }
            _ => None,
        }
    }

    /// Canonical string for persistence and as a display fallback:
    /// `YYYY-MM-DD` for a full date, `--MM-DD` for a year-less one. Round-trips
    /// through [`parse`](Self::parse).
    pub fn iso(&self) -> String {
        match self {
            Birthday::Full { year, month, day } => format!("{year:04}-{month:02}-{day:02}"),
            Birthday::MonthDay { month, day } => format!("--{month:02}-{day:02}"),
        }
    }

    /// Human-readable form, e.g. `December 10, 1815`, or `December 10` when the
    /// year is unknown.
    pub fn pretty(&self) -> String {
        let name = crate::date::month_name(self.month());
        match self {
            Birthday::Full { year, day, .. } => format!("{name} {day}, {year}"),
            Birthday::MonthDay { day, .. } => format!("{name} {day}"),
        }
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
    /// Structured birthday with day/month/year accessible (so Calendar can
    /// surface it). `None` when unset. Round-trips through the vCard `BDAY`
    /// field; a year-less birthday is kept as [`Birthday::MonthDay`].
    pub birthday: Option<Birthday>,
    pub notes: String,
    pub phones: Vec<Labeled>,
    pub emails: Vec<Labeled>,
    pub addresses: Vec<Address>,
    pub groups: Vec<GroupEntry>,
    pub favorite: bool,
}

impl Contact {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            ..Default::default()
        }
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
                .map_or_else(|| "?".to_string(), |c| c.to_string()),
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

    /// True if `query` matches anywhere in the
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
        if let Some(birthday) = self.birthday {
            hay.push_str(&birthday.iso());
            hay.push(' ');
            hay.push_str(&birthday.pretty());
            hay.push(' ');
        }
        hay.push_str(&self.notes);
        for p in &self.phones {
            hay.push(' ');
            hay.push_str(&p.label);
            hay.push(' ');
            hay.push_str(&p.value);
        }
        for e in &self.emails {
            hay.push(' ');
            hay.push_str(&e.label);
            hay.push(' ');
            hay.push_str(&e.value);
        }
        for a in &self.addresses {
            hay.push(' ');
            hay.push_str(&a.label);
            hay.push(' ');
            hay.push_str(&a.street);
            hay.push(' ');
            hay.push_str(&a.city);
            hay.push(' ');
            hay.push_str(&a.region);
            hay.push(' ');
            hay.push_str(&a.postal);
            hay.push(' ');
            hay.push_str(&a.country);
            hay.push(' ');
            hay.push_str(&a.one_line());
        }
        for g in &self.groups {
            hay.push(' ');
            hay.push_str(&g.value);
        }
        normalize_text(&hay).contains(&normalize_text(query))
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
                let g = g.value.trim();
                if g.is_empty() {
                    continue;
                }
                if let Some(existing) = set.iter_mut().find(|x| group_names_equal(x, g)) {
                    // CRDT/map arrival order is not a presentation contract.
                    // Pick one deterministic spelling for equivalent Unicode
                    // group names so separate peers expose the same filter.
                    if g < existing.as_str() {
                        *existing = g.to_string();
                    }
                } else {
                    set.push(g.to_string());
                }
            }
        }
        set.sort_by(|left, right| {
            normalize_text(left)
                .cmp(&normalize_text(right))
                .then_with(|| left.cmp(right))
        });
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
        let q = normalize_text(query.trim());
        let mut out: Vec<&Contact> = self
            .contacts
            .iter()
            .filter(|c| c.matches(&q))
            .filter(|c| match group {
                None => true,
                Some(g) => c.groups.iter().any(|x| group_names_equal(&x.value, g)),
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
pub(crate) fn sort_cmp(a: &Contact, b: &Contact, sort: SortKey) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let key = |c: &Contact| -> (String, String) {
        match sort {
            SortKey::Name => (
                normalize_text(c.last.trim()),
                normalize_text(c.first.trim()),
            ),
            SortKey::Org => (
                normalize_text(c.org.trim()),
                normalize_text(&c.display_name()),
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
            Ordering::Equal => normalize_text(&a.display_name())
                .cmp(&normalize_text(&b.display_name()))
                .then_with(|| a.id.cmp(&b.id)),
            other => other,
        },
        other => other,
    }
}

/// Named-field bundle for [`make_sample`] — keeps each seed contact's call
/// site self-describing instead of a wall of positional strings.
struct SampleSpec<'a> {
    first: &'a str,
    last: &'a str,
    org: &'a str,
    title: &'a str,
    phones: &'a [(&'a str, &'a str)],
    emails: &'a [(&'a str, &'a str)],
    addresses: Vec<Address>,
    birthday: &'a str,
    website: &'a str,
    notes: &'a str,
    groups: &'a [&'a str],
    favorite: bool,
}

/// Build one fully-populated seed [`Contact`] from a [`SampleSpec`].
fn make_sample(spec: SampleSpec<'_>) -> Contact {
    Contact {
        id: Uuid::new_v4(),
        first: spec.first.to_string(),
        last: spec.last.to_string(),
        org: spec.org.to_string(),
        title: spec.title.to_string(),
        website: spec.website.to_string(),
        birthday: Birthday::parse(spec.birthday),
        notes: spec.notes.to_string(),
        phones: spec
            .phones
            .iter()
            .map(|(l, v)| Labeled::new(l, v))
            .collect(),
        emails: spec
            .emails
            .iter()
            .map(|(l, v)| Labeled::new(l, v))
            .collect(),
        addresses: spec.addresses,
        groups: spec
            .groups
            .iter()
            .map(|value| GroupEntry::new(*value))
            .collect(),
        favorite: spec.favorite,
    }
}

/// First half of the seed address book: Lovelace through Johnson.
fn sample_contacts_part1() -> Vec<Contact> {
    vec![
        make_sample(SampleSpec {
            first: "Ada",
            last: "Lovelace",
            org: "Analytical Engines",
            title: "Chief Mathematician",
            phones: &[("mobile", "+44 20 7946 0101"), ("work", "+44 20 7946 0199")],
            emails: &[
                ("work", "ada@analyticalengines.co.uk"),
                ("home", "ada.lovelace@example.com"),
            ],
            addresses: vec![Address {
                id: Uuid::new_v4(),
                label: "home".into(),
                street: "12 St James's Square".into(),
                city: "London".into(),
                region: String::new(),
                postal: "SW1Y 4LE".into(),
                country: "United Kingdom".into(),
            }],
            birthday: "1815-12-10",
            website: "https://analyticalengines.co.uk",
            notes: "First programmer. Prefers correspondence by letter. Working on Note G.",
            groups: &["Work", "Favorites"],
            favorite: true,
        }),
        make_sample(SampleSpec {
            first: "Grace",
            last: "Hopper",
            org: "US Navy",
            title: "Rear Admiral",
            phones: &[("work", "+1 202 555 0143")],
            emails: &[("work", "grace.hopper@navy.mil")],
            addresses: vec![Address {
                id: Uuid::new_v4(),
                label: "work".into(),
                street: "9620 Maryland Ave".into(),
                city: "Arlington".into(),
                region: "VA".into(),
                postal: "22203".into(),
                country: "USA".into(),
            }],
            birthday: "1906-12-09",
            website: "",
            notes: "Coined the term \"debugging\". Keeps a nanosecond on her desk.",
            groups: &["Work"],
            favorite: true,
        }),
        make_sample(SampleSpec {
            first: "Alan",
            last: "Turing",
            org: "Bletchley Park",
            title: "Cryptanalyst",
            phones: &[("home", "+44 1908 555 0117")],
            emails: &[("home", "alan.turing@example.com")],
            addresses: vec![],
            birthday: "1912-06-23",
            website: "https://turing.org.uk",
            notes: "Morning runner. Reach out about the Bombe redesign.",
            groups: &["Friends"],
            favorite: false,
        }),
        make_sample(SampleSpec {
            first: "Katherine",
            last: "Johnson",
            org: "NASA",
            title: "Research Mathematician",
            phones: &[("work", "+1 757 555 0188"), ("mobile", "+1 757 555 0042")],
            emails: &[("work", "k.johnson@nasa.gov")],
            addresses: vec![Address {
                id: Uuid::new_v4(),
                label: "work".into(),
                street: "100 NASA Rd".into(),
                city: "Hampton".into(),
                region: "VA".into(),
                postal: "23666".into(),
                country: "USA".into(),
            }],
            birthday: "1918-08-26",
            website: "",
            notes: "Orbital mechanics. Double-checks every trajectory by hand.",
            groups: &["Work", "Favorites"],
            favorite: true,
        }),
    ]
}

/// Second half of the seed address book: Berners-Lee through Hamilton.
fn sample_contacts_part2() -> Vec<Contact> {
    vec![
        make_sample(SampleSpec {
            first: "Tim",
            last: "Berners-Lee",
            org: "W3C",
            title: "Director",
            phones: &[("mobile", "+41 22 555 0190")],
            emails: &[("work", "timbl@w3.org")],
            addresses: vec![],
            birthday: "1955-06-08",
            website: "https://w3.org",
            notes: "Invented the Web. Ask about standards, not implementations.",
            groups: &["Work"],
            favorite: false,
        }),
        make_sample(SampleSpec {
            first: "Radia",
            last: "Perlman",
            org: "",
            title: "Network Engineer",
            phones: &[("mobile", "+1 408 555 0173")],
            emails: &[("home", "radia@example.com")],
            addresses: vec![],
            birthday: "1951-12-18",
            website: "",
            notes: "Mother of the spanning tree protocol.",
            groups: &["Friends"],
            favorite: false,
        }),
        make_sample(SampleSpec {
            first: "Margaret",
            last: "Hamilton",
            org: "MIT",
            title: "Lead Software Engineer",
            phones: &[("work", "+1 617 555 0156")],
            emails: &[("work", "mhamilton@mit.edu")],
            addresses: vec![Address {
                id: Uuid::new_v4(),
                label: "work".into(),
                street: "77 Massachusetts Ave".into(),
                city: "Cambridge".into(),
                region: "MA".into(),
                postal: "02139".into(),
                country: "USA".into(),
            }],
            birthday: "1936-08-17",
            website: "",
            notes: "Apollo guidance software. Coined \"software engineering\".",
            groups: &["Work"],
            favorite: false,
        }),
    ]
}

/// Seed an empty store with a varied sample address book so a fresh
/// launch shows a populated list and a fully-filled detail pane.
pub fn sample_contacts() -> Vec<Contact> {
    let mut out = sample_contacts_part1();
    out.extend(sample_contacts_part2());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Birthdays parse ISO, common typed and year-less formats, round-trip and pretty-print month names.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn birthday_parses_formats_and_round_trips() {
        // --- phase: birthday_parses_iso_to_ymd ---
        {
            let b = Birthday::parse("1815-12-10").expect("valid ISO date");
            assert_eq!(b.year(), Some(1815));
            assert_eq!(b.month(), 12);
            assert_eq!(b.day(), 10);
            assert_eq!(
                b,
                Birthday::Full {
                    year: 1815,
                    month: 12,
                    day: 10
                }
            );
        }

        // --- phase: birthday_accepts_common_typed_formats ---
        {
            // Year-first with any separator.
            assert_eq!(
                Birthday::parse("2001/03/09"),
                Some(Birthday::Full {
                    year: 2001,
                    month: 3,
                    day: 9
                })
            );
            // Basic 8-digit.
            assert_eq!(
                Birthday::parse("20010309"),
                Some(Birthday::Full {
                    year: 2001,
                    month: 3,
                    day: 9
                })
            );
            // US month-first, year-last.
            assert_eq!(
                Birthday::parse("03/09/2001"),
                Some(Birthday::Full {
                    year: 2001,
                    month: 3,
                    day: 9
                })
            );
            // European day-first with dots.
            assert_eq!(
                Birthday::parse("09.03.2001"),
                Some(Birthday::Full {
                    year: 2001,
                    month: 3,
                    day: 9
                })
            );
            // Ambiguous order disambiguated by an out-of-range day/month.
            assert_eq!(
                Birthday::parse("25/12/1990"),
                Some(Birthday::Full {
                    year: 1990,
                    month: 12,
                    day: 25
                })
            );
            // Garbage is rejected, not silently coerced.
            assert_eq!(Birthday::parse("not a date"), None);
            assert_eq!(Birthday::parse(""), None);
            assert_eq!(Birthday::parse("2001-13-40"), None);
        }

        // --- phase: birthday_handles_year_less_month_day ---
        {
            // Typed MM-DD, and both vCard truncated forms, land on MonthDay.
            assert_eq!(
                Birthday::parse("12-10"),
                Some(Birthday::MonthDay { month: 12, day: 10 })
            );
            assert_eq!(
                Birthday::parse("--12-10"),
                Some(Birthday::MonthDay { month: 12, day: 10 })
            );
            assert_eq!(
                Birthday::parse("--1210"),
                Some(Birthday::MonthDay { month: 12, day: 10 })
            );
            let b = Birthday::parse("12-10").unwrap();
            assert_eq!(b.year(), None, "a year-less birthday has no year");
            assert_eq!((b.month(), b.day()), (12, 10));
        }

        // --- phase: birthday_iso_round_trips_through_parse ---
        {
            for b in [
                Birthday::Full {
                    year: 1815,
                    month: 12,
                    day: 10,
                },
                Birthday::Full {
                    year: 2000,
                    month: 2,
                    day: 29,
                },
                Birthday::MonthDay { month: 6, day: 23 },
            ] {
                assert_eq!(
                    Birthday::parse(&b.iso()),
                    Some(b),
                    "{} round-trips",
                    b.iso()
                );
            }
        }

        // --- phase: birthday_pretty_prints_month_name ---
        {
            assert_eq!(
                Birthday::Full {
                    year: 1815,
                    month: 12,
                    day: 10
                }
                .pretty(),
                "December 10, 1815"
            );
            assert_eq!(
                Birthday::MonthDay { month: 12, day: 10 }.pretty(),
                "December 10"
            );
        }
    }

    /// Display names fall back to org then placeholder, initials uppercase first and last, and matching searches across fields.
    #[test]
    fn display_names_initials_and_field_matching() {
        // --- phase: display_name_falls_back_to_org_then_placeholder ---
        {
            let mut c = Contact::new();
            assert_eq!(c.display_name(), "No Name");
            c.org = "ACME".into();
            assert_eq!(c.display_name(), "ACME");
            c.first = "Jane".into();
            c.last = "Doe".into();
            assert_eq!(c.display_name(), "Jane Doe");
        }

        // --- phase: initials_uppercase_first_and_last ---
        {
            let mut c = Contact::new();
            c.first = "ada".into();
            c.last = "lovelace".into();
            assert_eq!(c.initials(), "AL");
        }

        // --- phase: matches_searches_across_fields ---
        {
            let mut c = Contact::new();
            c.first = "Grace".into();
            c.emails.push(Labeled::new("work", "grace@navy.mil"));
            assert!(c.matches(&"navy".to_lowercase()));
            assert!(c.matches(&"grace".to_lowercase()));
            assert!(!c.matches(&"zzz".to_lowercase()));
        }
    }

    /// The book filters and sorts by name and group, dedups groups case- and Unicode-insensitively and upserts in place.
    #[test]
    fn book_filters_sorts_groups_and_upserts() {
        // --- phase: filtered_sorted_by_name_and_group ---
        {
            let mut store = ContactStore::new();
            for c in sample_contacts() {
                store.upsert(c);
            }
            // Name sort puts "Berners-Lee" before "Hopper" before "Lovelace".
            let ids = store.filtered_sorted("", None, SortKey::Name);
            let names: Vec<String> = ids
                .iter()
                .map(|id| store.get(*id).unwrap().last.clone())
                .collect();
            let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
            assert!(pos("Berners-Lee") < pos("Hopper"));
            assert!(pos("Hopper") < pos("Lovelace"));

            // Group filter narrows to Friends only.
            let friends = store.filtered_sorted("", Some("Friends"), SortKey::Name);
            assert!(friends.iter().all(|id| {
                store
                    .get(*id)
                    .unwrap()
                    .groups
                    .iter()
                    .any(|g| g.value.eq_ignore_ascii_case("Friends"))
            }));
            assert!(!friends.is_empty());
        }

        // --- phase: all_groups_dedupes_case_insensitively ---
        {
            let mut store = ContactStore::new();
            let mut a = Contact::new();
            a.groups = vec![
                GroupEntry::new("Work"),
                GroupEntry::new("work"),
                GroupEntry::new("Family"),
            ];
            store.upsert(a);
            let groups = store.all_groups();
            assert_eq!(groups.len(), 2, "Work/work collapse: {groups:?}");
        }

        // --- phase: groups_and_search_handle_canonically_equivalent_unicode ---
        {
            let mut store = ContactStore::new();
            let mut first = Contact::new();
            first.groups.push(GroupEntry::new("Cafe\u{301}"));
            let mut second = Contact::new();
            second.groups.push(GroupEntry::new("CAFÉ"));
            store.upsert(first.clone());
            store.upsert(second.clone());

            assert_eq!(store.all_groups(), vec!["CAFÉ".to_string()]);
            let visible = store.filtered_sorted("", Some("café"), SortKey::Name);
            assert_eq!(visible.len(), 2);
            assert!(first.matches("CAFÉ"));
        }

        // --- phase: upsert_replaces_in_place ---
        {
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
}
