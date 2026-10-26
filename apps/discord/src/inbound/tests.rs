use discord_wire::event::{AttachmentInfo, ThreadRef};
use serde_json::json;

use super::*;
use crate::layout::tests::mapping;
use crate::record::{Link, Mapping, Ownership};
use discord_wire::event::{Authorized, link_check, link_proof};

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
    let plan = plan_created(&guild(), &serde_json::Value::Null, &created("dm1", "hello"));
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
            account: "u7".into(),
            linked: None,
        }
    );
}

#[test]
fn a_linked_account_speaks_as_its_tilo_person() {
    let mut g = guild();
    g.links.push(Link {
        discord_user: "u7".into(),
        person: "u_ada".into(),
        user_name: "alice".into(),
        ..Link::default()
    });
    let chat = json!({"people": [
        {"id": "u_ada", "value": {"id": "u_ada", "name": "Ada Lovelace", "avatar": "blob-ada"}},
    ]});
    let post = &plan_created(&g, &chat, &created("dm1", "hello")).posts[0];
    assert_eq!(
        post.appearance.name, "Ada Lovelace",
        "Chat shows their Tilo name"
    );
    assert_eq!(post.appearance.avatar.as_deref(), Some("blob-ada"));
    assert_eq!(
        post.appearance.persona,
        Persona {
            service: "discord".into(),
            account: "u7".into(),
            linked: Some("u_ada".into()),
        },
        "still marked as written in Discord, by the person it is linked to"
    );

    // Unlinked again, or a person Chat does not know: their Discord name.
    let unknown = json!({"people": []});
    let post = &plan_created(&g, &unknown, &created("dm2", "hi")).posts[0];
    assert_eq!(post.appearance.name, "alice");
    g.links.clear();
    let post = &plan_created(&g, &chat, &created("dm3", "hey")).posts[0];
    assert_eq!(post.appearance.name, "alice");
}

#[test]
fn an_unknown_avatar_is_fetched_once_before_posting() {
    let mut g = guild();
    g.avatars.clear();
    let plan = plan_created(&g, &serde_json::Value::Null, &created("dm1", "hello"));
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
        plan_created(&g, &serde_json::Value::Null, &own),
        InboundPlan::default(),
        "the bridge's own webhook"
    );
    let mut bot = created("dm2", "x");
    bot.author.id = "b0".into();
    assert_eq!(
        plan_created(&g, &serde_json::Value::Null, &bot),
        InboundPlan::default(),
        "the bot user"
    );
    g.map.push(discord_origin("m3", "dm3", None));
    assert_eq!(
        plan_created(&g, &serde_json::Value::Null, &created("dm3", "again")),
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
        plan_created(&g, &serde_json::Value::Null, &created("dm9", "spam")),
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
    assert_eq!(
        plan_reaction(&g, &reaction, &json!({}), true),
        InboundPlan::default()
    );
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
        plan_created(&g, &serde_json::Value::Null, &reply).posts[0]
            .parent
            .as_deref(),
        Some("root")
    );

    let mut deep = created("d_deep", "agreed");
    deep.reply_to = Some("d_r1".into());
    deep.reply_author = Some("bo".into());
    deep.reply_text = Some("first line\nsecond".into());
    let post = &plan_created(&g, &serde_json::Value::Null, &deep).posts[0];
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
    let post = &plan_created(&g, &serde_json::Value::Null, &event).posts[0];
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
    let plan = plan_created(&guild(), &serde_json::Value::Null, &event);
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
    let posted: Vec<chat::model::Attachment> =
        plan_created(&guild(), &serde_json::Value::Null, &event)
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
    assert_eq!(plan_deleted(&g, &delete).deletes, ["m1"]);
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
    let posted = plan_created(&g, &serde_json::Value::Null, &event);
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
    );
    assert_eq!(deleted.deletes, ["m0", "m1", "m2"]);
    assert!(deleted.removes.is_empty());
}

#[test]
fn a_tilo_message_deleted_in_discord_goes_in_tilo_too_whoever_deleted_it() {
    let mut g = guild();
    g.map.push(mapping(MappedKind::Message, "m1", "dm1"));
    g.links.push(Link {
        discord_user: "d_ed".into(),
        person: "u_ed".into(),
        ..Link::default()
    });
    let by = |executor: Option<&str>| MessageDeleted {
        message_id: "dm1".into(),
        executor: executor.map(Into::into),
        ..MessageDeleted::default()
    };
    let removed = |executor| plan_deleted(&g, &by(executor)).removes;
    // Its linked author (or anyone linked), an unlinked server admin, and a
    // delete Discord names nobody for all take it away in Tilo.
    assert_eq!(
        removed(Some("d_ed")),
        [RemoveItem {
            message: "m1".into(),
            reason: "deleted in Discord by u_ed".into()
        }]
    );
    assert_eq!(removed(Some("d_admin")).len(), 1);
    assert_eq!(removed(None).len(), 1);
    assert_eq!(
        plan_deleted(&g, &by(Some("b0"))),
        InboundPlan::default(),
        "the bridge's own delete"
    );
}

