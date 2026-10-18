//! The address book's records: a contact and its repeated rows.
//!
//! [`Contact`] is stored as it is: every text field merges per character,
//! every phone, email, address and group row is stored under its own id so
//! two people editing different rows of one contact both keep their edit.
//! The window reads the book as it is presented: [`crate::resource::Contacts`].

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Id, Identified, JsonSchema, Model, Ordered, Refusal};
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

/// One deterministic display spelling for every logical group of
/// `contacts`, in the order of their identity. The key is the same
/// NFC/case-folded identity filtering and resource addressing use; choosing
/// the lexical spelling makes separate peers publish the same label even
/// when Automerge projects rows in a different order.
pub fn ordered_groups(contacts: &[Contact]) -> Vec<String> {
    let mut groups = std::collections::BTreeMap::<String, String>::new();
    for contact in contacts {
        for entry in &contact.groups {
            let name = entry.value.trim();
            if name.is_empty() {
                continue;
            }
            groups
                .entry(normalize_text(name))
                .and_modify(|existing| {
                    if name < existing.as_str() {
                        *existing = name.to_string();
                    }
                })
                .or_insert_with(|| name.to_string());
        }
    }
    groups.into_values().collect()
}

/// Stable identifier for a single contact record. Backs every
/// cross-reference and the CRDT map key.
pub type ContactId = Uuid;

/// A typed, repeatable single-value field — a phone number or an email
/// address. `label` is a free-form type tag (home / work / mobile / …);
/// `value` is the actual number or address.
#[derive(Model, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Labeled {
    /// The row's identity, the same on every replica.
    #[model(readonly)]
    pub id: Uuid,
    /// Type
    pub label: String,
    /// Value
    #[model(label)]
    pub value: String,
}

impl Identified for Labeled {
    fn id(&self) -> Id {
        Id::from_raw(self.id.to_string())
    }
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
#[derive(Model, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Address {
    /// The row's identity, the same on every replica.
    #[model(readonly)]
    pub id: Uuid,
    /// Type
    pub label: String,
    /// Street
    #[model(label)]
    pub street: String,
    /// City
    pub city: String,
    /// Region
    pub region: String,
    /// Postal code
    pub postal: String,
    /// Country
    pub country: String,
}

impl Identified for Address {
    fn id(&self) -> Id {
        Id::from_raw(self.id.to_string())
    }
}

/// One group a contact belongs to.
#[derive(Model, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GroupEntry {
    /// The row's identity, the same on every replica.
    #[model(readonly)]
    pub id: Uuid,
    /// Group
    #[model(label)]
    pub value: String,
}

impl Identified for GroupEntry {
    fn id(&self) -> Id {
        Id::from_raw(self.id.to_string())
    }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Birthday {
    Full { year: i32, month: u32, day: u32 },
    MonthDay { month: u32, day: u32 },
}

impl Default for Birthday {
    fn default() -> Self {
        Self::MonthDay { month: 1, day: 1 }
    }
}

tilo_resource::json_model!(Birthday);

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

