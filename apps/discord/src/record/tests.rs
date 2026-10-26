use discord_wire::event::{ChannelInfo, RoleInfo};
use discord_wire::op::ChannelKind;
use ui::tilo_resource::Replica;

use super::*;

pub(crate) const ADA: &str = "u_ada";
pub(crate) const INSTALL: &str = "inst1";

/// A guild Ada connected, observed once by the bridge bot.
pub(crate) fn connected() -> Replica<Guild> {
    connected_with(None, None)
}

/// [`connected`], opted into mirroring channels and mapping roles or not.
fn connected_with(mirror_channels: Option<bool>, map_roles: Option<bool>) -> Replica<Guild> {
    let mut ada = Replica::<Guild>::new().as_author(ADA);
    ada.apply(Guild::CONNECT.with(guild::Connect {
        guild_id: "1".into(),
        name: "Tilo".into(),
        only_channel: None,
        mirror_channels,
        map_roles,
    }))
    .unwrap();
    let mut bot = ada.fork().as_bot_for(INSTALL, ADA);
    bot.apply(Guild::OBSERVE_LAYOUT.with(guild::ObserveLayout {
        bot_user: "b0".into(),
        channels: vec![
            ChannelInfo {
                id: "d_chat".into(),
                kind: ChannelKind::Category,
                name: "chat".into(),
                position: 0,
                ..ChannelInfo::default()
            },
            ChannelInfo {
                id: "d_general".into(),
                kind: ChannelKind::Text,
                name: "general".into(),
                parent: Some("d_chat".into()),
                position: 1,
                ..ChannelInfo::default()
            },
        ],
        roles: vec![
            RoleInfo {
                id: "1".into(),
                name: "@everyone".into(),
                permissions: "0".into(),
                ..RoleInfo::default()
            },
            RoleInfo {
                id: "r_mods".into(),
                name: "Mods".into(),
                permissions: "8".into(),
                ..RoleInfo::default()
            },
        ],
        emojis: vec![discord_wire::event::EmojiInfo {
            id: "e55".into(),
            name: "tilo_hello".into(),
            animated: false,
        }],
    }))
    .unwrap();
    bot
}

pub(crate) fn map(kind: MappedKind, tilo: &str, discord: &str) -> ui::tilo_resource::Invoke<Guild> {
    Guild::MAP.with(guild::Map {
        kind,
        tilo: tilo.into(),
        discord: discord.into(),
        origin: Origin::Tilo,
        persona: None,
        root: None,
    })
}

#[test]
fn connect_starts_importing_and_never_rebinds() {
    let mut bot = connected();
    let state = bot.state().unwrap();
    assert_eq!(state.guild_id, "1");
    assert_eq!(state.ownership, Ownership::Importing);
    let mut ada = bot.fork().as_author(ADA);
    let again = ada.apply(Guild::CONNECT.with(guild::Connect {
        guild_id: "2".into(),
        name: "x".into(),
        only_channel: None,
        mirror_channels: None,
        map_roles: None,
    }));
    assert!(
        again.is_err(),
        "a connected record is never pointed at another server"
    );
}

#[test]
fn observe_layout_replaces_the_snapshot() {
    let bot = connected();
    let state = bot.state().unwrap();
    assert_eq!(state.bot_user, "b0");
    assert_eq!(state.channels.len(), 2);
    assert_eq!(
        state
            .channels
            .find(&Id::from_raw("d_general"))
            .unwrap()
            .parent
            .as_deref(),
        Some("d_chat")
    );
    assert_eq!(state.roles.len(), 2);
    assert_eq!(state.emoji("tilo_hello"), Some("e55"));
    assert_eq!(state.emoji("wave"), None);
}

