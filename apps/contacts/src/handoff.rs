//! Handing a person's phone number or address to the app on this device that
//! serves it: a call, a message, an email draft.
//!
//! Contacts keeps no dialler, message composer or mailbox of its own. It
//! names the number as the session's URI vocabulary already does — `tel:`,
//! `sms:` and `mailto:` — and the desktop hands it to whichever app (or host
//! handler) claims that scheme, exactly as a website row opens a page. When
//! the desktop answers that nothing here handles it, the window says so; a
//! verb that quietly did nothing would be worse than no verb.
//!
//! A vCard keeps numbers as people write them (`+44 (0)20 7946 0018`,
//! `555 0100`), which are not URIs. [`tel_uri`] and [`sms_uri`] keep the
//! digits and the RFC 3966 separators a number is allowed to carry and drop
//! what a URI cannot: whitespace, extensions and prose.

/// Characters a `tel:` or `sms:` URI may carry besides digits: the RFC 3966
/// dial-string set, which includes the visual separators people type.
fn dial_char(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, '+' | '*' | '#' | '-' | '.' | '(' | ')')
}

/// A number as the dial-string of a `tel:` URI, or `None` when nothing
/// dialable is left.
pub(crate) fn tel_uri(number: &str) -> Option<String> {
    let dial: String = number.chars().filter(|c| dial_char(*c)).collect();
    // A URI with no digits names nobody; a lone `+` or `-` is not a number.
    dial.chars()
        .any(|c| c.is_ascii_digit())
        .then(|| format!("tel:{dial}"))
}

/// A number as the recipient of an `sms:` URI, or `None` when nothing
/// dialable is left.
pub(crate) fn sms_uri(number: &str) -> Option<String> {
    let dial: String = number.chars().filter(|c| dial_char(*c)).collect();
    dial.chars()
        .any(|c| c.is_ascii_digit())
        .then(|| format!("sms:{dial}"))
}

/// The characters an address may keep un-encoded: the URI unreserved set
/// plus `@`, which is what an address is made of.
fn address_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~' | '@' | '+' | '!')
}

/// An address as a `mailto:` URI, or `None` when it is not one (no `@`, both
/// sides empty, or characters a recipient cannot be).
pub(crate) fn mailto_uri(address: &str) -> Option<String> {
    let address = address.trim();
    let (local, domain) = address.split_once('@')?;
    if local.is_empty()
        || domain.is_empty()
        || domain.contains('@')
        || !address.chars().all(address_char)
    {
        return None;
    }
    Some(format!("mailto:{address}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_as_people_write_it_becomes_a_dial_string() {
        assert_eq!(
            tel_uri("+44 (0)20 7946 0018").as_deref(),
            Some("tel:+44(0)2079460018")
        );
        assert_eq!(tel_uri("555 0100").as_deref(), Some("tel:5550100"));
        assert_eq!(tel_uri(" +1-555-0100 ").as_deref(), Some("tel:+1-555-0100"));
    }

    #[test]
    fn a_number_with_nothing_dialable_is_refused_rather_than_guessed() {
        for empty in ["", "   ", "ask Sam", "+", "–"] {
            assert_eq!(tel_uri(empty), None, "{empty:?} names no number");
            assert_eq!(sms_uri(empty), None, "{empty:?} names no number");
        }
    }

    #[test]
    fn a_message_goes_to_the_same_dial_string() {
        assert_eq!(sms_uri("555 0100").as_deref(), Some("sms:5550100"));
    }

    #[test]
    fn an_address_becomes_a_mailto_and_a_non_address_does_not() {
        assert_eq!(
            mailto_uri(" ada@example.test ").as_deref(),
            Some("mailto:ada@example.test")
        );
        assert_eq!(
            mailto_uri("ada+lists@example.test").as_deref(),
            Some("mailto:ada+lists@example.test")
        );
        for refused in [
            "",
            "ada",
            "@example.test",
            "ada@",
            "a@b@c",
            "ada lovelace@x",
        ] {
            assert_eq!(mailto_uri(refused), None, "{refused:?} is not an address");
        }
    }
}
