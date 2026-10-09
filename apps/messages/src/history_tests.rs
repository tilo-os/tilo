#![allow(
    clippy::expect_used,
    reason = "headless host fixtures must construct before assertions"
)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};

use tilo_contact_directory::{ContactDirectory, routing};
use tilo_messaging::{
    Delivery, Direction, HistoryClient, HistoryFuture, HistoryHost, HistoryPage, Message,
};
use ui::framework::testing::TestApp;
use ui::prelude::Id;

use crate::tests::live_messages_with_history;
use crate::{MessagesEffect, MessagesShell};

const PRIVATE: &str = "fixture private history";

#[derive(Clone)]
struct Host {
    ready: Arc<AtomicBool>,
    wakes: Arc<Mutex<Vec<Waker>>>,
    forged: bool,
    calls: Arc<AtomicUsize>,
}

impl Host {
    fn new(ready: bool, forged: bool) -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(ready)),
            wakes: Arc::default(),
            forged,
            calls: Arc::default(),
        }
    }
    fn release(&self) {
        self.ready.store(true, Ordering::SeqCst);
        for wake in self.wakes.lock().expect("fixture wakers").drain(..) {
            wake.wake();
        }
    }
    fn client(&self) -> HistoryClient {
        HistoryClient::new("11".repeat(32), Arc::new(self.clone()))
    }
}

impl HistoryHost for Host {
    fn read(&self, mut route: routing::Route, _: usize) -> HistoryFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let host = self.clone();
        if host.forged {
            route.scope = "another-provider-namespace".into();
        }
        Box::pin(std::future::poll_fn(move |cx| {
            if !host.ready.load(Ordering::SeqCst) {
                host.wakes
                    .lock()
                    .expect("fixture wakers")
                    .push(cx.waker().clone());
                return Poll::Pending;
            }
            Poll::Ready(Ok(HistoryPage {
                route: route.clone(),
                more: false,
                messages: vec![Message {
                    id: "fixture-message".into(),
                    text: PRIVATE.to_owned().into(),
                    direction: Direction::Outgoing,
                    timestamp_ms: 1,
                    delivery: Some(Delivery::Submitted),
                }],
            }))
        }))
    }
}

fn choice() -> String {
    serde_json::json!({"service":"signal", "account":"fixture-signal", "recipient":"+12025550123",
        "association":"signal", "scope":"fixture-signal-scope", "revision":"fixture-signal-revision"}).to_string()
}

fn selected(host: &Host) -> TestApp<MessagesShell> {
    let mut app = live_messages_with_history(Some(host.client()));
    app.effect(&MessagesEffect::SelectRoute { choice: choice() })
        .expect("select route");
    app
}

fn has_text(app: &mut TestApp<MessagesShell>, text: &str) -> bool {
    app.harness()
        .a11y_nodes()
        .iter()
        .any(|(_, node)| node.label() == Some(text))
}

#[test]
fn host_history_is_transient_and_submission_is_never_delivered() {
    let host = Host::new(true, false);
    let mut app = selected(&host);
    app.effect(&MessagesEffect::ReadHistory { choice: choice() })
        .expect("authorized history job");
    assert!(has_text(&mut app, PRIVATE));
    assert!(has_text(&mut app, "Submitted"));
    assert!(!has_text(&mut app, "Delivered to recipient"));
    assert!(!app.harness().dump_json().contains(PRIVATE));
    assert!(
        app.effect(&MessagesEffect::Send {
            choice: choice(),
            operation: "fixture-op".into(),
            text: "fixture".into()
        })
        .is_err()
    );
    let mut reopened = app
        .reopen()
        .expect("reopen without renewing service authority");
    assert!(
        reopened
            .effect(&MessagesEffect::ReadHistory { choice: choice() })
            .is_err()
    );
    assert!(!has_text(&mut reopened, PRIVATE));
}

#[test]
fn another_namespace_response_never_reaches_the_history_view() {
    let host = Host::new(true, true);
    let mut app = selected(&host);
    assert!(
        app.effect(&MessagesEffect::ReadHistory { choice: choice() })
            .is_err()
    );
    assert!(!has_text(&mut app, PRIVATE));
    assert!(!app.harness().dump_json().contains(PRIVATE));
}

#[test]
fn late_history_cannot_reappear_after_navigation_or_contact_revocation() {
    for revoke in [false, true] {
        let host = Host::new(false, false);
        let mut app = selected(&host);
        let invocation = app
            .start_effect(&MessagesEffect::ReadHistory { choice: choice() })
            .expect("pending history job");
        if revoke {
            app.shared_person::<ContactDirectory>()
                .expect("directory")
                .invoke(&ContactDirectory::FORGET_ENDPOINT.with((
                    Id::from_raw("00000000-0000-4000-8000-000000000001"),
                    Id::from_raw("signal"),
                )))
                .expect("revoke current recipient");
            app.settle();
        } else {
            app.effect(&MessagesEffect::Back)
                .expect("leave the current conversation");
        }
        host.release();
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(5), |harness| {
                    harness
                        .job_updates(invocation)
                        .iter()
                        .any(ui::tilo_actions::InvocationUpdate::is_terminal)
                }),
            "the pending result must actually complete"
        );
        assert!(!has_text(&mut app, PRIVATE));
        assert!(!app.harness().dump_json().contains(PRIVATE));
    }
}