#[test]
fn a_person_cannot_observe_the_layout_or_write_the_map() {
    let mut bot = connected();
    let mut ada = bot.fork().as_author(ADA);
    assert!(
        ada.apply(Guild::OBSERVE_LAYOUT.with(guild::ObserveLayout {
            bot_user: "evil".into(),
            channels: vec![],
            roles: vec![],
            emojis: vec![]
        }))
        .is_err()
    );
    assert!(
        ada.apply(map(MappedKind::Channel, "c1", "d_general"))
            .is_err()
    );
    let mut agent = bot.fork().as_agent_for("agent:x", ADA);
    assert!(
        agent
            .apply(map(MappedKind::Channel, "c1", "d_general"))
            .is_err(),
        "agents are not bots"
    );
}

#[test]
fn map_and_unmap_by_the_bot_with_reverse_lookup() {
    let mut bot = connected();
    bot.apply(map(MappedKind::Channel, "c1", "d_general"))
        .unwrap();
    bot.apply(map(MappedKind::Channel, "c1", "d_general2"))
        .unwrap();
    let state = bot.state().unwrap();
    assert_eq!(state.map.len(), 1, "one entry per kind and Tilo id");
    assert_eq!(
        state.mapping(MappedKind::Channel, "c1").unwrap().discord,
        "d_general2"
    );
    assert_eq!(
        state
            .by_discord(MappedKind::Channel, "d_general2")
            .unwrap()
            .tilo,
        "c1"
    );
    assert!(
        state.knows_channel("d_general2"),
        "a mapped channel is known before the next layout event"
    );
    bot.apply(map(MappedKind::Message, "chat.room/messages/m1", "d_m1"))
        .unwrap();
    assert_eq!(
        bot.state()
            .unwrap()
            .mapping(MappedKind::Message, "m1")
            .unwrap()
            .discord,
        "d_m1",
        "the item resource a Chat verb answered maps by its id"
    );
    bot.apply(Guild::UNMAP.with(guild::Unmap {
        kind: MappedKind::Channel,
        tilo: "c1".into(),
        keep_apart: None,
    }))
    .unwrap();
    assert!(
        bot.state()
            .unwrap()
            .mapping(MappedKind::Channel, "c1")
            .is_none()
    );
}

use discord_wire::op::{
    ChannelSpec, DeleteChannelOp, DiscordOp, FetchAssetOp, MemberOp, OutAttachment, SendOp,
};

fn send(channel: &str, webhook: &str, content: &str) -> ui::tilo_resource::Invoke<Guild> {
    Guild::SEND.with(guild::Send {
        op: SendOp {
            channel: channel.into(),
            webhook: webhook.into(),
            username: "Ada".into(),
            avatar_blob: Some("av".into()),
            content: content.into(),
            attachments: vec![OutAttachment {
                blob: "f1".into(),
                name: "a.png".into(),
                size: 4,
            }],
            tilo: "m1".into(),
            ..SendOp::default()
        },
    })
}

#[test]
fn send_queues_with_host_fields() {
    let mut bot = connected();
    bot.apply(map(MappedKind::Webhook, "c1", "wh1")).unwrap();
    bot.apply(send("d_general", "wh1", "hi")).unwrap();
    let state = bot.state().unwrap();
    let (_, queued) = state.outbox.pending().next().unwrap();
    assert!(matches!(&queued.op, DiscordOp::Send(op) if op.content == "hi"));
    assert_eq!(queued.publish, ["av"]);
    assert_eq!(queued.attach, ["f1"]);
}

#[test]
fn outside_verbs_refuse_unknown_ids_and_non_bots() {
    let mut bot = connected();
    bot.apply(map(MappedKind::Webhook, "c1", "wh1")).unwrap();
    assert!(
        bot.apply(send("d_elsewhere", "wh1", "hi")).is_err(),
        "unknown channel"
    );
    assert!(
        bot.apply(send("d_general", "wh_stolen", "hi")).is_err(),
        "not a bridge webhook"
    );
    assert!(
        bot.apply(send("d_general", "wh1", &"x".repeat(2001)))
            .is_err(),
        "over Discord's limit"
    );
    let mut agent = bot.fork().as_agent_for("agent:x", ADA);
    assert!(
        agent.apply(send("d_general", "wh1", "hi")).is_err(),
        "an agent queues nothing"
    );
    let mut ada = bot.fork().as_author(ADA);
    assert!(
        ada.apply(send("d_general", "wh1", "hi")).is_err(),
        "a person queues nothing"
    );
}

