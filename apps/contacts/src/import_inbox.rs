//! Import the canonical cards a same-computer handoff left for this app.
//!
//! The installer publishes the cards it holds in this app's own data area;
//! opening the app parses them through the same bounded vCard reader as a chosen
//! file and adds only cards the book does not already hold, so reopening a
//! resource is idempotent and a deleted contact is never revived.

use std::rc::Rc;

use ui::application::ProcessContext;
use ui::framework::Chain;

use crate::contact_cards::{Book, ContactCards};
use crate::model::Contact;
use crate::resource::Contacts;

const INBOX: &str = "personal-data-incoming/contacts";
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn import(context: &ProcessContext) -> Result<(), String> {
    let files = context.files();
    let data = files.data().map_err(|error| error.to_string())?;
    let Ok(inbox) = tilo_files::VPath::new(INBOX) else {
        return Ok(());
    };
    let Ok(entries) = tilo_files::blocking::list(&data, &inbox) else {
        return Ok(());
    };
    let mut cards: Vec<Contact> = Vec::new();
    for entry in entries {
        if entry.kind != tilo_files::Kind::File
            || !std::path::Path::new(&entry.name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("vcf"))
        {
            continue;
        }
        let Ok(name) = tilo_files::VPath::new(&entry.name) else {
            continue;
        };
        let Ok(bytes) =
            tilo_files::blocking::read_bounded(&data, &inbox.join(&name), MAX_FILE_BYTES)
        else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        cards.extend(crate::vcard::parse_vcards_report(text).contacts);
    }
    if cards.is_empty() {
        return Ok(());
    }
    // The cards join the parts beside the book, through their own verbs, as
    // the window adds them.
    let author = context.author().clone();
    let chain = Chain::<ContactCards>::detached_in(
        context.records().clone(),
        ui::apps::CONTACTS,
        &author,
        || Rc::new(Contacts::default()),
    )
    .ok_or_else(|| "the address book cannot be opened".to_owned())?;
    let book = Book::of(&chain.parts());
    let mut seen = std::collections::BTreeSet::new();
    cards.retain(|card| {
        seen.insert(card.id) && book.contact(card.id).is_none() && !book.removed.contains(&card.id)
    });
    if cards.is_empty() {
        return Ok(());
    }
    chain
        .invoke(&ContactCards::add_all(cards))
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_cards_become_contacts_in_the_target_resource() {
        let parsed = crate::vcard::parse_vcards(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:ada@example.test\r\nFN:Ada Lovelace\r\nEMAIL:ada@example.test\r\nEND:VCARD\r\n",
        );
        let mut part = ui::tilo_resource::Replica::<ContactCards>::new();
        part.apply(ContactCards::add_all(parsed)).unwrap();
        let held = part.state().unwrap();
        let card = held.contacts.iter().next().unwrap();
        assert_eq!(card.display_name(), "Ada Lovelace");
    }
}
