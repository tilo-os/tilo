//! Mail's message codecs and SMTP client, as an engine library beside the
//! shared runtime (`libtilo_mail_engine.so`).
//!
//! Mail reads messages with `mail-parser`, writes them with `mail-builder`
//! and sends them over SMTP with `lettre`. No other app reads or sends mail,
//! so none of them belongs in the runtime every app maps; linked into the
//! tapp they were about a tenth of Mail's code. Here they are a library of
//! their own in the runtime bundle that only Mail names `DT_NEEDED`, beside
//! the accounts engine its IMAP client comes from.
//!
//! Like `runtime-dylib`, this crate re-exports the crates it provides, which
//! records them as provided by this library for the apps' `-C prefer-dynamic`
//! link. Outside the thin build (`thin`) it is empty: see the manifest.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides, which the app's
// link then refuses (each crate may show up once).
#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use lettre;
#[cfg(feature = "thin")]
pub use mail_builder;
#[cfg(feature = "thin")]
pub use mail_parser;

/// Run the engine's own code: write a message with `mail-builder`, read its
/// subject and sender back with `mail-parser`, and address an SMTP envelope
/// to its sender with `lettre`.
///
/// The engine's check binary calls it, so a release proves the library loads
/// and runs against the runtime it ships with.
///
/// # Errors
/// Names the step that failed or what was read back wrongly.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    use std::str::FromStr as _;

    let written = mail_builder::MessageBuilder::new()
        .from(("Ada", "ada@example.org"))
        .to("bob@example.org")
        .subject("Engine check")
        .text_body("Hello")
        .write_to_vec()
        .map_err(|error| format!("mail-builder could not write the message: {error}"))?;
    let message = mail_parser::MessageParser::default()
        .parse(&written)
        .ok_or("mail-parser could not read the message back")?;
    if message.subject() != Some("Engine check") {
        return Err(format!("mail-parser read the subject {:?}", message.subject()));
    }
    let sender = message
        .from()
        .and_then(|from| from.first())
        .and_then(|address| address.address())
        .ok_or("mail-parser read no sender")?;
    let address = lettre::Address::from_str(sender)
        .map_err(|error| format!("lettre refused the sender {sender}: {error}"))?;
    let envelope = lettre::address::Envelope::new(Some(address.clone()), vec![address])
        .map_err(|error| format!("lettre refused the envelope: {error}"))?;
    Ok(format!(
        "wrote and read back a message of {} bytes, enveloped to {}",
        written.len(),
        envelope.to().len()
    ))
}