#[test]
fn layout_calls_wait_for_the_import() {
    let mut bot = connected();
    let delete = Guild::DELETE_CHANNEL.with(guild::DeleteChannel {
        op: DeleteChannelOp {
            channel: "d_general".into(),
        },
    });
    assert!(
        bot.apply(delete).is_err(),
        "nothing is deleted before the import ran"
    );
    let create = Guild::APPLY_CHANNEL.with(guild::ApplyChannel {
        op: ChannelSpec {
            tilo: "c9".into(),
            name: "new".into(),
            parent: Some("d_chat".into()),
            ..ChannelSpec::default()
        },
    });
    assert!(bot.apply(create).is_err());
}

#[test]
fn fetch_asset_reads_at_most_what_chat_holds() {
    let mut bot = connected();
    let fetch = |url: &str, max_bytes: u64| {
        Guild::FETCH_ASSET.with(guild::FetchAsset {
            op: FetchAssetOp {
                url: url.into(),
                max_bytes,
                attachment: None,
                inline: false,
            },
        })
    };
    bot.apply(fetch("https://cdn.discordapp.com/avatars/1/a.png", 1 << 20))
        .unwrap();
    assert!(
        bot.apply(fetch("https://cdn.discordapp.com/a.png", 64 << 20))
            .is_err()
    );
}

#[test]
fn member_calls_need_a_linked_account() {
    let mut bot = connected();
    let kick = Guild::KICK.with(guild::Kick {
        op: MemberOp {
            user: "d_user".into(),
            reason: String::new(),
        },
    });
    assert!(
        bot.apply(kick).is_err(),
        "an unlinked Discord user is the Discord moderators' business"
    );
}

use discord_wire::event::{LINK_TTL_MS, link_check, link_proof, sha256_hex};

/// The bot offers `user` under the token `token`, as `/link` does, or as a
/// sign-in from Tilo whose state is `state`.
fn offer(
    bot: &mut Replica<Guild>,
    user: &str,
    token: &str,
    state: Option<&str>,
) -> Result<(), String> {
    let check = link_check(&link_proof(token));
    let offered = match state {
        Some(state) => bot.apply(Guild::OFFER_SIGN_IN.with(guild::OfferSignIn {
            check,
            discord_user: user.into(),
            user_name: format!("{user}.name"),
            display_name: None,
            avatar: Some("a1b2".into()),
            state_sha256: sha256_hex(state.as_bytes()),
        })),
        None => bot.apply(Guild::OFFER_LINK.with(guild::OfferLink {
            check,
            discord_user: user.into(),
            user_name: format!("{user}.name"),
            display_name: None,
            avatar: Some("a1b2".into()),
            interaction: Some("i_1".to_owned()),
        })),
    };
    offered
        .map(|_| ())
        .map_err(|refusal| format!("{refusal:?}"))
}

/// `person` takes the offer `token` opens.
fn take(replica: &mut Replica<Guild>, token: &str) -> Result<(), String> {
    replica
        .apply(Guild::COMPLETE_LINK.with(guild::CompleteLink {
            proof: link_proof(token),
        }))
        .map(|_| ())
        .map_err(|refusal| format!("{refusal:?}"))
}

fn begin(replica: &mut Replica<Guild>, state: &str) {
    replica
        .apply(Guild::BEGIN_CONNECT.with(guild::BeginConnect {
            state_sha256: sha256_hex(state.as_bytes()),
        }))
        .unwrap();
}

fn linked_to(replica: &Replica<Guild>, user: &str) -> Option<String> {
    replica
        .state()
        .unwrap()
        .links
        .find(&Id::from_raw(user))
        .map(|l| l.person.clone())
}

/// Ada's account offered by the bot through `/link`, then taken by Ada.
fn link_ada(bot: Replica<Guild>) -> Replica<Guild> {
    let mut bot = bot;
    offer(&mut bot, "d_ada", "T0KEN", None).unwrap();
    let mut ada = bot.fork().as_author(ADA);
    take(&mut ada, "T0KEN").unwrap();
    ada.fork().as_bot_for(INSTALL, ADA)
}

