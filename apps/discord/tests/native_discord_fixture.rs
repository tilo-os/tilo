//! The bridged server this app writes, saved for the tilolang Discord to carry over.

use std::fmt::Write as _;

use discord::guild::guild;
use discord::guild::Guild;
use discord::{MappedKind, Origin};
use discord_wire::event::{link_check, link_proof, ChannelInfo};
use discord_wire::op::ChannelKind;
use ui::tilo_resource::Replica;

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[test]
#[ignore = "writes the fixture the tilolang Discord carries; run it by name to write it again"]
fn native_discord_fixture() {
    // Ada connects the Lounge, confined to one channel, with channels and roles mirrored.
    let mut ada = Replica::<Guild>::new().as_author("ada");
    ada.apply(Guild::CONNECT.with(guild::Connect {
        guild_id: "123".into(),
        name: "The Lounge".into(),
        only_channel: Some("456".into()),
        mirror_channels: Some(true),
        map_roles: Some(true),
    }))
    .unwrap();

    // The bridge bot observes the server's layout and joins both sides of it.
    let mut bot = ada.fork().as_bot_for("inst1", "ada");
    bot.apply(Guild::OBSERVE_LAYOUT.with(guild::ObserveLayout {
        bot_user: "900".into(),
        channels: vec![
            ChannelInfo {
                id: "456".into(),
                kind: ChannelKind::Text,
                name: "general".into(),
                position: 0,
                ..ChannelInfo::default()
            },
            ChannelInfo {
                id: "789".into(),
                kind: ChannelKind::Voice,
                name: "Lounge".into(),
                position: 1,
                ..ChannelInfo::default()
            },
        ],
        roles: Vec::new(),
        emojis: Vec::new(),
    }))
    .unwrap();
    for (kind, tilo, discord, origin, persona) in [
        (MappedKind::Channel, "tl:so.tilo.chat:c1", "456", Origin::Tilo, None),
        (MappedKind::Channel, "tl:so.tilo.chat:c2", "789", Origin::Tilo, None),
        (MappedKind::Webhook, "tl:so.tilo.chat:c1", "555", Origin::Tilo, None),
        (
            MappedKind::Message,
            "tl:so.tilo.chat:m1",
            "777",
            Origin::Discord,
            Some("3001"),
        ),
    ] {
        bot.apply(Guild::MAP.with(guild::Map {
            kind,
            tilo: tilo.into(),
            discord: discord.into(),
            origin,
            persona: persona.map(str::to_owned),
            root: None,
        }))
        .unwrap();
    }
    bot.apply(Guild::FINISH_IMPORT.with(guild::FinishImport {
        tilo_only: Vec::new(),
    }))
    .unwrap();
    ada.sync_with(&mut bot).unwrap();

    // The bot offers Ada's Discord account, and Ada takes it.
    let mut bot = ada.fork().as_bot_for("inst1", "ada");
    bot.apply(Guild::OFFER_LINK.with(guild::OfferLink {
        check: link_check(&link_proof("T0KEN")),
        discord_user: "3001".into(),
        user_name: "ada".into(),
        display_name: Some("Ada".into()),
        avatar: Some("abc".into()),
        interaction: Some("i_1".into()),
    }))
    .unwrap();
    ada.sync_with(&mut bot).unwrap();
    ada.apply(Guild::COMPLETE_LINK.with(guild::CompleteLink {
        proof: link_proof("T0KEN"),
    }))
    .unwrap();

    // The Lounge blocks one Discord account.
    ada.apply(Guild::BLOCK_PERSONA.with(guild::BlockPersona {
        discord_user: "3002".into(),
    }))
    .unwrap();

    let out = format!("@tilo:package-primary\t{}\n", hex(&ada.save()));
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/lang/tilolang-tilo/tests/legacy/rust_discord.hex");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, out).unwrap();
}
