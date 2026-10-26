#![allow(clippy::unwrap_used, reason = "test assertions")]

use serde_json::json;
use ui::framework::testing::TestApp;

use super::*;
use crate::record::guild;
use crate::resource::Discord;

fn connected() -> TestApp<Discord> {
    let mut app = TestApp::<Discord>::new().unwrap();
    app.invoke(Guild::CONNECT.with(guild::Connect {
        guild_id: "1".into(),
        name: "Tilo".into(),
        only_channel: None,
        mirror_channels: None,
        map_roles: None,
    }))
    .unwrap();
    app
}

const PAGE: &str = "https://discord.com/oauth2/authorize?response_type=code&client_id=1234&scope=identify\
                    &redirect_uri=https%3A%2F%2Frooms.example%2Fdiscord%2Fcallback&prompt=none";

#[test]
fn connect_goes_to_the_page_the_host_offers_and_binds_the_state_to_the_person() {
    let mut app = connected();
    let refused = app.effect(&DiscordEffect::ConnectAccount).unwrap_err();
    assert!(format!("{refused:?}").contains("sign"), "{refused:?}");
    let mut unbridged = TestApp::<Discord>::new().unwrap();
    assert!(unbridged.effect(&DiscordEffect::ConnectAccount).is_err());

    app.call_by(bot(), crate::KIND, "set-sign-in", json!({"page": PAGE}))
        .unwrap();
    let start: ui::linked_accounts::LinkStart = serde_json::from_value(
        app.effect(&DiscordEffect::ConnectAccount)
            .unwrap()
            .value
            .unwrap(),
    )
    .unwrap();
    let state = start.url.rsplit_once("&state=").unwrap().1.to_owned();
    assert!(start.url.starts_with(PAGE), "{}", start.url);
    assert_eq!(state.len(), 43);
    let connect = app.state().connects.iter().next().unwrap().clone();
    assert_eq!(connect.state_sha256, sha256_hex(state.as_bytes()));
    assert_ne!(
        connect.state_sha256, state,
        "the record keeps only its hash"
    );

    app.call_by(bot(), crate::KIND, "set-sign-in", json!({"page": ""}))
        .unwrap();
    assert!(
        app.effect(&DiscordEffect::ConnectAccount).is_err(),
        "withdrawn"
    );
    assert!(
        app.call_by(
            bot(),
            crate::KIND,
            "set-sign-in",
            json!({"page": "javascript:x"})
        )
        .is_err()
    );
}

#[test]
fn the_consent_page_asks_for_identify_only_and_comes_back_to_the_host() {
    let page = discord_wire::event::sign_in_page(
        "https://discord.com/oauth2/authorize",
        "1234",
        "https://rooms.example/discord/callback",
    );
    assert_eq!(page, PAGE);
    assert_eq!(consent_url(&page, "st/te"), format!("{PAGE}&state=st%2Fte"));
    let states: std::collections::BTreeSet<String> = (0..8).map(|_| new_state()).collect();
    assert_eq!(states.len(), 8, "each round trip its own state");
    assert!(states.iter().all(|state| state.len() == 43));
}

#[test]
fn plans_answer_what_the_program_loops_over() {
    let mut app = connected();
    let route = app
        .effect(&DiscordEffect::PlanRoute {
            event: json!({"kind": "chat.workspace", "item": {"kind": "messages", "id": "m1"}}),
        })
        .unwrap()
        .value
        .unwrap();
    assert_eq!(route, json!({"message": "m1", "channel": ""}));
    let plan = app
        .effect(&DiscordEffect::PlanEvent {
            event: json!({
                "app_id": "so.tilo.discord",
                "source": "layout-changed",
                "id": "1",
                "at_ms": 0,
                "payload": {"guild_id": "1", "bot_user": "9", "channels": [], "roles": []},
            }),
            chat: json!({}),
            context: None,
            room: None,
            emoji: None,
        })
        .unwrap()
        .value
        .unwrap();
    assert_eq!(plan["observe"][0]["bot_user"], "9", "{plan}");
    assert_eq!(plan["again"], true);
}