#[test]
fn a_link_from_discord_links_whoever_holds_its_token_once() {
    let mut bot = connected();
    offer(&mut bot, "d_ada", "T0KEN", None).unwrap();
    let offered = bot
        .state()
        .unwrap()
        .link_offers
        .iter()
        .next()
        .unwrap()
        .clone();
    assert_eq!(offered.check, link_check(&link_proof("T0KEN")));
    assert_ne!(offered.check, "T0KEN", "the record never holds the token");
    assert_eq!(offered.interaction.as_deref(), Some("i_1"));

    let mut ada = bot.fork().as_author(ADA);
    take(&mut ada, "T0KEN").unwrap();
    assert_eq!(linked_to(&ada, "d_ada").as_deref(), Some(ADA));
    let link = ada.state().unwrap().links.iter().next().unwrap().clone();
    assert_eq!(link.avatar.as_deref(), Some("a1b2"));
    assert_eq!(
        link.interaction.as_deref(),
        Some("i_1"),
        "its reply is edited"
    );
    assert!(ada.state().unwrap().link_offers.is_empty(), "used up");

    let mut bo = ada.fork().as_author("u_bo");
    let reused = take(&mut bo, "T0KEN").unwrap_err();
    assert!(reused.contains("already used"), "{reused}");
    assert_eq!(linked_to(&bo, "d_ada").as_deref(), Some(ADA));
}

#[test]
fn a_forged_or_expired_token_links_nobody() {
    let mut bot = connected();
    bot.set_now_ms(1_000);
    offer(&mut bot, "d_ada", "T0KEN", None).unwrap();
    let mut eve = bot.fork().as_author("u_eve");
    eve.set_now_ms(1_000);
    let forged = take(&mut eve, "GUESSED").unwrap_err();
    assert!(forged.contains("already used or has run out"), "{forged}");
    // The check the record shows opens nothing: only the token's proof does.
    let check = eve
        .state()
        .unwrap()
        .link_offers
        .iter()
        .next()
        .unwrap()
        .check
        .clone();
    assert!(
        eve.apply(Guild::COMPLETE_LINK.with(guild::CompleteLink { proof: check }))
            .is_err()
    );

    let mut ada = eve.fork().as_author(ADA);
    ada.set_now_ms(1_000 + LINK_TTL_MS + 1);
    let late = take(&mut ada, "T0KEN").unwrap_err();
    assert!(late.contains("ran out after ten minutes"), "{late}");
    assert_eq!(linked_to(&ada, "d_ada"), None);
}

#[test]
fn a_sign_in_from_tilo_links_only_the_person_who_started_it() {
    let mut bot = connected();
    let mut ada = bot.fork().as_author(ADA);
    begin(&mut ada, "STATE");
    let mut bot = ada.fork().as_bot_for(INSTALL, ADA);
    let unknown = offer(&mut bot, "d_ada", "T0KEN", Some("FORGED")).unwrap_err();
    assert!(unknown.contains("not started here"), "{unknown}");
    offer(&mut bot, "d_ada", "T0KEN", Some("STATE")).unwrap();
    assert!(
        bot.state().unwrap().connects.is_empty(),
        "the state is used up"
    );
    assert!(
        offer(&mut bot, "d_ada", "T0KEN2", Some("STATE")).is_err(),
        "a state comes back once"
    );

    let mut eve = bot.fork().as_author("u_eve");
    let theirs = take(&mut eve, "T0KEN").unwrap_err();
    assert!(theirs.contains("another Tilo account"), "{theirs}");
    let mut ada = eve.fork().as_author(ADA);
    take(&mut ada, "T0KEN").unwrap();
    assert_eq!(linked_to(&ada, "d_ada").as_deref(), Some(ADA));
}

