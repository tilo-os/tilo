//! What leaves with a message: whom it is sent as and every address it is
//! handed to its server for, read once from what was begun.
//!
//! A message names its recipients as text: three lists a person typed or an
//! agent wrote. The envelope a server is handed, the head the message itself
//! carries and whatever shows a person the message before it leaves have to
//! agree on what that text means. So it is read in one place, into a
//! [`Leaving`], and everything afterwards is made from that reading, never
//! from the text again.
//!
//! The reading is strict. An entry that is no address refuses the message
//! instead of being left out of the envelope and written into the head; a
//! comma inside a quoted name separates nothing; a group of addresses under
//! one name is refused; and a line break, or any other character a line of a
//! message's head cannot hold, refuses it too, whichever field carries it: it
//! would begin a header of the writer's own.

use tilo_actions::remote::person_accounts::SendMail;

/// One recipient of a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recipient {
    name: String,
    address: lettre::Address,
}

impl Recipient {
    /// The name the message's head shows beside the address; empty when it
    /// was given none. It is a name and nothing else: the message goes to
    /// [`Self::address`] whatever the name reads as.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The address the message is handed to its server for.
    #[must_use]
    pub fn address(&self) -> &str {
        self.address.as_ref()
    }
}

/// A message read for sending: what it says, whom it is sent as and whom it
/// goes to. Its recipients are read once, by [`Leaving::read`]; the envelope
/// and the message's head are made from that reading, and so is whatever
/// shows the message to a person before it leaves.
#[derive(Clone, Debug)]
pub struct Leaving {
    message: SendMail,
    sender: lettre::Address,
    to: Vec<Recipient>,
    cc: Vec<Recipient>,
    bcc: Vec<Recipient>,
}

impl Leaving {
    /// Read `message` for sending.
    ///
    /// # Errors
    /// Why it cannot be sent as it stands: a field of its head holds a line
    /// break or another control character, an entry of a list is no address
    /// or a group of them, the sending address is none, or it names nobody.
    pub fn read(mut message: SendMail) -> Result<Self, String> {
        one_line(
            "the sender's name",
            message.from_name.as_deref().unwrap_or_default(),
        )?;
        one_line("the sending address", &message.from_address)?;
        one_line("the subject", &message.subject)?;
        let ids = message.message_id.iter().chain(&message.in_reply_to);
        for id in ids.chain(&message.references) {
            one_line("an id that joins the message to its conversation", id)?;
        }
        let sender = message
            .from_address
            .parse::<lettre::Address>()
            .map_err(|error| format!("the sending address is not usable: {error}"))?;
        let to = recipients("To", &message.to)?;
        let cc = recipients("Copy to", &message.cc)?;
        let bcc = recipients("Blind copy to", &message.bcc)?;
        if to.is_empty() && cc.is_empty() && bcc.is_empty() {
            return Err("a message needs a recipient".into());
        }
        // From here on the lists say what was read and nothing else, to
        // whoever still reads them as text.
        message.to = written(&to);
        message.cc = written(&cc);
        message.bcc = written(&bcc);
        Ok(Self {
            message,
            sender,
            to,
            cc,
            bcc,
        })
    }

    /// What the message says. Its lists of recipients are written anew from
    /// what was read of them.
    #[must_use]
    pub const fn message(&self) -> &SendMail {
        &self.message
    }

    /// The address it is sent as.
    #[must_use]
    pub fn sender(&self) -> &str {
        self.sender.as_ref()
    }

    /// Whom it is addressed to.
    #[must_use]
    pub fn to(&self) -> &[Recipient] {
        &self.to
    }

    /// Whom a copy goes to.
    #[must_use]
    pub fn cc(&self) -> &[Recipient] {
        &self.cc
    }

    /// Whom a copy goes to that no other recipient is told of.
    #[must_use]
    pub fn bcc(&self) -> &[Recipient] {
        &self.bcc
    }

    /// Everyone it is handed to its server for: the envelope's recipients,
    /// in its order.
    pub fn recipients(&self) -> impl Iterator<Item = &Recipient> {
        self.to.iter().chain(&self.cc).chain(&self.bcc)
    }

    /// The envelope its server is handed: its sender and every recipient.
    pub(crate) fn envelope(&self) -> Result<lettre::address::Envelope, String> {
        let recipients = self
            .recipients()
            .map(|recipient| recipient.address.clone())
            .collect();
        lettre::address::Envelope::new(Some(self.sender.clone()), recipients)
            .map_err(|error| format!("the message's addresses are not usable: {error}"))
    }
}

/// Why `text` is no line of a message's head, if it is none: a line break or
/// any other control character in it would end the line it is written into.
fn one_line(what: &str, text: &str) -> Result<(), String> {
    if text.chars().any(char::is_control) {
        return Err(format!(
            "{what} holds a line break or another character a message's head cannot carry"
        ));
    }
    Ok(())
}

/// `text` as a refusal quotes it: escaped, and no longer than a line.
fn quoted(text: &str) -> String {
    let start: String = text.chars().take(80).collect();
    format!("{start:?}")
}

