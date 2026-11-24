use serde_json::{Value, json};

use super::*;
use crate::guild::Link;
use crate::layout::tests::mapping;

/// The published id of Chat's resource `id`.
fn at(id: &str) -> String {
    format!("tl:so.tilo.chat:{id}")
}

fn guild() -> Guild {
    let mut g = Guild {
        guild_id: "1".into(),
        bot_user: "9".into(),
        ownership: Ownership::Adopted,
        ..Guild::default()
    };
    g.map = [
        mapping(MappedKind::Channel, &at("c1"), "d1"),
        mapping(MappedKind::Webhook, &at("c1"), "wh1"),
    ]
    .into_iter()
    .collect();
    g
}

/// A message row of the tree.
fn row(id: &str, author: &str, body: &str, extra: &Value) -> Value {
    let mut fields =
        json!({"body": body, "by": "", "written_in": "", "written": 0, "reactions": {}});
    if let (Some(fields), Some(extra)) = (fields.as_object_mut(), extra.as_object()) {
        fields.extend(extra.clone());
    }
    json!({
        "item": at(id),
        "kind": "tl.so.tilo.chat.Message",
        "id": id,
        "created": {"who": {"actor": {"member": {"person": author}}}},
        "fields": fields,
    })
}

/// A chat whose `general` channel (under the category `chat`) holds `messages`, and `goal` at
/// the top.
fn chat_with(messages: &[Value]) -> crate::tilo::ChatRead {
    let channel = json!({
        "item": at("c1"),
        "kind": "tl.so.tilo.chat.TextChannel",
        "id": "c1",
        "fields": {"name": "general", "topic": "talk", "messages": {"items": messages}},
    });
    let goal = json!({
        "item": at("c_goal"),
        "kind": "tl.so.tilo.chat.TextChannel",
        "id": "c_goal",
        "fields": {"name": "goal", "topic": "", "messages": {"items": []}},
    });
    let category = json!({
        "item": at("k_chat"),
        "kind": "tl.so.tilo.chat.Category",
        "id": "k_chat",
        "fields": {"name": "chat", "channels": {"items": [channel]}},
    });
    crate::tilo::ChatRead::of(&json!({
        "records": [{
            "item": at("root"),
            "kind": "tl.so.tilo.chat.Conversations",
            "fields": {"channels": {"items": [goal, category]}},
        }],
    }))
    .knowing(&json!({"members": [{"member": {"person": "u_ada"}, "name": "Ada"}]}))
}

