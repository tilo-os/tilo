//! vCard import/export — hand-rolled parser + serializer covering the
//! 3.0 (RFC 2426) and 4.0 (RFC 6350) flavours an address book actually
//! meets in the wild.
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

use crate::model::{Address, Birthday, Contact, GroupEntry, Labeled};
use uuid::Uuid;

/// Import limits keep a malformed or hostile `.vcf` from monopolising the UI
/// thread.  The app also rejects oversized files before reading them; these
/// bounds protect callers that parse an in-memory document directly.
pub const MAX_VCARD_CARDS: usize = 2_048;
pub const MAX_VCARD_LOGICAL_LINE_BYTES: usize = 32 * 1024;

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
        lines.push(format!("URL:{}", esc(&c.website)));
    }
    if !c.notes.trim().is_empty() {
        lines.push(format!("NOTE:{}", esc(&c.notes)));
    }
    if !c.groups.is_empty() {
        let cats: Vec<String> = c.groups.iter().map(|group| esc(&group.value)).collect();
        lines.push(format!("CATEGORIES:{}", cats.join(",")));
    }

    lines.push("END:VCARD".to_string());

    // Fold long lines to <=75 octets and join with CRLF per the spec.
    let mut doc = String::new();
    for l in lines {
        doc.push_str(&fold_line(&l));
        doc.push_str("\r\n");
    }
    doc
}

/// Build the `TYPE=` parameter. 4.0 lower-cases by convention; 3.0
/// historically used upper-case bare types but `TYPE=` is valid in both.
fn type_param(label: &str, version: VCardVersion) -> String {
    let label = label.trim();
    let label = if label.is_empty() { "other" } else { label };
    match version {
        VCardVersion::V3 => format!("TYPE={}", label.to_uppercase()),
        VCardVersion::V4 => format!("TYPE={}", label.to_lowercase()),
    }
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

/// Escape a value for a vCard content line (RFC 6350 §3.4).
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            ',' => out.push_str("\\,"),
            ';' => out.push_str("\\;"),
            other => out.push(other),
        }
    }
    out
}

/// Fold a content line to 75-*octet* segments, continuation lines prefixed
/// with a single space (RFC 6350 §3.2).  The previous character-counting
/// implementation exceeded the wire limit for non-ASCII names and notes.
/// Split only at UTF-8 character boundaries, so each physical line remains
/// valid UTF-8 and every continuation includes its required leading space in
/// the 75-octet budget.
fn fold_line(line: &str) -> String {
    const LIMIT: usize = 75;
    let mut out = String::with_capacity(line.len() + line.len() / LIMIT * 3);
    let mut used = 0usize;
    let mut continuation = false;
    for ch in line.chars() {
        let bytes = ch.len_utf8();
        // A continuation's leading whitespace consumes one octet.
        let payload_limit = LIMIT - usize::from(continuation);
        if used > 0 && used + bytes > payload_limit {
            out.push_str("\r\n ");
            continuation = true;
            used = 0;
        }
        out.push(ch);
        used += bytes;
    }
    out
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
    let logical = unfold(input);
    let mut report = VCardParseReport::default();
    let mut cur: Option<(Contact, bool)> = None;

    for raw in logical {
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            continue;
        }
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("BEGIN:VCARD") {
            if cur.take().is_some() {
                report.skipped_cards += 1;
            }
            if report.contacts.len() >= MAX_VCARD_CARDS {
                report.limit_reached = true;
                break;
            }
            cur = Some((
                Contact {
                    id: Uuid::new_v4(),
                    ..Default::default()
                },
                false,
            ));
            continue;
        }
        if upper.starts_with("END:VCARD") {
            if let Some((c, invalid)) = cur.take() {
                if !invalid && has_contact_data(&c) {
                    report.contacts.push(c);
                } else {
                    report.skipped_cards += 1;
                }
            }
            continue;
        }
        let Some((contact, invalid)) = cur.as_mut() else {
            continue;
        };
        if line == "\0" {
            *invalid = true;
            continue;
        }
        if line.len() > MAX_VCARD_LOGICAL_LINE_BYTES {
            *invalid = true;
            continue;
        }
        let Some((props, value)) = split_line(line) else {
            continue;
        };
        apply_property(contact, &props, value);
    }
    if cur.is_some() {
        report.skipped_cards += 1;
    }
    report
}

