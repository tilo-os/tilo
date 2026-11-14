//! An address book's contacts, as the JSON Tilo Contacts' `add-contacts` verb
//! takes. Every id is derived from the profile, the book and the card's own
//! `UID`, so landing the same card again names the same contact.

use std::path::Path;

use pim_lines::{ContentLine, Unfolded, split_unescaped, unescape_text};
use serde_json::{Value, json};

use super::personal_data::{self, Sidecar};
use super::{MAX_SIDECAR_BYTES, find_reviewed, find_sidecar, read_reviewed};

/// The cards of one address book as Contacts' `Contact` values.
///
/// `profile` names a profile by its `source_id`, its label or one of its
/// accounts' addresses; `book` names a book by its `source_id` or its name.
///
/// # Errors
/// The home holds no such profile or book, the book is not a local file, or it
/// could not be read within its bounds.
pub fn contacts(home: &Path, profile: &str, book: &str) -> Result<Vec<Value>, String> {
    let scan = super::scan(home);
    let reviewed = find_reviewed(&scan.reviewed, profile)
        .ok_or_else(|| format!("no Thunderbird profile {profile:?}"))?;
    let file = find_sidecar(&reviewed.address_books, book)
        .ok_or_else(|| format!("no address book {book:?} in that profile"))?;
    if !file.present || !file.local {
        return Err("that address book is not a local file".into());
    }
    let relative = file
        .relative
        .as_deref()
        .ok_or_else(|| "the address book has no file".to_owned())?;
    let bytes =
        read_reviewed(reviewed, relative, MAX_SIDECAR_BYTES).map_err(|error| error.to_string())?;
    let text = personal_data::canonical(Sidecar::AddressBook, &bytes)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "that address book holds no cards this reader understands".to_owned())?;
    Ok(parse(&reviewed.profile.source_id, &file.source_id, &text))
}

fn parse(profile: &str, book: &str, text: &str) -> Vec<Value> {
    let mut cards = Vec::new();
    let mut current: Option<Card> = None;
    for line in pim_lines::unfold_bounded(text, 4 * 1024 * 1024) {
        let Unfolded::Line(raw) = line else {
            // An oversized property invalidates its card rather than being
            // imported in part.
            if let Some(card) = current.as_mut() {
                card.invalid = true;
            }
            continue;
        };
        let Some(line) = ContentLine::parse(&raw) else {
            continue;
        };
        if line.begins("VCARD") {
            current = Some(Card::default());
            continue;
        }
        if line.ends("VCARD") {
            if let Some(card) = current.take()
                && !card.invalid
                && card.has_data()
            {
                cards.push(card.to_value(profile, book));
            }
            continue;
        }
        if let Some(card) = current.as_mut() {
            card.apply(&line);
        }
        if cards.len() > 100_000 {
            break;
        }
    }
    cards
}

#[derive(Default)]
struct Card {
    uid: Option<String>,
    first: String,
    last: String,
    org: String,
    title: String,
    website: String,
    notes: String,
    birthday: Option<Value>,
    phones: Vec<(String, String)>,
    emails: Vec<(String, String)>,
    addresses: Vec<Address>,
    groups: Vec<String>,
    invalid: bool,
}

#[derive(Default)]
struct Address {
    label: String,
    street: String,
    city: String,
    region: String,
    postal: String,
    country: String,
}

impl Card {
    fn apply(&mut self, line: &ContentLine) {
        let value = line.value.as_str();
        match line.name.as_str() {
            "UID" => {
                let uid = unescape_text(value);
                let uid = uid.trim();
                if !uid.is_empty() {
                    self.uid = Some(uid.chars().take(512).collect());
                }
            }
            "FN" => {
                if self.first.is_empty() && self.last.is_empty() {
                    let name = unescape_text(value);
                    match name.split_once(' ') {
                        Some((first, last)) => {
                            self.first = String::from(first.trim());
                            self.last = String::from(last.trim());
                        }
                        None => self.first = String::from(name.trim()),
                    }
                }
            }
            "N" => {
                let parts = split_unescaped(value, ';');
                if let Some(last) = parts.first() {
                    self.last = String::from(unescape_text(last).trim());
                }
                if let Some(first) = parts.get(1) {
                    self.first = String::from(unescape_text(first).trim());
                }
            }
            "ORG" => {
                let parts = split_unescaped(value, ';');
                self.org = parts
                    .first()
                    .map(|part| String::from(unescape_text(part).trim()))
                    .unwrap_or_default();
            }
            "TITLE" => self.title = String::from(unescape_text(value).trim()),
            "TEL" => self.phones.push((
                label(line),
                String::from(unescape_text(strip_uri(value)).trim()),
            )),
            "EMAIL" => self
                .emails
                .push((label(line), String::from(unescape_text(value).trim()))),
            "ADR" => {
                let parts = split_unescaped(value, ';');
                let part = |index: usize| {
                    parts
                        .get(index)
                        .map(|part| String::from(unescape_text(part).trim()))
                        .unwrap_or_default()
                };
                let street = (0..3)
                    .map(part)
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                let address = Address {
                    label: label(line),
                    street,
                    city: part(3),
                    region: part(4),
                    postal: part(5),
                    country: part(6),
                };
                if !address.is_empty() {
                    self.addresses.push(address);
                }
            }
            "BDAY" => self.birthday = birthday(line),
            "URL" => self.website = String::from(unescape_text(strip_uri(value)).trim()),
            "NOTE" => self.notes = unescape_text(value),
            "CATEGORIES" => {
                for group in split_unescaped(value, ',') {
                    let group = String::from(unescape_text(group).trim());
                    if !group.is_empty() && !self.groups.contains(&group) {
                        self.groups.push(group);
                    }
                }
            }
            _ => {}
        }
    }