#[test]
fn a_sign_in_that_took_too_long_is_refused() {
    let mut bot = connected();
    let mut ada = bot.fork().as_author(ADA);
    ada.set_now_ms(1_000);
    begin(&mut ada, "STATE");
    let mut bot = ada.fork().as_bot_for(INSTALL, ADA);
    bot.set_now_ms(1_000 + LINK_TTL_MS + 1);
    assert!(offer(&mut bot, "d_ada", "T0KEN", Some("STATE")).is_err());
}

#[test]
fn linking_again_replaces_the_persons_old_link() {
    let mut bot = link_ada(connected());
    offer(&mut bot, "d_ada2", "T0KEN2", None).unwrap();
    let mut ada = bot.fork().as_author(ADA);
    take(&mut ada, "T0KEN2").unwrap();
    assert_eq!(linked_to(&ada, "d_ada"), None);
    assert_eq!(linked_to(&ada, "d_ada2").as_deref(), Some(ADA));
}

#[test]
fn only_a_person_begins_or_takes_and_only_bots_offer() {
    let mut bot = connected();
    assert!(
        bot.apply(Guild::BEGIN_CONNECT.with(guild::BeginConnect {
            state_sha256: sha256_hex(b"S")
        }))
        .is_err()
    );
    let mut ada = bot.fork().as_author(ADA);
    assert!(
        offer(&mut ada, "d_ada", "T0KEN", None).is_err(),
        "a person cannot vouch for an account"
    );
    assert!(
        ada.apply(Guild::BEGIN_CONNECT.with(guild::BeginConnect {
            state_sha256: "not hex".into()
        }))
        .is_err()
    );
}

#[test]
fn answers_go_out_through_the_outbox() {
    let mut bot = connected();
    bot.apply(Guild::ANSWER_INTERACTION.with(guild::AnswerInteraction {
        op: AnswerInteractionOp {
            interaction: "i_1".into(),
            content: "Linked to Ada.".into(),
        },
    }))
    .unwrap();
    assert!(
        bot.apply(Guild::ANSWER_INTERACTION.with(guild::AnswerInteraction {
            op: AnswerInteractionOp {
                interaction: "i_1".into(),
                content: String::new(),
            },
        }))
        .is_err()
    );
    let mut ada = bot.fork().as_author(ADA);
    assert!(
        ada.apply(Guild::ANSWER_INTERACTION.with(guild::AnswerInteraction {
            op: AnswerInteractionOp {
                interaction: "i_1".into(),
                content: "forged".into(),
            },
        }))
        .is_err()
    );
}

#[test]
fn unlink_by_the_person_or_the_bot_and_kick_needs_the_link() {
    let mut bot = link_ada(connected());
    let kick = Guild::KICK.with(guild::Kick {
        op: discord_wire::op::MemberOp {
            user: "d_ada".into(),
            reason: String::new(),
        },
    });
    bot.apply(kick).unwrap();
    let mut bo = bot.fork().as_author("u_bo");
    assert!(
        bo.apply(Guild::UNLINK.with(guild::Unlink {
            discord_user: "d_ada".into()
        }))
        .is_err(),
        "not Bo's link"
    );
    let mut ada = bot.fork().as_author(ADA);
    ada.apply(Guild::UNLINK.with(guild::Unlink {
        discord_user: "d_ada".into(),
    }))
    .unwrap();
    assert_eq!(linked_to(&ada, "d_ada"), None);
}

#[test]
fn blocking_a_persona_by_a_moderator_or_the_bot() {
    let mut bot = connected();
    bot.apply(Guild::BLOCK_PERSONA.with(guild::BlockPersona {
        discord_user: "d_spam".into(),
    }))
    .unwrap();
    assert!(
        bot.state()
            .unwrap()
            .blocked_personas
            .contains(&"d_spam".to_owned())
    );
    let mut ada = bot.fork().as_author(ADA);
    ada.apply(Guild::UNBLOCK_PERSONA.with(guild::UnblockPersona {
        discord_user: "d_spam".into(),
    }))
    .unwrap();
    assert!(
        !ada.state()
            .unwrap()
            .blocked_personas
            .contains(&"d_spam".to_owned())
    );
}