/// The recipients the list `what` names, in its order.
fn recipients(what: &str, list: &str) -> Result<Vec<Recipient>, String> {
    one_line(&format!("the list {what}"), list)?;
    entries(what, list)?
        .into_iter()
        .map(|entry| recipient(what, entry))
        .collect()
}

/// The entries of a list: what stands between its commas and semicolons. One
/// inside a quoted name or between an address's angle brackets separates
/// nothing.
fn entries<'a>(what: &str, list: &'a str) -> Result<Vec<&'a str>, String> {
    let unclosed = || format!("the list {what} holds a name or an address that is not closed");
    let mut entries = Vec::new();
    let (mut start, mut quoted, mut escaped, mut angled) = (0, false, false, false);
    for (at, character) in list.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' if !angled => quoted = !quoted,
            '<' if !quoted => {
                if angled {
                    return Err(unclosed());
                }
                angled = true;
            }
            '>' if !quoted => {
                if !angled {
                    return Err(unclosed());
                }
                angled = false;
            }
            ',' | ';' if !quoted && !angled => {
                entries.push(list.get(start..at).unwrap_or_default());
                start = at.saturating_add(character.len_utf8());
            }
            _ => {}
        }
    }
    if quoted || angled {
        return Err(unclosed());
    }
    entries.push(list.get(start..).unwrap_or_default());
    Ok(entries
        .into_iter()
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect())
}