#[test]
fn forged_history_requests_fail_before_reading_the_host() {
    let host = Host::new(true, false);
    let mut app = selected(&host);
    app.effect(&MessagesEffect::ReadHistory { choice: choice() })
        .expect("current history");
    let calls = host.calls.load(Ordering::SeqCst);
    for (field, value) in [
        ("account", "another-account"),
        ("scope", "other-scope"),
        ("recipient", "+12025550999"),
        ("revision", "new-identity"),
        ("association", "other-person"),
    ] {
        let mut forged: serde_json::Value =
            serde_json::from_str(&choice()).expect("fixture choice");
        forged[field] = value.into();
        assert!(
            app.effect(&MessagesEffect::ReadHistory {
                choice: forged.to_string()
            })
            .is_err()
        );
    }
    assert_eq!(host.calls.load(Ordering::SeqCst), calls);
}

#[test]
fn sending_text_is_secret_and_history_is_device_bound_read_only() {
    use ui::framework::Effect;
    let specs = MessagesEffect::specs();
    let send = specs
        .iter()
        .find(|spec| spec.id == MessagesEffect::SEND.id)
        .expect("send contract");
    assert!(
        send.params()
            .iter()
            .any(|param| param.name == "text" && param.ty == ui::tilo_actions::ParamType::Secret)
    );
    let history = MessagesEffect::declarations()
        .into_iter()
        .find(|decl| decl.id == MessagesEffect::READ_HISTORY.id)
        .expect("history contract");
    let private = MessagesEffect::Send {
        choice: choice(),
        operation: "fixture".into(),
        text: PRIVATE.into(),
    };
    assert!(!format!("{private:?}").contains(PRIVATE));
    assert!(history.device_only);
    assert_eq!(history.action_kind, ui::tilo_actions::ActionKind::Read);
}

#[test]
fn capture_only_history_is_readable_in_narrow_wide_and_os_themes() {
    for (width, theme) in [
        (420, ui::theme::Theme::dark()),
        (1180, ui::theme::Theme::dark()),
        (420, ui::theme::Theme::light()),
        (1180, ui::theme::Theme::high_contrast()),
    ] {
        let mut app = TestApp::<MessagesShell>::capture_at(Some("history"), (width, 820))
            .expect("capture history");
        app.harness().set_theme(theme);
        app.harness().relayout();
        let nodes = app.harness().a11y_nodes();
        for label in [
            "Are you nearby?",
            "On my way. See you in ten minutes!",
            "Delivered to recipient",
        ] {
            let node = nodes
                .iter()
                .find(|(_, node)| node.label() == Some(label))
                .expect("visible history text");
            let bounds = node.1.bounds().expect("history geometry");
            assert!(
                bounds.x0 >= 0.0
                    && bounds.x1 <= f64::from(width)
                    && bounds.y0 >= 0.0
                    && bounds.y1 <= 820.0,
                "history stays inside the viewport"
            );
        }
        assert!(!app.harness().dump_json().contains("Are you nearby?"));
    }
}

#[test]
fn semantic_people_and_choices_keep_matching_names_separate_and_refuse_revocation() {
    let mut app = live_messages_with_history(None);
    let directory = app.shared_person::<ContactDirectory>().expect("directory");
    let second = "00000000-0000-4000-8000-000000000002";
    directory
        .invoke(&ContactDirectory::PROJECT_CONTACT.with((Id::from_raw(second), "Alex".into())))
        .expect("different person with same label");
    app.settle();
    let people = app
        .effect(&MessagesEffect::ListPeople)
        .expect("semantic person listing")
        .value
        .expect("people value");
    let people = people.as_array().expect("people array");
    assert_eq!(people.len(), 2);
    assert!(
        people
            .iter()
            .any(|person| person.get("person").and_then(serde_json::Value::as_str) == Some(second))
    );
    let first = "00000000-0000-4000-8000-000000000001";
    let routes = app
        .effect(&MessagesEffect::ListRoutes {
            person: first.into(),
        })
        .expect("first person's choices")
        .value
        .expect("choices value");
    assert!(
        routes
            .as_array()
            .expect("choices array")
            .iter()
            .any(
                |route| route.get("recipient").and_then(serde_json::Value::as_str)
                    == Some("+12025550123")
            )
    );
    let other = app
        .effect(&MessagesEffect::ListRoutes {
            person: second.into(),
        })
        .expect("second person's choices")
        .value
        .expect("other choices value");
    assert!(
        other
            .as_array()
            .expect("other choices array")
            .iter()
            .all(|route| route.get("recipient").and_then(serde_json::Value::as_str) == Some(""))
    );
    directory
        .invoke(
            &ContactDirectory::FORGET_ENDPOINT.with((Id::from_raw(first), Id::from_raw("signal"))),
        )
        .expect("revoke exact identity association");
    app.settle();
    let revoked = app
        .effect(&MessagesEffect::ListRoutes {
            person: first.into(),
        })
        .expect("updated choices")
        .value
        .expect("revoked choices value");
    assert!(
        revoked
            .as_array()
            .expect("revoked choices array")
            .iter()
            .all(|route| route.get("recipient").and_then(serde_json::Value::as_str) == Some(""))
    );
}

#[test]
fn another_personas_host_capability_is_unavailable_before_any_read() {
    let host = Host::new(true, false);
    let foreign = HistoryClient::new("22".repeat(32), Arc::new(host.clone()));
    let mut app = live_messages_with_history(Some(foreign));
    app.effect(&MessagesEffect::SelectRoute { choice: choice() })
        .expect("metadata selection");
    assert!(
        app.effect(&MessagesEffect::ReadHistory { choice: choice() })
            .is_err()
    );
    assert_eq!(host.calls.load(Ordering::SeqCst), 0);
    assert!(!has_text(&mut app, PRIVATE));
}
