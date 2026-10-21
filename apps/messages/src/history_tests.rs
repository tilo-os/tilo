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
                .wait_until(tilo_test_env::hang_bound(), |harness| {
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

struct DelayedSend {
    pending: Host,
    error: tilo_messaging::SendError,
}

impl HistoryHost for DelayedSend {
    fn read(&self, route: routing::Route, limit: usize) -> HistoryFuture {
        Host::new(true, false).read(route, limit)
    }
}

impl tilo_messaging::MessagingHost for DelayedSend {
    fn supports(&self, _: &routing::Route) -> bool {
        true
    }
    fn send(&self, request: tilo_messaging::SendRequest) -> tilo_messaging::SendFuture {
        assert_eq!(request.route.service, "signal");
        let pending = self.pending.clone();
        let error = self.error;
        Box::pin(std::future::poll_fn(move |cx| {
            if !pending.ready.load(Ordering::SeqCst) {
                pending
                    .wakes
                    .lock()
                    .expect("send wakers")
                    .push(cx.waker().clone());
                return Poll::Pending;
            }
            Poll::Ready(Err(error))
        }))
    }
}

fn add_sms_route(app: &mut TestApp<MessagesShell>) -> String {
    let mut settings = TestApp::<crate::tests::AccountOwnerFixture>::for_person(&"11".repeat(32))
        .expect("original metadata owner");
    let owner = settings
        .shared_person::<tilo_messaging::PersonAccounts>()
        .expect("accounts owner");
    owner
        .invoke(&tilo_messaging::PersonAccounts::SAVE_ACCOUNT.with((
            "fixture-sms".into(),
            "sms".into(),
            "Personal SIM".into(),
            serde_json::json!({"messaging_scope":"fixture-sms-scope"}),
            true,
            0,
        )))
        .expect("second service metadata");
    app.shared_person::<tilo_messaging::PersonAccounts>()
        .expect("read-only accounts")
        .record()
        .merge_peer_document(owner.record().document().expect("owner document"))
        .expect("original record sync");
    app.shared_person::<ContactDirectory>()
        .expect("contacts")
        .invoke(&ContactDirectory::CONFIRM_ACCOUNT.with((
            Id::from_raw("00000000-0000-4000-8000-000000000001"),
            Id::from_raw("sms"),
            "sms".into(),
            "fixture-sms-scope".into(),
            "+12025550123".into(),
            "fixture-sms-revision".into(),
        )))
        .expect("reviewed SMS fixture identity");
    app.settle();
    serde_json::json!({"service":"sms", "account":"fixture-sms", "recipient":"+12025550123",
        "association":"sms", "scope":"fixture-sms-scope", "revision":"fixture-sms-revision"})
    .to_string()
}

#[test]
fn late_send_failure_preserves_the_selected_service_history_and_reports_failure() {
    for error in [
        tilo_messaging::SendError::Revoked,
        tilo_messaging::SendError::Unavailable,
    ] {
        for switch_back in [false, true] {
            let pending = Host::new(false, false);
            let client = tilo_messaging::MessagingClient::new(
                "11".repeat(32),
                Arc::new(DelayedSend {
                    pending: pending.clone(),
                    error,
                }),
            );
            let mut app = crate::tests::live_messages_with_service(Some(client));
            let sms = add_sms_route(&mut app);
            app.effect(&MessagesEffect::SelectRoute { choice: choice() })
                .expect("select Signal");
            let invocation = app
                .start_effect(&MessagesEffect::Send {
                    choice: choice(),
                    operation: "fixture-delayed-send".into(),
                    text: "private outgoing text".into(),
                })
                .expect("pending original send");
            app.effect(&MessagesEffect::SelectRoute {
                choice: sms.clone(),
            })
            .expect("switch explicitly to SMS");
            app.effect(&MessagesEffect::ReadHistory { choice: sms })
                .expect("selected SMS history");
            if switch_back {
                app.effect(&MessagesEffect::SelectRoute { choice: choice() })
                    .expect("return to Signal with a new conversation view");
                app.effect(&MessagesEffect::ReadHistory { choice: choice() })
                    .expect("fresh Signal history");
            }
            assert!(has_text(&mut app, PRIVATE));
            pending.release();
            assert!(
                app.harness()
                    .wait_until(tilo_test_env::hang_bound(), |harness| {
                        harness
                            .job_updates(invocation)
                            .iter()
                            .any(ui::tilo_actions::InvocationUpdate::is_terminal)
                    }),
                "the original send must finish"
            );
            assert!(
                app.harness().job_updates(invocation).iter().any(|update| {
                    matches!(update, ui::tilo_actions::InvocationUpdate::Failed { .. })
                }),
                "a late failure must remain a failure for its original caller"
            );
            assert!(
                has_text(&mut app, PRIVATE),
                "another service's history must survive the old send failure"
            );
            assert!(!app.harness().dump_json().contains(PRIVATE));
        }
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
fn repeated_person_and_route_selection_preserves_private_drafts_and_history() {
    const DRAFT: &str = "private unsent draft";
    for reopen_person in [false, true] {
        let client = tilo_messaging::MessagingClient::new(
            "11".repeat(32),
            Arc::new(DelayedSend {
                pending: Host::new(false, false),
                error: tilo_messaging::SendError::Unavailable,
            }),
        );
        let mut app = crate::tests::live_messages_with_service(Some(client));
        let sms = add_sms_route(&mut app);
        app.effect(&MessagesEffect::SelectRoute { choice: choice() })
            .expect("select original route");
        app.effect(&MessagesEffect::ReadHistory { choice: choice() })
            .expect("original private history");
        app.harness()
            .perform_named(
                "messages.composer",
                &ui::a11y::Action::SetValue(DRAFT.into()),
            )
            .expect("shared accessible composer");
        let repeated = if reopen_person {
            MessagesEffect::OpenPerson {
                person: "00000000-0000-4000-8000-000000000001".into(),
            }
        } else {
            MessagesEffect::SelectRoute { choice: choice() }
        };
        app.effect(&repeated)
            .expect("repeat the declared idempotent selection");
        assert!(
            app.harness()
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.value() == Some(DRAFT)),
            "reselecting the same person or service must preserve the draft"
        );
        assert!(
            has_text(&mut app, PRIVATE),
            "idempotent selection must preserve private history"
        );
        app.effect(&MessagesEffect::SelectRoute { choice: sms })
            .expect("change the actual service");
        assert!(
            !app.harness()
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.value() == Some(DRAFT)),
            "the old service draft must not cross into the new service"
        );
        assert!(!app.harness().dump_json().contains(DRAFT));
    }
}

#[test]
fn forgotten_recipient_discards_draft_and_requires_explicit_route_selection() {
    const DRAFT: &str = "private draft for the forgotten recipient";
    let client = tilo_messaging::MessagingClient::new(
        "11".repeat(32),
        Arc::new(DelayedSend {
            pending: Host::new(false, false),
            error: tilo_messaging::SendError::Revoked,
        }),
    );
    let mut app = crate::tests::live_messages_with_service(Some(client));
    app.effect(&MessagesEffect::SelectRoute { choice: choice() })
        .expect("original route");
    app.effect(&MessagesEffect::ReadHistory { choice: choice() })
        .expect("original history");
    app.harness()
        .perform_named(
            "messages.composer",
            &ui::a11y::Action::SetValue(DRAFT.into()),
        )
        .expect("private accessible draft");
    let directory = app.shared_person::<ContactDirectory>().expect("contacts");
    let person = Id::from_raw("00000000-0000-4000-8000-000000000001");
    directory
        .invoke(&ContactDirectory::FORGET_ENDPOINT.with((person.clone(), Id::from_raw("signal"))))
        .expect("forget recipient");
    app.settle();
    assert!(
        !app.harness()
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.value() == Some(DRAFT)),
        "forgetting the route must dispose of its private draft"
    );
    assert!(!has_text(&mut app, PRIVATE));
    directory
        .invoke(&ContactDirectory::CONFIRM_ACCOUNT.with((
            person,
            Id::from_raw("signal-restored"),
            "signal".into(),
            "fixture-signal-scope".into(),
            "+12025550123".into(),
            "fixture-signal-revision-restored".into(),
        )))
        .expect("new reviewed association fixture");
    app.settle();
    let restored = serde_json::json!({"service":"signal", "account":"fixture-signal", "recipient":"+12025550123",
        "association":"signal-restored", "scope":"fixture-signal-scope", "revision":"fixture-signal-revision-restored"}).to_string();
    assert!(
        app.effect(&MessagesEffect::ReadHistory {
            choice: restored.clone()
        })
        .is_err(),
        "restoring metadata must not silently reselect a forgotten route"
    );
    app.effect(&MessagesEffect::SelectRoute { choice: restored })
        .expect("explicit new selection");
    assert!(
        !app.harness()
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.value() == Some(DRAFT))
    );
    assert!(!app.harness().dump_json().contains(DRAFT));
}

