use discord_wire::op::{DiscordResult, Queued};
use ui::tilo_resource::Settled;

use super::*;
use crate::layout::tests::{dchan, mapping};
use crate::record::{Mapping, Origin, Ownership};
use crate::tilo::{Category, Channel};

/// A mapping of something the server had when it was adopted.
fn adopted(kind: MappedKind, tilo: &str, discord: &str) -> Mapping {
    Mapping {
        origin: Origin::Discord,
        ..mapping(kind, tilo, discord)
    }
}

/// A server laid out as Discord lists it: #goal at the top; "chat" with
/// #general, #mods (which the bridge does not carry), #ideas and a voice
/// channel; then "dev" with #plans. Positions have gaps and a repeat.
fn layout() -> Vec<DChannel> {
    vec![
        dchan("10", ChannelKind::Text, "goal", None, 0),
        dchan("20", ChannelKind::Category, "chat", None, 3),
        dchan("21", ChannelKind::Text, "general", Some("20"), 0),
        dchan("22", ChannelKind::Text, "mods", Some("20"), 2),
        dchan("23", ChannelKind::Text, "ideas", Some("20"), 2),
        dchan("24", ChannelKind::Voice, "voice", Some("20"), 0),
        dchan("30", ChannelKind::Category, "dev", None, 7),
        dchan("31", ChannelKind::Text, "plans", Some("30"), 0),
    ]
}

fn guild() -> Guild {
    Guild {
        guild_id: "1".into(),
        ownership: Ownership::Adopted,
        mirror_channels: true,
        channels: layout().into_iter().collect(),
        map: [
            adopted(MappedKind::Channel, "c_goal", "10"),
            adopted(MappedKind::Category, "k_chat", "20"),
            adopted(MappedKind::Channel, "c_general", "21"),
            adopted(MappedKind::Channel, "c_ideas", "23"),
            adopted(MappedKind::Channel, "c_voice", "24"),
            adopted(MappedKind::Category, "k_dev", "30"),
            mapping(MappedKind::Channel, "c_plans", "31"),
        ]
        .into_iter()
        .collect(),
        discord_only: ["22".to_owned()].into_iter().collect(),
        ..Guild::default()
    }
}

fn channel(id: &str, category: Option<&str>) -> Channel {
    Channel {
        id: id.into(),
        name: id.trim_start_matches("c_").into(),
        voice: id == "c_voice",
        category: category.map(Into::into),
        ..Channel::default()
    }
}

/// The workspace's chat listing what [`guild`] lists, in the same order, and
/// #help of Tilo's own in "chat".
fn workspace() -> WorkspaceChat {
    WorkspaceChat {
        categories: vec![
            Category {
                id: "k_chat".into(),
                name: "chat".into(),
            },
            Category {
                id: "k_dev".into(),
                name: "dev".into(),
            },
        ],
        channels: vec![
            channel("c_goal", None),
            channel("c_general", Some("k_chat")),
            channel("c_help", Some("k_chat")),
            channel("c_voice", Some("k_chat")),
            channel("c_ideas", Some("k_chat")),
            channel("c_plans", Some("k_dev")),
        ],
        busy: Vec::new(),
    }
}

/// `guild` with `id` moved in Discord.
fn moved(guild: &Guild, id: &str, parent: Option<&str>, position: i64) -> Guild {
    let mut after = guild.clone();
    let channel = after.channels.find_mut(&Id::from_raw(id)).unwrap();
    channel.parent = parent.map(Into::into);
    channel.position = position;
    after
}

fn place(channel: &str, category: Option<&str>) -> ChatPlace {
    ChatPlace {
        channel: channel.into(),
        category: category.map(Into::into),
    }
}

/// `guild` with `op` asked for, answered as `settled`.
fn asked(guild: &Guild, op: ArrangeOp, settled: Option<Settled<DiscordResult>>) -> Guild {
    let mut guild = guild.clone();
    let id = guild.outbox.push(
        Id::from_raw("arrange1"),
        Queued {
            op: DiscordOp::ArrangeChannels(op),
            publish: Vec::new(),
            attach: Vec::new(),
        },
    );
    if let Some(settled) = settled {
        guild.outbox.settle(&id, settled);
    }
    guild
}

/// `guild` with every place of `op` shown, one after another: the layout
/// after each update Discord sends.
fn echoes(guild: &Guild, op: &ArrangeOp) -> Vec<Guild> {
    let mut now = guild.clone();
    let mut out = Vec::new();
    for place in &op.categories {
        now = moved(&now, &place.category, None, place.position);
        out.push(now.clone());
    }
    for place in &op.channels {
        now = moved(
            &now,
            &place.channel,
            place.parent.as_deref(),
            place.position,
        );
        out.push(now.clone());
    }
    out
}

#[test]
fn both_sides_in_one_order_ask_for_nothing() {
    let guild = guild();
    assert_eq!(to_chat(&guild, &guild, &workspace()), None);
    assert_eq!(to_discord(&guild, &workspace()), None);
    assert!(!moved_in_discord(&guild, &guild, &workspace()));
}

