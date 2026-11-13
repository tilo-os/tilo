//! Mail's message codecs and SMTP client, as its own library in the runtime bundle
//! (`libtilo_mail_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

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
        return Err(format!(
            "mail-parser read the subject {:?}",
            message.subject()
        ));
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
