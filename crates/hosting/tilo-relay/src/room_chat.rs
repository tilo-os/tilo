//! The chat a persistent public room opens with: the channels its operator
//! names, in place of the `#chat` every room opens with, and its `#voice`.
//! Their ids are derived from their names, so what maps onto them (a bridged
//! Discord server's channels) finds the same ones on every start.

use tilo_resource::Fields;

/// The room's chat app.
pub(crate) const CHAT_APP: &str = "so.tilo.chat";
/// The longest starting channel name.
const CHANNEL_NAME_MAX: usize = 64;

/// `name` as a starting channel's name: lowercase letters, digits and `-`,
/// so its id ([`channel_id`]) is one no other name shares.
///
/// # Errors
/// The name is empty, too long or has another character.
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

/// The id of the starting channel `name`: `c_<name>` with `-` as `_`.
pub(crate) fn channel_id(name: &str) -> String {
    format!("c_{}", name.replace('-', "_"))
}

/// Give the chat `doc` the starting `channels` (names) while it has only
/// the `#chat` and `#voice` every room opens with. They take `#chat`'s
/// place: a visitor's first channel is the operator's first, not an empty
/// `#chat` apart from the rest, and they list uncategorised above `#voice`,
/// since the sidebar is titled "Channels" already. `#chat` goes only while
/// nobody has read it, so nothing anyone wrote is lost; nothing else is
/// ever removed, so this runs at every start and never resets.
///
/// The channels are written as they stand, not as field edits: the chat
/// keeps its layout to its moderators' actions and refuses every generic
/// edit of it, and those actions mint their own ids where these are
/// derived from the names.
pub(crate) fn settle_chat(
    doc: &mut automerge::AutoCommit,
    channels: &[String],
) -> Result<(), String> {
    use chat::record::{CHAT_CHANNEL, Channel, VOICE_CHANNEL};
    use tilo_resource::Id;

    tilo_relay::room_bots::opened::<chat::Chat>(doc)
        .map_err(|error| format!("read the chat: {error}"))?;
    let current = chat::Chat::read_fields(doc, &automerge::ROOT)
        .map_err(|error| format!("read the chat: {error}"))?;
    let mut target = current.clone();
    let opened_with = |id: &Id| id.as_str() == CHAT_CHANNEL || id.as_str() == VOICE_CHANNEL;
    let has_channels = target
        .channels
        .iter()
        .any(|(id, channel)| channel.conversation_with.is_none() && !opened_with(id));
    if !has_channels && !channels.is_empty() {
        let opener = Id::from_raw(CHAT_CHANNEL);
        let read = target
            .read
            .values()
            .any(|marks| marks.contains_key(CHAT_CHANNEL));
        if !read {
            target.channels.remove(&opener);
        }
        // Above `#voice` and whatever else opened the room, in the
        // operator's order.
        let mut at = target
            .channels
            .iter()
            .position(|(id, _)| *id == opener)
            .map_or(0, |index| index + 1);
        for name in channels {
            let id = Id::from_raw(&channel_id(name));
            if target.channels.contains(&id) {
                continue;
            }
            target.channels.insert(
                at,
                id,
                Channel {
                    name: name.clone(),
                    ..Channel::default()
                },
            );
            at += 1;
        }
    }
    target
        .write_changed_fields(
            &current,
            doc,
            &automerge::ROOT,
            &tilo_resource::Delta::default(),
        )
        .map_err(|error| format!("write the chat: {error}"))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]
mod tests {
    use super::*;
    use tilo_resource::{Replica, Resource};

    fn record<R: Resource>() -> automerge::AutoCommit {
        Replica::<R>::new().doc().clone()
    }

    /// The chat as the room's host prepares it: opened with `#chat` and
    /// `#voice`.
    fn prepared_chat() -> automerge::AutoCommit {
        let mut doc = record::<chat::Chat>();
        chat::demo::start(&mut doc, "host", "Host", "H").unwrap();
        doc
    }