#[test]
fn reactions_carry_the_persona() {
    let mut g = guild();
    g.map.push(mapping(MappedKind::Message, "m1", "dm1"));
    // A Discord custom emoji is the workspace emoji of its name in Tilo.
    let event = ReactionChanged {
        message_id: "dm1".into(),
        user_id: "u8".into(),
        user_name: "bo".into(),
        emoji: ":tilo_hello:".into(),
        emoji_id: Some("55".into()),
        animated: true,
        ..ReactionChanged::default()
    };
    let plan = plan_reaction(&g, &event, &json!({}), false);
    assert_eq!(plan.unreacts[0].message, "m1");
    assert_eq!(plan.unreacts[0].emoji, ":tilo_hello:");
    assert_eq!(plan.unreacts[0].appearance.name, "bo");
    assert_eq!(plan.unreacts[0].appearance.persona.service, "discord");
    assert_eq!(plan.unreacts[0].appearance.persona.account, "u8");
    assert!(plan.reacts.is_empty());
}

#[test]
fn a_unicode_reaction_takes_the_spelling_the_message_already_has() {
    let mut g = guild();
    g.map.push(mapping(MappedKind::Message, "m1", "dm1"));
    let chat = json!({"messages": [{"id": "m1", "value": {"channel": "c1", "author": "u_ada", "text": "x",
        "reactions": [{"emoji": "❤", "by": "u_ada", "at": 1}]}}]});
    let event = ReactionChanged {
        message_id: "dm1".into(),
        user_id: "u8".into(),
        emoji: "❤️".into(),
        ..ReactionChanged::default()
    };
    assert_eq!(plan_reaction(&g, &event, &chat, true).reacts[0].emoji, "❤");
    let toned = ReactionChanged {
        emoji: "👍🏽".into(),
        ..event
    };
    assert_eq!(
        plan_reaction(&g, &toned, &chat, true).reacts[0].emoji,
        "👍🏽",
        "a skin tone crosses as it is"
    );
}

#[test]
fn a_reaction_taken_off_by_id_alone_keeps_the_name_it_was_given() {
    let mut g = guild();
    g.map.push(mapping(MappedKind::Message, "m1", "dm1"));
    let chat = json!({"messages": [{"id": "m1", "value": {"channel": "c1", "author": "u_ada", "text": "x",
        "reactions": [{"emoji": "🎉", "by": "bot", "persona": "discord/u8", "name": "Bo", "at": 1}]}}]});
    // Discord's removal names the member by id, without their name.
    let removed = ReactionChanged {
        message_id: "dm1".into(),
        user_id: "u8".into(),
        emoji: "🎉".into(),
        ..ReactionChanged::default()
    };
    let plan = plan_reaction(&g, &removed, &chat, false);
    assert_eq!(plan.unreacts[0].appearance.name, "Bo");
    let unknown = ReactionChanged {
        user_id: "u9".into(),
        ..removed
    };
    assert_eq!(
        plan_reaction(&g, &unknown, &chat, false).unreacts[0]
            .appearance
            .name,
        "u9"
    );
}

