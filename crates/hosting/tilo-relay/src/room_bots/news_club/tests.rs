#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Mutex;

use super::model::Written;
use super::*;
use crate::rooms::tests::{governor, view_read_relay};

/// A person in the room.
const ADA: [u8; 32] = [8; 32];

/// Always the low end, always happens, always the first choice.
struct Eager;

impl Dice for Eager {
    fn between(&mut self, low: i64, _high: i64) -> i64 {
        low
    }
    fn chance(&mut self, _chance: f64) -> bool {
        true
    }
    fn pick(&mut self, _len: usize) -> usize {
        0
    }
}

/// Writes "<persona> on <story>" and keeps what it was asked.
#[derive(Clone, Default)]
struct Scribe {
    asked: Arc<Mutex<Vec<Ask>>>,
}

impl Writer for Scribe {
    fn write(&self, ask: &Ask) -> impl Future<Output = Result<Written, String>> + Send {
        self.asked.lock().unwrap().push(ask.clone());
        let text = format!("{} on {}", ask.persona.name, ask.story.title);
        async move {
            Ok(Written {
                political: false,
                text: Some(text),
            })
        }
    }
}

/// The front page as a fixed list.
struct Fixed(Vec<Story>);

impl Stories for Fixed {
    fn next(
        &self,
        taken: &(dyn Fn(u64) -> bool + Sync),
    ) -> impl Future<Output = Result<(Option<Story>, Vec<u64>), String>> + Send {
        let next = self.0.iter().find(|story| !taken(story.id)).cloned();
        async move { Ok((next, Vec::new())) }
    }
}

fn story() -> Story {
    Story {
        id: 4242,
        title: "A database in one file".into(),
        url: None,
        text: Some("We built it over a weekend.".into()),
        score: 300,
    }
}

/// A room with the chat, `#general` and Ada introduced, and the club
/// installed as its bot.
async fn club_room(
    scratch: &std::path::Path,
) -> (
    crate::Relay,
    String,
    DeviceCert,
    NewsClub<Scribe, Fixed>,
    Scribe,
) {
    let (key, cert) = governor();
    let _ = AppCatalogue::native();
    crate::surfaces::decide_receipt_from_registry("http://127.0.0.1:9");
    let relay = view_read_relay(scratch, &key, &cert).await;
    let rooms = relay.rooms().unwrap().clone();
    let catalogue = AppCatalogue::native();
    let room = rooms.create("Coffee").await.unwrap().room;
    rooms
        .prepare_application(&room, &cert, &catalogue, CHAT, "")
        .await
        .unwrap();
    let ada = hex::encode(ADA);
    rooms
        .author_application(&room, &cert, &catalogue, CHAT, "", |doc| {
            chat::demo::apply_as(
                doc,
                &hex::encode(cert.user_public_key),
                chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
                    name: "general".into(),
                    category: None,
                    kind: None,
                    topic: None,
                }),
            )
            .map_err(|error| error.to_string())?;
            chat::demo::introduce(doc, &ada, "Ada Person", "AP").map_err(|error| error.to_string())
        })
        .await
        .unwrap();
    let chat_bytes = rooms
        .record_document(&room, CHAT, "")
        .await
        .unwrap()
        .unwrap();
    let channel = channel_named(&chat_bytes, "general").expect("the room has #general");
    let dir = scratch.join("club");
    let bot = install(&dir, &channel).unwrap();
    // The governor's root is the room's owner, which installs its bots.
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let certifier = super::super::membership::HostRootCertifier::new(root);
    super::super::membership::keep_member(&rooms, &room, &cert, &certifier, &bot)
        .await
        .unwrap();
    let scribe = Scribe::default();
    let club = NewsClub::new(
        Placement {
            rooms,
            room: room.clone(),
            host: cert.clone(),
            bot_key: certifier.device_key(&bot.installation),
            bot,
            channel: "general".into(),
            state_file: dir.join("club.json"),
        },
        scribe.clone(),
        Fixed(vec![story()]),
        Pace::default(),
    )
    .unwrap();
    (relay, channel, cert, club, scribe)
}

/// The chat's messages as the room holds them, with the name each shows.
async fn messages(club: &NewsClub<Scribe, Fixed>, channel: &str) -> Vec<Said> {
    let chat = club.chat().await.unwrap().unwrap();
    club.said(&chat, channel).await.unwrap()
}

