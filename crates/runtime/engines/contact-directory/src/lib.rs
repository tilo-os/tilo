//! The person's contact directory, as its own library in the runtime bundle
//! (`libtilo_contact_directory_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use tilo_contact_directory;

/// Run the directory's own code: project a contact, confirm one of its
/// endpoints and resolve it back to the person, and canonicalise a mailbox
/// and a phone number.
///
/// The engine's check binary and the app's `--probe-…` call it, so a release
/// proves the library loads and runs against its runtime.
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