/// Where the first of `entry`'s characters `wanted` stands outside a quoted
/// name.
fn outside_quotes(entry: &str, wanted: char) -> Option<usize> {
    let (mut quoted, mut escaped) = (false, false);
    for (at, character) in entry.char_indices() {
        if escaped {
            escaped = false;
        } else if quoted && character == '\\' {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if !quoted && character == wanted {
            return Some(at);
        }
    }
    None
}

/// One entry of the list `what` as a recipient: an address alone, or a name
/// and the address in angle brackets after it.
fn recipient(what: &str, entry: &str) -> Result<Recipient, String> {
    let no_address = || {
        format!(
            "the list {what} names {}, which is not an address",
            quoted(entry)
        )
    };
    let a_group = || {
        format!(
            "the list {what} names a group of addresses ({}): name each address by itself",
            quoted(entry)
        )
    };
    let Some(open) = outside_quotes(entry, '<') else {
        // An address alone. A colon outside one is how a group is named.
        return match entry.parse::<lettre::Address>() {
            Ok(address) => Ok(Recipient {
                name: String::new(),
                address,
            }),
            Err(_) if outside_quotes(entry, ':').is_some() => Err(a_group()),
            Err(_) => Err(no_address()),
        };
    };
    let (name, rest) = entry.split_at(open);
    if outside_quotes(name, ':').is_some() {
        return Err(a_group());
    }
    // One address, and nothing after it: a second pair of brackets is a
    // second address the first would hide.
    let address = rest
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .filter(|address| !address.contains(['<', '>']))
        .ok_or_else(no_address)?;
    let address = address
        .trim()
        .parse::<lettre::Address>()
        .map_err(|_| no_address())?;
    Ok(Recipient {
        name: plain_name(name.trim()),
        address,
    })
}

/// A name as it was meant: without the quotation marks around it and the
/// backslashes that let it hold one. A name that is not one quoted piece
/// stays as it was written.
fn plain_name(name: &str) -> String {
    let Some(inside) = name
        .strip_prefix('"')
        .and_then(|name| name.strip_suffix('"'))
    else {
        return name.to_owned();
    };
    let mut plain = String::with_capacity(inside.len());
    let mut escaped = false;
    for character in inside.chars() {
        if escaped {
            plain.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return name.to_owned();
        } else {
            plain.push(character);
        }
    }
    plain
}

/// `recipients` as a list reads them back: each address, with its name
/// quoted before it when it has one.
fn written(recipients: &[Recipient]) -> String {
    recipients
        .iter()
        .map(|recipient| {
            if recipient.name.is_empty() {
                return recipient.address().to_owned();
            }
            let name = recipient.name.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{name}\" <{}>", recipient.address())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    fn to(list: &str) -> SendMail {
        SendMail {
            account: "ada@example.test".into(),
            from_address: "ada@example.test".into(),
            to: list.into(),
            subject: "Lunch".into(),
            body: "Friday at one works.".into(),
            ..SendMail::default()
        }
    }

    /// The addresses and names `list` is read as.
    fn read(list: &str) -> Vec<(String, String)> {
        Leaving::read(to(list))
            .unwrap_or_else(|refused| panic!("{list:?} reads: {refused}"))
            .to()
            .iter()
            .map(|one| (one.name().to_owned(), one.address().to_owned()))
            .collect()
    }

    fn pair(name: &str, address: &str) -> (String, String) {
        (name.to_owned(), address.to_owned())
    }

    /// A list is read as a mail program writes one: addresses alone or after
    /// a name, separated by commas or semicolons, with blanks and empty
    /// entries meaning nothing. What separates inside a quoted name
    /// separates nothing, and the address is the one in the brackets
    /// whatever the name before it reads as.
    #[test]
    fn a_list_is_read_into_the_addresses_it_names() {
        assert_eq!(read("bo@example.test"), [pair("", "bo@example.test")]);
        assert_eq!(
            read(" Bo <bo@example.test> ;; cy@example.test, "),
            [pair("Bo", "bo@example.test"), pair("", "cy@example.test")]
        );
        assert_eq!(
            read("\"Lee, Sam; Dr.\" <sam@example.test>, \"A \\\"B\\\" C\" <abc@example.test>"),
            [
                pair("Lee, Sam; Dr.", "sam@example.test"),
                pair("A \"B\" C", "abc@example.test")
            ]
        );
        assert_eq!(
            read("bo@example.test <eve@elsewhere.test>, \"cy@example.test\" <dee@elsewhere.test>"),
            [
                pair("bo@example.test", "eve@elsewhere.test"),
                pair("cy@example.test", "dee@elsewhere.test")
            ]
        );
        let far = format!("bo@example.test,{}eve@elsewhere.test", " ".repeat(700));
        assert_eq!(
            read(&far),
            [pair("", "bo@example.test"), pair("", "eve@elsewhere.test")]
        );
    }

    /// What is no address refuses the message; nothing is left out of the
    /// envelope in silence. So does a group, a second address behind the
    /// first, a name or an address left open, and a line break anywhere in
    /// the message's head.
    #[test]
    fn what_is_no_list_of_addresses_refuses_the_message() {
        for list in [
            "Bo",
            "bo@example.test, Bo",
            "bo@example.test eve@elsewhere.test",
            "Bo <bo@example.test> <eve@elsewhere.test>",
            "Bo <bo@example.test> eve@elsewhere.test",
            "Bo <bo@example.test",
            "\"Bo <bo@example.test>",
            "bo@example.test>",
            "friends: bo@example.test, eve@elsewhere.test;",
            "friends: Bo <bo@example.test>;",
            "bo@example.test\r\nBcc: eve@elsewhere.test",
            "bo@example.test\teve@elsewhere.test",
            "",
            " , ; ",
        ] {
            let refused = Leaving::read(to(list));
            assert!(refused.is_err(), "{list:?} read as {refused:?}");
        }
        let lunch = || to("bo@example.test");
        let smuggled = "1@example.test>\r\nReply-To: <eve@elsewhere.test".to_owned();
        for message in [
            SendMail {
                subject: "Lunch\r\nReply-To: eve@elsewhere.test".into(),
                ..lunch()
            },
            SendMail {
                from_name: Some("Ada\nReply-To: eve@elsewhere.test".into()),
                ..lunch()
            },
            SendMail {
                from_address: "ada@example.test>\r\nReply-To: <eve@elsewhere.test".into(),
                ..lunch()
            },
            SendMail {
                from_address: "Ada".into(),
                ..lunch()
            },
            SendMail {
                cc: "Cy".into(),
                ..lunch()
            },
            SendMail {
                bcc: "dee@example.test\u{b}eve@elsewhere.test".into(),
                ..lunch()
            },
            SendMail {
                message_id: Some(smuggled.clone()),
                ..lunch()
            },
            SendMail {
                in_reply_to: Some(smuggled.clone()),
                ..lunch()
            },
            SendMail {
                references: vec!["0@example.test".into(), smuggled],
                ..lunch()
            },
        ] {
            let refused = Leaving::read(message.clone());
            assert!(refused.is_err(), "{message:?} read as {refused:?}");
        }
    }

    /// What was read is what the envelope carries, in the lists' order, and
    /// what the lists say afterwards reads back as the very same recipients:
    /// nobody who reads them as text again finds another address there.
    #[test]
    fn the_envelope_and_the_lists_say_what_was_read() {
        let message = SendMail {
            cc: "\"Lee, Sam\" <sam@example.test>; \"a\\\\b \\\"c\\\"\" <abc@example.test>".into(),
            bcc: "bo@example.test <eve@elsewhere.test>".into(),
            ..to("Bo <bo@example.test>, cy@example.test")
        };
        let leaving = Leaving::read(message).unwrap();
        let envelope = leaving.envelope().unwrap();
        let handed: Vec<String> = envelope.to().iter().map(ToString::to_string).collect();
        assert_eq!(
            handed,
            [
                "bo@example.test",
                "cy@example.test",
                "sam@example.test",
                "abc@example.test",
                "eve@elsewhere.test"
            ]
        );
        let named: Vec<&str> = leaving.recipients().map(Recipient::address).collect();
        assert_eq!(named, handed);
        assert_eq!(
            envelope.from().map(ToString::to_string),
            Some("ada@example.test".into())
        );

        let again = Leaving::read(leaving.message().clone()).unwrap();
        assert_eq!(again.to(), leaving.to());
        assert_eq!(again.cc(), leaving.cc());
        assert_eq!(again.bcc(), leaving.bcc());
        assert_eq!(again.message(), leaving.message());
        assert_eq!(leaving.cc()[1].name(), "a\\b \"c\"");
    }
}
