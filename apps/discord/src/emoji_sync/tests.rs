use discord_wire::event::EmojiInfo;

use super::*;
use crate::record::{Origin, map_key};

const WAVE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CAT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn guild(server: &[(&str, &str)], uploaded: &[(&str, &str, &str)]) -> Guild {
    let mut guild = Guild {
        guild_id: "1000".into(),
        bot_user: "2000".into(),
        ..Guild::default()
    };
    guild.observe_emojis(
        server
            .iter()
            .map(|(name, id)| EmojiInfo {
                id: (*id).into(),
                name: (*name).into(),
                animated: false,
            })
            .collect(),
    );
    for (tilo, discord, picture) in uploaded {
        guild.map.push(Mapping {
            key: map_key(MappedKind::Emoji, tilo),
            kind: MappedKind::Emoji,
            tilo: (*tilo).into(),
            discord: (*discord).into(),
            origin: Origin::Tilo,
            persona: None,
            root: None,
            picture: Some((*picture).into()),
        });
    }
    guild
}

fn wanted(emoji: &[(&str, &str)]) -> Vec<Wanted> {
    emoji
        .iter()
        .map(|(name, picture)| Wanted {
            name: (*name).into(),
            picture: (*picture).into(),
        })
        .collect()
}

#[test]
fn a_new_workspace_emoji_is_uploaded_once() {
    let plan = plan(&guild(&[], &[]), &wanted(&[("tilo_wave", WAVE)]));
    assert!(plan.deletes.is_empty());
    assert_eq!(
        plan.applies,
        [ApplyItem {
            op: EmojiSpec {
                id: None,
                name: "tilo_wave".into(),
                picture: Some(WAVE.into()),
                tilo: "tilo_wave".into(),
            },
            picture: WAVE.into(),
            was: None,
        }]
    );
    // Once uploaded, planning again changes nothing.
    let uploaded = guild(&[("tilo_wave", "91")], &[("tilo_wave", "91", WAVE)]);
    assert_eq!(
        super::plan(&uploaded, &wanted(&[("tilo_wave", WAVE)])),
        EmojiPlan::default()
    );
}

#[test]
fn a_renamed_workspace_emoji_renames_the_one_the_bridge_uploaded() {
    let uploaded = guild(&[("tilo_wave", "91")], &[("tilo_wave", "91", WAVE)]);
    let plan = plan(&uploaded, &wanted(&[("hello", WAVE)]));
    assert!(plan.deletes.is_empty(), "{plan:?}");
    assert_eq!(
        plan.applies,
        [ApplyItem {
            op: EmojiSpec {
                id: Some("91".into()),
                name: "hello".into(),
                picture: Some(WAVE.into()),
                tilo: "hello".into(),
            },
            picture: WAVE.into(),
            was: Some("tilo_wave".into()),
        }]
    );
}

#[test]
fn a_removed_workspace_emoji_is_deleted_and_a_new_picture_uploaded_again() {
    let uploaded = guild(
        &[("tilo_wave", "91"), ("cat", "92")],
        &[("tilo_wave", "91", WAVE), ("cat", "92", WAVE)],
    );
    let plan = plan(&uploaded, &wanted(&[("cat", CAT)]));
    assert_eq!(
        plan.deletes,
        [
            DeleteEmojiOp {
                emoji: "91".into(),
                tilo: "tilo_wave".into(),
            },
            DeleteEmojiOp {
                emoji: "92".into(),
                tilo: "cat".into(),
            },
        ]
    );
    assert_eq!(plan.applies.len(), 1);
    assert_eq!(plan.applies[0].op.id, None);
    assert_eq!(plan.applies[0].op.picture.as_deref(), Some(CAT));
}

/// The server's own emoji are never touched: one of a workspace emoji's
/// name stands for it, and nothing the bridge did not upload is deleted.
#[test]
fn the_servers_own_emoji_are_left_alone() {
    let server = guild(&[("party", "7"), ("tilo_wave", "8")], &[]);
    let plan = plan(&server, &wanted(&[("tilo_wave", WAVE)]));
    assert_eq!(plan, EmojiPlan::default());
}

#[test]
fn nothing_is_uploaded_before_the_server_is_observed_or_for_one_channel() {
    let mut unobserved = guild(&[], &[]);
    unobserved.bot_user.clear();
    assert_eq!(
        plan(&unobserved, &wanted(&[("tilo_wave", WAVE)])),
        EmojiPlan::default()
    );
    let mut one_channel = guild(&[], &[]);
    one_channel.only_channel = Some("1101".into());
    assert_eq!(
        plan(&one_channel, &wanted(&[("tilo_wave", WAVE)])),
        EmojiPlan::default()
    );
}

#[test]
fn the_hosts_input_reads_as_the_workspaces_emoji() {
    let input = serde_json::json!({"emoji": [{"name": "tilo_wave", "picture": WAVE}]});
    assert_eq!(
        super::wanted(Some(&input)),
        Some(wanted(&[("tilo_wave", WAVE)]))
    );
    assert_eq!(super::wanted(None), None);
}