#[test]
fn a_voice_channel_among_text_channels_in_tilo_is_discords_order() {
    // Discord lists voice channels after text channels; Tilo may list the
    // voice channel first and still show the same order of each kind.
    let mut workspace = workspace();
    workspace.channels.swap(1, 3);
    assert_eq!(to_discord(&guild(), &workspace), None);
    assert_eq!(to_chat(&guild(), &guild(), &workspace), None);
}

#[test]
fn a_channel_moved_in_discord_moves_in_chat() {
    let before = guild();
    // #ideas dragged above #general.
    let after = moved(&before, "23", Some("20"), 0);
    let after = moved(&after, "21", Some("20"), 1);
    assert!(moved_in_discord(&before, &after, &workspace()));
    let arranged = to_chat(&after, &before, &workspace()).unwrap();
    assert_eq!(arranged.categories, ["k_chat", "k_dev"]);
    assert_eq!(
        arranged.channels,
        [
            place("c_goal", None),
            place("c_ideas", Some("k_chat")),
            place("c_general", Some("k_chat")),
            place("c_voice", Some("k_chat")),
            place("c_plans", Some("k_dev")),
        ],
        "Tilo's own #help is not named and keeps its place"
    );
}

#[test]
fn a_channel_moved_to_another_category_in_discord_follows_in_chat() {
    let before = guild();
    let after = moved(&before, "23", Some("30"), 1);
    assert!(moved_in_discord(&before, &after, &workspace()));
    let arranged = to_chat(&after, &before, &workspace()).unwrap();
    assert!(arranged.channels.contains(&place("c_ideas", Some("k_dev"))));
    let at = |id: &str| arranged.channels.iter().position(|p| p.channel == id);
    assert!(at("c_plans") < at("c_ideas"));
}

#[test]
fn a_category_moved_in_discord_moves_in_chat() {
    let before = guild();
    let after = moved(&before, "30", None, 0);
    assert!(moved_in_discord(&before, &after, &workspace()));
    let arranged = to_chat(&after, &before, &workspace()).unwrap();
    assert_eq!(arranged.categories, ["k_dev", "k_chat"]);
}

#[test]
fn changes_in_discord_that_move_nothing_are_no_move() {
    let before = guild();
    // Renumbered, same order; and a channel the bridge does not carry moved.
    let after = moved(&before, "31", Some("30"), 9);
    let after = moved(&after, "22", Some("20"), 0);
    assert!(!moved_in_discord(&before, &after, &workspace()));
}

#[test]
fn a_channel_moved_in_chat_moves_in_discord_around_the_servers_own() {
    let mut workspace = workspace();
    // #ideas moved above #general in Tilo.
    workspace.channels.swap(1, 4);
    let op = to_discord(&guild(), &workspace).unwrap();
    assert!(op.categories.is_empty(), "{op:?}");
    // "chat"'s text channels, as Discord sorts them: #general, #mods and
    // #ideas (a repeated position, by id); numbered anew with #ideas and
    // #general swapped, #mods keeping its place between them.
    assert_eq!(
        op.channels,
        [
            ChannelPlace {
                channel: "23".into(),
                parent: Some("20".into()),
                position: 0
            },
            ChannelPlace {
                channel: "22".into(),
                parent: Some("20".into()),
                position: 1
            },
            ChannelPlace {
                channel: "21".into(),
                parent: Some("20".into()),
                position: 2
            },
        ]
    );
}

#[test]
fn a_category_moved_in_chat_moves_in_discord() {
    let mut workspace = workspace();
    workspace.categories.reverse();
    let op = to_discord(&guild(), &workspace).unwrap();
    assert_eq!(
        op.categories,
        [
            CategoryPlace {
                category: "30".into(),
                position: 0
            },
            CategoryPlace {
                category: "20".into(),
                position: 1
            },
        ]
    );
    assert!(op.channels.is_empty());
}

#[test]
fn a_channel_filed_elsewhere_in_chat_moves_to_that_category_in_discord() {
    let mut workspace = workspace();
    // #ideas filed under "dev", where Tilo lists it before #plans.
    workspace.channels[4].category = Some("k_dev".into());
    let op = to_discord(&guild(), &workspace).unwrap();
    let dev = |channel: &str, position| ChannelPlace {
        channel: channel.into(),
        parent: Some("30".into()),
        position,
    };
    assert!(op.channels.contains(&dev("23", 0)), "{op:?}");
    assert!(op.channels.contains(&dev("31", 1)), "{op:?}");
}

#[test]
fn only_a_mirroring_bridge_moves_channels_in_discord() {
    let mut guild = guild();
    guild.mirror_channels = false;
    let mut workspace = workspace();
    workspace.categories.reverse();
    assert_eq!(to_discord(&guild, &workspace), None);
    // Discord's order still shows in Chat.
    assert!(to_chat(&guild, &guild, &workspace).is_some());
}

