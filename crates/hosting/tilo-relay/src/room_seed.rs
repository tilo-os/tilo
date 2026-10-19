//! What a persistent public room starts with, so a visitor who lands in it
//! has something to do at once: a welcome note, a canvas with a few shapes
//! and a chat with the channels its operator names. The content is built
//! through each app's own actions and written
//! into the room's record as the field edits its model makes from one state
//! to another, so a later reset to it is one ordinary change members'
//! replicas merge, never a new genesis.

use std::path::Path;

use tilo_resource::{Fields, Model, Replica, Resource};

/// The app holding the welcome text: its record is a list of notes with a
/// title and a body, the simplest of the document apps.
pub(crate) const WELCOME_APP: &str = "so.tilo.notes";
pub(crate) const CANVAS_APP: &str = "so.tilo.canvas";
/// The app whose content persists: it is given its starting channels, and
/// never reset.
pub(crate) const CHAT_APP: &str = "so.tilo.chat";
/// The category the chat's starting channels open under. Its id and each
/// channel's are derived, never random, so what maps onto them (a bridged
/// Discord server's channels) finds the same ones on every start.
const CHAT_CATEGORY: (&str, &str) = ("cat_channels", "Channels");
/// The longest starting channel name.
const CHANNEL_NAME_MAX: usize = 64;
/// How many snapshots of each app a reset keeps.
pub(crate) const SNAPSHOTS_KEPT: usize = 10;

/// The welcome note's id, fixed so every reset finds the same note.
const WELCOME_NOTE: &str = "6f2b3c1e-8a4d-4e7b-9c0a-5d1e2f3a4b5c";

const WELCOME_TITLE: &str = "Welcome";
const WELCOME_TEXT: &str = "\
Hi! This is a public Tilo workspace anyone can join.

Everyone here is live: every edit syncs to everybody instantly, and you see who else is around. \
The chat persists. Everything else (this note, the canvas) resets to a fresh start every few hours.

Try this: draw something on the canvas, then watch it appear for everyone.
";

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

/// Whether `app` has seeded content, and so is reset to it. Chat (and so
/// everything the Discord bridge carries) has none: it persists.
pub(crate) fn has_seed(app: &str) -> bool {
    matches!(app, WELCOME_APP | CANVAS_APP)
}

/// Make `doc`, `app`'s primary record, hold its seed content. Apps without
/// seed content (chat) are left as they are.
pub(crate) fn write_seed(app: &str, doc: &mut automerge::AutoCommit) -> Result<(), String> {
    match app {
        WELCOME_APP => become_state(doc, &welcome()?),
        CANVAS_APP => become_state(doc, &canvas()?),
        _ => Ok(()),
    }
}

/// Turn the record into `target` by the field edits its model makes: first
/// to an empty record, then to `target`. Going through empty removes every
/// item and adds the seed's items whole, so a value a model only edits in
/// place (a note's rich-text body has no whole `set`) never needs one.
fn become_state<R>(doc: &mut automerge::AutoCommit, target: &R) -> Result<(), String>
where
    R: Resource<State = R> + Model + Fields + Default,
{
    for step in [&R::default(), target] {
        let current = tilo_relay::room_bots::opened::<R>(doc)
            .map_err(|error| format!("read the record: {error}"))?;
        for edit in tilo_resource::Path::<R, R>::root().edits(&current, step) {
            edit.apply_local(doc)
                .map_err(|error| format!("write the seed: {error}"))?;
        }
    }
    Ok(())
}

/// Give the chat `doc` the starting `channels` (names) while it has only
/// the `#chat` and `#voice` every room opens with, beside them. Nothing is
/// ever removed: messages, people and channels are left as they are, so
/// this runs at every start and never resets.
pub(crate) fn settle_chat(
    doc: &mut automerge::AutoCommit,
    channels: &[String],
) -> Result<(), String> {
    use chat::record::{CHAT_CHANNEL, Category, Channel, VOICE_CHANNEL};
    use tilo_resource::Id;

    let current = tilo_relay::room_bots::opened::<chat::Chat>(doc)
        .map_err(|error| format!("read the chat: {error}"))?;
    let mut target = current.clone();
    let opened_with = |id: &Id| id.as_str() == CHAT_CHANNEL || id.as_str() == VOICE_CHANNEL;
    let has_channels = target
        .channels
        .iter()
        .any(|(id, channel)| channel.conversation_with.is_none() && !opened_with(id));
    if !has_channels && !channels.is_empty() {
        let (category, label) = CHAT_CATEGORY;
        let category = Id::from_raw(category);
        if !target.categories.contains(&category) {
            target
                .categories
                .push(category.clone(), Category { name: label.into() });
        }
        for name in channels {
            let id = channel_id(name);
            if target.channels.contains(&Id::from_raw(&id)) {
                continue;
            }
            target.channels.push(
                Id::from_raw(&id),
                Channel {
                    category: Some(category.clone()),
                    name: name.clone(),
                    ..Channel::default()
                },
            );
        }
    }
    for edit in tilo_resource::Path::<chat::Chat, chat::Chat>::root().edits(&current, &target) {
        edit.apply_local(doc)
            .map_err(|error| format!("write the chat: {error}"))?;
    }
    Ok(())
}

