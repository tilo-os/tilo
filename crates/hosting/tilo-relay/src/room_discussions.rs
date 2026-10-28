//! The discussion threads a persistent public room keeps for what is
//! published outside it.
//!
//! A text published elsewhere — an essay on a site, whose readers arrive by
//! a link naming it — has its discussion in the room's chat: one channel per
//! text, named the text's title and about the address it is read at. Its id
//! comes from the text's slug, so the same text reaches one thread on every
//! start, whatever order the operator announces them in, and a channel
//! someone has written in keeps its words when the step runs again.
//!
//! A reader who follows the text's link lands in the thread named by its
//! slug: the page opens `chat.channel:c_<slug>`, the channel this step
//! seeded.

use chat::record::{Channel, Chat};
use tilo_resource::{Fields, Id};

use crate::room_chat::channel_id;

/// The longest title a thread takes, in characters: the sidebar shows a
/// channel's name on one line.
const TITLE_MAX: usize = 200;
/// The longest address a thread is about, in bytes.
const ADDRESS_MAX: usize = 2048;

/// A text published outside the room, announced so its readers have a
/// discussion thread to arrive in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Published {
    address: String,
    title: String,
}

impl Published {
    /// The published text `address` announces as `title`.
    ///
    /// # Errors
    /// The address is not an `http`/`https` one naming a site, or the title
    /// is blank; either could name no thread, and the room would start with
    /// a thread nobody finds.
    pub fn new(address: &str, title: &str) -> Result<Self, String> {
        let address = address.trim();
        let (scheme, rest) = address
            .split_once("://")
            .filter(|(scheme, _)| {
                scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http")
            })
            .ok_or_else(|| format!("{address:?} is not a web address"))?;
        let (host, path) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
        if host.is_empty() {
            return Err(format!("{address:?} names no site"));
        }
        let title = title.trim();
        if title.is_empty() {
            return Err("a discussion thread needs the title of what it discusses".into());
        }
        if title.chars().count() > TITLE_MAX {
            return Err(format!(
                "a discussion thread's title is at most {TITLE_MAX} characters"
            ));
        }
        let address = format!(
            "{}://{}{path}",
            scheme.to_ascii_lowercase(),
            host.to_ascii_lowercase()
        );
        if address.len() > ADDRESS_MAX {
            return Err(format!(
                "a discussion thread's address is at most {ADDRESS_MAX} bytes"
            ));
        }
        Ok(Self {
            address,
            title: title.to_owned(),
        })
    }

    /// The id of the thread kept for this text: its last path segment, which
    /// is the slug a reader arrives by (`https://site/blog/a-post` →
    /// `c_a_post`).
    ///
    /// # Errors
    /// The address' last segment is not one a channel id can be made of.
    pub fn thread_id(&self) -> Result<String, String> {
        let path = self.address.split_once("://").map_or("", |(_, rest)| rest);
        let path = path.split(['?', '#']).next().unwrap_or_default();
        let slug = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default();
        let id = channel_id(slug);
        let parsed = Id::from_raw(id.as_str());
        if parsed.as_str() != id || id.len() > 64 || slug.is_empty() {
            return Err(format!(
                "the last part of {} is not a slug a thread can be named by",
                self.address
            ));
        }
        Ok(id)
    }
}

/// The flag's reader: `<address>=<title>`.
///
/// # Errors
/// As [`Published::new`], naming the flag so a deploy says what it got wrong.
pub fn parse(entry: &str) -> Result<Published, String> {
    let entry = entry.trim();
    let (address, title) = entry
        .split_once('=')
        .ok_or_else(|| format!("--discussion takes ADDRESS=TITLE, not {entry:?}"))?;
    Published::new(address, title)
}