    /// Whether the date is one the calendar has.
    pub fn is_valid(&self) -> bool {
        match *self {
            Birthday::Full { year, month, day } => Self::full(year, month, day).is_some(),
            Birthday::MonthDay { month, day } => Self::month_day(month, day).is_some(),
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

    /// Human-readable form in the toolkit's worded date, e.g.
    /// `Dec 10, 1815`, or `Dec 10` when the year is unknown.
    pub fn pretty(&self) -> String {
        match *self {
            Birthday::Full { year, month, day } => {
                ui::format::NaiveDate::from_ymd_opt(year, month, day)
                    .map_or_else(|| self.iso(), ui::format::month_day_year)
            }
            // Any leap year holds every month and day a year-less date can name.
            Birthday::MonthDay { month, day } => {
                ui::format::NaiveDate::from_ymd_opt(2000, month, day)
                    .map_or_else(|| self.iso(), ui::format::month_day)
            }
        }
    }
}

/// How much text one field of a contact may hold.
pub const MAX_CONTACT_FIELD_BYTES: usize = 32 * 1024;
/// How much text a contact's picture may take: a portrait is far smaller.
pub const MAX_CONTACT_PHOTO_BYTES: usize = 96 * 1024;
/// How many rows one repeated field of a contact may hold.
pub const MAX_CONTACT_ROWS: usize = 256;

/// One person or organization.
#[derive(Model, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[model(check = Contact::valid)]
pub struct Contact {
    /// The contact's identity, the same on every replica and in its vCard.
    #[model(readonly)]
    pub id: ContactId,
    /// First name
    #[model(label)]
    pub first: String,
    /// Last name
    pub last: String,
    /// Organization
    pub org: String,
    /// Title
    pub title: String,
    /// Website
    pub website: String,
    /// Birthday
    ///
    /// A full date or a month and day; Calendar shows it.
    pub birthday: Option<Birthday>,
    /// Notes
    pub notes: String,
    /// Phone numbers
    pub phones: Ordered<Labeled>,
    /// Email addresses
    pub emails: Ordered<Labeled>,
    /// Postal addresses
    pub addresses: Ordered<Address>,
    /// Groups
    pub groups: Ordered<GroupEntry>,
    /// Favorite
    pub favorite: bool,
    /// Picture
    ///
    /// A small square JPEG as base64 (`ui::held_images::portrait`), from an
    /// imported card or chosen by the person; empty for none.
    #[model(readonly)]
    #[serde(default)]
    pub photo: String,
}

impl Identified for Contact {
    fn id(&self) -> Id {
        Id::from_raw(self.id.to_string())
    }
}

impl Contact {
    /// Whether every field is bounded, every row has an id and the birthday
    /// is a date the calendar has.
    fn valid(&self) -> Result<(), Refusal> {
        let address_text = self.addresses.iter().flat_map(|address| {
            [
                &address.label,
                &address.street,
                &address.city,
                &address.region,
                &address.postal,
                &address.country,
            ]
        });
        let row_text = self
            .phones
            .iter()
            .chain(self.emails.iter())
            .flat_map(|row| [&row.label, &row.value])
            .chain(self.groups.iter().map(|group| &group.value))
            .chain(address_text);
        if [
            &self.first,
            &self.last,
            &self.org,
            &self.title,
            &self.website,
            &self.notes,
        ]
        .into_iter()
        .chain(row_text)
        .any(|text| text.len() > MAX_CONTACT_FIELD_BYTES)
        {
            return Err(Refusal::new(format!(
                "a contact's field holds at most {MAX_CONTACT_FIELD_BYTES} bytes"
            )));
        }
        if self.photo.len() > MAX_CONTACT_PHOTO_BYTES {
            return Err(Refusal::new(format!(
                "a contact's picture takes at most {MAX_CONTACT_PHOTO_BYTES} bytes"
            )));
        }
        if [
            self.phones.len(),
            self.emails.len(),
            self.addresses.len(),
            self.groups.len(),
        ]
        .into_iter()
        .any(|rows| rows > MAX_CONTACT_ROWS)
        {
            return Err(Refusal::new("a contact has too many repeated fields"));
        }
        let row_ids = self
            .phones
            .iter()
            .chain(self.emails.iter())
            .map(|row| row.id)
            .chain(self.addresses.iter().map(|address| address.id))
            .chain(self.groups.iter().map(|group| group.id));
        if self.id.is_nil() || row_ids.into_iter().any(|id| id.is_nil()) {
            return Err(Refusal::new("a contact and each of its rows has an id"));
        }
        if self.birthday.is_some_and(|birthday| !birthday.is_valid()) {
            return Err(Refusal::new("a birthday is a date the calendar has"));
        }
        Ok(())
    }

    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            ..Default::default()
        }
    }

    /// The picture's encoded bytes, if the contact has one.
    pub fn photo_bytes(&self) -> Option<Vec<u8>> {
        if self.photo.is_empty() {
            return None;
        }
        ui::held_images::picture_bytes(&self.photo)
    }

    /// Take `encoded` (any picture) as the contact's picture, made a small
    /// square. Returns whether it decoded.
    pub fn set_photo(&mut self, encoded: &[u8]) -> bool {
        match ui::held_images::portrait(encoded) {
            Some(portrait) => {
                self.photo = ui::held_images::picture_text(&portrait);
                true
            }
            None => false,
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
            "Unnamed contact".to_string()
        }
    }

    /// The one secondary fact a list row shows under the name: the
    /// organization, or the job title when there is none.
    pub fn subtitle(&self) -> String {
        if self.org.trim().is_empty() {
            self.title.trim().to_string()
        } else {
            self.org.trim().to_string()
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
            // The month's whole name too, so "december" finds the birthday.
            hay.push_str(&ui::format::month_name(birthday.month()));
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
            SortKey::Org => "Organization",
        }
    }
}

