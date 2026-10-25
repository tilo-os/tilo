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

fn planned_in(guild: &Guild, room: &Value, event: &Value) -> BridgePlan {
    plan(
        Reading {
            guild,
            chat: &chat(),
            context: &Context::default(),
            room,
        },
        event,
    )
}

#[test]
fn a_linked_tilo_moderators_delete_in_discord_removes_and_anyone_elses_resends() {
    let mut g = guild();
    g.map.push(mapping(MappedKind::Message, "m1", "dm1"));
    g.links.push(Link {
        discord_user: "d_mod".into(),
        person: "u_mod".into(),
        ..Link::default()
    });
    let deleted = discord(
        source::MESSAGE_DELETED,
        &json!({"guild_id": "1", "channel_id": "d1", "message_id": "dm1", "executor": "d_mod"}),
    );
    let room = json!({"members": [{"person": "u_mod", "role": "Moderator"}]});
    let removed = planned_in(&g, &room, &deleted);
    assert_eq!(removed.chat_removes.len(), 1, "{removed:?}");
    assert!(removed.sends.is_empty());
    // Without the room's roles nobody counts as a moderator.
    let resent = planned(&g, &deleted);
    assert!(resent.chat_removes.is_empty());
    assert_eq!(resent.sends.len(), 1, "{resent:?}");
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
    // Given a new role, or added back to the room with one.
    for verb in ["set-role", "add-member"] {
        let event = json!({
            "kind": ROOM_KIND,
            "did": {"did": "verb", "action": {"app": "tilo", "kind": ROOM_KIND, "action": verb}},
            "arguments": {"person": "u_ada", "role": "moderator"},
        });
        let plan = planned(&g, &event);
        assert_eq!(plan.member_roles[0]["user"], "42", "{verb}");
        assert_eq!(plan.member_roles[0]["add"], json!(["r_mod"]), "{verb}");
    }
}

#[test]
fn removing_banning_and_unbanning_a_linked_person_reaches_discord() {
    let mut g = guild();
    g.links.push(Link {
        discord_user: "42".into(),
        person: "u_ada".into(),
        ..Link::default()
    });
    let room = |verb: &str, person: &str| {
        json!({
            "kind": ROOM_KIND,
            "did": {"did": "verb", "action": {"app": "tilo", "kind": ROOM_KIND, "action": verb}},
            "arguments": {"person": person, "reason": "spam"},
        })
    };
    let banned = planned(&g, &room("ban", "u_ada"));
    assert_eq!(banned.bans, vec![json!({"user": "42", "reason": "spam"})]);
    assert!(banned.member_roles.is_empty());
    assert_eq!(planned(&g, &room("remove-member", "u_ada")).kicks.len(), 1);
    assert_eq!(planned(&g, &room("unban", "u_ada")).unbans.len(), 1);
    // A guest with no linked account has nothing to act on in Discord.
    assert!(planned(&g, &room("ban", "u_bo")).bans.is_empty());
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
                room: &Value::Null,
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