fn welcome() -> Result<notes::Notebook, String> {
    let mut notebook = Replica::<notes::Notebook>::new();
    notebook
        .apply(
            notes::Notebook::IMPORT_MARKDOWN.with(notes::notebook::notebook::ImportMarkdown {
                title: WELCOME_TITLE.into(),
                markdown: WELCOME_TEXT.into(),
                images: Vec::new(),
                note: Some(tilo_resource::Id::from(WELCOME_NOTE)),
            }),
        )
        .map_err(|error| format!("write the welcome note: {error}"))?;
    notebook.state().map_err(|error| error.to_string())
}

fn canvas() -> Result<canvas::Artwork, String> {
    let mut art = Replica::<canvas::Artwork>::new();
    let draw: canvas::actions::Draw = serde_json::from_value(serde_json::json!({
        "marks": [
            {"id": "welcome-sun", "shape": {"kind": "ellipse", "cx": 980.0, "cy": 180.0, "rx": 90.0, "ry": 90.0}, "fill": "#ffc94a"},
            {"id": "welcome-hill", "shape": {"kind": "ellipse", "cx": 600.0, "cy": 900.0, "rx": 760.0, "ry": 260.0}, "fill": "#7cc47f"},
            {"id": "welcome-house", "shape": {"kind": "rect", "x": 220.0, "y": 470.0, "width": 200.0, "height": 170.0, "radius": 6.0}, "fill": "#e8775a"},
            {"id": "welcome-roof", "shape": {"kind": "polygon", "points": [[200.0, 475.0], [320.0, 370.0], [440.0, 475.0]]}, "fill": "#8a4b3a"},
            {"id": "welcome-text", "shape": {"kind": "text", "x": 80.0, "y": 140.0, "text": "Draw here! Everyone sees it live.", "size": 56.0}, "fill": "#2b2b3a"}
        ]
    }))
    .map_err(|error| format!("the starter shapes: {error}"))?;
    art.apply(canvas::Artwork::DRAW.with(draw))
        .map_err(|error| format!("draw the starter shapes: {error}"))?;
    art.state().map_err(|error| error.to_string())
}

