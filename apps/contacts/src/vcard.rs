//! Dependency-free vCard import/export covering the 3.0 (RFC 2426) and 4.0
//! (RFC 6350) variants used by address books.
//!
//! We deliberately keep this small and dependency-free: contacts only
//! needs the property set a person/organization card uses — `FN`, `N`,
//! `ORG`, `TITLE`, `TEL`, `EMAIL`, `ADR`, `BDAY`, `URL`, `NOTE`,
//! `CATEGORIES` — plus correct value (un)escaping and line folding.
//!
//! ## What is handled
//! * Multiple cards in one `.vcf` stream.
//! * RFC line *unfolding* (a continuation line begins with SP/TAB).
//! * Parameter parsing (`TEL;TYPE=work,voice:…`) including the legacy
//!   3.0 bare-type form (`TEL;WORK:…`) and `item1.`-style group prefixes.
//! * Value escaping: `\n` `\,` `\;` `\\` on read; the inverse on write.
//! * Structured `N` and `ADR` component splitting on unescaped `;`.
//! * `BDAY` in both `YYYY-MM-DD` (3.0) and `YYYYMMDD` (4.0) shapes.
//!
//! The content-line layer (folding, parameter quoting, TEXT escaping) is the
//! shared `pim-lines` codec that iCalendar uses too.

use crate::model::{Address, Birthday, Contact, GroupEntry, Labeled};
use pim_lines::{
    ContentLine, Unfolded, encode_param_value, escape_text as esc, fold_into, split_unescaped,
    unescape_text as unescape,
};
use uuid::Uuid;

/// Import limits keep a malformed or hostile `.vcf` from monopolising the UI
/// thread.  The app also rejects oversized files before reading them; these
/// bounds protect callers that parse an in-memory document directly.
pub const MAX_VCARD_CARDS: usize = 2_048;
pub const MAX_VCARD_LOGICAL_LINE_BYTES: usize = 32 * 1024;
/// A `PHOTO` line carries a whole picture, so it may run longer; the picture
/// is made a small portrait on import.
pub const MAX_VCARD_PHOTO_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Result of a bounded vCard parse. `skipped_cards` includes empty, malformed,
/// and over-limit cards, allowing the UI to be honest about partial imports.
#[derive(Debug, Default)]
pub struct VCardParseReport {
    pub contacts: Vec<Contact>,
    pub skipped_cards: usize,
    pub limit_reached: bool,
}

/// The vCard version to emit. Affects the `VERSION` line, `BDAY`
/// formatting and the `TYPE=` parameter shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VCardVersion {
    V3,
    V4,
}

impl VCardVersion {
    pub fn label(self) -> &'static str {
        match self {
            VCardVersion::V3 => "vCard 3.0",
            VCardVersion::V4 => "vCard 4.0",
        }
    }
}

// ---------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------

/// Serialize a slice of contacts into a single `.vcf` document.
pub fn to_vcards(contacts: &[Contact], version: VCardVersion) -> String {
    let mut out = String::new();
    for c in contacts {
        out.push_str(&to_vcard(c, version));
    }
    out
}

/// Serialize one contact into a single `BEGIN:VCARD … END:VCARD` block
/// (trailing CRLF included).
pub fn to_vcard(c: &Contact, version: VCardVersion) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("BEGIN:VCARD".to_string());
    lines.push(match version {
        VCardVersion::V3 => "VERSION:3.0".to_string(),
        VCardVersion::V4 => "VERSION:4.0".to_string(),
    });
    // `UID` is the durable identity used by this address book.  Without it an
    // exported card comes back with a fresh random id, so importing one's own
    // backup silently creates duplicates instead of recognising the original
    // contact. A bare UUID is valid text in both vCard 3.0 and 4.0 and is
    // broadly understood by other address-book implementations.
    lines.push(format!("UID:{}", c.id));

    // FN is required; N is the structured surname/given form.
    lines.push(format!("FN:{}", esc(&c.display_name())));
    lines.push(format!("N:{};{};;;", esc(&c.last), esc(&c.first)));

    if !c.org.trim().is_empty() {
        lines.push(format!("ORG:{}", esc(&c.org)));
    }
    if !c.title.trim().is_empty() {
        lines.push(format!("TITLE:{}", esc(&c.title)));
    }

    for p in &c.phones {
        if p.value.trim().is_empty() {
            continue;
        }
        lines.push(format!(
            "TEL;{}:{}",
            type_param(&p.label, version),
            esc(&p.value)
        ));
    }
    for e in &c.emails {
        if e.value.trim().is_empty() {
            continue;
        }
        lines.push(format!(
            "EMAIL;{}:{}",
            type_param(&e.label, version),
            esc(&e.value)
        ));
    }
    for a in &c.addresses {
        if a.is_empty() {
            continue;
        }
        // ADR structured value: PO;EXT;STREET;CITY;REGION;POSTAL;COUNTRY
        lines.push(format!(
            "ADR;{}:;;{};{};{};{};{}",
            type_param(&a.label, version),
            esc(&a.street),
            esc(&a.city),
            esc(&a.region),
            esc(&a.postal),
            esc(&a.country),
        ));
    }

    if let Some(bday) = &c.birthday {
        lines.push(format!("BDAY:{}", format_bday(bday, version)));
    }
    if !c.website.trim().is_empty() {
        // URL is a URI value (RFC 6350 §6.7.8): no TEXT escaping, so commas
        // and semicolons in a link reach other clients intact.
        let url: String = c.website.chars().filter(|ch| !ch.is_control()).collect();
        lines.push(format!("URL:{url}"));
    }
    if !c.notes.trim().is_empty() {
        lines.push(format!("NOTE:{}", esc(&c.notes)));
    }
    if !c.groups.is_empty() {
        let cats: Vec<String> = c.groups.iter().map(|group| esc(&group.value)).collect();
        lines.push(format!("CATEGORIES:{}", cats.join(",")));
    }
    if c.photo_bytes().is_some() {
        lines.push(match version {
            VCardVersion::V3 => format!("PHOTO;ENCODING=b;TYPE=JPEG:{}", c.photo),
            VCardVersion::V4 => format!("PHOTO:data:image/jpeg;base64,{}", c.photo),
        });
    }

    lines.push("END:VCARD".to_string());

    // Fold long lines to <=75 octets and join with CRLF per the spec.
    let mut doc = String::new();
    for l in lines {
        fold_into(&mut doc, &l);
    }
    doc
}

