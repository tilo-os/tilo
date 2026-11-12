//! The Lounge's chat across its relay's restart.
//!
//! Posts made through the door are written by the relay's own device: its app workers run the
//! post for the member and the relay keeps the write under its own key. The relay's copy of the
//! workspace keeps every write it took in, and after a restart reads its records whole again
//! from what it kept: none of them may wait for a change it lacks, and every post the door
//! answered for is in the chat.
//!
//! The relay stops a moment after the last post, closed or dropped as a killed process is, at a
//! point drawn at random each round; `TILO_CHAT_RESTART_SEED` repeats a round and
//! `TILO_CHAT_RESTART_ROUNDS` runs more of them.

use super::door_on_relay::{OnRelay, approved_key, door_on_relay};
use super::front_door::{ISSUER, Mcp};
use super::*;
use serde_json::json;

/// The chat's posts as the relay's copy holds them, by text.
async fn posts_held(on_relay: &OnRelay) -> Result<Vec<String>, String> {
    let workspaces = on_relay.hosted.relay.workspaces().unwrap();
    let whole = workspaces
        .whole_records(&on_relay.lobby, None)
        .await
        .map_err(|error| error.to_string())?;
    if !whole.is_whole() {
        return Err(format!(
            "records that do not read whole: {:?}",
            whole.unread
        ));
    }
    let part = chat::parts::part(0);
    let route = part.route();
    let (_, held) = whole
        .documents
        .iter()
        .find(|((app, record), _)| app == "so.tilo.chat" && *record == route)
        .ok_or("the relay holds no part of the chat")?;
    let doc = automerge::AutoCommit::load(held).map_err(|error| error.to_string())?;
    Ok(chat::messages::read_part(&doc)
        .messages
        .iter()
        .map(|(_, message)| message.text.clone())
        .collect())
}

/// One round: the chat seated, posts through the door, the relay restarted, the chat read.
async fn round(seed: u64) {
    let mut draw = seed | 1;
    let mut next = |below: u64| {
        draw ^= draw << 13;
        draw ^= draw >> 7;
        draw ^= draw << 17;
        draw.checked_rem(below).unwrap_or(0)
    };
    let on_relay = door_on_relay(ISSUER).await;
    let lobby = on_relay.lobby.clone();
    let workspaces = on_relay.hosted.relay.workspaces().unwrap();
    let serving = workspaces.serving_certificate().clone();
    workspaces
        .prepare_application(
            &lobby,
            &serving,
            &crate::common::catalogue(),
            "so.tilo.chat",
            "",
        )
        .await
        .unwrap();
    workspaces
        .author_application(
            &lobby,
            &serving,
            &crate::common::catalogue(),
            "so.tilo.chat",
            "",
            |doc| {
                chat::demo::apply_as(
                    doc,
                    "operator",
                    chat::Chat::CREATE_CHANNEL_WITH_ID.with(
                        chat::record::chat::CreateChannelWithId {
                            id: "c_general".into(),
                            name: "general".parse().expect("a channel name"),
                            category: None,
                            kind: None,
                            topic: None,
                            participants: None,
                        },
                    ),
                )
                .map_err(|error| error.to_string())
            },
        )
        .await
        .unwrap();
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_relay, &ada, &page, "Lounge").await;
    let posts = usize::try_from(next(30)).unwrap().saturating_add(2);
    let mut posted = Vec::new();
    {
        let mut lounge = Mcp {
            door: &on_relay.door,
            path: format!("/w/{lobby}/mcp"),
            token: key,
            session: None,
        };
        let (status, init) = lounge.initialize().await;
        assert_eq!(status, 200, "{init}");
        for index in 0..posts {
            let text = format!("#{index} posted before the restart");
            let reply = lounge
                .tilo_do(json!({"path": "chat/Messages#post",
                                "args": {"channel": "c_general", "text": text}}))
                .await;
            assert_eq!(
                reply["result"]["structuredContent"]["status"], "ok",
                "post {index} landed: {}",
                reply["result"]["structuredContent"]["error"]
            );
            posted.push(text);
        }
    }
    let before = posts_held(&on_relay).await.unwrap();
    for text in &posted {
        assert!(
            before.contains(text),
            "seed {seed}: before the restart the chat lacks {text:?}"
        );
    }
    let pause = Duration::from_millis(next(1500));
    let killed = next(2) == 0;
    eprintln!("CHAT-RESTART seed={seed} posts={posts} pause={pause:?} killed={killed}");
    tokio::time::sleep(pause).await;
    let on_relay = on_relay.restart(killed).await;
    let after = posts_held(&on_relay).await.unwrap_or_else(|error| {
        panic!("seed {seed}: after the restart the chat does not read: {error}")
    });
    for text in &posted {
        assert!(
            after.contains(text),
            "seed {seed}: after the restart the chat lacks {text:?}, which the door answered \
             for; it holds {after:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_lounges_chat_keeps_every_door_post_across_a_restart() {
    let rounds: u64 = std::env::var("TILO_CHAT_RESTART_ROUNDS")
        .ok()
        .and_then(|rounds| rounds.parse().ok())
        .unwrap_or(2);
    let first: u64 = std::env::var("TILO_CHAT_RESTART_SEED")
        .ok()
        .and_then(|seed| seed.parse().ok())
        .unwrap_or_else(|| {
            u64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
                    % u128::from(u64::MAX),
            )
            .unwrap()
        });
    for n in 0..rounds {
        Box::pin(round(
            first.wrapping_add(n.wrapping_mul(0x9e37_79b9_7f4a_7c15)),
        ))
        .await;
    }
}
