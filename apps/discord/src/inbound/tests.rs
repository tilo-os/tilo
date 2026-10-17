use discord_wire::event::{AttachmentInfo, ThreadRef};
use serde_json::json;

use super::*;
use crate::layout::tests::mapping;
use crate::link;
use crate::record::{Link, Mapping, Ownership, PendingLink};

fn guild() -> Guild {
    let mut g = Guild {
        guild_id: "1".into(),
        bot_user: "b0".into(),
        ownership: Ownership::Adopted,
        ..Guild::default()
    };
    g.map = [
        mapping(MappedKind::Channel, "c1", "d1"),
        mapping(MappedKind::Webhook, "c1", "wh1"),
    ]
    .into_iter()
    .collect();
    g.avatars.insert("h1".into(), "blob1".into());
    g
}

fn author(id: &str) -> Author {
    Author {
        id: id.into(),
        name: "alice".into(),
        avatar: Some("h1".into()),
        avatar_url: Some("https://cdn.discordapp.com/avatars/u/h1.png".into()),
        ..Author::default()
    }
}

fn created(id: &str, content: &str) -> MessageCreated {
    MessageCreated {
        guild_id: "1".into(),
        channel_id: "d1".into(),
        message_id: id.into(),
        author: author("u7"),
        content: content.into(),
        ..MessageCreated::default()
    }
}

fn discord_origin(tilo: &str, discord: &str, root: Option<&str>) -> Mapping {
    Mapping {
        origin: Origin::Discord,
        persona: Some("u7".into()),
        root: root.map(Into::into),
        ..mapping(MappedKind::Message, tilo, discord)
    }
}

#[test]
fn a_message_is_posted_with_its_authors_appearance() {
    let plan = plan_created(&guild(), &created("dm1", "hello"));
    let post = &plan.posts[0];
    assert_eq!(
        (
            post.channel.as_str(),
            post.text.as_str(),
            post.discord.as_str()
        ),
        ("c1", "hello", "dm1")
    );
    assert_eq!(post.appearance.name, "alice");
    assert_eq!(post.appearance.avatar.as_deref(), Some("blob1"));
    assert_eq!(
        post.appearance.persona,
        Persona {
            service: "discord".into(),
            account: "u7".into()
        }
    );
}

#[test]
fn an_unknown_avatar_is_fetched_once_before_posting() {
    let mut g = guild();
    g.avatars.clear();
    let plan = plan_created(&g, &created("dm1", "hello"));
    assert!(plan.posts.is_empty());
    assert_eq!(plan.avatars[0].hash, "h1");
    assert_eq!(plan.avatars[0].op.max_bytes, MAX_AVATAR_BYTES);
}

#[test]
fn echoes_and_blocked_personas_plan_nothing() {
    let mut g = guild();
    let mut own = created("dm1", "x");
    own.author.webhook_id = Some("wh1".into());
    assert_eq!(
        plan_created(&g, &own),
        InboundPlan::default(),
        "the bridge's own webhook"
    );
    let mut bot = created("dm2", "x");
    bot.author.id = "b0".into();
    assert_eq!(
        plan_created(&g, &bot),
        InboundPlan::default(),
        "the bot user"
    );
    g.map.push(discord_origin("m3", "dm3", None));
    assert_eq!(
        plan_created(&g, &created("dm3", "again")),
        InboundPlan::default(),
        "a replayed event"
    );
}

#[test]
fn blocked_persona_is_skipped_for_every_event() {
    let mut g = guild();
    g.blocked_personas.insert("u7".into());
    g.map.push(discord_origin("m1", "dm1", None));
    assert_eq!(
        plan_created(&g, &created("dm9", "spam")),
        InboundPlan::default()
    );
    let update = MessageUpdated {
        message_id: "dm1".into(),
        author: author("u7"),
        content: "spam".into(),
        ..MessageUpdated::default()
    };
    assert_eq!(plan_updated(&g, &update), InboundPlan::default());
    let reaction = ReactionChanged {
        message_id: "dm1".into(),
        user_id: "u7".into(),
        emoji: "💩".into(),
        ..ReactionChanged::default()
    };
    assert_eq!(plan_reaction(&g, &reaction, true), InboundPlan::default());
    assert_eq!(
        plan_block(&g, "u7").deletes,
        ["m1"],
        "what was mirrored before the block is removed"
    );
}

#[test]
fn replies_join_the_roots_thread_and_quote_a_reply_to_a_reply() {
    let mut g = guild();
    g.map.push(discord_origin("root", "d_root", None));
    g.map.push(discord_origin("r1", "d_r1", Some("root")));
    let mut reply = created("d_new", "me too");
    reply.reply_to = Some("d_root".into());
    assert_eq!(
        plan_created(&g, &reply).posts[0].parent.as_deref(),
        Some("root")
    );

    let mut deep = created("d_deep", "agreed");
    deep.reply_to = Some("d_r1".into());
    deep.reply_author = Some("bo".into());
    deep.reply_text = Some("first line\nsecond".into());
    let post = &plan_created(&g, &deep).posts[0];
    assert_eq!(post.parent.as_deref(), Some("root"));
    assert_eq!(post.text, "> bo: first line\nagreed");
}

