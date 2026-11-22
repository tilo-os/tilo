//! The discussion threads a persistent public workspace keeps for what is
//! published outside it.
//!
//! A text published elsewhere, an essay on a site whose readers arrive by
//! a link naming it, has its discussion in the workspace's Chat: one channel per
//! text, named the text's title and about the address it is read at. The
//! address is what names the thread, so the same text reaches one thread on
//! every start, whatever order the operator announces them in, and a channel
//! someone has renamed or written in keeps its words when the step runs again.

use tilo_server::tl_chat;
use tilo_workspace_service::tl_records::Opened;
use tilo_workspace_service::tl_records::runtime::Actor;

/// The longest title a thread takes, in characters: the sidebar shows a
/// channel's name on one line.
const TITLE_MAX: usize = 200;
/// The longest address a thread is about, in bytes.
const ADDRESS_MAX: usize = 2048;

/// A text published outside the workspace, announced so its readers have a
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
    /// is blank; either could name no thread, and the workspace would start with
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

/// Give the workspace's Chat, opened as `opened`, a discussion thread for each of `published`
/// it keeps none for yet: a text channel named the text's title, about its address, made by
/// `who`, the workspace's owner, after the channels already open. A thread is found by its
/// address, so announcing the same text again leaves it as it is: the step is safe to run at
/// every start, and only ever adds. Answers whether it made a thread.
///
/// # Errors
/// Chat refused a thread.
pub(crate) fn settle_threads(
    opened: &mut Opened,
    who: &Actor,
    published: &[Published],
) -> Result<bool, String> {
    let mut made = false;
    for entry in published {
        let kept = tl_chat::sidebar(opened)?
            .channels
            .iter()
            .any(|channel| channel.topic == entry.address);
        if !kept {
            tl_chat::new_channel(opened, who, (&entry.title, &entry.address), None)?;
            made = true;
        }
    }
    Ok(made)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test these are the assertion")]
mod tests {
    use super::*;
    use tilo_actions::MemberId;
    use tilo_workspace_service::tl_records::RecordBytes;

    fn empty_chat() -> Opened {
        Opened::open(
            &RecordBytes::default(),
            &tl_chat::packages().unwrap(),
            tl_chat::viewer(),
        )
        .unwrap()
    }

    fn owner() -> Actor {
        tl_chat::member(MemberId::person("ab".repeat(32)))
    }

    fn channels(opened: &Opened) -> Vec<(String, String)> {
        tl_chat::sidebar(opened)
            .unwrap()
            .channels
            .into_iter()
            .map(|channel| (channel.name, channel.topic))
            .collect()
    }

    tilo_test_env::scenario! {
        fn a_published_text_has_one_thread_under_its_address;

        phase a_published_text_opens_a_thread_named_it;
        // A published text's thread is a channel named the text, about its address.
        {
            let mut chat = empty_chat();
            let essay =
                Published::new("https://tilo.so/blog/quiet-software", "Quiet software").unwrap();
            assert!(settle_threads(&mut chat, &owner(), &[essay]).unwrap());
            assert_eq!(
                channels(&chat),
                [(
                    "Quiet software".to_owned(),
                    "https://tilo.so/blog/quiet-software".to_owned()
                )]
            );
        }

        phase announcing_a_text_twice_keeps_one_thread;
        // Announcing the same text again leaves its thread alone, however it was spelled.
        {
            let mut chat = empty_chat();
            let quiet =
                Published::new("https://tilo.so/blog/quiet-software", "Quiet software").unwrap();
            let again =
                Published::new("HTTPS://Tilo.SO/blog/quiet-software", "Another wording").unwrap();
            assert!(settle_threads(&mut chat, &owner(), std::slice::from_ref(&quiet)).unwrap());
            assert!(!settle_threads(&mut chat, &owner(), &[quiet, again]).unwrap());
            assert_eq!(channels(&chat).len(), 1);
        }

        phase an_unusable_announcement_is_refused;
        // An announcement nothing could thread is refused, and the flag says
        // what it got.
        {
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

        phase a_title_may_hold_the_separator;
        // A title may hold an `=`.
        {
            let published = parse("https://tilo.so/blog/a-post=Lessons from x = y").unwrap();
            assert_eq!(published.title.as_str(), "Lessons from x = y");
        }
    }
}