#[test]
fn avatars_are_remembered_by_hash() {
    let mut bot = connected();
    let blob = "a".repeat(64);
    bot.apply(Guild::REMEMBER_AVATAR.with(guild::RememberAvatar {
        hash: "a_1f2e".into(),
        blob: blob.clone(),
    }))
    .unwrap();
    assert_eq!(bot.state().unwrap().avatars.get("a_1f2e"), Some(&blob));
    assert!(
        bot.apply(Guild::REMEMBER_AVATAR.with(guild::RememberAvatar {
            hash: "../x".into(),
            blob
        }))
        .is_err()
    );
}

fn set_photo(bot: &mut Replica<Guild>, avatar: &str, photo: &str) -> Result<(), String> {
    bot.apply(Guild::SET_LINK_PHOTO.with(guild::SetLinkPhoto {
        discord_user: "d_ada".into(),
        avatar: avatar.into(),
        photo: photo.into(),
    }))
    .map(|_| ())
    .map_err(|refusal| format!("{refusal:?}"))
}

fn photo_of(bot: &Replica<Guild>) -> (Option<String>, Option<String>) {
    let guild = bot.state().unwrap();
    let link = guild.links.find(&Id::from_raw("d_ada")).unwrap();
    (link.photo.clone(), link.photo_avatar.clone())
}

#[test]
fn a_linked_avatar_is_kept_as_a_picture_of_the_hash_it_is_of() {
    let mut bot = link_ada(connected());
    const PNG: &str = "data:image/png;base64,iVBORw0KGgo=";
    assert!(set_photo(&mut bot, "a1b2", "https://cdn.discordapp.com/x.png").is_err());
    assert!(set_photo(&mut bot, "a1b2", "data:image/gif;base64,R0lG").is_err());
    let long = format!("data:image/png;base64,{}", "A".repeat(MAX_PHOTO_CHARS));
    assert!(
        set_photo(&mut bot, "a1b2", &long).is_err(),
        "too long for a profile"
    );
    set_photo(&mut bot, "stale", PNG).unwrap();
    assert_eq!(
        photo_of(&bot),
        (None, None),
        "not the avatar the link holds"
    );
    set_photo(&mut bot, "a1b2", PNG).unwrap();
    assert_eq!(photo_of(&bot), (Some(PNG.into()), Some("a1b2".into())));
    let mut ada = bot.fork().as_author(ADA);
    assert!(set_photo(&mut ada, "a1b2", PNG).is_err(), "only the bridge");

    // A new avatar keeps the old picture until the new one is fetched; no
    // avatar drops it.
    let rename = |avatar: &str| {
        Guild::RENAME_LINK.with(guild::RenameLink {
            discord_user: "d_ada".into(),
            user_name: "d_ada.name".into(),
            display_name: None,
            avatar: Some(avatar.into()),
        })
    };
    bot.apply(rename("c3d4")).unwrap();
    assert_eq!(photo_of(&bot), (Some(PNG.into()), Some("a1b2".into())));
    bot.apply(rename("")).unwrap();
    assert_eq!(photo_of(&bot), (None, None));
}

#[test]
fn finish_import_adopts_the_server_once() {
    let mut bot = connected();
    bot.apply(Guild::FINISH_IMPORT.with(guild::FinishImport {
        tilo_only: Vec::new(),
    }))
    .unwrap();
    assert_eq!(bot.state().unwrap().ownership, Ownership::Adopted);
    assert!(
        bot.apply(Guild::FINISH_IMPORT.with(guild::FinishImport {
            tilo_only: Vec::new()
        }))
        .is_err()
    );
    let mut ada = bot.fork().as_author(ADA);
    assert!(
        ada.apply(Guild::FINISH_IMPORT.with(guild::FinishImport {
            tilo_only: Vec::new()
        }))
        .is_err(),
        "a person cannot adopt it"
    );
}

#[test]
fn an_agent_cannot_block() {
    let mut bot = connected();
    let mut agent = bot.fork().as_agent_for("agent:x", ADA);
    assert!(
        agent
            .apply(Guild::BLOCK_PERSONA.with(guild::BlockPersona {
                discord_user: "d".into()
            }))
            .is_err()
    );
}