fn linked_guild() -> Guild {
    let mut guild = Guild {
        guild_id: "1".into(),
        sign_in_page: Some(PAGE.into()),
        ..Guild::default()
    };
    guild.links.push(crate::Link {
        discord_user: "42".into(),
        person: "ada".into(),
        linked_at: 5,
        user_name: "ada.l".into(),
        display_name: Some("Ada L".into()),
        avatar: None,
        interaction: None,
        photo: Some("data:image/png;base64,AAAA".into()),
        photo_avatar: None,
    });
    guild.link_offers.push(crate::LinkOffer {
        check: link_check(&link_proof("FROM-DISCORD")),
        discord_user: "43".into(),
        user_name: "bo".into(),
        display_name: None,
        avatar: None,
        person: None,
        interaction: Some("i_1".into()),
        expires_ms: 100,
    });
    guild.link_offers.push(crate::LinkOffer {
        check: link_check(&link_proof("FROM-TILO")),
        discord_user: "44".into(),
        user_name: "cy".into(),
        display_name: Some("Cy".into()),
        avatar: None,
        person: Some("cy".into()),
        interaction: None,
        expires_ms: 100,
    });
    guild
}

#[test]
fn my_link_answers_for_its_reader_by_name() {
    let guild = linked_guild();
    let ada = my_link(&guild, Some("ada"));
    assert!(ada.connected);
    assert_eq!(ada.service, "Discord");
    let account = ada.account.unwrap();
    assert_eq!(account.user_name, "ada.l");
    assert_eq!(account.display_name.as_deref(), Some("Ada L"));
    assert_eq!(
        ada.connect.as_deref(),
        Some(DiscordEffect::ConnectAccount.id())
    );
    assert_eq!(ada.unlink.as_deref(), Some(DiscordEffect::UnlinkMine.id()));
    assert!(ada.elsewhere.unwrap().contains("/link"));

    let no_sign_in = Guild {
        sign_in_page: None,
        ..linked_guild()
    };
    let bo = my_link(&no_sign_in, Some("bo"));
    assert_eq!((bo.account, bo.connect, bo.unlink), (None, None, None));
    let one_channel = Guild {
        only_channel: Some("5".into()),
        ..linked_guild()
    };
    assert_eq!(
        my_link(&one_channel, Some("bo")).elsewhere,
        None,
        "/link is registered only for a whole server"
    );
    assert!(!my_link(&Guild::default(), Some("ada")).connected);
}

#[test]
fn an_offer_reads_for_whoever_holds_its_token_or_only_its_starter() {
    let guild = linked_guild();
    let bo = read_offer(&guild, "FROM-DISCORD", Some("anyone"), 50).unwrap();
    assert_eq!(bo.account.user_name, "bo");
    assert!(!bo.started_here, "a link from Discord asks first");
    let cy = read_offer(&guild, "FROM-TILO", Some("cy"), 50).unwrap();
    assert!(cy.started_here);
    let theirs = read_offer(&guild, "FROM-TILO", Some("eve"), 50).unwrap_err();
    assert!(theirs.contains("another Tilo account"), "{theirs}");
    let late = read_offer(&guild, "FROM-DISCORD", Some("bo"), 101).unwrap_err();
    assert!(late.contains("10 minutes"), "{late}");
    let forged = read_offer(&guild, "GUESSED", Some("bo"), 50).unwrap_err();
    assert!(forged.contains("/link"), "says what to do: {forged}");
}