fn type_param(label: &str, version: VCardVersion) -> String {
    let label = label.trim();
    let label = if label.is_empty() { "other" } else { label };
    let label = match version {
        VCardVersion::V3 => label.to_uppercase(),
        VCardVersion::V4 => label.to_lowercase(),
    };
    // A user label is free text; `;` or `:` in it must not end the parameter.
    format!("TYPE={}", encode_param_value(&label))
}

/// Birthday formatting: 4.0 prefers the basic `YYYYMMDD`; 3.0 keeps the
/// extended `YYYY-MM-DD`. A year-less birthday emits the RFC 6350 truncated
/// form (`--MMDD` for 4.0, `--MM-DD` for 3.0).
fn format_bday(bday: &Birthday, version: VCardVersion) -> String {
    match (bday, version) {
        (Birthday::Full { year, month, day }, VCardVersion::V3) => {
            format!("{year:04}-{month:02}-{day:02}")
        }
        (Birthday::Full { year, month, day }, VCardVersion::V4) => {
            format!("{year:04}{month:02}{day:02}")
        }
        (Birthday::MonthDay { month, day }, VCardVersion::V3) => format!("--{month:02}-{day:02}"),
        (Birthday::MonthDay { month, day }, VCardVersion::V4) => format!("--{month:02}{day:02}"),
    }
}

// ---------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------

/// Parse zero or more vCards out of a `.vcf` document. Unknown
/// properties are ignored; malformed cards are skipped rather than
/// aborting the whole import.
pub fn parse_vcards(input: &str) -> Vec<Contact> {
    parse_vcards_report(input).contacts
}

/// Parse a bounded vCard stream and retain enough outcome data for callers to
/// distinguish a clean import from a partial one.
pub fn parse_vcards_report(input: &str) -> VCardParseReport {
    let logical = pim_lines::unfold_bounded(input, MAX_VCARD_PHOTO_LINE_BYTES);
    let mut report = VCardParseReport::default();
    let mut cur: Option<(Contact, bool)> = None;

    for raw in logical {
        let Unfolded::Line(raw) = raw else {
            // An oversized property invalidates its card rather than being
            // imported in part.
            if let Some((_, invalid)) = cur.as_mut() {
                *invalid = true;
            }
            continue;
        };
        if raw.is_empty() {
            continue;
        }
        let Some(line) = ContentLine::parse(&raw) else {
            continue;
        };
        if raw.len() > MAX_VCARD_LOGICAL_LINE_BYTES && line.name != "PHOTO" {
            // An oversized property invalidates its card rather than being
            // imported in part.
            if let Some((_, invalid)) = cur.as_mut() {
                *invalid = true;
            }
            continue;
        }
        if line.begins("VCARD") {
            if cur.take().is_some() {
                report.skipped_cards += 1;
            }
            if report.contacts.len() >= MAX_VCARD_CARDS {
                report.limit_reached = true;
                break;
            }
            cur = Some((
                Contact {
                    // A valid UID below replaces this sentinel. UID-less
                    // imports are assigned a deterministic semantic identity
                    // when the complete card is available at END:VCARD.
                    id: Uuid::nil(),
                    ..Default::default()
                },
                false,
            ));
            continue;
        }
        if line.ends("VCARD") {
            if let Some((mut c, invalid)) = cur.take() {
                if !invalid && has_contact_data(&c) {
                    assign_import_identities(&mut c);
                    report.contacts.push(c);
                } else {
                    report.skipped_cards += 1;
                }
            }
            continue;
        }
        let Some((contact, _)) = cur.as_mut() else {
            continue;
        };
        apply_property(contact, &line);
    }
    if cur.is_some() {
        report.skipped_cards += 1;
    }
    report
}