#[test]
fn the_bots_own_reactions_are_echoes() {
    let mut g = guild();
    g.bot_user = "b0".into();
    g.map.push(mapping(MappedKind::Message, "m1", "dm1"));
    let echo = ReactionChanged {
        message_id: "dm1".into(),
        user_id: "b0".into(),
        emoji: "👍".into(),
        ..ReactionChanged::default()
    };
    assert_eq!(
        plan_reaction(&g, &echo, &json!({}), true),
        InboundPlan::default()
    );
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
        ..Link::default()
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

fn named(user_name: &str, display_name: Option<&str>) -> UserName {
    UserName {
        user_name: user_name.into(),
        display_name: display_name.map(str::to_owned),
    }
}

fn chat_with_ada() -> serde_json::Value {
    json!({"people": [{"id": "u_ada", "name": "Ada Lovelace"}]})
}

#[test]
fn link_offers_the_account_under_the_tokens_check_only() {
    let g = guild();
    let linking = Command {
        user_id: "u7".into(),
        name: "link".into(),
        interaction: "i_1".into(),
        link_check: Some(link_check(&link_proof("T0KEN"))),
        user: Some(named("ada.l", Some("Ada L"))),
        avatar: Some("a1b2".into()),
        ..Command::default()
    };
    let plan = plan_command(&g, &json!({}), &linking);
    assert_eq!(
        plan.offers,
        [OfferItem {
            check: link_check(&link_proof("T0KEN")),
            discord_user: "u7".into(),
            user_name: "ada.l".into(),
            display_name: "Ada L".into(),
            avatar: "a1b2".into(),
            interaction: "i_1".into(),
            state_sha256: String::new(),
        }]
    );
    assert!(
        plan.answers.is_empty(),
        "the worker answered with the button"
    );
    let without = Command {
        link_check: None,
        ..linking
    };
    assert!(
        plan_command(&g, &json!({}), &without).offers.is_empty(),
        "no token, nothing to offer"
    );
}

#[test]
fn unlink_answers_with_what_came_of_it() {
    let mut g = guild();
    let unlink = Command {
        user_id: "u7".into(),
        name: "unlink".into(),
        interaction: "i_2".into(),
        ..Command::default()
    };
    let plan = plan_command(&g, &chat_with_ada(), &unlink);
    assert!(plan.unlinks.is_empty(), "not linked");
    assert!(plan.answers[0].content.contains("nothing to unlink"));

    g.links.push(Link {
        discord_user: "u7".into(),
        person: "u_ada".into(),
        ..Link::default()
    });
    let plan = plan_command(&g, &chat_with_ada(), &unlink);
    assert_eq!(plan.unlinks, ["u7"]);
    assert_eq!(
        plan.answers,
        [AnswerItem {
            interaction: "i_2".into(),
            content: "Unlinked from Ada Lovelace. What you write here shows under your Discord name again."
                .into(),
        }]
    );
}

#[test]
fn a_sign_in_from_tilo_is_offered_with_its_state() {
    let authorized = Authorized {
        user_id: "u7".into(),
        user: Some(named("ada.l", None)),
        avatar: None,
        state_sha256: "5".repeat(64),
        link_check: "c".repeat(64),
    };
    let plan = plan_authorized(&authorized);
    assert!(plan.offers.is_empty(), "not for whoever holds the token");
    let offer = &plan.sign_in_offers[0];
    assert_eq!(offer.state_sha256, "5".repeat(64));
    assert_eq!(offer.check, "c".repeat(64));
    assert_eq!(
        (offer.user_name.as_str(), offer.interaction.as_str()),
        ("ada.l", "")
    );
}

#[test]
fn a_link_made_from_discord_edits_its_reply_to_the_tilo_name() {
    let mut g = guild();
    g.links.push(Link {
        discord_user: "u7".into(),
        person: "u_ada".into(),
        interaction: Some("i_1".into()),
        ..Link::default()
    });
    assert_eq!(
        plan_linked(&g, &chat_with_ada(), "u_ada").answers,
        [AnswerItem {
            interaction: "i_1".into(),
            content: linked_reply("Ada Lovelace"),
        }]
    );
    assert!(linked_reply("Ada Lovelace").starts_with("Linked to Ada Lovelace ✓"));
    g.links[0].interaction = None;
    assert!(
        plan_linked(&g, &chat_with_ada(), "u_ada")
            .answers
            .is_empty(),
        "a sign-in from Tilo has no reply to edit"
    );
}

#[test]
fn a_member_seen_renamed_updates_the_link() {
    let mut g = guild();

    g.links.push(Link {
        discord_user: "u7".into(),
        person: "u_ada".into(),
        user_name: "ada.l".into(),
        display_name: Some("Ada L".into()),
        ..Link::default()
    });
    let seen = |user: UserName| MemberChanged {
        user_id: "u7".into(),
        user: Some(user),
        ..MemberChanged::default()
    };
    assert!(
        plan_member(&g, &seen(named("ada.l", Some("Ada L"))), &json!({}))
            .renames
            .is_empty(),
        "the same names change nothing"
    );
    assert_eq!(
        plan_member(&g, &seen(named("ada.lovelace", None)), &json!({})).renames,
        [RenameItem {
            discord_user: "u7".into(),
            user_name: "ada.lovelace".into(),
            display_name: String::new(),
        }]
    );
}

#[test]
fn names_come_from_the_member_object() {
    let member = json!({"nick": null, "user": {"username": "ada.l", "global_name": "Ada L"}});
    assert_eq!(
        UserName::of_member(&member),
        Some(UserName {
            user_name: "ada.l".into(),
            display_name: Some("Ada L".into()),
        })
    );
    let same = json!({"user": {"username": "ada", "global_name": "ada"}});
    assert_eq!(UserName::of_member(&same).unwrap().display_name, None);
    assert_eq!(UserName::of_member(&json!({})), None);
}