#[test]
fn the_linking_actions_are_offered_to_every_member_through_the_host() {
    assert_eq!(DiscordEffect::MyLink.id(), ui::linked_accounts::READ_ACTION);
    let mut app = connected();
    let mut own: Vec<String> = app
        .actions()
        .unwrap()
        .into_iter()
        .filter(|entry| {
            entry
                .contract
                .tags
                .iter()
                .any(|tag| tag == ui::tilo_actions::MEMBER_OWN_TAG)
        })
        .map(|entry| {
            assert_eq!(
                entry.address.resource,
                crate::KIND,
                "on the record's own resource"
            );
            entry.address.action
        })
        .collect();
    own.sort();
    assert_eq!(
        own,
        [
            "accept-link",
            "connect-account",
            "link-offer",
            "my-link",
            "unlink-mine"
        ]
    );
    let reading: ui::linked_accounts::Reading =
        serde_json::from_value(app.effect(&DiscordEffect::MyLink).unwrap().value.unwrap()).unwrap();
    assert!(reading.connected);
    assert_eq!(reading.account, None);
    assert_eq!(reading.connect, None, "the host offers no sign-in yet");
    assert_eq!(reading.offer, "link-offer");
    assert_eq!(reading.accept, "accept-link");
}

/// The bridge bot, which offers accounts Discord vouched for.
fn bot() -> ui::tilo_actions::ActionInvoker {
    ui::tilo_actions::ActionInvoker::actor("bridge").with_kind(ui::tilo_actions::ActorKind::Bot)
}

/// The bot offers Discord user `user`, named `user_name`, under `token`,
/// as `/link` does.
fn offer(app: &mut TestApp<Discord>, token: &str, user: &str, user_name: &str) {
    app.call_by(
        bot(),
        crate::KIND,
        "offer-link",
        json!({
            "check": link_check(&link_proof(token)),
            "discord_user": user,
            "user_name": user_name,
            "display_name": "",
            "avatar": "",
            "interaction": "",
        }),
    )
    .unwrap();
}

#[test]
fn a_link_keeps_the_names_and_unlinking_mine_takes_only_mine() {
    let mut app = connected();
    offer(&mut app, "MINE", "42", "ada.l");
    offer(&mut app, "THEIRS", "43", "bo");
    app.effect(&DiscordEffect::AcceptLink {
        token: "MINE".into(),
    })
    .unwrap();
    let me = app.state().links.iter().next().unwrap().person.clone();
    app.effect_by(
        "someone-else",
        &DiscordEffect::AcceptLink {
            token: "THEIRS".into(),
        },
    )
    .unwrap();
    let names: Vec<(String, String)> = app
        .state()
        .links
        .iter()
        .map(|l| (l.discord_user.clone(), l.user_name.clone()))
        .collect();
    assert_eq!(
        names,
        [("42".into(), "ada.l".into()), ("43".into(), "bo".into())]
    );
    let reused = app
        .effect_by(
            "a-third-person",
            &DiscordEffect::AcceptLink {
                token: "MINE".into(),
            },
        )
        .unwrap_err();
    assert!(format!("{reused:?}").contains("already used"), "{reused:?}");

    app.call_by(
        bot(),
        crate::KIND,
        "rename-link",
        json!({"discord_user": "42", "user_name": "ada.lovelace", "display_name": "Ada"}),
    )
    .unwrap();
    let reading = my_link(&app.state(), Some(&me));
    let account = reading.account.unwrap();
    assert_eq!(account.user_name, "ada.lovelace");
    assert_eq!(account.display_name.as_deref(), Some("Ada"));

    // Nobody reads or unlinks another person's link.
    let theirs: ui::linked_accounts::Reading = serde_json::from_value(
        app.effect_by("a-third-person", &DiscordEffect::MyLink)
            .unwrap()
            .value
            .unwrap(),
    )
    .unwrap();
    assert_eq!((theirs.account, theirs.unlink), (None, None));
    app.effect_by("a-third-person", &DiscordEffect::UnlinkMine)
        .unwrap();
    assert_eq!(app.state().links.len(), 2, "a third person unlinks nothing");

    app.effect(&DiscordEffect::UnlinkMine).unwrap();
    let links: Vec<String> = app
        .state()
        .links
        .iter()
        .map(|l| l.discord_user.clone())
        .collect();
    assert_eq!(links, ["43"]);
}