    fn has_data(&self) -> bool {
        !self.first.is_empty()
            || !self.last.is_empty()
            || !self.org.is_empty()
            || !self.title.is_empty()
            || !self.website.is_empty()
            || self.birthday.is_some()
            || !self.notes.is_empty()
            || !self.groups.is_empty()
            || !self.phones.is_empty()
            || !self.emails.is_empty()
            || !self.addresses.is_empty()
    }

    fn to_value(&self, profile: &str, book: &str) -> Value {
        let card_key = self
            .uid
            .clone()
            .unwrap_or_else(|| blake3::hash(self.content().as_bytes()).to_hex().to_string());
        let id = deterministic_id(&["contact", profile, book, &card_key]);
        let phones: Vec<Value> = self
            .phones
            .iter()
            .enumerate()
            .filter(|(_, (_, value))| !value.is_empty())
            .map(|(index, (label, value))| {
                json!({
                    "id": deterministic_id(&[&id, "phone", &index.to_string(), label, value]),
                    "label": label,
                    "value": value,
                })
            })
            .collect();
        let emails: Vec<Value> = self
            .emails
            .iter()
            .enumerate()
            .filter(|(_, (_, value))| !value.is_empty())
            .map(|(index, (label, value))| {
                json!({
                    "id": deterministic_id(&[&id, "email", &index.to_string(), label, value]),
                    "label": label,
                    "value": value,
                })
            })
            .collect();
        let addresses: Vec<Value> = self
            .addresses
            .iter()
            .enumerate()
            .map(|(index, address)| {
                json!({
                    "id": deterministic_id(&[&id, "address", &index.to_string(), &address.label, &address.street, &address.city, &address.region, &address.postal, &address.country]),
                    "label": address.label,
                    "street": address.street,
                    "city": address.city,
                    "region": address.region,
                    "postal": address.postal,
                    "country": address.country,
                })
            })
            .collect();
        let groups: Vec<Value> = self
            .groups
            .iter()
            .enumerate()
            .map(|(index, group)| {
                json!({
                    "id": deterministic_id(&[&id, "group", &index.to_string(), group]),
                    "value": group,
                })
            })
            .collect();
        json!({
            "id": id,
            "first": self.first,
            "last": self.last,
            "org": self.org,
            "title": self.title,
            "website": self.website,
            "notes": self.notes,
            "birthday": self.birthday.clone().unwrap_or(Value::Null),
            "phones": phones,
            "emails": emails,
            "addresses": addresses,
            "groups": groups,
            "favorite": false,
        })
    }

    /// The card's own properties, for a card that named no `UID`: the derived
    /// id then changes only when the card itself does.
    fn content(&self) -> String {
        let mut lines = vec![
            format!("{}\u{1f}{}", self.first, self.last),
            self.org.clone(),
            self.title.clone(),
            self.website.clone(),
            self.notes.clone(),
            format!("{:?}", self.birthday),
        ];
        for (label, value) in &self.phones {
            lines.push(format!("tel\u{1f}{label}\u{1f}{value}"));
        }
        for (label, value) in &self.emails {
            lines.push(format!("email\u{1f}{label}\u{1f}{value}"));
        }
        for address in &self.addresses {
            lines.push(format!(
                "adr\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
                address.label,
                address.street,
                address.city,
                address.region,
                address.postal,
                address.country
            ));
        }
        for group in &self.groups {
            lines.push(format!("group\u{1f}{group}"));
        }
        lines.join("\n")
    }
}

impl Address {
    fn is_empty(&self) -> bool {
        self.street.is_empty()
            && self.city.is_empty()
            && self.region.is_empty()
            && self.postal.is_empty()
            && self.country.is_empty()
    }
}

