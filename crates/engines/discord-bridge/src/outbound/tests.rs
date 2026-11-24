use serde_json::{Value, json};

use super::*;
use crate::guild::{Link, Mapping, Ownership};
use crate::layout::tests::mapping;

/// The published id of Chat's resource `id`.
fn at(id: &str) -> String {
    format!("tl:so.tilo.chat:{id}")
}

fn guild() -> Guild {
    let mut g = Guild {
        guild_id: "1".into(),
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

/// A member as the tree read names one in a message's `created`.
fn actor(author: &str) -> Value {
    let member = author
        .strip_prefix("agent:")
        .map_or_else(|| json!({"person": author}), |id| json!({"agent": id}));
    json!({"who": {"actor": {"member": member}}})
}

/// A message row of the tree.
fn row(id: &str, author: &str, text: &str, extra: &Value) -> Value {
    let mut fields = json!({
        "body": text,
        "by": "",
        "written_in": "",
        "written": 0,
        "reactions": {},
    });
    if let (Some(fields), Some(extra)) = (fields.as_object_mut(), extra.as_object()) {
        fields.extend(extra.clone());
    }
    let created = actor(author);
    json!({
        "item": at(id),
        "kind": "tl.so.tilo.chat.Message",
        "id": id,
        "created": created,
        "fields": fields,
    })
}

fn msg(id: &str, author: &str, text: &str, extra: &Value) -> Value {
    row(id, author, text, extra)
}

fn chat(messages: &[Value]) -> crate::tilo::ChatRead {
    let channel = json!({
        "item": at("c1"),
        "kind": "tl.so.tilo.chat.TextChannel",
        "id": "c1",
        "fields": {"name": "general", "topic": "", "messages": {"items": messages}},
    });
    crate::tilo::ChatRead::of(&json!({
        "records": [{
            "item": at("root"),
            "kind": "tl.so.tilo.chat.Conversations",
            "fields": {"channels": {"items": [channel]}},
        }],
    }))
    .knowing(&json!({
        "members": [
            {"member": {"person": "u_ada"}, "name": "Ada"},
            {
                "member": {"person": "u_bo"},
                "name": "Bo",
                "avatar": "https://eu1.tilo.sh/peer/v1/avatars/bo",
            },
            {"member": {"agent": "x"}, "name": "Discord Scribe"}
        ]
    }))
}

fn context() -> Context {
    Context {
        avatars: "https://workspaces.example/avatars/".into(),
        link: "https://app.example/#m".into(),
        links: None,
    }
}

const WORKSPACE: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90";

tilo_test_env::scenario! {
    fn a_context_names_its_workspace;

    phase the_workspace_is_read_and_written_under_its_name;
    {
        let context: Context = serde_json::from_value(json!({
            "avatars": "https://workspaces.example/avatars/",
            "link": "https://app.example/#m",
            "links": {
                "web_entry": "https://tilo.example/",
                "origin": "https://lounge.tilo.example",
                "workspace": WORKSPACE,
                "host_device": "ab".repeat(32),
            },
        }))
        .unwrap();
        let links = context.links.unwrap();
        assert_eq!(links.workspace, WORKSPACE);
        let written = serde_json::to_string(&links).unwrap();
        assert!(written.contains("\"workspace\""), "{written}");
        assert_eq!(serde_json::from_str::<Links>(&written).unwrap(), *links);
    }
}

tilo_test_env::scenario! {
    fn chat_messages_are_planned_as_discord_calls;

    phase a_post_goes_out_through_the_channels_webhook_under_the_authors_name;
    {
        let chat = chat(&[msg("m1", "u_ada", "hello", &json!({}))]);
        let plan = plan_post(&guild(), &chat, "m1", &context());
        assert!(plan.threads.is_empty());
        let send = &plan.sends[0].op;
        assert_eq!(
            (send.channel.as_str(), send.webhook.as_str()),
            ("d1", "wh1")
        );
        assert_eq!(send.username, "Ada");
        assert_eq!(send.content, "hello");
        assert_eq!(send.tilo, at("m1"));
    }

    phase a_linked_account_shows_its_discord_avatar;
    {
        let mut g = guild();
        g.links.push(Link {
            discord_user: "d_ada".into(),
            person: "u_ada".into(),
            avatar: Some("aa".into()),
            ..Link::default()
        });
        let chat = chat(&[msg("m1", "u_ada", "hello", &json!({}))]);
        let send = &plan_post(&g, &chat, "m1", &context()).sends[0].op;
        assert_eq!(
            send.avatar_url.as_deref(),
            Some("https://cdn.discordapp.com/avatars/d_ada/aa.png?size=128")
        );
        assert!(send.avatar_blob.is_none());
    }

    phase a_tilo_only_person_shows_the_picture_they_set;
    {
        // Bo linked no Discord account: the picture they set in Tilo, which their workspace's
        // host serves at a public address, is what Discord shows beside their name.
        let chat = chat(&[msg("m1", "u_bo", "hello", &json!({}))]);
        let send = &plan_post(&guild(), &chat, "m1", &context()).sends[0].op;
        assert_eq!(
            send.avatar_url.as_deref(),
            Some("https://eu1.tilo.sh/peer/v1/avatars/bo")
        );
        assert!(send.avatar_blob.is_none());
    }

    phase agents_get_the_robot_mark_and_a_cleaned_name;
    {
        let chat = chat(&[msg("m1", "agent:x", "done", &json!({}))]);
        let plan = plan_post(&guild(), &chat, "m1", &context());
        assert_eq!(plan.sends[0].op.username, "🤖 Scribe");
    }

    phase a_relayed_discord_message_is_not_sent_back;
    {
        let chat = chat(&[msg("m1", "u_ada", "hello", &json!({"by": "Ada", "written_in": "Discord"}))]);
        assert!(
            plan_post(&guild(), &chat, "m1", &context()).sends.is_empty(),
            "it came from Discord"
        );
    }

    phase unmapped_channels_and_mapped_messages_send_nothing;
    {
        let mut g = guild();
        let elsewhere = chat(&[]);
        assert!(plan_post(&g, &elsewhere, "m1", &context()).sends.is_empty());
        let chat2 = chat(&[msg("m2", "u_ada", "hello", &json!({}))]);
        g.map.push(mapping(MappedKind::Message, &at("m2"), "d_m2"));
        assert!(
            plan_post(&g, &chat2, "m2", &context()).sends.is_empty(),
            "already across"
        );
    }

    phase long_text_is_cut_with_link;
    {
        let chat = chat(&[msg("m1", "u_ada", &"é".repeat(5000), &json!({}))]);
        let content = &plan_post(&guild(), &chat, "m1", &context()).sends[0]
            .op
            .content;
        assert_eq!(content.chars().count(), MAX_CONTENT_CHARS);
        assert!(content.ends_with("…\nhttps://app.example/#m"));
    }

    phase a_reply_starts_the_thread_first_then_posts_into_it;
    {
        let mut g = guild();
        g.map.push(mapping(MappedKind::Message, &at("root"), "d_root"));
        let thread = json!({
            "item": at("t1"),
            "kind": "tl.so.tilo.chat.Thread",
            "id": "t1",
            "fields": {"name": "question?", "messages": {"items": [
                row("r1", "u_ada", "answer", &json!({}))
            ]}},
        });
        let channel = json!({
            "item": at("c1"),
            "kind": "tl.so.tilo.chat.TextChannel",
            "id": "c1",
            "fields": {"name": "general", "topic": "", "threads": {"items": [thread]},
                       "messages": {"items": [
                json!({
                    "item": at("root"),
                    "kind": "tl.so.tilo.chat.Message",
                    "id": "root",
                    "created": actor("u_bo"),
                    "fields": {"body": "question?\nmore", "by": "", "written_in": "", "written": 0,
                               "reactions": {}, "thread": {"type": "tl.so.tilo.chat.Thread", "id": at("t1")}},
                }),
            ]}},
        });
        let chat = crate::tilo::ChatRead::of(&json!({
            "records": [{
                "item": at("root-conversations"),
                "kind": "tl.so.tilo.chat.Conversations",
                "fields": {"channels": {"items": [channel]}},
            }],
        }))
        .knowing(&json!({"members": [{"member": {"person": "u_ada"}, "name": "Ada"}]}));
        let first = plan_post(&g, &chat, "r1", &context());
        assert!(first.sends.is_empty());
        assert_eq!(first.threads[0].message, "d_root");
        assert_eq!(first.threads[0].name, "question?");
        g.map
            .push(mapping(MappedKind::Thread, &at("root"), "d_thread"));
        let second = plan_post(&g, &chat, "r1", &context());
        assert_eq!(second.sends[0].op.thread.as_deref(), Some("d_thread"));
        assert_eq!(second.sends[0].root.as_deref(), Some(at("root").as_str()));
    }

    phase a_large_file_becomes_a_line;
    {
        let attached = |name: &str, media: &str, size: i64, blob: &str| {
            json!({"fields": {"name": name, "media": media, "size": size, "blob": blob}})
        };
        let small = attached("a.txt", "text/plain", 5, "tilo-blob://ff");
        let big = attached("film.mkv", "video/x-matroska", 20_971_520, "tilo-blob://ee");
        let chat = chat(&[
            msg("m1", "u_ada", "", &json!({"attachments": {"items": [small]}})),
            msg("m2", "u_ada", "watch", &json!({"attachments": {"items": [big]}})),
        ]);
        let one = &plan_post(&guild(), &chat, "m1", &context()).sends[0].op;
        assert_eq!(
            one.attachments,
            [OutAttachment {
                blob: "ff".into(),
                name: "a.txt".into(),
                size: 5
            }]
        );
        let two = &plan_post(&guild(), &chat, "m2", &context()).sends[0].op;
        assert!(two.attachments.is_empty());
        assert_eq!(
            two.content,
            "watch\n📎 film.mkv (21 MB) – open in Tilo https://app.example/#m"
        );
    }

    phase edits_and_deletes_follow_the_map;
    {
        let mut g = guild();
        let chat = chat(&[msg("m1", "u_ada", "fixed", &json!({}))]);
        assert!(
            plan_edit(&g, &chat, "m1", &context()).is_empty(),
            "not across yet"
        );
        g.map.push(mapping(MappedKind::Message, &at("m1"), "d_m1"));
        let edit = &plan_edit(&g, &chat, "m1", &context())[0];
        assert_eq!(
            (edit.message.as_str(), edit.content.as_str()),
            ("d_m1", "fixed")
        );
        let delete = &plan_delete(&g, &chat, &at("c1"), &at("m1"))[0];
        assert_eq!(delete.op.webhook.as_deref(), Some("wh1"));

        let mut from_discord = Mapping {
            origin: Origin::Discord,
            ..mapping(MappedKind::Message, &at("m9"), "d_m9")
        };
        from_discord.persona = Some("d_user".into());
        g.map.push(from_discord);
        assert!(
            plan_edit(&g, &chat, "m9", &context()).is_empty(),
            "a Discord message is edited in Discord"
        );
        assert_eq!(
            plan_delete(&g, &chat, &at("c1"), &at("m9"))[0].op.webhook,
            None,
            "removed by the bot user"
        );
    }

    phase clearing_an_attachment_caption_queues_an_empty_discord_edit;
    {
        // The app's verbs queue the edit; the planner's part is the content one carries.
        let mut bot = guild();
        bot.map.push(mapping(MappedKind::Message, &at("m1"), "dm1"));
        let attachment = json!({"attachments": {"items": [
            {"fields": {"name": "notes.txt", "media": "text/plain", "size": 5, "blob": "tilo-blob://ff"}}
        ]}});
        let after = chat(&[msg("m1", "u_ada", "", &attachment)]);
        let edits = plan_edit(&bot, &after, "m1", &context());
        assert_eq!(edits.len(), 1, "clearing a caption must reach Discord");
        assert_eq!(edits[0].message, "dm1");
        assert_eq!(edits[0].content, "");

        let empty_post = chat(&[msg("m2", "u_ada", "", &json!({}))]);
        assert!(
            plan_post(&bot, &empty_post, "m2", &context())
                .sends
                .is_empty(),
            "an empty new post still has nothing to send"
        );
    }

    phase the_bot_reacts_once_per_emoji_while_tilo_people_have_it;
    {
        let mut g = guild();
        g.map.push(mapping(MappedKind::Message, &at("m1"), "d_m1"));
        let reacted = |who: &[&str]| {
            let items: Vec<Value> = who
                .iter()
                .map(|person| json!({"actor": {"member": {"person": person}}}))
                .collect();
            chat(&[msg(
                "m1",
                "u_ada",
                "x",
                &json!({"reactions": {"👍": {"count": items.len(), "items": items}}}),
            )])
        };
        let one = reacted(&["u_ada"]);
        let (reacts, unreacts) = plan_reaction(&g, &one, "m1", "👍", true);
        assert_eq!(
            reacts,
            [ReactOp {
                channel: "d1".into(),
                message: "d_m1".into(),
                emoji: "👍".into(),
            }]
        );
        assert!(unreacts.is_empty());
        let two = reacted(&["u_ada", "u_bo"]);
        assert!(
            plan_reaction(&g, &two, "m1", "👍", true).0.is_empty(),
            "already there"
        );
        assert!(
            plan_reaction(&g, &one, "m1", "👍", false).1.is_empty(),
            "Ada still has it"
        );
        // The bridge's own reaction for Discord people never goes back out.
        let bot_only = chat(&[msg(
            "m1",
            "u_ada",
            "x",
            &json!({"reactions": {"👍": {"count": 1, "items": [{"actor": {"member": {"bot": "i"}}}]}}}),
        )]);
        assert_eq!(
            plan_reaction(&g, &bot_only, "m1", "👍", false).1.len(),
            1,
            "the last Tilo person took it back"
        );
        // One emoji spelled with or without its variation selector is one.
        let hearts = chat(&[msg(
            "m1",
            "u_ada",
            "x",
            &json!({"reactions": {
                "❤": {"count": 1, "items": [{"actor": {"member": {"person": "u_bo"}}}]},
                "❤️": {"count": 1, "items": [{"actor": {"member": {"person": "u_ada"}}}]},
            }}),
        )]);
        assert!(
            plan_reaction(&g, &hearts, "m1", "❤️", true).0.is_empty(),
            "Bo's heart is already the bot's"
        );
    }

    phase a_workspace_emoji_crosses_as_the_servers_emoji_of_its_name_only;
    {
        let mut g = guild();
        g.map.push(mapping(MappedKind::Message, &at("m1"), "d_m1"));
        g.observe_emojis(&[discord_wire::event::EmojiInfo {
            id: "55".into(),
            name: "tilo_hello".into(),
            animated: false,
        }]);
        let reacted = |emoji: &str, person: &str| {
            chat(&[msg(
                "m1",
                "u_ada",
                "x",
                &json!({"reactions": {emoji: {"count": 1, "items": [{"actor": {"member": {"person": person}}}]}}}),
            )])
        };
        assert_eq!(
            plan_reaction(&g, &reacted(":tilo_hello:", "u_ada"), "m1", ":tilo_hello:", true).0[0]
                .emoji,
            "tilo_hello:55",
            "Discord's reaction routes name a custom emoji `name:id`"
        );
        assert_eq!(
            plan_reaction(&g, &chat(&[msg("m1", "u_ada", "x", &json!({}))]), "m1", ":tilo_hello:", false).1[0]
                .emoji,
            "tilo_hello:55"
        );
        assert_eq!(
            plan_reaction(&g, &reacted(":pizza:", "u_ada"), "m1", ":pizza:", true),
            (Vec::new(), Vec::new()),
            "a workspace emoji the server has no emoji of stays in Tilo"
        );
    }
}
