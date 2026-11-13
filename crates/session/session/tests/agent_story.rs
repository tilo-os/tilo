//! An agent working beside the person takes a short story across apps and the shell by published
//! actions alone, as its catalogue lists them: it opens a notebook in a window, puts the person's
//! banners on hold, warms the screen, shares the notebook with a person, copies a line for them to
//! paste, puts a control away on their Quick Settings and shows About. Nothing is pressed and no
//! pixel is read; every step is one action of the resource a person's own control invokes,
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
#[allow(
    clippy::too_many_lines,
    reason = "one story told end to end: each step shapes the one after it, and a story split in \
              two would start a session of its own for its second half"
)]
fn an_agent_takes_a_story_across_apps_and_the_shell_by_actions_alone() {
    let root = crate::common::private_home();
    let mut session = crate::common::seeded_session(root.path());

    // Open the notebook in a window, as the arrangement's published `open` does for a person.
    let notebook = found(&session, "tl.so.tilo.lang.notes.Notebook");
    act(
        &mut session,
        "shell:arrangement",
        "open",
        &json!({ "resource": notebook }),
    );
    crate::common::until(&mut session, "the notebook's window", |session| {
        crate::common::places(session)
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
    let quiet = crate::common::wait_for(&mut session, "the centre to say it is quiet", |session| {
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
    let sent = crate::common::wait_for(&mut session, "the send to be carried out", |session| {
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

    // Copy a line the person is to paste somewhere else: it is on the clipboard, and the history
    // keeps it as it keeps what they copied themselves.
    act(
        &mut session,
        "session:clipboard",
        "copy",
        &json!({ "text": "the autumn release" }),
    );
    let history = crate::common::wait_for(
        &mut session,
        "the line in the clipboard's history",
        |session| {
            session
                .read("session:clipboard")
                .filter(|history| history.to_string().contains("the autumn release"))
        },
    );
    assert!(
        history.is_array(),
        "the history lists what was copied: {history}"
    );

    // Take a control off Quick Settings, as the panel's own arranging does: the arrangement is this
    // device's setting, so it reads back whether the panel is drawn or not.
    act(
        &mut session,
        "session:quick-settings",
        "hide-control",
        &json!({ "control": "bluetooth" }),
    );
    let arranged = session
        .read("session:quick-settings")
        .expect("the session publishes the panel's arrangement");
    assert!(
        !arranged["controls"].to_string().contains("bluetooth"),
        "the control is off the panel: {arranged}"
    );
    assert!(
        arranged["controls"].to_string().contains("wifi"),
        "Wi-Fi stays, as the way a desktop without a network gets online: {arranged}"
    );

    // Show the About sheet, as the Tilo menu's own About does.
    act(
        &mut session,
        "desktop:chrome",
        "about",
        &json!({ "open": true }),
    );
}
