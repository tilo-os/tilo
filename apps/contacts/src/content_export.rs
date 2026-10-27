//! The address book as vCards kept as content, for whoever acts without a
//! file chooser: an agent hands the `.vcf` on (attaches it to a mail,
//! shares it) where a person exports to a file they choose.

use crate::model::Contact;
use crate::resource::Contacts;
use crate::vcard::{VCardVersion, to_vcards};

/// Keep every contact of `book`, or the one `contact` (or `name`, ignoring
/// case) names, as vCard 4.0 in the window's content store; answer its
/// address, how many cards it holds and its size.
///
/// # Errors
/// Returns that no contact matches, that the book is empty, or why the
/// content store refused the file.
pub fn vcards(
    book: &Contacts,
    contact: Option<uuid::Uuid>,
    name: Option<&str>,
) -> Result<serde_json::Value, String> {
    let chosen: Vec<Contact> = match (contact, name.map(str::trim).filter(|n| !n.is_empty())) {
        (Some(id), _) => vec![
            book.contact(id)
                .cloned()
                .ok_or("no such contact; search the address book for its id")?,
        ],
        (None, Some(name)) => vec![named(book, name)?.clone()],
        (None, None) => book.contacts.iter().cloned().collect(),
    };
    if chosen.is_empty() {
        return Err("the address book holds no contacts to export".into());
    }
    let text = to_vcards(&chosen, VCardVersion::V4);
    let store = ui::tilo_resource::content::current()
        .ok_or("this window keeps no content to export into")?;
    let hash = store
        .put(text.as_bytes())
        .map_err(|error| format!("the export could not be kept: {error}"))?;
    let content =
        ui::tilo_actions::content_resource(&hash).ok_or("the export has no content address")?;
    Ok(serde_json::json!({
        "content": content,
        "format": "vcard",
        "media_type": "text/vcard",
        "contacts": chosen.len(),
        "names": chosen.iter().map(Contact::display_name).collect::<Vec<_>>(),
        "bytes": text.len(),
    }))
}

/// The one contact whose shown name is `name`, ignoring case.
fn named<'a>(book: &'a Contacts, name: &str) -> Result<&'a Contact, String> {
    let wanted = name.to_lowercase();
    let mut alike = book
        .contacts
        .iter()
        .filter(|contact| contact.display_name().to_lowercase() == wanted);
    match (alike.next(), alike.next()) {
        (Some(contact), None) => Ok(contact),
        (Some(_), Some(_)) => Err(format!(
            "several contacts are called \u{201c}{name}\u{201d}: give the contact's id"
        )),
        (None, _) => Err(format!("no contact is called \u{201c}{name}\u{201d}")),
    }
}
