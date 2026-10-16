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
        workspace: "w1".into(),
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
    assert_eq!(
        route,
        json!({"message": "m1", "channel": "", "workspace": "w1"})
    );
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