/// RFC line unfolding: any line beginning with SP or TAB continues the
/// previous one (the leading whitespace is dropped).
fn unfold(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in input.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if (line.starts_with(' ') || line.starts_with('\t'))
            && let Some(last) = out.last_mut()
        {
            // The leading char is a single-byte SP/TAB, so byte offset 1 is
            // always a char boundary; `.get` is a defensive, never-panicking
            // way to say so.
            let continuation = line.get(1..).unwrap_or("");
            if last.len().saturating_add(continuation.len()) > MAX_VCARD_LOGICAL_LINE_BYTES {
                // Keep a sentinel that the parser cannot mistake for a
                // property. This prevents a huge folded value from being
                // accumulated or partially imported.
                last.clear();
                last.push('\0');
            } else {
                last.push_str(continuation);
            }
            continue;
        }
        if line.len() > MAX_VCARD_LOGICAL_LINE_BYTES {
            out.push("\0".to_string());
        } else {
            out.push(line.to_string());
        }
    }
    out
}

/// Split a content line into its property part (`NAME;PARAM=…`) and its
/// raw value, on the first unescaped colon.
fn split_line(line: &str) -> Option<(String, &str)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut prev_backslash = false;
    while i < bytes.len() {
        // ':' is ASCII, so a byte-exact match can't land mid-codepoint of a
        // multi-byte char; `.get` still guards it defensively instead of
        // trusting that reasoning at the panic site.
        let c = *bytes.get(i)? as char;
        if c == ':' && !prev_backslash {
            return Some((line.get(..i)?.to_string(), line.get(i + 1..)?));
        }
        prev_backslash = c == '\\' && !prev_backslash;
        i += 1;
    }
    None
}

/// Parsed property descriptor: the upper-cased name plus its params.
struct PropDesc {
    name: String,
    params: Vec<(String, String)>,
}

fn parse_props(props: &str) -> PropDesc {
    // Strip an optional `group.` prefix (e.g. `item1.TEL`).
    let parts = split_unescaped(props, ';');
    let mut iter = parts.into_iter();
    let raw_name = iter.next().unwrap_or_default();
    let name = match raw_name.split_once('.') {
        Some((_group, n)) => n,
        None => raw_name.as_str(),
    }
    .to_ascii_uppercase();

    let mut params = Vec::new();
    for p in iter {
        if let Some((k, v)) = p.split_once('=') {
            params.push((k.to_ascii_uppercase(), v.to_string()));
        } else if !p.is_empty() {
            // Legacy bare type (vCard 2.1/3.0): `TEL;WORK:…`.
            params.push(("TYPE".to_string(), p));
        }
    }
    PropDesc { name, params }
}

/// Collect every `TYPE=` value (comma-joined or repeated), lower-cased,
/// dropping the catch-all `voice`/`internet`/`pref` noise so the label
/// reads cleanly (home/work/mobile/…).
fn collect_types(desc: &PropDesc) -> String {
    let mut types: Vec<String> = Vec::new();
    for (k, v) in &desc.params {
        if k == "TYPE" {
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
    }
    types
        .first()
        .cloned()
        .unwrap_or_else(|| "other".to_string())
}

fn apply_property(c: &mut Contact, props: &str, value: &str) {
    let desc = parse_props(props);
    match desc.name.as_str() {
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
                &collect_types(&desc),
                &unescape(strip_uri(value)),
            ));
        }
        "EMAIL" => {
            c.emails
                .push(Labeled::new(&collect_types(&desc), &unescape(value)));
        }
        "ADR" => {
            let comps = split_unescaped(value, ';');
            // ADR = PO;EXT;STREET;CITY;REGION;POSTAL;COUNTRY
            let g = |i: usize| comps.get(i).map(|s| unescape(s)).unwrap_or_default();
            let addr = Address {
                id: Uuid::new_v4(),
                label: collect_types(&desc),
                street: g(2),
                city: g(3),
                region: g(4),
                postal: g(5),
                country: g(6),
            };
            if !addr.is_empty() {
                c.addresses.push(addr);
            }
        }
        "BDAY" => c.birthday = Birthday::parse(&unescape(value)),
        "URL" => c.website = unescape(strip_uri(value)),
        "NOTE" => c.notes = unescape(value),
        "CATEGORIES" => {
            for cat in split_unescaped(value, ',') {
                let cat = unescape(&cat).trim().to_string();
                if !cat.is_empty() {
                    c.groups.push(GroupEntry::new(cat));
                }
            }
        }
        _ => {}
    }
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

/// Split `s` on `delim`, honouring backslash escaping so an escaped
/// delimiter (`\;` inside a NOTE) doesn't split the value.
fn split_unescaped(s: &str, delim: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_backslash = false;
    for ch in s.chars() {
        if ch == delim && !prev_backslash {
            out.push(std::mem::take(&mut cur));
            continue;
        }
        // Preserve the backslash in the segment; `unescape` resolves it.
        cur.push(ch);
        prev_backslash = ch == '\\' && !prev_backslash;
    }
    out.push(cur);
    out
}