/// Give imported data stable identities before it crosses the action/CRDT
/// boundary. External cards frequently omit UID, while vCard has no standard
/// identity for repeated TEL/EMAIL/ADR/CATEGORIES rows. Deriving both from
/// bounded semantic content makes an exact re-import idempotent and makes two
/// disconnected peers produce the same absolute list operations.
fn assign_import_identities(contact: &mut Contact) {
    if contact.id.is_nil() {
        let birthday = contact
            .birthday
            .as_ref()
            .map(Birthday::iso)
            .unwrap_or_default();
        let mut parts = vec![
            "contact".to_string(),
            contact.first.clone(),
            contact.last.clone(),
            contact.org.clone(),
            contact.title.clone(),
            contact.website.clone(),
            birthday,
            contact.notes.clone(),
        ];
        parts.extend(
            contact
                .phones
                .iter()
                .flat_map(|row| ["phone".to_string(), row.label.clone(), row.value.clone()]),
        );
        parts.extend(
            contact
                .emails
                .iter()
                .flat_map(|row| ["email".to_string(), row.label.clone(), row.value.clone()]),
        );
        parts.extend(contact.addresses.iter().flat_map(|row| {
            [
                "address".to_string(),
                row.label.clone(),
                row.street.clone(),
                row.city.clone(),
                row.region.clone(),
                row.postal.clone(),
                row.country.clone(),
            ]
        }));
        parts.extend(
            contact
                .groups
                .iter()
                .flat_map(|row| ["group".to_string(), row.value.clone()]),
        );
        contact.id = deterministic_uuid(parts.iter().map(String::as_str));
    }

    let contact_id = contact.id.to_string();
    for (index, row) in contact.phones.iter_mut().enumerate() {
        row.id = deterministic_uuid([
            contact_id.as_str(),
            "phone",
            &index.to_string(),
            &row.label,
            &row.value,
        ]);
    }
    for (index, row) in contact.emails.iter_mut().enumerate() {
        row.id = deterministic_uuid([
            contact_id.as_str(),
            "email",
            &index.to_string(),
            &row.label,
            &row.value,
        ]);
    }
    for (index, row) in contact.addresses.iter_mut().enumerate() {
        row.id = deterministic_uuid([
            contact_id.as_str(),
            "address",
            &index.to_string(),
            &row.label,
            &row.street,
            &row.city,
            &row.region,
            &row.postal,
            &row.country,
        ]);
    }
    for (index, row) in contact.groups.iter_mut().enumerate() {
        row.id = deterministic_uuid([contact_id.as_str(), "group", &index.to_string(), &row.value]);
    }
}