#[test]
fn a_bridge_of_one_channel_queues_nothing_elsewhere() {
    let connect = |only: &str| {
        Guild::CONNECT.with(guild::Connect {
            guild_id: "1".into(),
            name: "Tilo".into(),
            only_channel: Some(only.into()),
            mirror_channels: None,
            map_roles: None,
        })
    };
    let mut ada = Replica::<Guild>::new().as_author(ADA);
    assert!(ada.fork().apply(connect("general")).is_err(), "not an id");
    ada.apply(connect("2")).unwrap();
    let mut bot = ada.fork().as_bot_for(INSTALL, ADA);
    let text = |id: &str| ChannelInfo {
        id: id.into(),
        kind: ChannelKind::Text,
        name: id.into(),
        ..ChannelInfo::default()
    };
    bot.apply(Guild::OBSERVE_LAYOUT.with(guild::ObserveLayout {
        bot_user: "b0".into(),
        channels: vec![text("2"), text("3")],
        roles: Vec::new(),
        emojis: Vec::new(),
    }))
    .unwrap();
    bot.apply(map(MappedKind::Webhook, "c1", "wh1")).unwrap();
    bot.apply(send("2", "wh1", "hi")).unwrap();
    assert!(
        bot.apply(send("3", "wh1", "hi")).is_err(),
        "another channel"
    );
    let overwrite = Guild::SET_OVERWRITE.with(guild::SetOverwrite {
        op: OverwriteOp {
            channel: "2".into(),
            target: "1".into(),
            target_kind: OverwriteKind::Role,
            allow: "0".into(),
            deny: "0".into(),
        },
    });
    assert!(bot.apply(overwrite).is_err(), "the server's permissions");
}

fn adopt(bot: &mut Replica<Guild>) {
    bot.apply(Guild::MAP.with(guild::Map {
        kind: MappedKind::Channel,
        tilo: "c_general".into(),
        discord: "d_general".into(),
        origin: Origin::Discord,
        persona: None,
        root: None,
    }))
    .unwrap();
    bot.apply(Guild::FINISH_IMPORT.with(guild::FinishImport {
        tilo_only: vec!["c_help".into()],
    }))
    .unwrap();
}

fn role(id: Option<&str>) -> ui::tilo_resource::Invoke<Guild> {
    Guild::APPLY_ROLE.with(guild::ApplyRole {
        op: RoleSpec {
            id: id.map(Into::into),
            tilo: "viewer".into(),
            name: "Tilo Viewer".into(),
            permissions: "1024".into(),
            color: 0,
        },
    })
}

fn channel(id: Option<&str>) -> ui::tilo_resource::Invoke<Guild> {
    Guild::APPLY_CHANNEL.with(guild::ApplyChannel {
        op: ChannelSpec {
            id: id.map(Into::into),
            tilo: "c_new".into(),
            name: "new".into(),
            parent: Some("d_chat".into()),
            ..ChannelSpec::default()
        },
    })
}

fn overwrite(target: &str) -> ui::tilo_resource::Invoke<Guild> {
    Guild::SET_OVERWRITE.with(guild::SetOverwrite {
        op: OverwriteOp {
            channel: "d_general".into(),
            target: target.into(),
            target_kind: OverwriteKind::Role,
            allow: "0".into(),
            deny: "2048".into(),
        },
    })
}

#[test]
fn an_adopted_server_is_changed_only_where_its_bridge_was_told_to() {
    let mut bot = connected();
    adopt(&mut bot);
    let state = bot.state().unwrap();
    assert!(state.tilo_only.contains(&"c_help".to_owned()));
    // Not opted in: no channel, role or permission is the bridge's to make.
    assert!(bot.apply(channel(None)).is_err(), "no channel made");
    assert!(bot.apply(role(None)).is_err(), "no role made");
    assert!(bot.apply(overwrite("1")).is_err(), "@everyone untouched");
}

