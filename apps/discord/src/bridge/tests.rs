use serde_json::{Value, json};

use super::*;
use crate::layout::tests::mapping;
use crate::record::Link;

fn guild() -> Guild {
    let mut g = Guild {
        guild_id: "1".into(),
        bot_user: "9".into(),
        ownership: Ownership::Adopted,
        ..Guild::default()
    };
    g.map = [
        mapping(MappedKind::Channel, "c1", "d1"),
        mapping(MappedKind::Webhook, "c1", "wh1"),
    ]
    .into_iter()
    .collect();
    g
}

fn chat() -> Value {
    json!({
        "messages": [{"id": "m1", "value": {"channel": "c1", "author": "u_ada", "text": "hello", "parent": null}}],
        "people": [{"id": "u_ada", "name": "Ada", "agent": false}]
    })
}

fn verb(name: &str) -> Value {
    json!({"did": "verb", "action": {"app": "so.tilo.chat", "kind": CHAT_KIND, "action": name}})
}

fn planned(guild: &Guild, event: &Value) -> BridgePlan {
    planned_in(guild, &Value::Null, event)
}

fn planned_in(guild: &Guild, workspace: &Value, event: &Value) -> BridgePlan {
    plan(
        Reading {
            guild,
            chat: &chat(),
            context: &Context::default(),
            workspace,
            emoji: None,
        },
        event,
    )
}

#[test]
fn a_tilo_message_deleted_in_discord_is_removed_in_chat_not_sent_again() {
    let mut g = guild();
    g.map.push(mapping(MappedKind::Message, "m1", "dm1"));
    let deleted = discord(
        source::MESSAGE_DELETED,
        &json!({"guild_id": "1", "channel_id": "d1", "message_id": "dm1", "executor": "d_admin"}),
    );
    let removed = planned(&g, &deleted);
    assert_eq!(
        removed.chat_removes,
        [json!({"message": "m1"})],
        "{removed:?}"
    );
    assert!(removed.sends.is_empty(), "never sent again: {removed:?}");
}

fn discord(source: &str, payload: &Value) -> Value {
    json!({"app_id": crate::APP_ID, "source": source, "id": "e1", "at_ms": 0, "payload": payload})
}

#[test]
fn a_chat_post_is_routed_to_its_message_and_sent_once() {
    let event =
        json!({"kind": CHAT_KIND, "did": verb("post"), "item": {"kind": "messages", "id": "m1"}});
    assert_eq!(route(&guild(), &event), "m1");
    let plan = planned(&guild(), &event);
    assert_eq!(plan.sends.len(), 1);
    assert_eq!(plan.sends[0]["op"]["webhook"], "wh1");
    assert_eq!(plan.sends[0]["op"]["username"], "Ada");
    assert!(!plan.again, "a send unblocks nothing");
}

#[test]
fn a_post_entry_names_its_channel_and_the_plan_finds_the_message_it_made() {
    // Chat records a post on the channel it was made in, with its text.
    let event = json!({
        "kind": CHAT_KIND,
        "did": verb("post"),
        "item": {"kind": "channels", "id": "c1"},
        "actor": {"id": "u_ada"},
        "arguments": {"channel": "c1", "text": "hello"},
    });
    assert_eq!(route_channel(&event), "c1");
    let plan = planned(&guild(), &event);
    assert_eq!(plan.sends.len(), 1, "{:?}", plan.sends);
    assert_eq!(plan.sends[0]["op"]["tilo"], "m1");
    let mut sent = guild();
    sent.map.push(mapping(MappedKind::Message, "m1", "dm1"));
    assert!(
        planned(&sent, &event).sends.is_empty(),
        "a message already in Discord is not sent again"
    );
}

/// Sharing a window or a resource posts a message, which goes across like
/// any post; its sharer stopping the share edits the Discord copy.
#[test]
fn sharing_posts_and_stopping_a_share_edits_its_copy() {
    let shared = json!({
        "kind": CHAT_KIND,
        "did": verb("share-window"),
        "item": {"kind": "channels", "id": "c1"},
        "actor": {"id": "u_ada"},
        "arguments": {"channel": "c1"},
    });
    assert_eq!(route_channel(&shared), "c1");
    let plan = planned(&guild(), &shared);
    assert_eq!(plan.sends.len(), 1, "{:?}", plan.sends);
    assert_eq!(plan.sends[0]["op"]["tilo"], "m1");
    let mut sent = guild();
    sent.map.push(mapping(MappedKind::Message, "m1", "dm1"));
    let stopped = json!({
        "kind": CHAT_KIND,
        "did": verb("stop-sharing"),
        "item": {"kind": "messages", "id": "m1"},
        "arguments": {"message": "m1"},
    });
    let plan = planned(&sent, &stopped);
    assert_eq!(plan.edits.len(), 1, "{plan:?}");
    assert_eq!(plan.edits[0]["message"], "dm1");
}

