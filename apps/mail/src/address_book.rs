//! Who the person writes to: the addresses their mail came from and went
//! to, for completing a recipient as it is typed.
//!
//! The people written to count most, then the people who wrote; a match is
//! an address, or a word of a name, that starts with what was typed.

use std::collections::HashMap;

use crate::model::{Addr, Folder, Mailbox};

/// How many completions the composer offers at once.
pub(crate) const OFFERED: usize = 6;

/// The person's correspondents, most written-to first.
#[derive(Clone, Debug, Default)]
pub(crate) struct AddressBook {
    people: Vec<Addr>,
}

impl AddressBook {
    /// Everyone in `mailbox` except the person's own addresses (`own`).
    pub(crate) fn of(mailbox: &Mailbox, own: &[String]) -> Self {
        let own: Vec<String> = own.iter().map(|email| email.to_ascii_lowercase()).collect();
        let mut seen: HashMap<String, (Addr, u32)> = HashMap::new();
        let mut count = |addr: &Addr, weight: u32| {
            let key = addr.email.trim().to_ascii_lowercase();
            if key.is_empty() || !key.contains('@') || own.contains(&key) {
                return;
            }
            let entry = seen.entry(key).or_insert_with(|| (addr.clone(), 0));
            entry.1 += weight;
            // A name learned later fills one the address had none for.
            if entry.0.name.is_none() && addr.name.is_some() {
                entry.0.name.clone_from(&addr.name);
            }
        };
        for message in mailbox.messages.values() {
            if message.folder == Folder::Sent {
                for addr in message.to.iter().chain(&message.cc) {
                    count(addr, 3);
                }
            } else if message.folder != Folder::Junk {
                count(&message.from, 1);
            }
        }
        let mut people: Vec<(Addr, u32)> = seen.into_values().collect();
        people.sort_by(|(a, a_count), (b, b_count)| {
            b_count
                .cmp(a_count)
                .then_with(|| a.display().cmp(&b.display()))
        });
        Self {
            people: people.into_iter().map(|(addr, _)| addr).collect(),
        }
    }

    /// The correspondents `typed` could be the start of, best first, leaving
    /// out the ones in `taken`.
    pub(crate) fn complete(&self, typed: &str, taken: &str) -> Vec<Addr> {
        let typed = typed.trim().to_lowercase();
        if typed.is_empty() {
            return Vec::new();
        }
        let taken = taken.to_ascii_lowercase();
        self.people
            .iter()
            .filter(|addr| !taken.contains(&addr.email.to_ascii_lowercase()))
            .filter(|addr| {
                addr.email.to_lowercase().starts_with(&typed)
                    || addr.name.as_deref().is_some_and(|name| {
                        let name = name.to_lowercase();
                        name.starts_with(&typed)
                            || name.split_whitespace().any(|word| word.starts_with(&typed))
                    })
            })
            .take(OFFERED)
            .cloned()
            .collect()
    }
}

/// What is typed into a recipient field split at its last recipient: the
/// finished ones before it (with their separator), and the one being typed.
pub(crate) fn last_recipient(field: &str) -> (&str, &str) {
    match field.rfind([',', ';']) {
        Some(at) => (
            field.get(..=at).unwrap_or_default(),
            field.get(at + 1..).unwrap_or_default().trim_start(),
        ),
        None => ("", field.trim_start()),
    }
}

/// `field` with the recipient being typed replaced by `chosen`, ready for
/// the next one.
pub(crate) fn complete_with(field: &str, chosen: &str) -> String {
    let (done, _) = last_recipient(field);
    if done.is_empty() {
        format!("{chosen}, ")
    } else {
        format!("{done} {chosen}, ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Message;

    fn message(from: Addr, to: Vec<Addr>, folder: Folder, at: i64) -> Message {
        let raw = format!("From: x@x\r\nSubject: s\r\nMessage-ID: <{at}@x>\r\n\r\nb\r\n");
        let mut message = crate::transport::parse_eml(raw.as_bytes(), at, folder).unwrap();
        message.from = from;
        message.to = to;
        message
    }

    #[test]
    fn people_written_to_come_first_and_a_word_of_a_name_matches() {
        let mut mailbox = Mailbox::default();
        let me = Addr::new("Sam", "sam@x.example");
        let maya = Addr::new("Maya Chen", "maya@northwind.example");
        let mark = Addr::new("Mark Lee", "mark@x.example");
        mailbox.insert(message(mark.clone(), vec![me.clone()], Folder::Inbox, 1));
        mailbox.insert(message(mark, vec![me.clone()], Folder::Inbox, 2));
        mailbox.insert(message(me.clone(), vec![maya], Folder::Sent, 3));
        mailbox.insert(message(
            Addr::bare("spam@junk.example"),
            vec![me],
            Folder::Junk,
            4,
        ));
        let book = AddressBook::of(&mailbox, &["SAM@x.example".to_owned()]);
        let emails =
            |found: Vec<Addr>| found.into_iter().map(|addr| addr.email).collect::<Vec<_>>();
        assert_eq!(
            emails(book.complete("m", "")),
            ["maya@northwind.example", "mark@x.example"],
            "the person written to ranks above the one who wrote twice"
        );
        assert_eq!(
            emails(book.complete("chen", "")),
            ["maya@northwind.example"]
        );
        assert!(
            book.complete("sam", "").is_empty(),
            "never the person themself"
        );
        assert!(book.complete("spam", "").is_empty(), "never junk");
        assert_eq!(
            emails(book.complete("m", "Maya Chen <maya@northwind.example>, ")),
            ["mark@x.example"],
            "who is already in the field is not offered again"
        );
    }

    #[test]
    fn a_completion_replaces_only_the_recipient_being_typed() {
        assert_eq!(last_recipient("ma"), ("", "ma"));
        assert_eq!(last_recipient("a@x, ma"), ("a@x,", "ma"));
        assert_eq!(complete_with("ma", "Maya <m@x>"), "Maya <m@x>, ");
        assert_eq!(complete_with("a@x, ma", "Maya <m@x>"), "a@x, Maya <m@x>, ");
    }
}