#[test]
fn a_message_in_a_discord_thread_replies_to_its_root() {
    let mut g = guild();
    g.map.push(mapping(MappedKind::Thread, "root", "d_thread"));
    let mut event = created("dm", "in thread");
    event.channel_id = "d_thread".into();
    event.thread = Some(ThreadRef {
        id: "d_thread".into(),
        parent: "d1".into(),
    });
    let post = &plan_created(&g, &event).posts[0];
    assert_eq!(
        (post.channel.as_str(), post.parent.as_deref()),
        ("c1", Some("root"))
    );
}

#[test]
fn attachments_are_copied_up_to_chats_limit_and_linked_beyond() {
    let mut event = created("dm", "look");
    event.attachments = vec![
        AttachmentInfo {
            name: "cat.png".into(),
            size: 10,
            content_type: "image/png".into(),
            url: "https://cdn.discordapp.com/a/cat.png".into(),
            width: 4,
            height: 3,
        },
        AttachmentInfo {
            name: "huge.zip".into(),
            size: MAX_FETCH_BYTES + 1,
            content_type: "application/zip".into(),
            url: "https://cdn.discordapp.com/a/huge.zip".into(),
            ..AttachmentInfo::default()
        },
    ];
    let plan = plan_created(&guild(), &event);
    assert_eq!(plan.files.len(), 1);
    assert_eq!(
        plan.files[0].fetch.attachment.as_ref().unwrap().kind,
        "image"
    );
    assert!(plan.posts.is_empty(), "the text goes with the file");
    assert_eq!(
        plan.files[0].post.text,
        "look\n📎 huge.zip https://cdn.discordapp.com/a/huge.zip"
    );
}

/// What the program hands Chat's `post` is the fetch's attachment with the
/// worker's blob: it must be an attachment Chat accepts, of the right kind.
#[test]
fn a_fetched_attachment_is_one_chat_posts() {
    let file = |name: &str, content_type: &str, width: u32| AttachmentInfo {
        name: name.into(),
        size: 10,
        content_type: content_type.into(),
        url: format!("https://cdn.discordapp.com/a/{name}"),
        width,
        height: width,
    };
    let mut event = created("dm", "");
    event.attachments = vec![
        file("cat.png", "image/png", 4),
        file("odd.png", "image/png", 0),
        file("clip.mp4", "video/mp4", 0),
        file("notes.txt", "text/plain", 0),
    ];
    let posted: Vec<chat::model::Attachment> = plan_created(&guild(), &event)
        .files
        .into_iter()
        .map(|post| {
            let attachment = InAttachment {
                blob: "ab".repeat(32),
                ..post.fetch.attachment.unwrap()
            };
            let update: chat::record::AttachmentUpdate =
                serde_json::from_value(serde_json::to_value(attachment).unwrap()).unwrap();
            update.attachment().unwrap()
        })
        .collect();
    let blob = "ab".repeat(32);
    assert_eq!(
        posted,
        [
            chat::model::Attachment::Image {
                name: "cat.png".into(),
                blob: blob.clone(),
                w: 4,
                h: 4,
            },
            chat::model::Attachment::File {
                name: "odd.png".into(),
                blob: blob.clone(),
                size: 10,
            },
            chat::model::Attachment::Video {
                name: "clip.mp4".into(),
                blob: blob.clone(),
                w: 0,
                h: 0,
                size: 10,
            },
            chat::model::Attachment::File {
                name: "notes.txt".into(),
                blob,
                size: 10,
            },
        ]
    );
}

#[test]
fn edits_and_deletes_of_discord_messages_follow_the_map() {
    let mut g = guild();
    g.map.push(discord_origin("m1", "dm1", None));
    let update = MessageUpdated {
        message_id: "dm1".into(),
        author: author("u7"),
        content: "fixed".into(),
        ..MessageUpdated::default()
    };
    assert_eq!(
        plan_updated(&g, &update).edits,
        [EditItem {
            message: "m1".into(),
            text: "fixed".into()
        }]
    );
    let delete = MessageDeleted {
        message_id: "dm1".into(),
        ..MessageDeleted::default()
    };
    assert_eq!(plan_deleted(&g, &delete, &json!({})).deletes, ["m1"]);
}