/// Its sharer closing or reopening the shared window edits the Discord copy,
/// so it says whether the share is live right now.
#[test]
fn closing_or_reopening_a_shared_window_edits_its_copy() {
    let mut sent = guild();
    sent.map.push(mapping(MappedKind::Message, "m1", "dm1"));
    for open in [false, true] {
        let event = json!({
            "kind": CHAT_KIND,
            "did": verb("share-open"),
            "item": {"kind": "messages", "id": "m1"},
            "actor": {"id": "u_ada"},
            "arguments": {"open": open},
        });
        let plan = planned(&sent, &event);
        assert_eq!(plan.edits.len(), 1, "{plan:?}");
        assert_eq!(plan.edits[0]["message"], "dm1");
    }
}

#[test]
fn a_chat_delete_finds_the_channel_the_message_was_in() {
    let mut g = guild();
    g.map.push(mapping(MappedKind::Message, "m1", "dm1"));
    let event = json!({
        "kind": CHAT_KIND,
        "did": {"did": "group", "members": [verb("delete-message")]},
        "item": {"kind": "messages", "id": "m1"},
        "before": {"channel": "c1"},
    });
    let plan = planned(&g, &event);
    assert_eq!(plan.deletes[0]["op"]["message"], "dm1");
    assert_eq!(plan.deletes[0]["tilo"], "m1");
}

#[test]
fn a_discord_message_posts_on_the_mapped_channels_item_with_an_appearance() {
    let payload = json!({
        "guild_id": "1",
        "channel_id": "d1",
        "message_id": "dm7",
        "content": "hi from discord",
        "author": {"id": "42", "name": "thor"},
    });
    let plan = planned(&guild(), &discord("message-created", &payload));
    assert_eq!(plan.posts.len(), 1, "{plan:?}");
    let post = &plan.posts[0];
    assert_eq!(post["channel"], "c1");
    assert_eq!(post["appearance"]["name"], "thor");
    assert_eq!(post["appearance"]["persona"]["account"], "42");
    assert_eq!(post["discord"], "dm7");

    let echo = json!({"guild_id": "1", "channel_id": "d1", "message_id": "dm8", "content": "x", "author": {"id": "9", "name": "bridge"}});
    assert!(
        planned(&guild(), &discord("message-created", &echo))
            .posts
            .is_empty(),
        "the bot's own message is not mirrored back"
    );
}

#[test]
fn a_layout_event_is_recorded_first_and_reconciled_once_recorded() {
    let payload = json!({"guild_id": "1", "bot_user": "9", "channels": [], "roles": []});
    let mut g = guild();
    g.bot_user = String::new();
    let first = planned(&g, &discord("layout-changed", &payload));
    assert_eq!(first.observe.len(), 1);
    assert!(first.again);
    assert!(first.apply_channels.is_empty());

    g.observe("9".into(), Vec::new(), Vec::new());
    let second = planned(&g, &discord("layout-changed", &payload));
    assert!(second.observe.is_empty());
    assert!(!second.again);
}

#[test]
fn a_tilo_role_change_reaches_the_linked_discord_account() {
    let mut g = guild();
    g.map_roles = true;
    g.map.push(mapping(MappedKind::Role, "moderator", "r_mod"));
    g.links.push(Link {
        discord_user: "42".into(),
        person: "u_ada".into(),
        ..Link::default()
    });
    // Given a new role, or added back to the workspace with one.
    for verb in ["set-role", "add-member"] {
        let event = json!({
            "kind": WORKSPACE_KIND,
            "did": {"did": "verb", "action": {"app": "tilo", "kind": WORKSPACE_KIND, "action": verb}},
            "arguments": {"person": "u_ada", "role": "moderator"},
        });
        let plan = planned(&g, &event);
        assert_eq!(plan.member_roles[0]["user"], "42", "{verb}");
        assert_eq!(plan.member_roles[0]["add"], json!(["r_mod"]), "{verb}");
    }
}