#[test]
fn opted_in_the_bridge_still_changes_only_what_it_made() {
    let mut bot = connected_with(Some(true), Some(true));
    assert!(
        bot.apply(channel(None)).is_err(),
        "nothing is made before the server is adopted"
    );
    adopt(&mut bot);
    assert!(
        bot.apply(channel(Some("d_general"))).is_err(),
        "the server's own channel is never changed"
    );
    assert!(
        bot.apply(Guild::DELETE_CHANNEL.with(guild::DeleteChannel {
            op: DeleteChannelOp {
                channel: "d_general".into()
            }
        }))
        .is_err(),
        "nor deleted"
    );
    bot.apply(channel(None)).unwrap();
    bot.apply(map(MappedKind::Channel, "c_new", "d_new"))
        .unwrap();
    bot.apply(channel(Some("d_new"))).unwrap();

    assert!(
        bot.apply(role(Some("1"))).is_err(),
        "@everyone is never changed"
    );
    assert!(
        bot.apply(role(Some("r_mods"))).is_err(),
        "nor the server's own roles"
    );
    assert!(bot.apply(overwrite("1")).is_err());
    assert!(bot.apply(overwrite("r_mods")).is_err());
    bot.apply(role(None)).unwrap();
    bot.apply(map(MappedKind::Role, "viewer", "r_viewer"))
        .unwrap();
    bot.apply(role(Some("r_viewer"))).unwrap();
    bot.apply(overwrite("r_viewer")).unwrap();
}

#[test]
fn a_pair_let_go_stays_apart_until_mapped_again() {
    let mut bot = connected();
    adopt(&mut bot);
    bot.apply(Guild::UNMAP.with(guild::Unmap {
        kind: MappedKind::Channel,
        tilo: "c_general".into(),
        keep_apart: Some(true),
    }))
    .unwrap();
    let state = bot.state().unwrap();
    assert!(state.tilo_only.contains(&"c_general".to_owned()));
    assert!(state.discord_only.contains(&"d_general".to_owned()));
    bot.apply(map(MappedKind::Channel, "c_general", "d_general"))
        .unwrap();
    let state = bot.state().unwrap();
    assert!(!state.tilo_only.contains(&"c_general".to_owned()));
    assert!(!state.discord_only.contains(&"d_general".to_owned()));
}

/// A guild from before a channel's kind and permission overwrites were
/// values: each was one JSON string. Opened now, every channel reads as the
/// bot observed it, whether it was stored at version 1 or already at 2.
#[test]
fn channels_kept_with_json_kinds_migrate_to_values() {
    for version in [1, 2] {
        json_kinds_migrate_from(version);
    }
}

fn json_kinds_migrate_from(version: u32) {
    use ui::tilo_resource::{Slot, evolution, read_key};
    let mut bot = connected();
    let expected = bot.state().unwrap();
    let doc = bot.doc_mut();
    let root = ui::automerge::ROOT;
    for (_, channel) in evolution::map_values(doc, &root, "channels").unwrap() {
        let kind: ChannelKind = read_key(doc, &channel, "kind").unwrap();
        let overwrites: Vec<discord_wire::event::OverwriteInfo> =
            read_key(doc, &channel, "overwrites").unwrap();
        for (key, text) in [
            ("kind", serde_json::to_string(&kind).unwrap()),
            ("overwrites", serde_json::to_string(&overwrites).unwrap()),
        ] {
            let slot = Slot::Key(&channel, key);
            slot.remove(doc).unwrap();
            slot.put_scalar(doc, ui::automerge::ScalarValue::Str(text.into()))
                .unwrap();
        }
    }
    Slot::Key(&root, evolution::VERSION_KEY)
        .remove(doc)
        .unwrap();
    evolution::write_version(doc, version).unwrap();
    doc.commit();
    let mut old = doc.fork();
    assert!(
        evolution::read_checked::<Guild>(&old).is_err(),
        "unmigrated from {version}"
    );
    assert!(evolution::migrate::<Guild>(&mut old).unwrap());
    assert_eq!(
        evolution::read_checked::<Guild>(&old).unwrap(),
        expected,
        "from {version}"
    );
}
