//! A notification the person hands to their agent.
//!
//! The centre lists what the person was told, each row with the way to hand it to the agent they
//! keep in the workspace in front: the notification's title is what the agent is asked, and the
//! whole of it is the context it reads. The hand is the session's one send
//! (`session:send-to#send`), the same send a window's box and the canvas make, so it lands in
//! their conversation with the agent exactly as a message they typed does; a workspace that
//! keeps no agent gets a new one, as a send makes one.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use serde_json::{Map, Value, json};

/// The centre as `session:notifications` reads it now, once it lists a notice.
fn centre_notice(session: &mut tilo_session::Session) -> Value {
    crate::common::wait_for(session, "the notice in the centre", |session| {
        session.settle_services();
        session
            .read("session:notifications")
            .and_then(|centre| centre["notices"].as_array()?.first().cloned())
    })
}

#[test]
fn a_notification_handed_to_a_live_agent_becomes_its_task() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let mut session = crate::common::seeded_session(root.path());

    // The person is told something, as an app's notification tells them.
    session
        .invoke(
            "session:notifications",
            "notify",
            &Map::from_iter([
                ("title".to_owned(), Value::String("Tea is ready".to_owned())),
                (
                    "body".to_owned(),
                    Value::String("Steeped four minutes".to_owned()),
                ),
                ("from".to_owned(), Value::String("Clocks".to_owned())),
            ]),
        )
        .unwrap_or_else(|error| panic!("notify: {error:?}"));
    let notice = centre_notice(&mut session);
    let number = notice["number"].as_u64().expect("the notice's number");

    // They hand it to their agent from its row.
    session
        .invoke(
            "session:notifications",
            "hand-to-agent",
            &Map::from_iter([("notification".to_owned(), json!(number))]),
        )
        .unwrap_or_else(|error| panic!("hand-to-agent: {error:?}"));

    // The hand is the one send: the notification's title asks, and the whole of it is what the
    // agent reads, sent to the agent the workspace keeps (a new one where it keeps none).
    let sent = crate::common::sent(&mut session, 0);
    assert_eq!(
        sent["text"], "Tea is ready\n\nSteeped four minutes",
        "{sent}"
    );
    let to = sent["to"].as_array().expect("whom it went to");
    assert_eq!(to.len(), 1, "one agent is asked: {sent}");
    assert_eq!(to[0]["kind"], "new-agent", "{sent}");
    assert_eq!(to[0]["name"], "Assistant", "{sent}");
}
