//! An agent working beside the person takes a short story across apps and the shell by published
//! actions alone, as its catalogue lists them: it opens a notebook in a window, puts the person's
//! banners on hold, warms the screen and shares the notebook with a person. Nothing is pressed and
//! no pixel is read; every step is one action of the resource a person's own control invokes,
//! attributed to the agent.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

mod common;

use serde_json::json;
use tilo_session::Session;

/// The agent taking the story.
const AGENT: &str = "agent:helper";

/// The resource of kind `kind`, found as an agent finds it: by surveying the catalogue.
fn found(session: &Session, kind: &str) -> String {
    session
        .resources()
        .into_iter()
        .find(|resource| resource.kind == kind)
        .map_or_else(
            || panic!("the catalogue lists a {kind}"),
            |resource| resource.id.as_str().to_owned(),
        )
}

/// Run `action` of `resource` as the agent.
fn act(session: &mut Session, resource: &str, action: &str, arguments: &serde_json::Value) {
    session
        .invoke_with_actor(AGENT, resource, action, arguments)
        .unwrap_or_else(|error| panic!("{resource}#{action}: {error:?}"));
}

#[test]
fn an_agent_takes_a_story_across_apps_and_the_shell_by_actions_alone() {
    let root = common::private_home();
    let mut session = common::seeded_session(root.path());

    // Open the notebook in a window, as the arrangement's published `open` does for a person.
    let notebook = found(&session, "tl.so.tilo.lang.notes.Notebook");
    act(
        &mut session,
        "shell:arrangement",
        "open",
        &json!({ "resource": notebook }),
    );
    common::until(&mut session, "the notebook's window", |session| {
        common::places(session)
            .iter()
            .any(|place| place.record == notebook || place.shows == notebook)
    });

    // Hold the person's banners back, as the Quick Settings switch does.
    let before = session.read("session:notifications");
    act(
        &mut session,
        "session:notifications",
        "set-quiet",
        &json!({ "on": true }),
    );
    let quiet = common::wait_for(&mut session, "the centre to say it is quiet", |session| {
        session.settle_services();
        session
            .read("session:notifications")
            .filter(|now| Some(now) != before.as_ref())
    });
    assert!(
        quiet["quiet"].is_string(),
        "the centre says why banners are held back: {quiet}"
    );

    // Warm the screen, as the night light switch does.
    act(
        &mut session,
        "session:night-light",
        "toggle",
        &json!({ "on": true }),
    );

    // Share the notebook with a person, as a person's send-to box does: the one send action, the
    // notebook by the id the catalogue lists, the person by name — no conversation named.
    act(
        &mut session,
        "session:send-to",
        "send",
        &json!({
            "item": notebook,
            "to": [{ "kind": "person", "name": "Ada" }],
            "text": "the plan for Friday",
        }),
    );
    let sent = common::wait_for(&mut session, "the send to be carried out", |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|sent| sent.as_array()?.last().cloned())
    });
    assert_eq!(sent["text"], "the plan for Friday", "{sent}");
    assert_eq!(sent["to"][0]["kind"], "person", "{sent}");
    let carried = &sent["attached"][0];
    assert_eq!(
        carried["kind"], "resource",
        "the notebook goes as the record it is, never a picture of it: {sent}"
    );
    assert_eq!(carried["at"]["id"], notebook.as_str(), "{sent}");
    assert!(
        carried["at"]["app"]
            .as_str()
            .is_some_and(|app| !app.is_empty()),
        "and names the app that keeps it: {sent}"
    );
}