/// Give the chat `doc` a discussion thread for each of `published` it keeps
/// none for yet. A thread is a text channel named the text's title, about
/// its address, listed uncategorised after the channels already open.
///
/// A thread's channel is named by the text's slug, so announcing the same
/// text again leaves it as it is: the step is safe to run at every start,
/// and only ever adds. Returns whether it changed the chat.
///
/// # Errors
/// A published text the flag should have refused, and a chat that cannot be
/// read or written back.
pub(crate) fn settle_threads(
    doc: &mut automerge::AutoCommit,
    published: &[Published],
) -> Result<bool, String> {
    let wanted = published
        .iter()
        .map(|entry| Ok((entry.thread_id()?, entry)))
        .collect::<Result<Vec<_>, String>>()?;
    tilo_relay::room_bots::opened::<Chat>(doc)
        .map_err(|error| format!("read the chat: {error}"))?;
    let current = Chat::read_fields(doc, &automerge::ROOT)
        .map_err(|error| format!("read the chat: {error}"))?;
    let missing: Vec<_> = wanted
        .into_iter()
        .filter(|(id, _)| current.channels.get(&Id::from_raw(id.as_str())).is_none())
        .collect();
    if missing.is_empty() {
        return Ok(false);
    }
    let mut target = current.clone();
    let mut at = target.channels.len();
    for (id, entry) in missing {
        target.channels.insert(
            at,
            Id::from_raw(id.as_str()),
            Channel {
                name: entry.title.clone(),
                topic: entry.address.clone(),
                ..Channel::default()
            },
        );
        at = at.saturating_add(1);
    }
    target
        .write_changed_fields(
            &current,
            doc,
            &automerge::ROOT,
            &tilo_resource::Delta::default(),
        )
        .map_err(|error| format!("write the chat: {error}"))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_doc() -> automerge::AutoCommit {
        let mut doc = automerge::AutoCommit::new();
        Chat::default()
            .write_fields(&mut doc, &automerge::ROOT)
            .unwrap();
        doc
    }

    fn channels(doc: &automerge::AutoCommit) -> Vec<(String, String, String)> {
        let chat = Chat::read_fields(doc, &automerge::ROOT).unwrap();
        chat.channels
            .iter()
            .map(|(id, channel)| {
                (
                    id.as_str().to_owned(),
                    channel.name.clone(),
                    channel.topic.clone(),
                )
            })
            .collect()
    }

    /// A published text's thread is a channel named the text, about its
    /// address, under the id its slug makes.
    #[test]
    fn a_published_text_opens_a_thread_named_it() {
        let mut doc = chat_doc();
        let essay =
            Published::new("https://tilo.so/blog/quiet-software", "Quiet software").unwrap();
        assert_eq!(essay.thread_id().unwrap(), "c_quiet_software");
        assert!(settle_threads(&mut doc, &[essay]).unwrap());
        assert_eq!(
            channels(&doc),
            [(
                "c_quiet_software".to_owned(),
                "Quiet software".to_owned(),
                "https://tilo.so/blog/quiet-software".to_owned()
            )]
        );
    }

    /// Announcing the same text again leaves its thread alone, and the
    /// wording a person may have given it stays.
    #[test]
    fn announcing_a_text_twice_keeps_one_thread() {
        let mut doc = chat_doc();
        let quiet =
            Published::new("https://tilo.so/blog/quiet-software", "Quiet software").unwrap();
        assert!(settle_threads(&mut doc, std::slice::from_ref(&quiet)).unwrap());
        let mut chat = Chat::read_fields(&doc, &automerge::ROOT).unwrap();
        chat.channels.insert(
            0,
            Id::from_raw("c_quiet_software"),
            Channel {
                name: "Quiet software, discussed".into(),
                topic: "https://tilo.so/blog/quiet-software".into(),
                ..Channel::default()
            },
        );
        chat.write_fields(&mut doc, &automerge::ROOT).unwrap();
        assert!(!settle_threads(&mut doc, &[quiet]).unwrap());
        assert_eq!(
            channels(&doc)[0].1,
            "Quiet software, discussed",
            "a thread the room already keeps is left as it is"
        );
    }

    /// The address is the thread's id: two spellings of one address, and a
    /// trailing slash, are one thread.
    #[test]
    fn one_address_is_one_thread() {
        let a = Published::new("HTTPS://Tilo.SO/blog/a-post/", "A post").unwrap();
        let b = Published::new("https://tilo.so/blog/a-post", "Another wording").unwrap();
        assert_eq!(a.thread_id().unwrap(), b.thread_id().unwrap());
        assert_eq!(a.address.as_str(), "https://tilo.so/blog/a-post/");
    }

    /// An announcement nothing could thread is refused, and the flag says
    /// what it got.
    #[test]
    fn an_unusable_announcement_is_refused() {
        for bad in [
            "https://tilo.so/blog/a-post",
            "tilo.so/blog/a-post=A post",
            "https:///blog/a-post=A post",
            "https://tilo.so/blog/a-post=   ",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(parse("https://tilo.so/blog/a-post=A post").is_ok());
        assert!(Published::new("https://tilo.so/blog/a-post", &"x".repeat(TITLE_MAX + 1)).is_err());
        assert!(
            Published::new(
                &format!("https://tilo.so/{}", "x".repeat(ADDRESS_MAX)),
                "A post"
            )
            .is_err()
        );
    }

    /// A title may hold an `=`, and its address ends where the query does.
    #[test]
    fn a_title_may_hold_the_separator() {
        let published = parse("https://tilo.so/blog/a-post=Lessons from x = y").unwrap();
        assert_eq!(published.title.as_str(), "Lessons from x = y");
        let queried =
            Published::new("https://tilo.so/blog/a-post?ref=elsewhere", "A post").unwrap();
        assert_eq!(queried.thread_id().unwrap(), "c_a_post");
    }
}
