use discord_wire::op::{DiscordResult, Queued};

use super::*;
use crate::record::{Mapping, Origin, map_key};
use crate::tilo::Category;

pub(crate) fn dchan(
    id: &str,
    kind: ChannelKind,
    name: &str,
    parent: Option<&str>,
    position: i64,
) -> DChannel {
    DChannel {
        id: id.into(),
        kind,
        name: name.into(),
        parent: parent.map(Into::into),
        position,
        topic: String::new(),
        overwrites: Vec::new(),
    }
}

/// A mapping the bridge made, its Discord side made for Tilo's.
pub(crate) fn mapping(kind: MappedKind, tilo: &str, discord: &str) -> Mapping {
    Mapping {
        key: map_key(kind, tilo),
        kind,
        tilo: tilo.into(),
        discord: discord.into(),
        origin: Origin::Tilo,
        persona: None,
        root: None,
    }
}

/// A mapping of something the server had when it was adopted.
fn adopted(kind: MappedKind, tilo: &str, discord: &str) -> Mapping {
    Mapping {
        origin: Origin::Discord,
        ..mapping(kind, tilo, discord)
    }
}

fn room_chat() -> RoomChat {
    RoomChat {
        busy: Vec::new(),
        categories: vec![Category {
            id: "k_chat".into(),
            name: "chat".into(),
        }],
        channels: vec![
            Channel {
                id: "c_goal".into(),
                name: "goal".into(),
                ..Channel::default()
            },
            Channel {
                id: "c_general".into(),
                name: "General".into(),
                category: Some("k_chat".into()),
                ..Channel::default()
            },
            Channel {
                id: "c_voice".into(),
                name: "voice".into(),
                voice: true,
                category: Some("k_chat".into()),
                ..Channel::default()
            },
        ],
    }
}

/// A server adopted into [`room_chat`], webhooks made.
fn settled() -> Guild {
    Guild {
        guild_id: "1".into(),
        ownership: Ownership::Adopted,
        channels: [
            dchan("d_goal", ChannelKind::Text, "goal", None, 0),
            dchan("d_chat", ChannelKind::Category, "chat", None, 0),
            dchan("d_general", ChannelKind::Text, "general", Some("d_chat"), 0),
            dchan("d_voice", ChannelKind::Voice, "voice", Some("d_chat"), 1),
        ]
        .into_iter()
        .collect(),
        map: [
            adopted(MappedKind::Category, "k_chat", "d_chat"),
            adopted(MappedKind::Channel, "c_goal", "d_goal"),
            adopted(MappedKind::Channel, "c_general", "d_general"),
            adopted(MappedKind::Channel, "c_voice", "d_voice"),
            mapping(MappedKind::Webhook, "c_goal", "wh_goal"),
            mapping(MappedKind::Webhook, "c_general", "wh_general"),
        ]
        .into_iter()
        .collect(),
        ..Guild::default()
    }
}

fn mirroring() -> Guild {
    Guild {
        mirror_channels: true,
        ..settled()
    }
}

#[test]
fn a_settled_server_plans_nothing() {
    for guild in [settled(), mirroring()] {
        let planned = plan(&guild, &room_chat());
        assert!(planned.is_empty(), "{planned:?}");
    }
}

#[test]
fn nothing_happens_before_the_server_is_adopted() {
    let mut guild = mirroring();
    guild.ownership = Ownership::Importing;
    guild.map = ui::tilo_resource::Keyed::new();
    assert!(plan(&guild, &room_chat()).is_empty());
}

#[test]
fn the_servers_own_channels_are_never_renamed_moved_or_deleted() {
    // Discord's names differ from Tilo's, a channel left its category and
    // one was made there: an adopted server's channels stay as they are.
    let mut guild = mirroring();
    guild.channels = [
        dchan("d_goal", ChannelKind::Text, "my-goal", None, 0),
        dchan("d_chat", ChannelKind::Category, "Chat!", None, 0),
        dchan("d_general", ChannelKind::Text, "general-2", None, 5),
        dchan("d_voice", ChannelKind::Voice, "voice", Some("d_chat"), 2),
        dchan("d_new", ChannelKind::Text, "new", None, 3),
    ]
    .into_iter()
    .collect();
    let mut ws = room_chat();
    ws.channels.retain(|c| c.id != "c_general");
    let planned = plan(&guild, &ws);
    assert!(planned.apply.is_empty(), "{planned:?}");
    assert!(planned.delete.is_empty(), "{planned:?}");
}

#[test]
fn a_channel_deleted_in_discord_is_let_go_never_recreated() {
    let mut guild = mirroring();
    guild.channels.take(&Id::from_raw("d_goal"));
    guild.vanished.insert("d_goal".into());
    let planned = plan(&guild, &room_chat());
    assert!(planned.apply.is_empty(), "{planned:?}");
    assert_eq!(
        planned.unmap,
        [
            UnmapItem {
                kind: MappedKind::Channel,
                tilo: "c_goal".into(),
                keep_apart: true
            },
            UnmapItem {
                kind: MappedKind::Webhook,
                tilo: "c_goal".into(),
                keep_apart: false
            }
        ]
    );
}

