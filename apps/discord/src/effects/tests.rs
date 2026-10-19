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

#[test]
fn linking_shows_the_code_once_and_keeps_only_its_hash() {
    let mut app = connected();
    let output = app.effect(&DiscordEffect::LinkAccount).unwrap();
    let code = output.value.unwrap()["code"].as_str().unwrap().to_owned();
    assert_eq!(code.len(), 12);
    let state = app.state();
    let pending = state.pending_links.iter().next().unwrap();
    assert_eq!(pending.code_sha256, link::hash(&code));
    assert_ne!(pending.code_sha256, code);
}

#[test]
fn a_record_that_is_not_connected_makes_no_code() {
    let mut app = TestApp::<Discord>::new().unwrap();
    assert!(app.effect(&DiscordEffect::LinkAccount).is_err());
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
        ..Guild::default()
    };
    guild.links.push(crate::Link {
        discord_user: "42".into(),
        person: "ada".into(),
        linked_at: 5,
        user_name: "ada.l".into(),
        display_name: Some("Ada L".into()),
    });
    guild.pending_links.push(crate::PendingLink {
        person: "bo".into(),
        code_sha256: link::hash("ABCDEFGHIJKL"),
        expires_ms: 100,
    });
    guild
}

#[test]
fn my_link_answers_for_its_reader_by_name() {
    let guild = linked_guild();
    let ada = my_link(&guild, Some("ada"), 50);
    assert!(ada.connected);
    assert_eq!(ada.service, "Discord");
    let account = ada.account.unwrap();
    assert_eq!(account.user_name, "ada.l");
    assert_eq!(account.display_name.as_deref(), Some("Ada L"));
    assert_eq!(ada.link, DiscordEffect::LinkAccount.id());
    assert_eq!(ada.unlink.as_deref(), Some(DiscordEffect::UnlinkMine.id()));

    let bo = my_link(&guild, Some("bo"), 50);
    assert_eq!((bo.account, bo.pending, bo.unlink), (None, true, None));
    assert!(
        !my_link(&guild, Some("bo"), 101).pending,
        "the code expired"
    );
    assert!(!my_link(&Guild::default(), Some("ada"), 0).connected);
}

#[test]
fn the_linking_actions_are_offered_to_every_member_through_the_host() {
    assert_eq!(DiscordEffect::MyLink.id(), ui::linked_accounts::READ_ACTION);
    let mut app = connected();
    let own: Vec<String> = app
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
            entry.address.action.to_string()
        })
        .collect();
    assert_eq!(own, ["link-account", "my-link", "unlink-mine"]);
    let reading: ui::linked_accounts::Reading =
        serde_json::from_value(app.effect(&DiscordEffect::MyLink).unwrap().value.unwrap()).unwrap();
    assert!(reading.connected);
    assert_eq!(reading.account, None);
    let code: ui::linked_accounts::LinkCode = serde_json::from_value(
        app.effect(&DiscordEffect::LinkAccount)
            .unwrap()
            .value
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        code.instruction,
        "In Discord, type /link with this code. It works for 10 minutes."
    );
}

/// The bridge bot, which completes links from Discord's `/link`.
fn bot() -> ui::tilo_actions::ActionInvoker {
    ui::tilo_actions::ActionInvoker::actor("bridge").with_kind(ui::tilo_actions::ActorKind::Bot)
}

/// `person`'s code completed by the bot as Discord user `user`, named
/// `user_name`.
fn complete(app: &mut TestApp<Discord>, code: &str, person: &str, user: &str, user_name: &str) {
    app.call_by(
        bot(),
        crate::KIND,
        "complete-link",
        json!({
            "discord_user": user,
            "person": person,
            "code_sha256": link::hash(code),
            "user_name": user_name,
            "display_name": "",
        }),
    )
    .unwrap();
}

fn code_of(output: &ui::tilo_actions::ActionOutput) -> String {
    output.value.as_ref().unwrap()["code"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn a_link_keeps_the_names_and_unlinking_mine_takes_only_mine() {
    let mut app = connected();
    let mine = code_of(&app.effect(&DiscordEffect::LinkAccount).unwrap());
    let me = app
        .state()
        .pending_links
        .iter()
        .next()
        .unwrap()
        .person
        .clone();
    let theirs = code_of(
        &app.effect_by("someone-else", &DiscordEffect::LinkAccount)
            .unwrap(),
    );
    complete(&mut app, &mine, &me, "42", "ada.l");
    complete(&mut app, &theirs, "someone-else", "43", "bo");
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

    app.call_by(
        bot(),
        crate::KIND,
        "rename-link",
        json!({"discord_user": "42", "user_name": "ada.lovelace", "display_name": "Ada"}),
    )
    .unwrap();
    let reading = my_link(&app.state(), Some(&me), 0);
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