/// Every `TYPE=` value, lower-cased, dropping the protocol noise; the first is
/// the label Contacts shows.
fn label(line: &ContentLine) -> String {
    let mut labels = Vec::new();
    let mut values: Vec<&str> = Vec::new();
    for param in &line.params {
        if param.values.is_empty() {
            values.push(&param.name);
        } else if param.name == "TYPE" {
            values.extend(param.values.iter().map(String::as_str));
        }
    }
    for value in values {
        for item in value.split(',') {
            let item = item.trim().to_lowercase();
            if item.is_empty() || matches!(item.as_str(), "voice" | "internet" | "pref") {
                continue;
            }
            if !labels.contains(&item) {
                labels.push(item);
            }
        }
    }
    labels
        .first()
        .cloned()
        .unwrap_or_else(|| "other".to_owned())
}

fn strip_uri(value: &str) -> &str {
    for scheme in ["tel:", "mailto:"] {
        if let Some(rest) = value.strip_prefix(scheme) {
            return rest;
        }
    }
    value
}

/// `BDAY` in the forms other clients write: `YYYY-MM-DD`, `YYYYMMDD`, a
/// year-less `--MM-DD`/`--MMDD`, or Apple's placeholder year.
fn birthday(line: &ContentLine) -> Option<Value> {
    let value = unescape_text(&line.value);
    let date = value
        .split_once('T')
        .map_or(value.as_str(), |(date, _)| date);
    let date = date.trim();
    let yearless = date.strip_prefix("--");
    let digits: String = date.chars().filter(char::is_ascii_digit).collect();
    let omitted = line
        .param_value("X-APPLE-OMIT-YEAR")
        .and_then(|year| year.trim().parse::<i32>().ok());
    let (year, month, day) = match (yearless, digits.len()) {
        (Some(_), 4) => (None, digits.get(0..2)?, digits.get(2..4)?),
        (None, 8) => (
            Some(digits.get(0..4)?.parse().ok()?),
            digits.get(4..6)?,
            digits.get(6..8)?,
        ),
        _ => return None,
    };
    let month: u32 = month.parse().ok()?;
    let day: u32 = day.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    match year {
        Some(year) if omitted != Some(year) => Some(json!({
            "Full": { "year": year, "month": month, "day": day }
        })),
        _ => Some(json!({ "MonthDay": { "month": month, "day": day } })),
    }
}

/// A UUID derived from `parts` with BLAKE3. The same parts always name the
/// same UUID, so a re-import is idempotent and two peers agree.
#[must_use]
pub fn deterministic_id(parts: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_becomes_the_shape_contacts_takes() {
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada@example.test\r\nFN:Ada Lovelace\r\nN:Lovelace;Ada;;;\r\nORG:Analytical Engines\r\nTITLE:Mathematician\r\nTEL;TYPE=cell:+44 20 7946 0101\r\nEMAIL;TYPE=work:ada@example.test\r\nADR;TYPE=home:;;12 St James's Square;London;Greater London;SW1Y 4JH;UK\r\nBDAY:1815-12-10\r\nURL:https://example.test/ada\r\nNOTE:First programmer\r\nCATEGORIES:Friends,Friends\r\nEND:VCARD\r\n";
        let cards = parse("profile-1", "book-1", card);
        assert_eq!(cards.len(), 1);
        let contact = &cards[0];
        assert_eq!(contact["first"], "Ada");
        assert_eq!(contact["last"], "Lovelace");
        assert_eq!(contact["org"], "Analytical Engines");
        assert_eq!(contact["title"], "Mathematician");
        assert_eq!(contact["phones"][0]["label"], "cell");
        assert_eq!(contact["emails"][0]["value"], "ada@example.test");
        assert_eq!(contact["addresses"][0]["city"], "London");
        assert_eq!(contact["birthday"]["Full"]["year"], 1815);
        assert_eq!(contact["groups"].as_array().unwrap().len(), 1);
        assert!(uuid::Uuid::parse_str(contact["id"].as_str().unwrap()).is_ok());
        let phone_id = contact["phones"][0]["id"].as_str().unwrap();
        assert!(uuid::Uuid::parse_str(phone_id).is_ok());
        assert_ne!(contact["id"], phone_id);
    }

    #[test]
    fn the_same_card_lands_as_the_same_contact() {
        let card = "BEGIN:VCARD\r\nUID:same\r\nFN:Same Person\r\nEND:VCARD\r\n";
        assert_eq!(
            parse("p", "b", card),
            parse("p", "b", card),
            "landing the same card twice names the same contact"
        );
        assert_ne!(parse("p", "b", card), parse("p", "other", card));
    }
}