/// Ada answers in thread `parent`, as her page does.
async fn ada_answers(
    club: &NewsClub<Scribe, Fixed>,
    cert: &DeviceCert,
    (channel, parent, text): (&str, &str, &str),
) {
    let rooms = &club.at.rooms;
    let room = &club.at.room;
    let catalogue = AppCatalogue::native();
    let chat = club.chat().await.unwrap().unwrap();
    let parts = club.parts().await.unwrap();
    let ada = hex::encode(ADA);
    let taken = {
        let mut page = Replica::open(
            &catalogue,
            CHAT,
            room,
            &ada,
            ReplicaActor::Owner,
            Seed::Existing {
                record: ui::record::RecordId::package_primary(),
                changes: vec![chat],
            },
        )
        .unwrap();
        for (part, bytes) in parts {
            page.apply_record(&part, &DocumentMutation::Snapshot(bytes))
                .unwrap();
        }
        let arguments = serde_json::json!({ "channel": channel, "parent": parent, "text": text });
        page.invoke(
            &tilo_actions::door::ActionAddress::new(
                ui::record::chain_resource_id(&chat::parts::chain()),
                "post",
            ),
            arguments.as_object().unwrap(),
            false,
        )
        .unwrap();
        page.take_record_mutations()
    };
    for (record, mutation) in taken {
        let bytes = match mutation {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
        };
        rooms
            .author_application(room, cert, &catalogue, CHAT, record.as_str(), |doc| {
                tilo_automerge::ApplyIncoming::load_incoming(doc, &bytes)
                    .map(drop)
                    .map_err(|error| error.to_string())
            })
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_club_opens_a_story_as_its_bot_and_a_named_persona_answers_a_person() {
    let scratch = tempfile::tempdir().unwrap();
    let (_relay, channel, cert, mut club, scribe) = club_room(scratch.path()).await;
    let bot = format!("bot:{}", club.at.bot.installation);

    // A new club opens the front page's story at once.
    club.wake(1_000, &mut Eager).await.unwrap();
    let said = messages(&club, &channel).await;
    let [opening] = &said[..] else {
        panic!("one opening post: {said:?}");
    };
    assert_eq!(
        opening.persona.as_deref(),
        Some("Ada"),
        "shown as a persona"
    );
    assert!(opening.parent.is_none(), "the opening starts a thread");
    assert!(opening.text.starts_with("Ada on A database in one file"));
    assert!(
        opening.text.ends_with(&story().discussion()),
        "with the link"
    );
    assert!(club.club().took(story().id));
    let part = club.parts().await.unwrap().remove(0).1;
    let held = chat::messages::read_part(&AutoCommit::load(&part).unwrap());
    let (_, message) = held.messages.iter().next().unwrap();
    assert_eq!(
        message.author.0, bot,
        "the post is the bot's, whatever name it shows"
    );

    // A person names a persona in the thread; that persona answers them
    // after a while, having read what they said.
    ada_answers(
        &club,
        &cert,
        (
            &channel,
            &opening.id,
            "Hana, would you trust this with real data?",
        ),
    )
    .await;
    club.wake(1_010, &mut Eager).await.unwrap();
    assert_eq!(messages(&club, &channel).await.len(), 2, "not at once");
    let due = club.club().planned[0].due;
    assert_eq!(club.club().planned[0].persona, "Hana");
    club.wake(due, &mut Eager).await.unwrap();
    let said = messages(&club, &channel).await;
    let answer = said.last().unwrap();
    assert_eq!(answer.persona.as_deref(), Some("Hana"));
    assert_eq!(
        answer.parent.as_deref(),
        Some(opening.id.as_str()),
        "in the thread"
    );
    let asked = scribe.asked.lock().unwrap().last().cloned().unwrap();
    let answering = asked.answering.expect("it answers Ada's line");
    assert_eq!(answering.name, "Ada Person");
    assert!(!answering.club);
    assert_eq!(asked.thread.len(), 2, "the opening and Ada's line");
    assert!(asked.thread[0].club);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_club_carries_on_with_what_it_kept() {
    let scratch = tempfile::tempdir().unwrap();
    let (_relay, _channel, _cert, mut club, _scribe) = club_room(scratch.path()).await;
    club.wake(1_000, &mut Eager).await.unwrap();
    let kept = club.club().clone();
    let again = NewsClub::new(
        Placement {
            rooms: club.at.rooms.clone(),
            room: club.at.room.clone(),
            host: club.at.host.clone(),
            bot_key: club.at.bot_key.clone(),
            bot: club.at.bot.clone(),
            channel: club.at.channel.clone(),
            state_file: club.at.state_file.clone(),
        },
        Scribe::default(),
        Fixed(vec![story()]),
        Pace::default(),
    )
    .unwrap();
    assert_eq!(again.club(), &kept, "the threads, plans and taken stories");
    assert!(
        again.club().took(story().id),
        "the story is not opened twice"
    );
}

#[test]
fn the_installed_program_only_posts_in_chat_and_may_show_other_names() {
    let shortcut = program("c_general");
    let needs = tilo_automation::requirements::requirements(&shortcut);
    assert!(
        needs.appearance,
        "the room records that it posts as personas"
    );
    assert_eq!(needs.apps.into_iter().collect::<Vec<_>>(), [CHAT]);
    assert!(
        shortcut.triggers.is_empty(),
        "the host decides when it posts"
    );
}

#[test]
fn a_reinstalled_club_keeps_its_installation_and_takes_the_new_program() {
    let scratch = tempfile::tempdir().unwrap();
    let first = install(scratch.path(), "c_general").unwrap();
    assert_eq!(install(scratch.path(), "c_general").unwrap(), first);
    let moved = install(scratch.path(), "c_lounge").unwrap();
    assert_eq!(moved.installation, first.installation);
    assert_ne!(moved.program, first.program);
}