/// Keep `bytes`, a record before a reset, as `<dir>/<unix>.bin`, and drop
/// all but the newest [`SNAPSHOTS_KEPT`].
pub(crate) fn keep_snapshot(dir: &Path, unix: u64, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    tilo_files::native::atomic::write_with_mode(&dir.join(format!("{unix}.bin")), bytes, 0o600)?;
    let mut kept: Vec<(u64, std::path::PathBuf)> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let stamp = path.file_stem()?.to_str()?.parse().ok()?;
            (path.extension()? == "bin").then_some((stamp, path))
        })
        .collect();
    kept.sort_unstable_by_key(|(stamp, _)| std::cmp::Reverse(*stamp));
    for (_, old) in kept.into_iter().skip(SNAPSHOTS_KEPT) {
        std::fs::remove_file(old)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming as _;

    fn record<R: Resource>() -> automerge::AutoCommit {
        Replica::<R>::new().doc().clone()
    }

    fn note_text(doc: &automerge::AutoCommit) -> String {
        let notebook = notes::Notebook::read(doc).unwrap();
        notebook
            .pages
            .iter()
            .map(|(_, page)| {
                let blocks: Vec<String> = page
                    .blocks()
                    .iter()
                    .map(|block| block.text.clone())
                    .collect();
                format!("{}\n{}", page.title, blocks.join("\n"))
            })
            .collect::<Vec<_>>()
            .join("\n---\n")
    }

    #[test]
    fn the_seeded_note_welcomes_and_suggests_something_to_try() {
        let mut doc = record::<notes::Notebook>();
        write_seed(WELCOME_APP, &mut doc).unwrap();
        let text = note_text(&doc);
        assert!(text.contains(WELCOME_TITLE), "{text}");
        assert!(text.contains("Try this: draw something"), "{text}");
        assert!(text.contains("chat persists"), "{text}");
    }

    #[test]
    fn the_seeded_canvas_has_starter_shapes() {
        let mut doc = record::<canvas::Artwork>();
        write_seed(CANVAS_APP, &mut doc).unwrap();
        let art = canvas::Artwork::read(&doc).unwrap();
        for id in ["welcome-sun", "welcome-house", "welcome-text"] {
            assert!(art.marks.iter().any(|mark| mark.id == id), "{id} missing");
        }
    }

    /// A reset is a change on top of what members did: their replicas merge
    /// it and end up at the seed, with nothing they added left behind.
    #[test]
    fn a_reset_of_a_vandalised_record_converges_members_on_the_seed() {
        let mut host = record::<notes::Notebook>();
        write_seed(WELCOME_APP, &mut host).unwrap();
        host.commit();
        let mut member = Replica::<notes::Notebook>::from_doc(host.fork());
        member
            .apply(notes::Notebook::IMPORT_MARKDOWN.with(
                notes::notebook::notebook::ImportMarkdown {
                    title: "spam".into(),
                    markdown: "spam spam".into(),
                    images: Vec::new(),
                    note: None,
                },
            ))
            .unwrap();
        host.merge_incoming(member.doc_mut()).unwrap();
        assert!(note_text(&host).contains("spam"));

        write_seed(WELCOME_APP, &mut host).unwrap();
        host.commit();
        member.doc_mut().merge_incoming(&mut host).unwrap();
        let after = note_text(member.doc());
        assert!(!after.contains("spam"), "{after}");
        assert!(after.contains(WELCOME_TITLE), "{after}");

        let mut art = record::<canvas::Artwork>();
        write_seed(CANVAS_APP, &mut art).unwrap();
        let seeded = canvas::Artwork::read(&art).unwrap().marks.len();
        let mut scribbled = Replica::<canvas::Artwork>::from_doc(art.fork());
        let scribble: canvas::actions::Draw = serde_json::from_value(serde_json::json!({
            "marks": [{"shape": {"kind": "rect", "x": 0.0, "y": 0.0, "width": 5.0, "height": 5.0}, "fill": "#000000"}]
        }))
        .unwrap();
        scribbled
            .apply(canvas::Artwork::DRAW.with(scribble))
            .unwrap();
        art.merge_incoming(scribbled.doc_mut()).unwrap();
        assert_eq!(canvas::Artwork::read(&art).unwrap().marks.len(), seeded + 1);
        write_seed(CANVAS_APP, &mut art).unwrap();
        assert_eq!(canvas::Artwork::read(&art).unwrap().marks.len(), seeded);
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
        assert_eq!(
            channel_names(&doc),
            ["chat", "voice", "general", "help", "show-and-tell"]
        );
        let ids: Vec<&str> = chat.channels.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "c_chat",
                "c_voice",
                "c_general",
                "c_help",
                "c_show_and_tell"
            ]
        );
    }

    #[test]
    fn without_channels_named_the_chat_opens_with_chat_and_voice() {
        let mut doc = prepared_chat();
        settle_chat(&mut doc, &[]).unwrap();
        let chat = chat::Chat::read(&doc).unwrap();
        assert_eq!(channel_names(&doc), ["chat", "voice"]);
        assert!(
            !chat
                .categories
                .contains(&tilo_resource::Id::from_raw(CHAT_CATEGORY.0))
        );
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
            tilo_resource::Invoke::edit(
                &chat::Chat::paths()
                    .channels
                    .remove(&tilo_resource::Id::from_raw("c_help")),
            )
            .unwrap(),
        )
        .unwrap();
        settle_chat(&mut doc, &channels).unwrap();
        assert_eq!(
            channel_names(&doc),
            ["chat", "voice", "general", "show-and-tell"]
        );
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
        let current = chat::Chat::read(&doc).unwrap();
        let mut adopted = current.clone();
        adopted.categories.push(
            Id::from_raw("k_chat"),
            chat::record::Category {
                name: "chat".into(),
            },
        );
        adopted
            .channels
            .get_mut(&Id::from_raw("c_general"))
            .unwrap()
            .category = Some(Id::from_raw("k_chat"));
        adopted.channels.push(
            Id::from_raw("c_goal"),
            chat::record::Channel {
                name: "goal".into(),
                ..chat::record::Channel::default()
            },
        );
        for edit in tilo_resource::Path::<chat::Chat, chat::Chat>::root().edits(&current, &adopted)
        {
            edit.apply_local(&mut doc).unwrap();
        }
        settle_chat(&mut doc, &channels).unwrap();
        let chat = chat::Chat::read(&doc).unwrap();
        assert_eq!(channel_names(&doc), ["chat", "voice", "general", "goal"]);
        assert_eq!(
            chat.channels
                .get(&Id::from_raw("c_general"))
                .unwrap()
                .category,
            Some(Id::from_raw("k_chat"))
        );
    }

    #[test]
    fn chat_is_never_reset() {
        assert!(!has_seed(CHAT_APP));
    }

    #[test]
    fn snapshots_keep_the_newest_ten() {
        let dir = tempfile::tempdir().unwrap();
        for unix in 1..=12 {
            keep_snapshot(dir.path(), unix, b"x").unwrap();
        }
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left.len(), SNAPSHOTS_KEPT);
        assert!(!left.contains(&"1.bin".to_owned()));
        assert!(!left.contains(&"2.bin".to_owned()));
        assert!(left.contains(&"12.bin".to_owned()));
    }
}