#[test]
fn a_workspace_published_under_its_old_kind_is_still_followed() {
    let mut g = guild();
    g.links.push(Link {
        discord_user: "42".into(),
        person: "u_ada".into(),
        ..Link::default()
    });
    // A host that still publishes the workspace's own record under the
    // kind it had before the rename plans the same as one that does not.
    let event = |kind: &str| {
        json!({
            "kind": kind,
            "did": {"did": "verb", "action": {"app": "tilo", "kind": kind, "action": "ban"}},
            "arguments": {"person": "u_ada", "reason": "spam"},
        })
    };
    let named = planned(&g, &event(WORKSPACE_KIND));
    let old = planned(&g, &event(LEGACY_WORKSPACE_KIND));
    assert_eq!(named.bans, vec![json!({"user": "42", "reason": "spam"})]);
    assert_eq!(old.bans, named.bans);
}

#[test]
fn removing_banning_and_unbanning_a_linked_person_reaches_discord() {
    let mut g = guild();
    g.links.push(Link {
        discord_user: "42".into(),
        person: "u_ada".into(),
        ..Link::default()
    });
    let workspace = |verb: &str, person: &str| {
        json!({
            "kind": WORKSPACE_KIND,
            "did": {"did": "verb", "action": {"app": "tilo", "kind": WORKSPACE_KIND, "action": verb}},
            "arguments": {"person": person, "reason": "spam"},
        })
    };
    let banned = planned(&g, &workspace("ban", "u_ada"));
    assert_eq!(banned.bans, vec![json!({"user": "42", "reason": "spam"})]);
    assert!(banned.member_roles.is_empty());
    assert_eq!(
        planned(&g, &workspace("remove-member", "u_ada"))
            .kicks
            .len(),
        1
    );
    assert_eq!(planned(&g, &workspace("unban", "u_ada")).unbans.len(), 1);
    // A guest with no linked account has nothing to act on in Discord.
    assert!(planned(&g, &workspace("ban", "u_bo")).bans.is_empty());
}

/// A Chat change's history entry for a reaction: the framework's field
/// edit of a message's reactions.
fn reaction_edit(op: &str, message: &str, emoji: &str) -> Value {
    json!({"did": "edit", "ty": "message", "field": "reactions", "op": op, "new": emoji,
           "path": [{"field": "messages"}, {"item": message}, {"field": "reactions"}]})
}

#[test]
fn a_tilo_reaction_is_routed_to_its_message_and_becomes_the_bots() {
    let mut g = guild();
    g.map.push(mapping(MappedKind::Message, "m1", "dm1"));
    let event = json!({"kind": MESSAGES_KIND, "did": {"did": "group", "members": [reaction_edit("react", "m1", "👍")]}});
    assert_eq!(route(&g, &event), "m1");
    let chat = json!({"messages": [{"id": "m1", "value": {"channel": "c1", "author": "u_ada", "text": "hello",
        "reactions": [{"emoji": "👍", "by": "u_bo", "at": 1}]}}]});
    let read = |chat: &Value, event: &Value| {
        plan(
            Reading {
                guild: &g,
                chat,
                context: &Context::default(),
                workspace: &Value::Null,
                emoji: None,
            },
            event,
        )
    };
    let planned = read(&chat, &event);
    assert_eq!(
        planned.reacts,
        [json!({"channel": "d1", "message": "dm1", "emoji": "👍"})]
    );
    assert!(planned.apply_channels.is_empty() && !planned.again);

    let taken = json!({"kind": MESSAGES_KIND, "did": reaction_edit("unreact", "m1", "👍")});
    let none = json!({"messages": [{"id": "m1", "value": {"channel": "c1", "author": "u_ada", "text": "hello", "reactions": []}}]});
    assert_eq!(
        read(&none, &taken).unreacts,
        [json!({"channel": "d1", "message": "dm1", "emoji": "👍"})]
    );
    // Another field edit of a message is no reaction.
    let pinned = json!({"kind": MESSAGES_KIND, "did": {"did": "edit", "op": "set", "new": true,
        "path": [{"field": "messages"}, {"item": "m1"}, {"field": "pinned"}]}});
    assert_eq!(read(&chat, &pinned), BridgePlan::default());
}

#[test]
fn a_discord_reaction_is_the_members_own_reaction_on_the_mapped_message() {
    let mut g = guild();
    g.map.push(mapping(MappedKind::Message, "m1", "dm1"));
    let event = discord(
        source::REACTION_ADDED,
        &json!({"guild_id": "1", "channel_id": "d1", "message_id": "dm1", "user_id": "d7",
                "user_name": "Dee", "emoji": ":party:", "emoji_id": "77", "animated": true}),
    );
    assert_eq!(route(&g, &event), "m1", "read with the message it is on");
    let planned = planned(&g, &event);
    assert_eq!(planned.chat_reacts.len(), 1, "{planned:?}");
    let react = &planned.chat_reacts[0];
    assert_eq!(react["message"], "m1");
    assert_eq!(react["emoji"], ":party:");
    assert_eq!(react["appearance"]["name"], "Dee");
    assert_eq!(react["appearance"]["persona"]["account"], "d7");
    // The bot's own reaction comes back from Discord as nothing.
    let echo = discord(
        source::REACTION_REMOVED,
        &json!({"guild_id": "1", "channel_id": "d1", "message_id": "dm1", "user_id": "9", "emoji": "👍"}),
    );
    assert_eq!(planned_in(&g, &Value::Null, &echo), BridgePlan::default());
}