    fn channel_names(doc: &automerge::AutoCommit) -> Vec<String> {
        let chat = chat::Chat::read(doc).unwrap();
        chat.channels
            .values()
            .map(|channel| channel.name.clone())
            .collect()
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    // eu1 2026-10-19: a relay from 3287d858 never started on the Lounge an
    // 8fca8fe781 relay had kept, its Chat stored at version 3: "open the
    // chat's channels: read the chat: stored at version 3; it is brought up
    // to version 5 when it opens".
    #[test]
    fn a_newer_relay_settles_a_chat_an_older_one_stored_at_an_older_version() {
        use automerge::transaction::Transactable as _;
        let mut doc = prepared_chat();
        let channels = names(&["general"]);
        settle_chat(&mut doc, &channels).unwrap();
        doc.put(
            automerge::ROOT,
            tilo_resource::evolution::VERSION_KEY,
            automerge::ScalarValue::Uint(3),
        )
        .unwrap();
        doc.commit();
        let before = channel_names(&{
            let mut migrated = doc.fork();
            tilo_resource::evolution::migrate::<chat::Chat>(&mut migrated).unwrap();
            migrated
        });
        assert!(
            chat::Chat::read(&doc).is_err(),
            "stored at an older version"
        );

        settle_chat(&mut doc, &channels).unwrap();
        assert_eq!(
            tilo_resource::evolution::stored_version(&doc),
            <chat::Chat as Resource>::VERSION,
            "brought up to this build's version"
        );
        assert_eq!(channel_names(&doc), before, "nothing it held is lost");
        // And the next start finds nothing to do.
        let heads = doc.get_heads();
        settle_chat(&mut doc, &channels).unwrap();
        assert_eq!(doc.get_heads(), heads);
    }

    #[test]
    fn the_chat_opens_with_the_named_channels() {
        let mut doc = prepared_chat();
        let channels = names(&["general", "help", "show-and-tell"]);
        settle_chat(&mut doc, &channels).unwrap();
        let chat = chat::Chat::read(&doc).unwrap();
        // The operator's channels take the unread `#chat`'s place, first
        // and uncategorised, so a visitor lands in `#general` and the
        // sidebar has no second "Channels" heading.
        assert_eq!(
            channel_names(&doc),
            ["general", "help", "show-and-tell", "voice"]
        );
        let ids: Vec<&str> = chat.channels.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["c_general", "c_help", "c_show_and_tell", "c_voice"]);
        assert!(chat.categories.is_empty());
        assert!(
            chat.channels
                .values()
                .all(|channel| channel.category.is_none())
        );
    }

    #[test]
    fn a_chat_someone_has_read_keeps_its_chat_channel() {
        let mut doc = prepared_chat();
        let mut chat = chat::Chat::read_fields(&doc, &automerge::ROOT).unwrap();
        let before = chat.clone();
        chat.read.entry("host".into()).or_default().insert(
            chat::record::CHAT_CHANNEL.into(),
            chat::parts::ReadMark::default(),
        );
        chat.write_changed_fields(
            &before,
            &mut doc,
            &automerge::ROOT,
            &tilo_resource::Delta::default(),
        )
        .unwrap();
        settle_chat(&mut doc, &names(&["general"])).unwrap();
        assert_eq!(channel_names(&doc), ["chat", "general", "voice"]);
    }

    #[test]
    fn without_channels_named_the_chat_opens_with_chat_and_voice() {
        let mut doc = prepared_chat();
        settle_chat(&mut doc, &[]).unwrap();
        let chat = chat::Chat::read(&doc).unwrap();
        assert_eq!(channel_names(&doc), ["chat", "voice"]);
        assert!(chat.categories.is_empty());
    }

    #[test]
    fn a_channel_name_maps_to_one_id() {
        assert_eq!(channel_name("show-and-tell").unwrap(), "show-and-tell");
        assert_eq!(channel_id("show-and-tell"), "c_show_and_tell");
        for bad in ["", "Help", "a_b", "a b", &"x".repeat(CHANNEL_NAME_MAX + 1)] {
            assert!(channel_name(bad).is_err(), "{bad:?}");
        }
    }

    /// Every start settles the chat again: it never brings back or doubles
    /// channels, nor undoes what people did. (What they
    /// wrote is kept in the chat's message parts, which settling never
    /// opens.)
    #[test]
    fn settling_the_chat_again_keeps_what_people_did() {
        let mut doc = prepared_chat();
        let channels = names(&["general", "help", "show-and-tell"]);
        settle_chat(&mut doc, &channels).unwrap();
        chat::demo::apply_as(
            &mut doc,
            "host",
            chat::Chat::DELETE_CHANNEL.with(chat::record::chat::DeleteChannel {
                channel: tilo_resource::Id::from_raw("c_help"),
            }),
        )
        .unwrap();
        settle_chat(&mut doc, &channels).unwrap();
        assert_eq!(channel_names(&doc), ["general", "show-and-tell", "voice"]);
    }

    /// A bridged server adopts the starting channels: `#general` is joined
    /// with the server's and may sit in the server's category, beside the
    /// channels the server brought. A later start leaves all of that be.
    #[test]
    fn settling_again_after_a_server_is_adopted_neither_doubles_nor_moves_back() {
        use tilo_resource::Id;
        let mut doc = prepared_chat();
        let channels = names(&["general"]);
        settle_chat(&mut doc, &channels).unwrap();
        // The server's category and channel, under the ids the bridge maps
        // them to.
        chat::demo::insert_category(&mut doc, "k_chat", "chat").unwrap();
        chat::demo::insert_channel(
            &mut doc,
            "c_goal",
            "",
            "goal",
            chat::model::ChannelKind::Text,
        )
        .unwrap();
        chat::demo::apply_as(
            &mut doc,
            "host",
            chat::Chat::SET_CHANNEL_CATEGORY.with(chat::record::chat::SetChannelCategory {
                channel: Id::from_raw("c_general"),
                category: Some(Id::from_raw("k_chat")),
            }),
        )
        .unwrap();
        settle_chat(&mut doc, &channels).unwrap();
        let chat = chat::Chat::read(&doc).unwrap();
        assert_eq!(channel_names(&doc), ["general", "voice", "goal"]);
        assert_eq!(
            chat.channels
                .get(&Id::from_raw("c_general"))
                .unwrap()
                .category,
            Some(Id::from_raw("k_chat"))
        );
    }
}