#[test]
fn deleting_a_discord_post_removes_every_attachment_copy() {
    let mut g = guild();
    let mut event = created("dm1", "three files");
    event.attachments = ["one.txt", "two.txt", "three.txt"]
        .into_iter()
        .map(|name| AttachmentInfo {
            name: name.into(),
            size: 10,
            url: format!("https://cdn.discordapp.com/a/{name}"),
            ..AttachmentInfo::default()
        })
        .collect();
    let posted = plan_created(&g, &event);
    assert_eq!(posted.files.len(), 3);
    for (index, file) in posted.files.iter().enumerate() {
        g.map.push(discord_origin(
            &format!("m{index}"),
            &file.post.discord,
            None,
        ));
    }
    g.map.push(discord_origin("unrelated", "dm2", None));
    let deleted = plan_deleted(
        &g,
        &MessageDeleted {
            message_id: event.message_id,
            ..MessageDeleted::default()
        },
        &json!({}),
    );
    assert_eq!(deleted.deletes, ["m0", "m1", "m2"]);
    assert!(deleted.removes.is_empty());
    assert!(deleted.resends.is_empty());
}

#[test]
fn a_tilo_message_deleted_in_discord_is_resent_unless_a_linked_moderator_did_it() {
    let mut g = guild();
    g.map.push(mapping(MappedKind::Message, "m1", "dm1"));
    g.links.push(Link {
        discord_user: "d_mod".into(),
        person: "u_mod".into(),
        linked_at: 0,
    });
    g.links.push(Link {
        discord_user: "d_ed".into(),
        person: "u_ed".into(),
        linked_at: 0,
    });
    let room = json!({"members": [{"person": "u_mod", "role": "moderator"}, {"person": "u_ed", "role": "editor"}]});
    let by = |executor: Option<&str>| MessageDeleted {
        message_id: "dm1".into(),
        executor: executor.map(Into::into),
        ..MessageDeleted::default()
    };
    assert_eq!(plan_deleted(&g, &by(Some("d_mod")), &room).removes.len(), 1);
    assert_eq!(plan_deleted(&g, &by(Some("d_ed")), &room).resends, ["m1"]);
    assert_eq!(plan_deleted(&g, &by(None), &room).resends, ["m1"]);
    assert_eq!(
        plan_deleted(&g, &by(Some("b0")), &room),
        InboundPlan::default(),
        "the bridge's own delete"
    );
}

#[test]
fn reactions_carry_the_persona() {
    let mut g = guild();
    g.map.push(mapping(MappedKind::Message, "m1", "dm1"));
    let event = ReactionChanged {
        message_id: "dm1".into(),
        user_id: "u8".into(),
        user_name: "bo".into(),
        emoji: ":tilo_hello:".into(),
        ..ReactionChanged::default()
    };
    let plan = plan_reaction(&g, &event, false);
    assert_eq!(plan.unreacts[0].emoji, ":tilo_hello:");
    assert_eq!(plan.unreacts[0].appearance.persona.account, "u8");
}

#[test]
fn a_ban_blocks_and_hand_made_role_changes_are_reverted() {
    let mut g = guild();
    g.map.push(discord_origin("m1", "dm1", None));
    let ban = MemberChanged {
        user_id: "u7".into(),
        banned: true,
        ..MemberChanged::default()
    };
    let banned = plan_member(&g, &ban, &json!({}));
    assert_eq!(banned.blocks, ["u7"]);
    assert_eq!(banned.deletes, ["m1"], "what was mirrored of them goes");
    g.map_roles = true;
    g.map.push(mapping(MappedKind::Role, "editor", "r_ed"));
    g.map.push(mapping(MappedKind::Role, "owner", "r_own"));
    g.links.push(Link {
        discord_user: "u7".into(),
        person: "u_ada".into(),
        linked_at: 0,
    });
    let room = json!({"members": [{"person": "u_ada", "role": "editor"}]});
    let changed = MemberChanged {
        user_id: "u7".into(),
        roles: vec!["r_own".into()],
        ..MemberChanged::default()
    };
    let roles = &plan_member(&g, &changed, &room).member_roles[0];
    assert_eq!(
        (roles.add.as_slice(), roles.remove.as_slice()),
        (&["r_ed".to_owned()][..], &["r_own".to_owned()][..])
    );
}

#[test]
fn link_commands_find_the_pending_code_or_count_a_failure() {
    let mut g = guild();
    g.pending_links.push(PendingLink {
        person: "u_ada".into(),
        code_sha256: link::hash("AAAABBBBCCCC"),
        expires_ms: i64::MAX,
    });
    let cmd = |code: &str| Command {
        user_id: "u7".into(),
        name: "link".into(),
        code_sha256: Some(link::hash(code)),
        ..Command::default()
    };
    assert_eq!(
        plan_command(&g, &cmd("aaaa bbbb cccc")).complete_links[0].person,
        "u_ada"
    );
    assert_eq!(
        plan_command(&g, &cmd("WRONG")).complete_links[0].person,
        "",
        "complete-link records the failure"
    );
    let unlink = Command {
        user_id: "u7".into(),
        name: "unlink".into(),
        ..Command::default()
    };
    assert!(plan_command(&g, &unlink).unlinks.is_empty(), "not linked");
}