/// Resolve `\n \, \; \\` escape sequences in a value.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(',') => out.push(','),
                Some(';') => out.push(';'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
            phones: vec![Labeled::new("mobile", "+44 20 7946 0101")],
            emails: vec![Labeled::new("work", "ada@example.com")],
            addresses: vec![Address {
                id: Uuid::new_v4(),
                label: "home".into(),
                street: "12 St James's Square".into(),
                city: "London".into(),
                region: String::new(),
                postal: "SW1Y 4LE".into(),
                country: "UK".into(),
            }],
            groups: vec![GroupEntry::new("Work"), GroupEntry::new("Favorites")],
            favorite: true,
        }
    }

    #[test]
    fn round_trip_v3_preserves_fields() {
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
        // Escaped punctuation + newline survives the round trip.
        assert_eq!(r.notes, "Note: with, punctuation; and a\nnewline");
    }

    #[test]
    fn round_trip_v4_uses_basic_bday_and_lowercase_types() {
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

    #[test]
    fn year_less_birthday_round_trips_through_vcard() {
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

    #[test]
    fn absent_birthday_emits_no_bday_line() {
        let mut c = sample();
        c.birthday = None;
        let doc = to_vcard(&c, VCardVersion::V3);
        assert!(!doc.contains("BDAY:"), "no BDAY line when unset: {doc}");
        assert_eq!(parse_vcards(&doc)[0].birthday, None);
    }

    #[test]
    fn parses_legacy_bare_type_and_unfolds() {
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

    #[test]
    fn parses_multiple_cards() {
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

    #[test]
    fn parses_uri_tel_scheme() {
        let vcf = "BEGIN:VCARD\nVERSION:4.0\nFN:T B\nTEL;VALUE=uri:tel:+15551234\nURL:https://w3.org\nEND:VCARD\n";
        let back = parse_vcards(vcf);
        assert_eq!(back[0].phones[0].value, "+15551234");
        assert_eq!(back[0].website, "https://w3.org");
    }

    #[test]
    fn folding_keeps_value_recoverable() {
        let long = "x".repeat(200);
        let folded = fold_line(&format!("NOTE:{long}"));
        assert!(folded.contains("\r\n "), "long line must fold");
        // Unfolding the folded output restores the original logical line.
        let restored = unfold(&format!("{folded}\r\n"));
        assert_eq!(restored[0], format!("NOTE:{long}"));
    }

    #[test]
    fn folding_respects_the_octet_limit_for_unicode() {
        let line = format!("NOTE:{}", "é".repeat(80));
        let folded = fold_line(&line);
        for physical in folded.split("\r\n") {
            assert!(
                physical.len() <= 75,
                "physical line is {} bytes: {physical:?}",
                physical.len()
            );
        }
        assert_eq!(unfold(&format!("{folded}\r\n"))[0], line);
    }

    #[test]
    fn uri_uid_is_accepted_for_cross_client_merges() {
        let id = Uuid::new_v4();
        let card = format!(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:{id}\r\nEMAIL:me@example.com\r\nEND:VCARD\r\n"
        );
        let parsed = parse_vcards(&card);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, id);
    }

    #[test]
    fn empty_or_truncated_cards_do_not_create_blank_contacts() {
        assert!(parse_vcards("BEGIN:VCARD\r\nEND:VCARD\r\n").is_empty());
        assert!(parse_vcards("BEGIN:VCARD\r\nFN:Never closed").is_empty());
    }

    #[test]
    fn oversized_properties_are_rejected_without_partial_contact_import() {
        let input = format!(
            "BEGIN:VCARD\r\nFN:Safe name\r\nNOTE:{}\r\nEND:VCARD\r\n",
            "x".repeat(MAX_VCARD_LOGICAL_LINE_BYTES)
        );
        let report = parse_vcards_report(&input);
        assert!(report.contacts.is_empty());
        assert_eq!(report.skipped_cards, 1);
    }

    #[test]
    fn parser_stops_at_the_card_limit() {
        let card = "BEGIN:VCARD\r\nFN:Bounded\r\nEND:VCARD\r\n";
        let report = parse_vcards_report(&card.repeat(MAX_VCARD_CARDS + 1));
        assert_eq!(report.contacts.len(), MAX_VCARD_CARDS);
        assert!(report.limit_reached);
    }
}