#[test]
fn a_bridge_of_one_channel_keeps_out_of_the_order() {
    let mut guild = guild();
    guild.only_channel = Some("21".into());
    let mut workspace = workspace();
    workspace.categories.reverse();
    assert_eq!(to_discord(&guild, &workspace), None);
    assert_eq!(to_chat(&guild, &guild, &workspace), None);
}

#[test]
fn channels_kept_on_one_side_or_archived_keep_their_places() {
    let mut workspace = workspace();
    // Tilo's #help and an archived #ideas move; neither is shared.
    workspace.channels[2].category = None;
    workspace.channels[4].archived = true;
    workspace.channels.swap(2, 4);
    assert_eq!(to_discord(&guild(), &workspace), None);
}

#[test]
fn the_echo_of_the_bridges_arrangement_is_no_move_and_not_asked_again() {
    let mut workspace = workspace();
    workspace.channels.swap(1, 4);
    workspace.categories.reverse();
    let op = to_discord(&guild(), &workspace).unwrap();
    let waiting = asked(&guild(), op.clone(), None);
    // Asked, not answered: not asked again.
    assert_eq!(to_discord(&waiting, &workspace), None);
    // Discord's updates arrive one by one, before and after its answer.
    let mut before = waiting.clone();
    for (n, after) in echoes(&waiting, &op).into_iter().enumerate() {
        assert!(!moved_in_discord(&before, &after, &workspace), "update {n}");
        assert_eq!(to_chat(&after, &before, &workspace), None, "update {n}");
        assert_eq!(to_discord(&after, &workspace), None, "update {n}");
        before = after;
    }
    // Answered and shown whole, it is done with.
    let done = asked(
        &guild(),
        op.clone(),
        Some(Settled::Done(DiscordResult::Done)),
    );
    let shown = echoes(&done, &op).pop().unwrap();
    assert_eq!(finished(&done, &shown.channels), [Id::from_raw("arrange1")]);
    let half = echoes(&done, &op).remove(0);
    assert!(finished(&done, &half.channels).is_empty());
    // A channel of it deleted in Discord meanwhile is waited for no longer.
    // (#mods, the second channel placed, is the one deleted.)
    let mut deleted = shown;
    deleted.channels.take(&Id::from_raw("22"));
    assert_eq!(finished(&done, &deleted.channels).len(), 1);
    let mut before_mods = echoes(&done, &op)
        .into_iter()
        .nth(op.categories.len())
        .unwrap();
    before_mods.channels.take(&Id::from_raw("22"));
    assert!(
        finished(&done, &before_mods.channels).is_empty(),
        "#general is still to come"
    );
}

#[test]
fn a_move_in_discord_while_an_arrangement_is_on_its_way_keeps_the_arrangement() {
    let mut workspace = workspace();
    workspace.categories.reverse();
    let op = to_discord(&guild(), &workspace).unwrap();
    let waiting = asked(&guild(), op, None);
    // Someone moves #plans to the top before Discord shows the categories
    // moved: Chat follows that move and keeps its own.
    let after = moved(&waiting, "31", None, 1);
    assert!(moved_in_discord(&waiting, &after, &workspace));
    let arranged = to_chat(&after, &waiting, &workspace).unwrap();
    assert_eq!(arranged.categories, ["k_dev", "k_chat"]);
    assert_eq!(arranged.channels[1], place("c_plans", None));
}

#[test]
fn a_failed_arrangement_is_done_with_and_laid_over_nothing() {
    let mut workspace = workspace();
    workspace.categories.reverse();
    let op = to_discord(&guild(), &workspace).unwrap();
    let failed = asked(
        &guild(),
        op.clone(),
        Some(Settled::Failed {
            reason: "Missing Permissions".into(),
            partial: DiscordResult::Done,
        }),
    );
    assert_eq!(
        finished(&failed, &failed.channels),
        [Id::from_raw("arrange1")]
    );
    assert_eq!(to_discord(&failed, &workspace), Some(op));
}

#[test]
fn a_channel_being_made_is_put_where_tilo_has_it() {
    let mut guild = guild();
    guild.map.push(mapping(MappedKind::Channel, "c_new", "40"));
    let mut workspace = workspace();
    // Made at the top of "chat" in Tilo; Discord has not shown it yet.
    workspace
        .channels
        .insert(1, channel("c_new", Some("k_chat")));
    let op = to_discord(&guild, &workspace).unwrap();
    assert_eq!(
        op.channels.first(),
        Some(&ChannelPlace {
            channel: "40".into(),
            parent: Some("20".into()),
            position: 0
        }),
        "{op:?}"
    );
    // Discord showing it last before the arrangement lands is no move.
    let waiting = asked(&guild, op, None);
    let mut shown = waiting.clone();
    shown
        .channels
        .push(dchan("40", ChannelKind::Text, "new", Some("20"), 3));
    assert!(!moved_in_discord(&waiting, &shown, &workspace));
}
