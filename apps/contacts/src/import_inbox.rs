//! Import the canonical cards a same-computer handoff left for this app.
//!
//! The installer retains the source address book byte for byte, and publishes
//! the cards it actually holds in this app's own data area. Opening the app
//! parses them through the same bounded vCard reader as a chosen file and adds
//! only cards the book does not already hold, so reopening a record is
//! idempotent and a deleted contact is never revived.

use ui::application::HostContext;
use ui::tilo_resource::Replica;

use crate::model::Contact;
use crate::resource::Contacts;

const INBOX: &str = "personal-data-incoming/contacts";
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;

/// Add the cards waiting in this app's import inbox.
pub(crate) fn import(context: &HostContext) -> Result<(), String> {
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
        let Ok(Some(bytes)) =
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
    let held = context
        .read_record(<Contacts as ui::tilo_resource::Resource>::read)
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    let mut seen = std::collections::BTreeSet::new();
    cards.retain(|card| {
        seen.insert(card.id) && held.contact(card.id).is_none() && !held.removed.contains(&card.id)
    });
    if cards.is_empty() {
        return Ok(());
    }
    let author = context.author().0.clone();
    context
        .migrate_record(move |doc| apply(doc, cards, &author).map(|()| true))
        .map(drop)
}

fn apply(doc: &mut automerge::AutoCommit, cards: Vec<Contact>, author: &str) -> Result<(), String> {
    let mut replica = Replica::<Contacts>::from_doc(doc.clone()).as_author(author);
    replica
        .apply_confirmed(Contacts::add_all(cards))
        .map_err(|error| error.to_string())?;
    *doc = replica.doc_mut().clone();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_cards_become_contacts_in_the_target_record() {
        let parsed = crate::vcard::parse_vcards(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:ada@example.test\r\nFN:Ada Lovelace\r\nEMAIL:ada@example.test\r\nEND:VCARD\r\n",
        );
        let mut replica = Replica::<Contacts>::new();
        let mut doc = replica.doc_mut().clone();
        apply(&mut doc, parsed, "firstboot").unwrap();
        let held = <Contacts as ui::tilo_resource::Resource>::read(&doc).unwrap();
        let card = held.contacts.iter().next().unwrap();
        assert_eq!(card.display_name(), "Ada Lovelace");
    }
}
