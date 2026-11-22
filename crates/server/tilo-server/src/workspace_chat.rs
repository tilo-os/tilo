//! The Chat a persistent public workspace opens with: the channels its operator names, made
//! through Chat's own `new_channel` while the workspace has no channel yet, and the channel its
//! members report problems to, there on every start. They are found again by their names, so
//! what maps onto them (a bridged Discord server's channels) finds the same ones on every start.

use tilo_server::tl_chat;
use tilo_workspace_service::tl_records::Opened;
use tilo_workspace_service::tl_records::runtime::Actor;

/// The longest starting channel name.
const CHANNEL_NAME_MAX: usize = 64;

/// `name` as a starting channel's name: lowercase letters, digits and `-`, so no two names the
/// operator gives read alike.
pub(crate) fn channel_name(name: &str) -> Result<String, String> {
    let valid = (1..=CHANNEL_NAME_MAX).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if valid {
        Ok(name.to_owned())
    } else {
        Err(format!(
            "a channel name is 1-{CHANNEL_NAME_MAX} lowercase letters, digits and -, not {name:?}"
        ))
    }
}

/// Give the workspace's Chat, opened as `opened`, the starting `channels` (names, in order)
/// while it has no channel at all, each made by `who`, the workspace's owner. Nothing anyone made or wrote is changed, so this
/// runs at every start and never resets: a workspace whose people deleted a starting channel
/// keeps it deleted. Answers whether it made a channel.
///
/// # Errors
/// Chat refused a channel.
pub(crate) fn settle_chat(
    opened: &mut Opened,
    who: &Actor,
    channels: &[String],
) -> Result<bool, String> {
    let sidebar = tl_chat::sidebar(opened)?;
    let mut made = false;
    if sidebar.channels.is_empty() {
        for name in channels {
            tl_chat::new_channel(opened, who, (name, ""), None)?;
            made = true;
        }
    }
    Ok(made)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]
mod tests {
    use super::*;
    use tilo_actions::MemberId;
    use tilo_workspace_service::tl_records::RecordBytes;

    /// The workspace's Chat as a workspace starts with it: empty.
    fn empty_chat() -> Opened {
        Opened::open(
            &RecordBytes::default(),
            &tl_chat::packages().unwrap(),
            tl_chat::viewer(),
            None,
        )
        .unwrap()
    }

    fn owner() -> Actor {
        tl_chat::member(MemberId::person("ab".repeat(32)))
    }

    fn names(opened: &Opened) -> Vec<String> {
        tl_chat::sidebar(opened)
            .unwrap()
            .channels
            .into_iter()
            .map(|channel| channel.name)
            .collect()
    }

    fn owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    tilo_test_env::scenario! {
        fn settling_the_chat_opens_its_channels_and_keeps_what_people_did;

        phase the_chat_opens_with_the_named_channels;
        {
            let mut chat = empty_chat();
            let channels = owned(&["general", "help", "show-and-tell"]);
            assert!(settle_chat(&mut chat, &owner(), &channels).unwrap());
            assert_eq!(names(&chat), ["general", "help", "show-and-tell"]);
        }

        phase settling_again_keeps_what_people_did;
        // A start after people deleted a starting channel brings nothing back.
        {
            let mut chat = empty_chat();
            let channels = owned(&["general", "help", "show-and-tell"]);
            settle_chat(&mut chat, &owner(), &channels).unwrap();
            let help = tl_chat::sidebar(&chat).unwrap().channel_named("help").unwrap().at.clone();
            tl_chat::delete_channel(&mut chat, &owner(), &help, None).unwrap();
            assert!(!settle_chat(&mut chat, &owner(), &channels).unwrap());
            assert_eq!(names(&chat), ["general", "show-and-tell"]);
        }

        phase a_channel_name_is_a_slug;
        {
            assert_eq!(channel_name("show-and-tell").unwrap(), "show-and-tell");
            for bad in ["", "Help", "a_b", "a b", &"x".repeat(CHANNEL_NAME_MAX + 1)] {
                assert!(channel_name(bad).is_err(), "{bad:?}");
            }
        }
    }
}