/// The chat as `read-workspace` answers it with a filed report's message: its
/// text read out of the report, its picture as an image, a reaction on it.
fn with_report() -> Value {
    json!({
        "messages": [{"id": "m_feedback_r1", "value": {
            "channel": "c1", "author": "u_ada",
            "text": "Saving does nothing\n1. Files: Save (button)",
            "attachment": {"$variant": "image", "name": "report.png", "blob": "ab", "w": 320, "h": 200},
            "reactions": [{"emoji": "👍", "by": "u_bo", "at": 1}]
        }}],
        "people": [{"id": "u_ada", "name": "Ada", "agent": false}]
    })
}

fn read_with_report(guild: &Guild, event: &Value) -> BridgePlan {
    plan(
        Reading {
            guild,
            chat: &with_report(),
            context: &Context::default(),
            workspace: &Value::Null,
            emoji: None,
        },
        event,
    )
}

#[test]
fn a_filed_report_goes_across_as_its_message_with_its_picture() {
    // Chat records filing on the channel, with the submission it filed.
    let event = json!({
        "kind": CHAT_KIND,
        "did": verb(SUBMIT_FEEDBACK),
        "item": {"kind": "channels", "id": "c1"},
        "actor": {"id": "u_ada"},
        "arguments": {"channel": "c1", "submission": {"id": "r1"}},
    });
    assert_eq!(route(&guild(), &event), "m_feedback_r1");
    let planned = read_with_report(&guild(), &event);
    assert_eq!(planned.sends.len(), 1, "{planned:?}");
    let op = &planned.sends[0]["op"];
    assert_eq!(op["tilo"], "m_feedback_r1");
    assert_eq!(op["username"], "Ada");
    assert_eq!(
        op["content"],
        "Saving does nothing\n1. Files: Save (button)"
    );
    assert_eq!(
        op["attachments"],
        json!([{"blob": "ab", "name": "report.png", "size": 0}])
    );
    assert!(
        planned.apply_channels.is_empty(),
        "filing changes no layout"
    );
    let mut sent = guild();
    sent.map
        .push(mapping(MappedKind::Message, "m_feedback_r1", "dm9"));
    assert!(
        read_with_report(&sent, &event).sends.is_empty(),
        "a report already in Discord is not sent again"
    );
}

#[test]
fn a_reaction_to_a_filed_report_goes_across_on_its_message() {
    let mut g = guild();
    g.map
        .push(mapping(MappedKind::Message, "m_feedback_r1", "dm9"));
    let edit = json!({"did": "edit", "op": "react", "new": "👍",
        "path": [{"field": "feedback"}, {"item": "r1"}, {"field": "reactions"}]});
    let event = json!({"kind": CHAT_KIND, "did": edit});
    assert_eq!(route(&g, &event), "m_feedback_r1");
    assert_eq!(
        read_with_report(&g, &event).reacts,
        [json!({"channel": "d1", "message": "dm9", "emoji": "👍"})]
    );
}

#[test]
fn the_servers_custom_emoji_are_recorded_with_its_layout() {
    let payload = json!({"guild_id": "1", "bot_user": "9", "channels": [], "roles": [],
        "emojis": [{"id": "55", "name": "tilo_hello", "animated": false}]});
    let mut g = guild();
    let first = planned(&g, &discord("layout-changed", &payload));
    assert_eq!(first.observe[0]["emojis"][0]["name"], "tilo_hello");
    g.observe_emojis(vec![discord_wire::event::EmojiInfo {
        id: "55".into(),
        name: "tilo_hello".into(),
        animated: false,
    }]);
    assert!(
        planned(&g, &discord("layout-changed", &payload))
            .observe
            .is_empty()
    );
}

/// A mirroring bridge of "talk" with #one and #two, Discord listing them so.
fn arranged_guild() -> Guild {
    let mut g = guild();
    g.mirror_channels = true;
    g.map = [
        mapping(MappedKind::Category, "k_talk", "d_talk"),
        mapping(MappedKind::Channel, "c_one", "d_one"),
        mapping(MappedKind::Channel, "c_two", "d_two"),
        mapping(MappedKind::Webhook, "c_one", "wh_one"),
        mapping(MappedKind::Webhook, "c_two", "wh_two"),
    ]
    .into_iter()
    .collect();
    observed(&mut g, &[("d_one", 0), ("d_two", 1)]);
    g
}