fn deterministic_uuid<'a>(parts: impl IntoIterator<Item = &'a str>) -> Uuid {
    let mut first = 0xcbf2_9ce4_8422_2325_u64;
    let mut second = 0x8422_2325_cbf2_9ce4_u64;
    for part in parts {
        // Length framing ensures ["ab", "c"] cannot alias ["a", "bc"].
        for byte in part.len().to_be_bytes().into_iter().chain(part.bytes()) {
            first = (first ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
            second = (second ^ u64::from(byte)).wrapping_mul(0x9e37_79b1_85eb_ca87);
        }
    }
    let mut bytes = [0_u8; 16];
    let (first_half, second_half) = bytes.split_at_mut(8);
    first_half.copy_from_slice(&first.to_be_bytes());
    second_half.copy_from_slice(&second.to_be_bytes());
    if let Some(version) = bytes.get_mut(6) {
        *version = (*version & 0x0f) | 0x50;
    }
    if let Some(variant) = bytes.get_mut(8) {
        *variant = (*variant & 0x3f) | 0x80;
    }
    Uuid::from_bytes(bytes)
}

/// Collect every `TYPE=` value (comma-joined or repeated), lower-cased,
/// dropping the catch-all `voice`/`internet`/`pref` noise so the label
/// reads cleanly (home/work/mobile/…).
fn collect_types(line: &ContentLine) -> String {
    let mut types: Vec<String> = Vec::new();
    // `TYPE=work,voice`, `TYPE="work,voice"`, repeated `TYPE=` parameters and
    // the legacy bare form (`TEL;WORK:`) all contribute.
    let mut values: Vec<&str> = Vec::new();
    for param in &line.params {
        if param.values.is_empty() {
            values.push(&param.name);
        } else if param.name == "TYPE" {
            values.extend(param.values.iter().map(String::as_str));
        }
    }
    for v in values {
        for t in v.split(',') {
            let t = t.trim().to_lowercase();
            if t.is_empty() {
                continue;
            }
            if matches!(t.as_str(), "voice" | "internet" | "pref") {
                continue;
            }
            if !types.contains(&t) {
                types.push(t);
            }
        }
    }
    types
        .first()
        .cloned()
        .unwrap_or_else(|| "other".to_string())
}

fn apply_property(c: &mut Contact, line: &ContentLine) {
    let value = line.value.as_str();
    match line.name.as_str() {
        "FN" => {
            // Only use FN to seed names if N didn't already populate them.
            if c.first.is_empty() && c.last.is_empty() {
                let unescaped = unescape(value);
                if let Some((first, last)) = unescaped.split_once(' ') {
                    c.first = first.trim().to_string();
                    c.last = last.trim().to_string();
                } else {
                    c.first = unescaped.trim().to_string();
                }
            }
        }
        "N" => {
            let comps = split_unescaped(value, ';');
            // N = Family;Given;Additional;Prefix;Suffix
            if let Some(last) = comps.first() {
                c.last = unescape(last).trim().to_string();
            }
            if let Some(first) = comps.get(1) {
                c.first = unescape(first).trim().to_string();
            }
        }
        "ORG" => {
            // ORG can be `Org;Unit` — keep the leading org name.
            let comps = split_unescaped(value, ';');
            c.org = comps
                .first()
                .map(|s| unescape(s).trim().to_string())
                .unwrap_or_default();
        }
        "TITLE" => c.title = unescape(value).trim().to_string(),
        "UID" => {
            // Own exports use a bare UUID; accepting the common URI spelling
            // lets those backups be recognised for safe review too.
            let raw = unescape(value);
            let raw = raw
                .strip_prefix("urn:uuid:")
                .or_else(|| raw.strip_prefix("URN:UUID:"))
                .unwrap_or(&raw);
            if let Ok(id) = Uuid::parse_str(raw.trim()) {
                c.id = id;
            }
        }
        "TEL" => {
            c.phones.push(Labeled::new(
                &collect_types(line),
                &unescape(strip_uri(value)),
            ));
        }
        "EMAIL" => {
            c.emails
                .push(Labeled::new(&collect_types(line), &unescape(value)));
        }
        "ADR" => {
            let comps = split_unescaped(value, ';');
            // ADR = PO;EXT;STREET;CITY;REGION;POSTAL;COUNTRY
            let g = |i: usize| comps.get(i).map(|s| unescape(s)).unwrap_or_default();
            let addr = Address {
                id: Uuid::new_v4(),
                label: collect_types(line),
                // The model has one delivery field. Keep PO boxes and extended
                // addresses there rather than silently throwing them away.
                street: (0..3)
                    .map(g)
                    .filter(|part| !part.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\n"),
                city: g(3),
                region: g(4),
                postal: g(5),
                country: g(6),
            };
            if !addr.is_empty() {
                c.addresses.push(addr);
            }
        }
        "BDAY" => c.birthday = birthday_value(line),
        // An embedded picture only: a `VALUE=uri` link to a server is never
        // fetched. vCard 4 writes a `data:` URI, vCard 3 `ENCODING=b`.
        "PHOTO" => {
            let embedded = value
                .strip_prefix("data:")
                .and_then(|uri| uri.split_once(";base64,").map(|(_, data)| data))
                .or_else(|| {
                    line.param_value("ENCODING")
                        .is_some_and(|encoding| {
                            matches!(encoding.to_ascii_lowercase().as_str(), "b" | "base64")
                        })
                        .then_some(value)
                });
            if let Some(bytes) = embedded.and_then(ui::held_images::picture_bytes) {
                c.set_photo(&bytes);
            }
        }
        "URL" => c.website = unescape(strip_uri(value)),
        "NOTE" => c.notes = unescape(value),
        "CATEGORIES" => {
            for cat in split_unescaped(value, ',') {
                let cat = unescape(cat).trim().to_string();
                if !cat.is_empty() {
                    c.groups.push(GroupEntry::new(cat));
                }
            }
        }
        _ => {}
    }
}

/// A `BDAY` as other clients write it. Apple writes a birthday whose year is
/// unknown with a placeholder year it names in `X-APPLE-OMIT-YEAR`
/// (`BDAY;X-APPLE-OMIT-YEAR=1604:1604-03-15`), and a date-and-time keeps its
/// date.
fn birthday_value(line: &ContentLine) -> Option<Birthday> {
    let value = unescape(&line.value);
    let date = value
        .split_once('T')
        .map_or(value.as_str(), |(date, _)| date);
    let birthday = Birthday::parse(date)?;
    let omitted_year = line
        .param_value("X-APPLE-OMIT-YEAR")
        .and_then(|year| year.trim().parse::<i32>().ok());
    Some(match birthday {
        Birthday::Full { year, month, day } if omitted_year == Some(year) => {
            Birthday::MonthDay { month, day }
        }
        birthday => birthday,
    })
}

/// Empty BEGIN/END pairs are not contacts.  They occur in truncated exports
/// and in hand-edited files; accepting them creates an untitled, unusable
/// record and makes the import success count misleading.  Name-less cards
/// with an actual endpoint, note, group, or organization remain valid.
fn has_contact_data(c: &Contact) -> bool {
    !c.first.trim().is_empty()
        || !c.last.trim().is_empty()
        || !c.org.trim().is_empty()
        || !c.title.trim().is_empty()
        || !c.website.trim().is_empty()
        || c.birthday.is_some()
        || !c.notes.trim().is_empty()
        || !c.groups.is_empty()
        || c.phones.iter().any(|row| !row.value.trim().is_empty())
        || c.emails.iter().any(|row| !row.value.trim().is_empty())
        || c.addresses.iter().any(|address| !address.is_empty())
}

/// Drop a `tel:` / `mailto:` style URI scheme prefix so `TEL;VALUE=uri:
/// tel:+15551234` reads as a plain number.
fn strip_uri(value: &str) -> &str {
    for scheme in ["tel:", "mailto:"] {
        if let Some(rest) = value.strip_prefix(scheme) {
            return rest;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All delivery lines must survive import, including a PO-box-only card.
    #[test]
    fn regression_import_preserves_postal_delivery_lines() {
        for (adr, street) in [
            ("PO Box 42;;;;;;", "PO Box 42"),
            (
                ";Suite 5;10 Main Street;London;;;UK",
                "Suite 5\n10 Main Street",
            ),
            (
                "PO Box 42;Building B;10 Main Street;London;;;UK",
                "PO Box 42\nBuilding B\n10 Main Street",
            ),
        ] {
            let report = parse_vcards_report(&format!(
                "BEGIN:VCARD\r\nVERSION:3.0\r\nADR;TYPE=work:{adr}\r\nEND:VCARD\r\n"
            ));
            assert_eq!(report.skipped_cards, 0);
            assert_eq!(report.contacts.len(), 1, "postal-only cards are contacts");
            let contact = &report.contacts[0];
            assert_eq!(contact.addresses.len(), 1);
            assert_eq!(contact.addresses[0].street, street);
            for version in [VCardVersion::V3, VCardVersion::V4] {
                let reimported = parse_vcards(&to_vcard(contact, version));
                assert_eq!(reimported[0].addresses[0].street, street);
            }
        }
    }

    fn sample() -> Contact {
        Contact {
            id: Uuid::new_v4(),
            first: "Ada".into(),
            last: "Lovelace".into(),
            org: "Analytical Engines".into(),
            title: "Mathematician".into(),
            website: "https://example.com".into(),
            birthday: Birthday::parse("1815-12-10"),
            notes: "Note: with, punctuation; and a\nnewline".into(),
            phones: vec![Labeled::new("mobile", "+44 20 7946 0101")].into(),
            emails: vec![Labeled::new("work", "ada@example.com")].into(),
            addresses: vec![Address {
                id: Uuid::new_v4(),
                label: "home".into(),
                street: "12 St James's Square".into(),
                city: "London".into(),
                region: String::new(),
                postal: "SW1Y 4LE".into(),
                country: "UK".into(),
            }]
            .into(),
            groups: vec![GroupEntry::new("Work"), GroupEntry::new("Favorites")].into(),
            favorite: true,
            photo: String::new(),
        }
    }

    /// v3 and v4 round-trips preserve fields, birthdays (including year-less and absent) and unescaped URLs.
    #[test]
    fn vcard_round_trips_fields_and_birthdays() {
        // --- phase: round_trip_v3_preserves_fields ---
        {
            let c = sample();
            let doc = to_vcard(&c, VCardVersion::V3);
            assert!(doc.contains("VERSION:3.0"));
            assert!(doc.contains(&format!("UID:{}", c.id)));
            let back = parse_vcards(&doc);
            assert_eq!(back.len(), 1);
            let r = &back[0];
            assert_eq!(r.id, c.id, "own exports retain their merge identity");
            assert_eq!(r.first, "Ada");
            assert_eq!(r.last, "Lovelace");
            assert_eq!(r.org, "Analytical Engines");
            assert_eq!(r.title, "Mathematician");
            assert_eq!(r.website, "https://example.com");
            assert_eq!(
                r.birthday,
                Some(Birthday::Full {
                    year: 1815,
                    month: 12,
                    day: 10
                })
            );
            assert_eq!(r.phones[0].value, "+44 20 7946 0101");
            assert_eq!(r.phones[0].label, "mobile");
            assert_eq!(r.emails[0].value, "ada@example.com");
            assert_eq!(r.addresses[0].city, "London");
            assert_eq!(r.addresses[0].postal, "SW1Y 4LE");
            assert_eq!(
                r.groups
                    .iter()
                    .map(|group| group.value.as_str())
                    .collect::<Vec<_>>(),
                vec!["Work", "Favorites"]
            );
            assert_eq!(r.notes, "Note: with, punctuation; and a\nnewline");
        }

        // --- phase: round_trip_v4_uses_basic_bday_and_lowercase_types ---
        {
            let c = sample();
            let doc = to_vcard(&c, VCardVersion::V4);
            assert!(doc.contains("VERSION:4.0"));
            assert!(doc.contains("BDAY:18151210"), "v4 emits basic date: {doc}");
            assert!(doc.to_lowercase().contains("type=mobile"));
            // …and parsing it back recovers the structured birthday.
            let back = parse_vcards(&doc);
            assert_eq!(
                back[0].birthday,
                Some(Birthday::Full {
                    year: 1815,
                    month: 12,
                    day: 10
                })
            );
            assert_eq!(back[0].phones[0].label, "mobile");
        }

        // --- phase: year_less_birthday_round_trips_through_vcard ---
        {
            // A birthday whose year is unknown survives both flavours as the
            // RFC 6350 truncated `--MMDD` / `--MM-DD` form.
            let mut c = sample();
            c.birthday = Some(Birthday::MonthDay { month: 3, day: 9 });

            let v3 = to_vcard(&c, VCardVersion::V3);
            assert!(v3.contains("BDAY:--03-09"), "v3 truncated form: {v3}");
            assert_eq!(
                parse_vcards(&v3)[0].birthday,
                Some(Birthday::MonthDay { month: 3, day: 9 })
            );

            let v4 = to_vcard(&c, VCardVersion::V4);
            assert!(v4.contains("BDAY:--0309"), "v4 truncated form: {v4}");
            assert_eq!(
                parse_vcards(&v4)[0].birthday,
                Some(Birthday::MonthDay { month: 3, day: 9 })
            );
        }

        // --- phase: absent_birthday_emits_no_bday_line ---
        {
            let mut c = sample();
            c.birthday = None;
            let doc = to_vcard(&c, VCardVersion::V3);
            assert!(!doc.contains("BDAY:"), "no BDAY line when unset: {doc}");
            assert_eq!(parse_vcards(&doc)[0].birthday, None);
        }

        // --- phase: urls_are_written_without_text_escaping ---
        {
            let mut c = sample();
            c.website = "https://example.com/a,b;c".into();
            let doc = to_vcard(&c, VCardVersion::V4);
            assert!(doc.contains("URL:https://example.com/a,b;c\r\n"), "{doc}");
            assert_eq!(parse_vcards(&doc)[0].website, c.website);
        }
    }

    /// Birthdays as an iPhone and other clients export them: Apple's
    /// placeholder year stands for an unknown one, and a time is dropped.
    #[test]
    fn other_clients_birthdays_import_as_the_date_they_mean() {
        let birthday = |line: &str| {
            let card = format!("BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada\r\n{line}\r\nEND:VCARD\r\n");
            parse_vcards(&card)[0].birthday
        };
        assert_eq!(
            birthday("BDAY;X-APPLE-OMIT-YEAR=1604;VALUE=date:1604-03-15"),
            Some(Birthday::MonthDay { month: 3, day: 15 })
        );
        assert_eq!(
            birthday("BDAY;X-APPLE-OMIT-YEAR=1604:1604-02-29"),
            Some(Birthday::MonthDay { month: 2, day: 29 })
        );
        // The marker only drops the year it names.
        assert_eq!(
            birthday("BDAY;X-APPLE-OMIT-YEAR=1604:1990-03-15"),
            Some(Birthday::Full {
                year: 1990,
                month: 3,
                day: 15
            })
        );
        assert_eq!(
            birthday("BDAY:1990-05-12T00:00:00Z"),
            Some(Birthday::Full {
                year: 1990,
                month: 5,
                day: 12
            })
        );
        assert_eq!(
            birthday("BDAY:19900512T000000"),
            Some(Birthday::Full {
                year: 1990,
                month: 5,
                day: 12
            })
        );
    }

    /// The parser handles legacy bare types, folding, multiple cards, tel URIs, quoted types and parameters, and URI UIDs.
    #[test]
    fn vcard_parses_legacy_and_quoted_syntax() {
        // --- phase: parses_legacy_bare_type_and_unfolds ---
        {
            let vcf = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Grace Hopper\r\nN:Hopper;Grace;;;\r\nTEL;WORK:+1 202 555 0143\r\nNOTE:A very long note that should be fol\r\n ded across two physical lines\r\nEND:VCARD\r\n";
            let back = parse_vcards(vcf);
            assert_eq!(back.len(), 1);
            assert_eq!(back[0].phones[0].label, "work");
            assert_eq!(back[0].phones[0].value, "+1 202 555 0143");
            assert_eq!(
                back[0].notes,
                "A very long note that should be folded across two physical lines"
            );
        }

        // --- phase: parses_multiple_cards ---
        {
            let a = to_vcard(&sample(), VCardVersion::V3);
            let mut b = sample();
            b.first = "Grace".into();
            b.last = "Hopper".into();
            let doc = format!("{a}{}", to_vcard(&b, VCardVersion::V4));
            let back = parse_vcards(&doc);
            assert_eq!(back.len(), 2);
            assert_eq!(back[0].first, "Ada");
            assert_eq!(back[1].first, "Grace");
        }

        // --- phase: parses_uri_tel_scheme ---
        {
            let vcf = "BEGIN:VCARD\nVERSION:4.0\nFN:T B\nTEL;VALUE=uri:tel:+15551234\nURL:https://w3.org\nEND:VCARD\n";
            let back = parse_vcards(vcf);
            assert_eq!(back[0].phones[0].value, "+15551234");
            assert_eq!(back[0].website, "https://w3.org");
        }

        // --- phase: quoted_type_lists_yield_clean_labels ---
        {
            // RFC 6350's own example quotes the TYPE list.
            let vcf = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Q\r\nTEL;VALUE=uri;PREF=1;TYPE=\"voice,home\":tel:+1-555-555-5555\r\nEMAIL;TYPE=\"work\":q@example.com\r\nEND:VCARD\r\n";
            let back = parse_vcards(vcf);
            assert_eq!(back[0].phones[0].label, "home");
            assert_eq!(back[0].phones[0].value, "+1-555-555-5555");
            assert_eq!(back[0].emails[0].label, "work");
        }

        // --- phase: quoted_parameters_with_colons_keep_the_value_intact ---
        {
            let vcf = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Label\r\nADR;LABEL=\"1 Main St\\nSpringfield: IL\";TYPE=home:;;1 Main St;Springfield;IL;62701;US\r\nEND:VCARD\r\n";
            let back = parse_vcards(vcf);
            assert_eq!(back[0].addresses[0].street, "1 Main St");
            assert_eq!(back[0].addresses[0].city, "Springfield");
            assert_eq!(back[0].addresses[0].label, "home");
        }

        // --- phase: uri_uid_is_accepted_for_cross_client_merges ---
        {
            let id = Uuid::new_v4();
            let card = format!(
                "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:{id}\r\nEMAIL:me@example.com\r\nEND:VCARD\r\n"
            );
            let parsed = parse_vcards(&card);
            assert_eq!(parsed.len(), 1);
            assert_eq!(parsed[0].id, id);
        }
    }

    /// Folding stays recoverable within the octet limit, labels and continuations cannot inject lines, and size/card limits reject without partial imports.
    #[test]
    fn vcard_folding_and_limits_resist_injection() {
        // --- phase: folding_keeps_value_recoverable ---
        {
            let mut c = sample();
            c.notes = "x".repeat(200);
            let doc = to_vcard(&c, VCardVersion::V4);
            assert!(doc.contains("\r\n "), "long line must fold");
            assert_eq!(parse_vcards(&doc)[0].notes, c.notes);
        }

        // --- phase: folding_respects_the_octet_limit_for_unicode ---
        {
            let mut c = sample();
            c.notes = "é".repeat(80);
            let doc = to_vcard(&c, VCardVersion::V3);
            for physical in doc.split("\r\n") {
                assert!(
                    physical.len() <= 75,
                    "physical line is {} bytes: {physical:?}",
                    physical.len()
                );
            }
            assert_eq!(parse_vcards(&doc)[0].notes, c.notes);
        }

        // --- phase: free_text_labels_cannot_break_the_content_line ---
        {
            let mut c = sample();
            c.phones = vec![Labeled::new("desk; ext: 12", "+1 555 0100")].into();
            let doc = to_vcard(&c, VCardVersion::V4);
            assert!(
                doc.contains("TEL;TYPE=\"desk; ext: 12\":+1 555 0100"),
                "{doc}"
            );
            let back = parse_vcards(&doc);
            assert_eq!(back[0].phones[0].value, "+1 555 0100");
            assert_eq!(back[0].phones[0].label, "desk; ext: 12");
        }

        // --- phase: continuations_of_an_oversized_property_cannot_smuggle_data ---
        {
            let input = format!(
                "BEGIN:VCARD\r\nFN:Safe name\r\nNOTE:{}\r\n EMAIL:smuggled@example.com\r\nEND:VCARD\r\n",
                "x".repeat(MAX_VCARD_LOGICAL_LINE_BYTES)
            );
            let report = parse_vcards_report(&input);
            assert!(report.contacts.is_empty());
            assert_eq!(report.skipped_cards, 1);
        }

        // --- phase: oversized_properties_are_rejected_without_partial_contact_import ---
        {
            let input = format!(
                "BEGIN:VCARD\r\nFN:Safe name\r\nNOTE:{}\r\nEND:VCARD\r\n",
                "x".repeat(MAX_VCARD_LOGICAL_LINE_BYTES)
            );
            let report = parse_vcards_report(&input);
            assert!(report.contacts.is_empty());
            assert_eq!(report.skipped_cards, 1);
        }

        // --- phase: empty_or_truncated_cards_do_not_create_blank_contacts ---
        {
            assert!(parse_vcards("BEGIN:VCARD\r\nEND:VCARD\r\n").is_empty());
            assert!(parse_vcards("BEGIN:VCARD\r\nFN:Never closed").is_empty());
        }

        // --- phase: parser_stops_at_the_card_limit ---
        {
            let card = "BEGIN:VCARD\r\nFN:Bounded\r\nEND:VCARD\r\n";
            let report = parse_vcards_report(&card.repeat(MAX_VCARD_CARDS + 1));
            assert_eq!(report.contacts.len(), MAX_VCARD_CARDS);
            assert!(report.limit_reached);
        }
    }

    /// Uidless imports get stable contact and row identities that are length-framed and content-sensitive.
    #[test]
    fn uidless_cards_get_stable_framed_identities() {
        // --- phase: uidless_import_has_stable_contact_and_row_identities ---
        {
            let card = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Grace Hopper\r\nN:Hopper;Grace;;;\r\nTEL;TYPE=work:+1 202 555 0143\r\nEMAIL;TYPE=work:grace@example.com\r\nADR;TYPE=home:;;12 Main St;Arlington;VA;22201;US\r\nCATEGORIES:Pioneers,Friends\r\nEND:VCARD\r\n";
            let first = parse_vcards(card).pop().unwrap();
            let second = parse_vcards(card).pop().unwrap();

            assert!(!first.id.is_nil());
            assert_eq!(first.id, second.id, "exact reimports keep one identity");
            assert_eq!(first.phones[0].id, second.phones[0].id);
            assert_eq!(first.emails[0].id, second.emails[0].id);
            assert_eq!(first.addresses[0].id, second.addresses[0].id);
            assert_eq!(first.groups[0].id, second.groups[0].id);
            assert_eq!(first.groups[1].id, second.groups[1].id);
        }

        // --- phase: stable_identity_is_length_framed_and_content_sensitive ---
        {
            assert_ne!(
                deterministic_uuid(["ab", "c"]),
                deterministic_uuid(["a", "bc"])
            );
            let original = parse_vcards(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ada Lovelace\r\nEMAIL:ada@example.com\r\nEND:VCARD\r\n",
        )[0]
        .clone();
            let changed = parse_vcards(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ada Lovelace\r\nEMAIL:ada@elsewhere.example\r\nEND:VCARD\r\n",
        )[0]
        .clone();
            assert_ne!(original.id, changed.id);
        }
    }

    fn png_picture() -> Vec<u8> {
        let image = image::RgbImage::from_pixel(300, 200, image::Rgb([30, 120, 220]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    /// A card's embedded picture becomes the contact's portrait in either
    /// vCard version, a linked one is never fetched, and exports carry it.
    #[test]
    fn embedded_photos_import_and_export_and_links_are_not_fetched() {
        let encoded = ui::held_images::picture_text(&png_picture());
        let v3 = format!(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada Lovelace\r\nPHOTO;ENCODING=b;TYPE=PNG:{encoded}\r\nEND:VCARD\r\n"
        );
        let v4 = format!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ada Lovelace\r\nPHOTO:data:image/png;base64,{encoded}\r\nEND:VCARD\r\n"
        );
        for card in [v3, v4] {
            let contacts = parse_vcards(&card);
            assert_eq!(contacts.len(), 1);
            let photo = contacts[0].photo_bytes().expect("the picture imported");
            let portrait = image::load_from_memory(&photo).unwrap();
            assert_eq!(portrait.width(), ui::held_images::PORTRAIT_SIDE);
            assert_eq!(portrait.width(), portrait.height());

            for version in [VCardVersion::V3, VCardVersion::V4] {
                let back = parse_vcards(&to_vcard(&contacts[0], version));
                assert!(back[0].photo_bytes().is_some(), "{version:?} keeps it");
            }
        }

        let linked = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ada\r\nPHOTO;VALUE=uri:https://example.com/ada.jpg\r\nEND:VCARD\r\n";
        let contacts = parse_vcards(linked);
        assert_eq!(contacts.len(), 1);
        assert!(contacts[0].photo.is_empty());
    }
}