#[test]
fn a_channel_deleted_in_tilo_leaves_the_servers_own_in_discord() {
    let mut ws = room_chat();
    ws.channels.retain(|c| c.id != "c_general");
    let planned = plan(&mirroring(), &ws);
    assert!(planned.delete.is_empty());
    assert_eq!(planned.unmap.len(), 2, "the channel and its webhook");
    assert!(planned.unmap[0].keep_apart);
}

#[test]
fn text_channels_without_a_webhook_get_one() {
    let mut guild = settled();
    guild
        .map
        .take(&Id::from_raw(map_key(MappedKind::Webhook, "c_general")));
    let planned = plan(&guild, &room_chat());
    assert_eq!(
        planned.webhooks,
        [CreateWebhookOp {
            channel: "d_general".into(),
            tilo: "c_general".into()
        }]
    );
}

/// [`room_chat`] with what was made in Tilo after the adoption: a channel
/// in the adopted category, a category with a channel, a channel at the
/// top; and one Tilo had before, kept Tilo's own.
fn grown() -> RoomChat {
    let mut ws = room_chat();
    ws.categories.push(Category {
        id: "k_new".into(),
        name: "Projects".into(),
    });
    ws.channels.extend([
        Channel {
            id: "c_ideas".into(),
            name: "Ideas".into(),
            category: Some("k_chat".into()),
            ..Channel::default()
        },
        Channel {
            id: "c_plans".into(),
            name: "plans".into(),
            category: Some("k_new".into()),
            ..Channel::default()
        },
        Channel {
            id: "c_seeded".into(),
            name: "help".into(),
            ..Channel::default()
        },
    ]);
    ws
}

fn with_seeded_kept(mut guild: Guild) -> Guild {
    guild.tilo_only.insert("c_seeded".into());
    guild
}

#[test]
fn without_mirroring_channels_made_in_tilo_stay_in_tilo() {
    let planned = plan(&with_seeded_kept(settled()), &grown());
    assert!(planned.is_empty(), "{planned:?}");
}

#[test]
fn mirroring_makes_what_tilo_made_after_the_adoption_last_in_its_place() {
    let planned = plan(&with_seeded_kept(mirroring()), &grown());
    let made: Vec<_> = planned
        .apply
        .iter()
        .map(|a| {
            (
                a.kind,
                a.op.id.as_deref(),
                a.op.name.as_str(),
                a.op.parent.as_deref(),
                a.op.position,
            )
        })
        .collect();
    assert_eq!(
        made,
        [
            (MappedKind::Category, None, "Projects", None, 1),
            (MappedKind::Channel, None, "ideas", Some("d_chat"), 1),
        ],
        "a channel in a new category waits for it; Tilo's own stays Tilo's"
    );
    assert!(planned.delete.is_empty() && planned.unmap.is_empty());
}

#[test]
fn a_channel_the_bridge_made_follows_tilo_and_goes_with_it() {
    let mut guild = mirroring();
    guild.channels.push(dchan(
        "d_ideas",
        ChannelKind::Text,
        "ideas",
        Some("d_chat"),
        1,
    ));
    guild
        .map
        .push(mapping(MappedKind::Channel, "c_ideas", "d_ideas"));
    guild
        .map
        .push(mapping(MappedKind::Webhook, "c_ideas", "wh_ideas"));
    let mut ws = room_chat();
    ws.channels.push(Channel {
        id: "c_ideas".into(),
        name: "Better ideas".into(),
        category: Some("k_chat".into()),
        ..Channel::default()
    });
    let renamed = plan(&guild, &ws);
    assert_eq!(
        renamed
            .apply
            .iter()
            .map(|a| (a.op.id.as_deref(), a.op.name.as_str(), a.op.position))
            .collect::<Vec<_>>(),
        [(Some("d_ideas"), "better-ideas", 1)]
    );

    ws.channels.retain(|c| c.id != "c_ideas");
    let gone = plan(&guild, &ws);
    assert_eq!(
        gone.delete,
        [DeleteChannelOp {
            channel: "d_ideas".into()
        }]
    );
}

#[test]
fn a_channel_being_made_is_not_made_twice() {
    let mut guild = mirroring();
    guild.outbox.push(
        Id::from_raw("op1"),
        Queued {
            op: discord_wire::op::DiscordOp::ApplyChannel(ChannelSpec {
                id: None,
                tilo: "c_ideas".into(),
                kind: ChannelKind::Text,
                name: "ideas".into(),
                topic: String::new(),
                parent: Some("d_chat".into()),
                position: 1,
            }),
            publish: Vec::new(),
            attach: Vec::new(),
        },
    );
    let _: Option<&DiscordResult> = None;
    let mut ws = room_chat();
    ws.channels.push(Channel {
        id: "c_ideas".into(),
        name: "ideas".into(),
        category: Some("k_chat".into()),
        ..Channel::default()
    });
    assert!(plan(&guild, &ws).apply.is_empty());
}