/// Discord's layout with "talk"'s channels at these positions.
fn layout_payload(positions: &[(&str, i64)], connected: bool) -> Value {
    let mut channels =
        vec![json!({"id": "d_talk", "kind": "category", "name": "talk", "position": 0})];
    channels.extend(positions.iter().map(|(id, position)| {
        json!({"id": id, "kind": "text", "name": id.trim_start_matches("d_"),
            "parent": "d_talk", "position": position})
    }));
    json!({"guild_id": "1", "bot_user": "9", "channels": channels, "roles": [],
        "connected": connected})
}

/// `g` with "talk"'s channels recorded at these positions.
fn observed(g: &mut Guild, positions: &[(&str, i64)]) {
    let layout: discord_wire::event::LayoutChanged =
        serde_json::from_value(layout_payload(positions, false)).unwrap();
    g.observe("9".into(), layout.channels, Vec::new());
}

/// The workspace's chat with "talk" listing `channels` in this order.
fn talk(channels: &[&str]) -> Value {
    json!({
        "categories": [{"id": "k_talk", "value": {"name": "talk"}}],
        "channels": channels.iter().map(|id| json!({"id": id, "value": {
            "category": "k_talk", "name": id.trim_start_matches("c_"), "topic": "",
            "kind": "text", "archived": false}})).collect::<Vec<_>>(),
    })
}

fn planned_on(guild: &Guild, chat: &Value, event: &Value) -> BridgePlan {
    plan(
        Reading {
            guild,
            chat,
            context: &Context::default(),
            workspace: &Value::Null,
            emoji: None,
        },
        event,
    )
}

#[test]
fn a_channel_moved_in_discord_rearranges_chat_with_the_layout() {
    let g = arranged_guild();
    let event = discord(
        "layout-changed",
        &layout_payload(&[("d_one", 1), ("d_two", 0)], false),
    );
    let first = planned_on(&g, &talk(&["c_one", "c_two"]), &event);
    assert_eq!(first.observe.len(), 1);
    assert_eq!(
        first.chat_arrangements,
        [json!({"categories": ["k_talk"], "channels": [
            {"channel": "c_two", "category": "k_talk"},
            {"channel": "c_one", "category": "k_talk"},
        ]})]
    );
    assert!(first.arrange_channels.is_empty(), "never sent back");
    // Once recorded, with Chat rearranged, nothing is left to do.
    let mut recorded = g;
    observed(&mut recorded, &[("d_one", 1), ("d_two", 0)]);
    let second = planned_on(&recorded, &talk(&["c_two", "c_one"]), &event);
    assert!(second.chat_arrangements.is_empty() && second.arrange_channels.is_empty());
}

#[test]
fn a_reconnect_brings_chat_in_line_with_discord() {
    let g = arranged_guild();
    let chat = talk(&["c_two", "c_one"]);
    let same = layout_payload(&[("d_one", 0), ("d_two", 1)], false);
    assert!(
        planned_on(&g, &chat, &discord("layout-changed", &same))
            .chat_arrangements
            .is_empty(),
        "a layout that moved nothing leaves Chat's order be"
    );
    let connected = layout_payload(&[("d_one", 0), ("d_two", 1)], true);
    assert_eq!(
        planned_on(&g, &chat, &discord("layout-changed", &connected))
            .chat_arrangements
            .len(),
        1
    );
}

#[test]
fn a_channel_moved_in_chat_moves_in_discord() {
    let g = arranged_guild();
    let chat = talk(&["c_two", "c_one"]);
    let moved = json!({"kind": CHAT_KIND, "did": verb("move-channel"),
        "item": {"id": "c_two"}});
    let plan = planned_on(&g, &chat, &moved);
    assert_eq!(
        plan.arrange_channels,
        [json!({"categories": [], "channels": [
            {"channel": "d_two", "parent": "d_talk", "position": 0},
            {"channel": "d_one", "parent": "d_talk", "position": 1},
        ]})]
    );
    assert!(plan.chat_arrangements.is_empty());
    // A change that rearranges nothing does not move Discord's channels.
    let read = json!({"kind": CHAT_KIND, "did": verb("mark-read"), "item": {"id": "c_two"}});
    assert!(planned_on(&g, &chat, &read).arrange_channels.is_empty());
}