#[test]
fn confirmed_account_change_clears_old_draft_and_invalidates_pending_send() {
    const DRAFT: &str = "draft for the previous service";
    let pending = Host::new(false, false);
    let client = tilo_messaging::MessagingClient::new(
        "11".repeat(32),
        Arc::new(DelayedSend {
            pending: pending.clone(),
            error: tilo_messaging::SendError::Revoked,
        }),
    );
    let mut app = crate::tests::live_messages_with_service(Some(client));
    let sms = add_sms_route(&mut app);
    app.effect(&MessagesEffect::SelectRoute { choice: choice() })
        .expect("select Signal");
    app.harness()
        .perform_named(
            "messages.composer",
            &ui::a11y::Action::SetValue(DRAFT.into()),
        )
        .expect("original service draft");
    let invocation = app
        .start_effect(&MessagesEffect::Send {
            choice: choice(),
            operation: "fixture-confirmation-send".into(),
            text: DRAFT.into(),
        })
        .expect("pending original service send");
    app.effect_confirmed(&MessagesEffect::ConfirmAccount {
        person: "00000000-0000-4000-8000-000000000001".into(),
        choice: sms.clone(),
    })
    .expect("review a different account through the published action");
    assert!(
        !app.harness()
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.value() == Some(DRAFT)),
        "account confirmation must not carry the old draft into the newly selected service"
    );
    app.effect(&MessagesEffect::ReadHistory { choice: sms })
        .expect("confirmed service history");
    assert!(has_text(&mut app, PRIVATE));
    pending.release();
    assert!(
        app.harness()
            .wait_until(tilo_test_env::hang_bound(), |harness| {
                harness
                    .job_updates(invocation)
                    .iter()
                    .any(ui::tilo_actions::InvocationUpdate::is_terminal)
            }),
        "the old service result must complete"
    );
    assert!(
        app.harness()
            .job_updates(invocation)
            .iter()
            .any(|update| { matches!(update, ui::tilo_actions::InvocationUpdate::Failed { .. }) }),
        "the original caller must still receive failure"
    );
    assert!(
        has_text(&mut app, PRIVATE),
        "the confirmed service history must survive an old send result"
    );
    assert!(!app.harness().dump_json().contains(DRAFT));
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