/// What the window reads of the address book: its contacts in the orders
/// and groupings the list shows. Order in `contacts` is not significant —
/// every read path sorts on demand.
impl crate::resource::Contacts {
    /// Every distinct group across all contacts, sorted alphabetically
    /// ([`ordered_groups`]).
    pub fn all_groups(&self) -> Vec<String> {
        ordered_groups(&self.contacts)
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

/// Case-insensitive comparison the list sorts by.
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
        addresses: spec.addresses.into(),
        groups: spec
            .groups
            .iter()
            .map(|value| GroupEntry::new(*value))
            .collect(),
        favorite: spec.favorite,
        photo: String::new(),
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
            groups: &["Work", "Mathematics"],
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
            groups: &["Work", "Mathematics"],
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
/// Each sample person keeps one identity across runs, so their avatar
/// colour and every capture of the book are the same each time.
pub fn sample_contacts() -> Vec<Contact> {
    let mut out = sample_contacts_part1();
    out.extend(sample_contacts_part2());
    for (index, contact) in (1_u128..).zip(out.iter_mut()) {
        contact.id = Uuid::from_u128(0x5a3e_c0de_0000_4000_8000_0000_0000_0000 | index);
    }
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
                "Dec 10, 1815"
            );
            assert_eq!(
                Birthday::MonthDay { month: 12, day: 10 }.pretty(),
                "Dec 10"
            );
        }
    }

    /// Display names fall back to org then placeholder, and matching searches across fields.
    #[test]
    fn display_names_and_field_matching() {
        // --- phase: display_name_falls_back_to_org_then_placeholder ---
        {
            let mut c = Contact::new();
            assert_eq!(c.display_name(), "Unnamed contact");
            c.org = "ACME".into();
            assert_eq!(c.display_name(), "ACME");
            c.first = "Jane".into();
            c.last = "Doe".into();
            assert_eq!(c.display_name(), "Jane Doe");
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

    fn book(contacts: Vec<Contact>) -> crate::resource::Contacts {
        crate::resource::Contacts {
            contacts: contacts.into(),
            ..crate::resource::Contacts::default()
        }
    }

    /// The book filters and sorts by name and group, and dedups groups case- and Unicode-insensitively.
    #[test]
    fn book_filters_sorts_and_groups() {
        // --- phase: filtered_sorted_by_name_and_group ---
        {
            let store = book(sample_contacts());
            // Name sort puts "Berners-Lee" before "Hopper" before "Lovelace".
            let ids = store.filtered_sorted("", None, SortKey::Name);
            let names: Vec<String> = ids
                .iter()
                .map(|id| store.contact(*id).unwrap().last.clone())
                .collect();
            let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
            assert!(pos("Berners-Lee") < pos("Hopper"));
            assert!(pos("Hopper") < pos("Lovelace"));

            // Group filter narrows to Friends only.
            let friends = store.filtered_sorted("", Some("Friends"), SortKey::Name);
            assert!(friends.iter().all(|id| {
                store
                    .contact(*id)
                    .unwrap()
                    .groups
                    .iter()
                    .any(|g| g.value.eq_ignore_ascii_case("Friends"))
            }));
            assert!(!friends.is_empty());
        }

        // --- phase: all_groups_dedupes_case_insensitively ---
        {
            let mut a = Contact::new();
            a.groups = vec![
                GroupEntry::new("Work"),
                GroupEntry::new("work"),
                GroupEntry::new("Family"),
            ]
            .into();
            let store = book(vec![a]);
            let groups = store.all_groups();
            assert_eq!(groups.len(), 2, "Work/work collapse: {groups:?}");
        }

        // --- phase: groups_and_search_handle_canonically_equivalent_unicode ---
        {
            let mut first = Contact::new();
            first.groups.push(GroupEntry::new("Cafe\u{301}"));
            let mut second = Contact::new();
            second.groups.push(GroupEntry::new("CAFÉ"));
            let store = book(vec![first.clone(), second]);

            assert_eq!(store.all_groups(), vec!["CAFÉ".to_string()]);
            let visible = store.filtered_sorted("", Some("café"), SortKey::Name);
            assert_eq!(visible.len(), 2);
            assert!(first.matches("CAFÉ"));
        }
    }
}
