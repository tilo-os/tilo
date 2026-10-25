use serde_json::{Value, json};

use super::*;
use crate::layout::tests::mapping;
use crate::record::{Link, Mapping, Ownership};

fn guild() -> Guild {
    let mut g = Guild {
        guild_id: "1".into(),
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

fn msg(id: &str, author: &str, text: &str, extra: &Value) -> Value {
    let mut value = json!({"channel": "c1", "author": author, "text": text, "parent": null});
    if let (Some(v), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        v.extend(extra.clone());
    }
    json!({"id": id, "value": value})
}

fn chat(messages: &[Value]) -> Value {
    json!({
        "messages": messages,
        "people": [
            {"id": "u_ada", "name": "Ada", "agent": false, "avatar": "av1"},
            {"id": "u_bo", "name": "Bo", "agent": false},
            {"id": "agent:x", "name": "Discord Scribe", "agent": true}
        ]
    })
}

fn context() -> Context {
    Context {
        avatars: "https://rooms.example/avatars/".into(),
        link: "https://app.example/#m".into(),
    }
}

#[test]
fn a_post_goes_out_through_the_channels_webhook_under_the_authors_name() {
    let chat = chat(&[msg("m1", "u_ada", "hello", &json!({}))]);
    let plan = plan_post(&guild(), &chat, "m1", &context());
    assert!(plan.threads.is_empty());
    let send = &plan.sends[0].op;
    assert_eq!(
        (send.channel.as_str(), send.webhook.as_str()),
        ("d1", "wh1")
    );
    assert_eq!(send.username, "Ada");
    assert_eq!(
        send.avatar_url.as_deref(),
        Some("https://rooms.example/avatars/av1")
    );
    assert_eq!(send.avatar_blob.as_deref(), Some("av1"));
    assert_eq!(send.content, "hello");
    assert_eq!(send.tilo, "m1");
}

#[test]
fn agents_get_the_robot_mark_and_a_cleaned_name() {
    let chat = chat(&[msg("m1", "agent:x", "done", &json!({}))]);
    let plan = plan_post(&guild(), &chat, "m1", &context());
    assert_eq!(plan.sends[0].op.username, "🤖 Scribe");
}

#[test]
fn unmapped_channels_and_mapped_messages_send_nothing() {
    let mut g = guild();
    let chat = chat(&[msg("m1", "u_ada", "hello", &json!({"channel": "c_other"}))]);
    assert!(plan_post(&g, &chat, "m1", &context()).sends.is_empty());
    let chat2 = self::chat(&[msg("m2", "u_ada", "hello", &json!({}))]);
    g.map.push(mapping(MappedKind::Message, "m2", "d_m2"));
    assert!(
        plan_post(&g, &chat2, "m2", &context()).sends.is_empty(),
        "already across"
    );
}

#[test]
fn long_text_is_cut_with_link() {
    let chat = chat(&[msg("m1", "u_ada", &"é".repeat(5000), &json!({}))]);
    let content = &plan_post(&guild(), &chat, "m1", &context()).sends[0]
        .op
        .content;
    assert_eq!(content.chars().count(), MAX_CONTENT_CHARS);
    assert!(content.ends_with("…\nhttps://app.example/#m"));
}

#[test]
fn linked_mentions_ping_and_others_are_plain() {
    let mut g = guild();
    g.links.push(Link {
        discord_user: "d_bo".into(),
        person: "u_bo".into(),
        ..Link::default()
    });
    let mentions = "[{\"user\":\"u_bo\",\"start\":3,\"len\":3,\"is_agent\":false},{\"user\":\"u_ada\",\"start\":11,\"len\":4,\"is_agent\":false}]";
    let chat = chat(&[msg(
        "m1",
        "u_ada",
        "hi @Bo and @Ada @everyone",
        &json!({"mentions": mentions}),
    )]);
    let content = &plan_post(&g, &chat, "m1", &context()).sends[0].op.content;
    assert_eq!(
        content, "hi <@d_bo> and Ada @everyone",
        "@everyone stays text; the send allows user mentions only"
    );
}

#[test]
fn a_reply_starts_the_thread_first_then_posts_into_it() {
    let mut g = guild();
    g.map.push(mapping(MappedKind::Message, "root", "d_root"));
    let chat = chat(&[
        msg("root", "u_bo", "question?\nmore", &json!({})),
        msg("r1", "u_ada", "answer", &json!({"parent": "root"})),
    ]);
    let first = plan_post(&g, &chat, "r1", &context());
    assert!(first.sends.is_empty());
    assert_eq!(first.threads[0].message, "d_root");
    assert_eq!(first.threads[0].name, "question?");
    g.map.push(mapping(MappedKind::Thread, "root", "d_thread"));
    let second = plan_post(&g, &chat, "r1", &context());
    assert_eq!(second.sends[0].op.thread.as_deref(), Some("d_thread"));
    assert_eq!(second.sends[0].root.as_deref(), Some("root"));
}

#[test]
fn a_large_file_becomes_a_line() {
    let small = "{\"File\":{\"name\":\"a.txt\",\"blob\":\"ff\",\"size\":5}}";
    let big = "{\"File\":{\"name\":\"film.mkv\",\"blob\":\"ee\",\"size\":20971520}}";
    let chat = chat(&[
        msg("m1", "u_ada", "", &json!({"attachment": small})),
        msg("m2", "u_ada", "watch", &json!({"attachment": big})),
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

#[test]
fn edits_and_deletes_follow_the_map() {
    let mut g = guild();
    let chat = chat(&[msg("m1", "u_ada", "fixed", &json!({}))]);
    assert!(
        plan_edit(&g, &chat, "m1", &context()).is_empty(),
        "not across yet"
    );
    g.map.push(mapping(MappedKind::Message, "m1", "d_m1"));
    let edit = &plan_edit(&g, &chat, "m1", &context())[0];
    assert_eq!(
        (edit.message.as_str(), edit.content.as_str()),
        ("d_m1", "fixed")
    );
    let delete = &plan_delete(&g, "c1", "m1")[0];
    assert_eq!(delete.op.webhook.as_deref(), Some("wh1"));

    let mut from_discord = Mapping {
        origin: Origin::Discord,
        ..mapping(MappedKind::Message, "m9", "d_m9")
    };
    from_discord.persona = Some("d_user".into());
    g.map.push(from_discord);
    assert!(
        plan_edit(&g, &chat, "m9", &context()).is_empty(),
        "a Discord message is edited in Discord"
    );
    assert_eq!(
        plan_delete(&g, "c1", "m9")[0].op.webhook,
        None,
        "removed by the bot user"
    );
}

#[test]
fn clearing_an_attachment_caption_queues_an_empty_discord_edit() {
    use crate::record::{guild, tests};

    let mut bot = tests::connected();
    for action in [
        tests::map(MappedKind::Channel, "c1", "d_general"),
        tests::map(MappedKind::Webhook, "c1", "wh1"),
    ] {
        bot.apply(action).unwrap();
    }
    let attachment = json!({"attachment": {"File": {
        "name": "notes.txt", "blob": "ff", "size": 5
    }}});
    let before = chat(&[msg("m1", "u_ada", "old caption", &attachment)]);
    let post = plan_post(&bot.state().unwrap(), &before, "m1", &context());
    assert_eq!(post.sends.len(), 1);
    assert_eq!(post.sends[0].op.attachments.len(), 1);
    bot.apply(Guild::SEND.with(guild::Send {
        op: post.sends[0].op.clone(),
    }))
    .unwrap();
    bot.apply(tests::map(MappedKind::Message, "m1", "dm1"))
        .unwrap();

    let after = chat(&[msg("m1", "u_ada", "", &attachment)]);
    let edits = plan_edit(&bot.state().unwrap(), &after, "m1", &context());
    assert_eq!(edits.len(), 1, "clearing a caption must reach Discord");
    assert_eq!(edits[0].message, "dm1");
    assert_eq!(edits[0].content, "");
    bot.apply(Guild::EDIT.with(guild::Edit {
        op: edits[0].clone(),
    }))
    .unwrap();
    let state = bot.state().unwrap();
    let queued: Vec<_> = state
        .outbox
        .pending()
        .filter_map(|(_, queued)| match &queued.op {
            discord_wire::op::DiscordOp::Edit(op) => Some(op),
            _ => None,
        })
        .collect();
    assert_eq!(queued, [&edits[0]]);
    let mut oversized = edits[0].clone();
    oversized.content = "x".repeat(MAX_CONTENT_CHARS + 1);
    assert!(
        bot.apply(Guild::EDIT.with(guild::Edit { op: oversized }))
            .is_err(),
        "empty captions must not relax Discord's length limit"
    );

    let empty_post = chat(&[msg("m2", "u_ada", "", &json!({}))]);
    assert!(
        plan_post(&state, &empty_post, "m2", &context())
            .sends
            .is_empty(),
        "an empty new post still has nothing to send"
    );
}

#[test]
fn the_bot_reacts_once_per_emoji_while_tilo_people_have_it() {
    let mut g = guild();
    g.map.push(mapping(MappedKind::Message, "m1", "d_m1"));
    let reacted = |rs: Value| chat(&[msg("m1", "u_ada", "x", &json!({"reactions": rs}))]);
    let one = reacted(json!([{"emoji": "👍", "by": "u_ada", "at": 1}]));
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
    let two = reacted(json!([
        {"emoji": "👍", "by": "u_ada", "at": 1},
        {"emoji": "👍", "by": "u_bo", "at": 2}
    ]));
    assert!(
        plan_reaction(&g, &two, "m1", "👍", true).0.is_empty(),
        "already there"
    );
    assert!(
        plan_reaction(&g, &one, "m1", "👍", false).1.is_empty(),
        "Ada still has it"
    );
    // What the bridge reacted for Discord people never goes back out.
    let persona_only = reacted(json!([
        {"emoji": "👍", "by": "bot:i", "persona": "discord/d7", "name": "Dee", "at": 1}
    ]));
    assert_eq!(
        plan_reaction(&g, &persona_only, "m1", "👍", false).1.len(),
        1,
        "the last Tilo person took it back"
    );
    // One emoji spelled with or without its variation selector is one.
    let hearts = reacted(json!([
        {"emoji": "❤", "by": "u_bo", "at": 1},
        {"emoji": "❤️", "by": "u_ada", "at": 2}
    ]));
    assert!(
        plan_reaction(&g, &hearts, "m1", "❤️", true).0.is_empty(),
        "Bo's heart is already the bot's"
    );
}

#[test]
fn a_workspace_emoji_crosses_as_the_servers_emoji_of_its_name_only() {
    let mut g = guild();
    g.map.push(mapping(MappedKind::Message, "m1", "d_m1"));
    g.observe_emojis(vec![discord_wire::event::EmojiInfo {
        id: "55".into(),
        name: "tilo_hello".into(),
        animated: false,
    }]);
    let reacted = |rs: Value| chat(&[msg("m1", "u_ada", "x", &json!({"reactions": rs}))]);
    let hello = reacted(json!([{"emoji": ":tilo_hello:", "by": "u_ada", "at": 1}]));
    assert_eq!(
        plan_reaction(&g, &hello, "m1", ":tilo_hello:", true).0[0].emoji,
        "tilo_hello:55",
        "Discord's reaction routes name a custom emoji `name:id`"
    );
    assert_eq!(
        plan_reaction(&g, &reacted(json!([])), "m1", ":tilo_hello:", false).1[0].emoji,
        "tilo_hello:55"
    );
    let pizza = reacted(json!([{"emoji": ":pizza:", "by": "u_ada", "at": 1}]));
    assert_eq!(
        plan_reaction(&g, &pizza, "m1", ":pizza:", true),
        (Vec::new(), Vec::new()),
        "a workspace emoji the server has no emoji of stays in Tilo"
    );
}