fn chat() -> crate::tilo::ChatRead {
    // The bridge's own channel `c1`, holding Ada's `hello`.
    let channel = json!({
        "item": at("c1"),
        "kind": "tl.so.tilo.chat.TextChannel",
        "id": "c1",
        "fields": {"name": "general", "topic": "", "messages": {"items": [
            row("m1", "u_ada", "hello", &json!({}))
        ]}},
    });
    crate::tilo::ChatRead::of(&json!({
        "records": [{
            "item": at("root"),
            "kind": "tl.so.tilo.chat.Conversations",
            "fields": {"channels": {"items": [channel]}},
        }],
    }))
    .knowing(&json!({"members": [{"member": {"person": "u_ada"}, "name": "Ada"}]}))
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

tilo_test_env::scenario! {
    fn what_happens_in_discord_is_planned_for_tilo;

    phase a_tilo_message_deleted_in_discord_is_removed_in_chat_not_sent_again;
    {
        let mut g = guild();
        g.map.push(mapping(MappedKind::Message, &at("m1"), "dm1"));
        let deleted = discord(
            source::MESSAGE_DELETED,
            &json!({"guild_id": "1", "channel_id": "d1", "message_id": "dm1", "executor": "d_admin"}),
        );
        let removed = planned(&g, &deleted);
        assert_eq!(
            removed.chat_removes,
            [json!({"message": at("m1"), "channel": at("c1"),
                    "channel_kind": "tl.so.tilo.chat.TextChannel",
                    "reason": "deleted in Discord"})],
            "{removed:?}"
        );
        assert!(removed.sends.is_empty(), "never sent again: {removed:?}");
    }

    phase a_discord_message_is_relayed_on_the_mapped_channels_item_with_its_author;
    {
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
        assert_eq!(post["channel"], at("c1"));
        assert_eq!(post["by"], "thor");
        assert_eq!(post["written_in"], "Discord");
        assert_eq!(post["persona"], "42");
        assert_eq!(post["discord"], "dm7");

        let echo = json!({"guild_id": "1", "channel_id": "d1", "message_id": "dm8", "content": "x", "author": {"id": "9", "name": "bridge"}});
        assert!(
            planned(&guild(), &discord("message-created", &echo))
                .posts
                .is_empty(),
            "the bot's own message is not mirrored back"
        );
    }

    phase a_layout_event_is_recorded_first_and_reconciled_once_recorded;
    {
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

    phase a_discord_reaction_is_relayed_onto_the_mapped_message_under_its_givers_name;
    {
        let mut g = guild();
        g.map.push(mapping(MappedKind::Message, &at("m1"), "dm1"));
        let event = discord(
            source::REACTION_ADDED,
            &json!({"guild_id": "1", "channel_id": "d1", "message_id": "dm1", "user_id": "d7",
                    "user_name": "Dee", "emoji": ":party:", "emoji_id": "77", "animated": true}),
        );
        let planned = planned(&g, &event);
        assert_eq!(planned.chat_reacts.len(), 1, "{planned:?}");
        let react = &planned.chat_reacts[0];
        assert_eq!(react["message"], at("m1"));
        assert_eq!(react["emoji"], ":party:");
        assert_eq!(react["persona"], "discord/d7");
        assert_eq!(react["by"], "Dee");
        // The bot's own reaction comes back from Discord as nothing.
        let echo = discord(
            source::REACTION_REMOVED,
            &json!({"guild_id": "1", "channel_id": "d1", "message_id": "dm1", "user_id": "9", "emoji": "👍"}),
        );
        assert_eq!(planned_in(&g, &Value::Null, &echo), BridgePlan::default());
    }

    phase the_servers_custom_emoji_are_recorded_with_its_layout;
    {
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
}

fn discord(source: &str, payload: &Value) -> Value {
    json!({"app_id": crate::APP_ID, "source": source, "id": "e1", "at_ms": 0, "payload": payload})
}

/// Who made a change, as the program's `event` input carries it: its acting
/// tree, where a person acting for themselves is a member of their own.
fn actor(person: &str) -> Value {
    json!({"by": {"member": {"person": person}}})
}

tilo_test_env::scenario! {
    fn what_happens_in_tilo_is_planned_for_discord;

    phase a_chat_send_is_routed_to_its_message_and_sent_once;
    {
        let event = json!({
            "kind": "tl.so.tilo.chat.TextChannel",
            "did": verb("send"),
            "item": {"kind": "tl.so.tilo.chat.TextChannel", "id": "c1"},
            "actor": actor("u_ada"),
            "arguments": {"body": "hello"},
        });
        let plan = planned(&guild(), &event);
        assert_eq!(plan.sends.len(), 1);
        assert_eq!(plan.sends[0]["op"]["webhook"], "wh1");
        assert_eq!(plan.sends[0]["op"]["username"], "Ada");
        assert_eq!(plan.sends[0]["op"]["tilo"], at("m1"));
        assert!(!plan.again, "a send unblocks nothing");
        let mut sent = guild();
        sent.map.push(mapping(MappedKind::Message, &at("m1"), "dm1"));
        assert!(
            planned(&sent, &event).sends.is_empty(),
            "a message already in Discord is not sent again"
        );
    }

    phase a_chat_delete_finds_the_channel_the_message_was_in;
    {
        let mut g = guild();
        g.map.push(mapping(MappedKind::Message, &at("m1"), "dm1"));
        let event = json!({
            "kind": "tl.so.tilo.chat.TextChannel",
            "did": {"did": "group", "members": [verb("delete")]},
            "item": {"kind": "tl.so.tilo.chat.TextChannel", "id": "c1"},
            "arguments": {"m": at("m1")},
        });
        let plan = planned(&g, &event);
        assert_eq!(plan.deletes[0]["op"]["message"], "dm1");
        assert_eq!(plan.deletes[0]["tilo"], at("m1"));
    }

    phase a_tilo_role_change_reaches_the_linked_discord_account;
    {
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

    phase removing_banning_and_unbanning_a_linked_person_reaches_discord;
    {
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

    phase a_tilo_reaction_is_routed_to_its_message_and_becomes_the_bots;
    {
        let mut g = guild();
        g.map.push(mapping(MappedKind::Message, &at("m1"), "dm1"));
        let reacted = chat_with(&[row(
            "m1",
            "u_ada",
            "hello",
            &json!({"reactions": {"👍": {"count": 1,
                "items": [{"actor": {"member": {"person": "u_bo"}}}]}}}),
        )]);
        let read = |chat: &crate::tilo::ChatRead, event: &Value| {
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
        let event = json!({
            "kind": "tl.so.tilo.chat.Message",
            "did": verb("react"),
            "item": {"kind": "tl.so.tilo.chat.Message", "id": "m1"},
            "actor": actor("u_bo"),
            "arguments": {"emoji": "👍"},
        });
        let planned = read(&reacted, &event);
        assert_eq!(
            planned.reacts,
            [json!({"channel": "d1", "message": "dm1", "emoji": "👍"})]
        );
        assert!(planned.apply_channels.is_empty() && !planned.again);

        let none = chat_with(&[row("m1", "u_ada", "hello", &json!({}))]);
        assert_eq!(
            read(&none, &event).unreacts,
            [json!({"channel": "d1", "message": "dm1", "emoji": "👍"})],
            "Bo took it back"
        );
        // Another kind of change to a message is no reaction.
        let pinned = json!({
            "kind": "tl.so.tilo.chat.Message",
            "did": verb("pin"),
            "item": {"kind": "tl.so.tilo.chat.Message", "id": "m1"},
        });
        assert!(!read(&reacted, &pinned).again, "{:?}", read(&reacted, &pinned));
    }

    phase a_reaction_relayed_from_elsewhere_is_not_sent_back;
    {
        let mut g = guild();
        g.map.push(mapping(MappedKind::Message, &at("m1"), "dm1"));
        let relayed = json!({
            "kind": "tl.so.tilo.chat.Message",
            "did": verb("relay_reaction"),
            "item": {"kind": "tl.so.tilo.chat.Message", "id": "m1"},
            "actor": actor("u_bo"),
            "arguments": {"emoji": "🎉", "persona": "discord/3001", "by": "Bo", "given": true},
        });
        let plan = planned(&g, &relayed);
        assert!(
            plan.reacts.is_empty() && plan.unreacts.is_empty(),
            "{plan:?}"
        );
        assert!(!plan.again, "a relayed reaction unblocks nothing");
    }
}

/// A mirroring bridge of "talk" with #one and #two, Discord listing them so.
fn arranged_guild() -> Guild {
    let mut g = guild();
    g.mirror_channels = true;
    g.map = [
        mapping(MappedKind::Category, &at("k_talk"), "d_talk"),
        mapping(MappedKind::Channel, &at("c_one"), "d_one"),
        mapping(MappedKind::Channel, &at("c_two"), "d_two"),
        mapping(MappedKind::Webhook, &at("c_one"), "wh_one"),
        mapping(MappedKind::Webhook, &at("c_two"), "wh_two"),
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
fn talk(channels: &[&str]) -> crate::tilo::ChatRead {
    let listed: Vec<Value> = channels
        .iter()
        .enumerate()
        .map(|(order, id)| {
            json!({
                "item": at(id),
                "kind": "tl.so.tilo.chat.TextChannel",
                "id": id,
                "fields": {"name": id.trim_start_matches("c_"), "topic": "",
                           "messages": {"items": []}, "order": order},
            })
        })
        .collect();
    let category = json!({
        "item": at("k_talk"),
        "kind": "tl.so.tilo.chat.Category",
        "id": "k_talk",
        "fields": {"name": "talk", "channels": {"items": listed}},
    });
    crate::tilo::ChatRead::of(&json!({
        "records": [{
            "item": at("root"),
            "kind": "tl.so.tilo.chat.Conversations",
            "fields": {"channels": {"items": [category]}},
        }],
    }))
}

fn planned_on(guild: &Guild, chat: &crate::tilo::ChatRead, event: &Value) -> BridgePlan {
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

tilo_test_env::scenario! {
    fn a_channel_moved_on_one_side_is_planned_for_the_other;

    phase a_channel_moved_in_discord_rearranges_chat_with_the_layout;
    {
        let g = arranged_guild();
        let event = discord(
            "layout-changed",
            &layout_payload(&[("d_one", 1), ("d_two", 0)], false),
        );
        let first = planned_on(&g, &talk(&["c_one", "c_two"]), &event);
        assert_eq!(first.observe.len(), 1);
        assert!(
            first
                .chat_moves
                .iter()
                .any(|moved| moved["id"].as_str().unwrap_or_default() == "tl:so.tilo.chat:c_two"),
            "{:?}",
            first.chat_moves
        );
        assert!(
            first.chat_moves.iter().any(|moved| {
                moved["id"] == json!(at("c_two"))
                    && moved["category"]["id"] == json!(at("k_talk"))
                    && moved["at"] == json!(0)
            }),
            "{:?}",
            first.chat_moves
        );
        assert!(first.arrange_channels.is_empty(), "never sent back");
        // Once recorded, with Chat rearranged, nothing is left to do.
        let mut recorded = g;
        observed(&mut recorded, &[("d_one", 1), ("d_two", 0)]);
        let second = planned_on(&recorded, &talk(&["c_two", "c_one"]), &event);
        assert!(second.chat_moves.is_empty() && second.arrange_channels.is_empty());
    }

    phase a_reconnect_brings_chat_in_line_with_discord;
    {
        let g = arranged_guild();
        let chat = talk(&["c_two", "c_one"]);
        let same = layout_payload(&[("d_one", 0), ("d_two", 1)], false);
        assert!(
            planned_on(&g, &chat, &discord("layout-changed", &same))
                .chat_moves
                .is_empty(),
            "a layout that moved nothing leaves Chat's order be"
        );
        let connected = layout_payload(&[("d_one", 0), ("d_two", 1)], true);
        assert_eq!(
            planned_on(&g, &chat, &discord("layout-changed", &connected))
                .chat_moves
                .len(),
            3,
            "the category and its two channels"
        );
    }

    phase a_channel_moved_in_chat_moves_in_discord;
    {
        let g = arranged_guild();
        let chat = talk(&["c_two", "c_one"]);
        let moved = json!({"kind": "tl.so.tilo.chat.TextChannel", "did": verb("move_channel"),
            "item": {"kind": "tl.so.tilo.chat.TextChannel", "id": "c_two"}});
        let plan = planned_on(&g, &chat, &moved);
        assert_eq!(
            plan.arrange_channels,
            [json!({"categories": [], "channels": [
                {"channel": "d_two", "parent": "d_talk", "position": 0},
                {"channel": "d_one", "parent": "d_talk", "position": 1},
            ]})]
        );
        assert!(plan.chat_moves.is_empty());
        // A change that rearranges nothing does not move Discord's channels.
        let read = json!({"kind": "tl.so.tilo.chat.TextChannel", "did": verb("mark-read"),
            "item": {"kind": "tl.so.tilo.chat.TextChannel", "id": "c_two"}});
        assert!(planned_on(&g, &chat, &read).arrange_channels.is_empty());
    }
}
