//! The person's contact directory, as an engine library beside the shared
//! runtime (`libtilo_contact_directory_engine.so`).
//!
//! Contacts owns the directory (`tilo-contact-directory`); Mail and Messages
//! read it to suggest and resolve recipients. Inside `libruntime_dylib.so`
//! every app process mapped and relocated it. Here it is a library of its
//! own in the runtime bundle that only those three apps name `DT_NEEDED`, and
//! no tapp carries a copy; the runtime keeps `tilo-resource`, which every
//! app uses.
//!
//! Like `runtime-dylib`, this crate re-exports the crate it provides, which
//! records it as provided by this library for the apps' `-C prefer-dynamic`
//! link. Outside the thin build (`thin`) it is empty: see the manifest.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides, which the app's
// link then refuses (each crate may show up once).
#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use tilo_contact_directory;

/// Run the directory's own code: project a contact, confirm one of its
/// endpoints and resolve it back to the person, and canonicalise a mailbox
/// and a phone number.
///
/// The engine's check binary and the apps' `--probe-contact-directory-engine`
/// call it, so a release proves the library loads and runs against the
/// runtime it ships with, and that the apps load it themselves.
///
/// # Errors
/// Names the step the directory answered wrongly.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    use tilo_contact_directory::{ContactDirectory, canonical_email_hint, canonical_phone};
    use tilo_resource::Id;

    let person = Id::from_raw("00000000-0000-4000-8000-000000000001");
    let endpoint = Id::from_raw("00000000-0000-4000-8000-000000000002");
    let recipient = "+436601234567";
    let mut directory = ContactDirectory::default();
    directory
        .project_contact(person.clone(), "Ada".to_owned())
        .map_err(|refusal| format!("the directory refused a contact: {refusal}"))?;
    directory
        .confirm_endpoint(
            person.clone(),
            endpoint,
            "signal".to_owned(),
            recipient.to_owned(),
        )
        .map_err(|refusal| format!("the directory refused an endpoint: {refusal}"))?;
    let resolved = directory
        .resolve("signal", recipient)
        .map_err(|refusal| format!("the directory did not resolve its endpoint: {refusal}"))?;
    if resolved != person {
        return Err(format!(
            "the directory resolved its endpoint to {}",
            resolved.as_str()
        ));
    }
    let mailbox = canonical_email_hint("Ada@Example.ORG");
    if mailbox.as_deref() != Some("Ada@example.org") {
        return Err(format!(
            "the directory canonicalised a mailbox to {mailbox:?}"
        ));
    }
    let phone = canonical_phone("+43 (660) 123-4567");
    if phone.as_deref() != Some(recipient) {
        return Err(format!(
            "the directory canonicalised a phone number to {phone:?}"
        ));
    }
    Ok(format!(
        "people {}, endpoint resolved to its person, mailbox and phone canonical",
        directory.people.len()
    ))
}
