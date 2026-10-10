#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! Effects on the real component runtime: a typed vocabulary published on
//! the resource, answered by the view or surface that registered it, its
//! jobs reporting progress and finishing with actions; and a projection the
//! resource's actions queue, drained after every change.

use std::cell::RefCell;

use tilo_app::framework::{EffectCx, EffectStatus, Effected};
use tilo_app::portal::{PickRequest, PortalOutcome};
use tilo_app::prelude::tilo_actions::Action as _;
use tilo_app::prelude::*;
use tilo_resource::Effect;

thread_local! {
    static CAUSAL_REPLIES: RefCell<Vec<tilo_app::framework::EffectReply>> = const { RefCell::new(Vec::new()) };
    static CAUSAL_CALLERS: RefCell<Vec<tilo_actions::InvocationId>> = const { RefCell::new(Vec::new()) };
    static RECEIPT_REPLIES: RefCell<Vec<tilo_app::framework::EffectReply>> = const { RefCell::new(Vec::new()) };
    static RECEIPT_CALLERS: RefCell<Vec<tilo_actions::InvocationId>> = const { RefCell::new(Vec::new()) };
    static RECEIPT_SOURCES: RefCell<Vec<Option<tilo_actions::InvocationId>>> = const { RefCell::new(Vec::new()) };
}

struct CausalMailApp;

impl ResourceApp for CausalMailApp {
    type Resource = Mailbox;
    const ID: &'static str = "so.tilo.test-causal-mailbox";
    const TITLE: &'static str = "Mailbox";

    fn view(cx: &mut ViewCx<Mailbox>) -> Element {
        cx.drain_causal(
            |mail: &Mailbox| {
                mail.outbox
                    .iter()
                    .map(|(id, text)| (id.clone(), text.clone()))
                    .collect()
            },
            |_, cx| {
                CAUSAL_CALLERS.with(|callers| callers.borrow_mut().push(cx.invocation().unwrap()));
                CAUSAL_REPLIES.with(|replies| replies.borrow_mut().push(cx.reply()));
                Ok(Effected::Pending)
            },
        );
        Box::new(Column::new())
    }
}

fn start_causal(app: &mut TestApp<CausalMailApp>, text: &str) -> tilo_actions::InvocationId {
    start_causal_action(app, "send", &serde_json::json!({"text": text}))
}

fn start_causal_action(
    app: &mut TestApp<CausalMailApp>,
    action: &str,
    arguments: &serde_json::Value,
) -> tilo_actions::InvocationId {
    let instance_id = app.harness().component_instance_id().unwrap();
    let address = tilo_app::bound_dispatch::BoundActionAddress {
        instance_id,
        address: tilo_actions::ActionAddress::new("test.mailbox", action),
    };
    let (invocation, started) = app
        .harness()
        .start_bound_job_for(None, &address, arguments.as_object().unwrap(), true)
        .unwrap();
    assert!(
        started.completed().is_none(),
        "document consequence must remain pending"
    );
    invocation
}

#[test]
fn semantic_undo_consequences_keep_undos_own_new_invocation() {
    let mut app = TestApp::<CausalMailApp>::new().unwrap();
    let original = start_causal(&mut app, "original");
    CAUSAL_REPLIES
        .with(|replies| replies.borrow_mut().pop().unwrap())
        .finish(Ok(tilo_actions::ActionOutput::default()));
    app.settle();
    let entry = app.state().outbox.iter().next().unwrap().0.clone();
    app.call("delivered", serde_json::json!({"entry": entry}))
        .unwrap();
    assert!(app.state().outbox.is_empty());
    let undo = start_causal_action(&mut app, "undo", &serde_json::json!({}));
    assert_ne!(undo, original);
    assert_eq!(
        CAUSAL_CALLERS.with(|callers| callers.borrow().clone()),
        vec![original, undo]
    );
    app.settle();
    assert!(
        !app.harness()
            .job_updates(undo)
            .iter()
            .any(|update| matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }))
    );
    CAUSAL_REPLIES
        .with(|replies| replies.borrow_mut().pop().unwrap())
        .finish(Err("undo compensation refused".into()));
    app.settle();
    assert!(app.harness().job_updates(undo).iter().any(
        |update| matches!(update, tilo_actions::InvocationUpdate::Failed { invocation, error }
            if *invocation == undo && error.message.contains("undo compensation refused"))
    ));
    assert!(
        !app.harness()
            .job_updates(undo)
            .iter()
            .any(|update| matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }))
    );
}

#[test]
fn causal_document_jobs_keep_the_actual_invocation_and_wait_for_terminal_receipts() {
    let mut app = TestApp::<CausalMailApp>::new().unwrap();
    let invocation = start_causal(&mut app, "first");
    assert_eq!(
        CAUSAL_CALLERS.with(|callers| callers.borrow().clone()),
        vec![invocation]
    );
    app.settle();
    assert!(
        !app.harness()
            .job_updates(invocation)
            .iter()
            .any(|update| matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }))
    );
    CAUSAL_REPLIES
        .with(|replies| replies.borrow_mut().pop().unwrap())
        .finish(Ok(tilo_actions::ActionOutput::default()));
    app.settle();
    assert!(app.harness().job_updates(invocation).iter()
        .any(|update| matches!(update, tilo_actions::InvocationUpdate::Succeeded { invocation: actual, .. } if *actual == invocation)));
    // The old outbox entry remains pending. Another caller's edit must not
    // adopt it, even though an ordinary drain would retry pending work.
    app.call("fetched", serde_json::json!({"text": "unrelated"}))
        .unwrap();
    app.settle();
    assert_eq!(
        CAUSAL_CALLERS.with(|callers| callers.borrow().clone()),
        vec![invocation]
    );
    let second = start_causal(&mut app, "second");
    assert_eq!(
        CAUSAL_CALLERS.with(|callers| callers.borrow().clone()),
        vec![invocation, second]
    );
    CAUSAL_REPLIES
        .with(|replies| replies.borrow_mut().pop().unwrap())
        .finish(Err("compensation refused".into()));
    app.settle();
    assert!(app.harness().job_updates(second).iter()
        .any(|update| matches!(update, tilo_actions::InvocationUpdate::Failed { error, .. } if error.message.contains("compensation refused"))));
}

#[test]
fn causal_document_job_cancellation_stops_the_same_delayed_consequence() {
    let mut app = TestApp::<CausalMailApp>::new().unwrap();
    let invocation = start_causal(&mut app, "cancel");
    assert!(app.harness().cancel_job(invocation).is_some());
    let reply = CAUSAL_REPLIES.with(|replies| replies.borrow_mut().pop().unwrap());
    assert!(reply.is_cancelled());
    reply.finish(Ok(tilo_actions::ActionOutput::default()));
    app.settle();
    assert!(
        !app.harness()
            .job_updates(invocation)
            .iter()
            .any(|update| matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }))
    );
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Mailbox {
    pub outbox: Collection<String>,
    pub sent: Collection<String>,
    pub fetched: String,
}

#[resource(kind = "test.mailbox", label = "Mailbox")]
impl Mailbox {
    /// Queue a message for delivery.
    #[action]
    pub fn send(&mut self, cx: &mut ActionCx, text: String) {
        self.outbox.push(cx.id(), text);
    }

    /// Record that a queued message was delivered.
    #[action]
    pub fn delivered(&mut self, cx: &mut ActionCx, entry: Id) -> Result<(), &'static str> {
        let text = self.outbox.remove(&entry).ok_or("no such message")?;
        self.sent.push(cx.id(), text);
        Ok(())
    }

    /// Record what a fetch brought.
    #[action]
    pub fn fetched(&mut self, text: String) {
        self.fetched = text;
    }
}

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "test.mailbox.effects")]
pub enum MailEffect {
    /// Count the delivered messages.
    #[effect(returns, permission = "read")]
    Count,
    /// Fetch new mail.
    #[effect(job)]
    Fetch { from: String },
    /// Connect to the server.
    Connect,
    /// Promise an answer without starting anything.
    #[effect(job)]
    Stall,
    /// Settle an external receipt as the original caller.
    #[effect(job)]
    Receipt,
    /// A read receipt cannot authorize a document edit.
    #[effect(job, permission = "read")]
    ReadReceipt,
    /// Note what was read, as an edit.
    #[effect(permission = "read")]
    Note { text: String },
    /// Show the sent folder.
    #[effect(view)]
    ShowSent,
}

thread_local! {
    /// What the outbox drain delivered, in order: the outside world.
    static DELIVERED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

struct MailApp;

fn deferred_receipt(cx: &mut EffectCx<Mailbox, ()>) -> Effected {
    RECEIPT_SOURCES.with(|sources| sources.borrow_mut().push(cx.source_invocation()));
    RECEIPT_REPLIES.with(|replies| {
        replies.borrow_mut().push(cx.reply_then(|(), output, cx| {
            RECEIPT_SOURCES.with(|sources| sources.borrow_mut().push(cx.source_invocation()));
            RECEIPT_CALLERS.with(|callers| {
                callers.borrow_mut().push(cx.invocation().unwrap());
            });
            let text = output
                .value
                .and_then(|value| value.as_str().map(str::to_owned))
                .ok_or("receipt has no text")?;
            cx.invoke(Mailbox::FETCHED.with(text));
            Ok(Effected::done())
        }));
    });
    Effected::Pending
}

impl ResourceApp for MailApp {
    type Resource = Mailbox;
    const ID: &'static str = "so.tilo.test-mailbox";
    const TITLE: &'static str = "Mailbox";

    fn view(cx: &mut ViewCx<Mailbox>) -> Element {
        let online = cx.local(false);
        let reached = online.clone();
        let shown = cx.local(false);
        let shown_text = shown.clone();
        cx.effects(move |effect: MailEffect, cx| match effect {
            MailEffect::Count => Ok(Effected::value(cx.live().with(|mail| mail.sent.len()))),
            MailEffect::Fetch { from } => {
                let online = online.clone();
                Ok(cx.thread(
                    move |progress| {
                        progress.report("connecting", Some(0), Some(1));
                        format!("hello from {from}")
                    },
                    move |(), text, cx| {
                        online.set(true);
                        cx.invoke(Mailbox::FETCHED.with(text.clone()));
                        Ok(Effected::value(text))
                    },
                ))
            }
            MailEffect::Connect => Ok(Effected::done()),
            MailEffect::Stall => Ok(Effected::Pending),
            MailEffect::Receipt | MailEffect::ReadReceipt => Ok(deferred_receipt(cx)),
            MailEffect::Note { text } => {
                cx.invoke(Mailbox::FETCHED.with(text));
                Ok(Effected::done())
            }
            MailEffect::ShowSent => {
                shown.set(true);
                Ok(Effected::done())
            }
        })
        .status(move || {
            if reached.get() {
                Vec::new()
            } else {
                vec![
                    EffectStatus::unavailable(MailEffect::CONNECT, "offline", "Not fetched yet")
                        .with_label("Connect (offline)"),
                ]
            }
        });
        cx.drain(
            |mail: &Mailbox| {
                mail.outbox
                    .iter()
                    .map(|(id, text)| (id.clone(), (id.clone(), text.clone())))
                    .collect()
            },
            |(id, text), cx| {
                DELIVERED.with(|delivered| delivered.borrow_mut().push(text));
                cx.invoke(Mailbox::DELIVERED.with(id));
                Ok(Effected::done())
            },
        );
        Box::new(
            Column::new()
                .push(Box::new(cx.effect_button(
                    "Fetch",
                    &MailEffect::Fetch {
                        from: "a button".into(),
                    },
                )))
                .push(Box::new(
                    cx.text(|mail| format!("Fetched: {}", mail.fetched)),
                ))
                .push(Box::new(Text::from_state(&shown_text, |shown| {
                    if *shown {
                        "Showing sent"
                    } else {
                        "Showing inbox"
                    }
                    .to_owned()
                })))
                .push(Box::new(Text::from_state(
                    &cx.dispatcher().refused(),
                    |refused| {
                        refused
                            .as_ref()
                            .map(|refused| {
                                format!("Refused {}: {}", refused.action, refused.reason)
                            })
                            .unwrap_or_default()
                    },
                ))),
        )
    }
}

fn effect<'a>(surface: &'a serde_json::Value, id: &str) -> Option<&'a serde_json::Value> {
    surface
        .as_array()?
        .iter()
        .find(|row| row["resource"] == "test.mailbox" && row["action"] == id)
}

#[test]
fn effects_are_published_and_answer_every_caller_through_the_door() {
    let mut app = TestApp::<MailApp>::new().unwrap();

    // phase: the vocabulary is published on the resource, as callers see it now
    let surface = app.agent_surface();
    for id in ["count", "fetch", "connect"] {
        assert_eq!(effect(&surface, id).unwrap()["operation"], "Effect", "{id}");
    }
    assert_eq!(
        effect(&surface, "connect").unwrap()["label"],
        "Connect (offline)"
    );
    let refused = app.effect(&MailEffect::Connect).unwrap_err();
    assert!(
        matches!(&refused, tilo_actions::DoorError::Unavailable { code, .. } if code == "offline"),
        "{refused:?}"
    );

    // phase: an immediate effect answers at once
    let count = app.effect(&MailEffect::Count).unwrap();
    assert_eq!(count.value, Some(serde_json::json!(0)));

    // phase: a view is published as one and reaches its handler the same way
    assert_eq!(effect(&surface, "show-sent").unwrap()["operation"], "View");
    app.effect(&MailEffect::ShowSent).unwrap();
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("Showing sent"),
        "the view's handler ran"
    );

    // phase: a job reports progress, then finishes on the UI thread with an
    // action and its result
    let fetched = app.effect(&MailEffect::Fetch {
        from: "an agent".into(),
    });
    assert_eq!(
        fetched.unwrap().value,
        Some(serde_json::json!("hello from an agent"))
    );
    let progress = app.effect_progress();
    assert!(
        progress.iter().any(|update| matches!(
            update,
            tilo_actions::InvocationUpdate::Progress { phase, total: Some(1), .. } if phase == "connecting"
        )),
        "{progress:?}"
    );
    app.settle();
    assert_eq!(app.state().fetched, "hello from an agent");

    // phase: availability follows the view's state
    let surface = app.agent_surface();
    assert_eq!(
        effect(&surface, "connect").unwrap()["label"],
        "Connect to the server"
    );
    app.effect(&MailEffect::Connect).unwrap();

    // phase: a job that started nothing fails instead of hanging its caller
    let stalled = app.effect(&MailEffect::Stall).unwrap_err();
    assert!(
        format!("{stalled:?}").contains("started no job"),
        "{stalled:?}"
    );
}

#[test]
fn a_button_bound_to_an_effect_runs_it() {
    let mut app = TestApp::<MailApp>::new().unwrap();
    app.harness().click_named("fetch").unwrap();
    let fetched = app
        .harness()
        .wait_until(tilo_app::time::Duration::from_secs(10), |harness| {
            harness
                .inspection_json()
                .is_ok_and(|tree| tree.contains("Fetched: hello from a button"))
        });
    assert!(fetched, "the button's job finished and its action applied");
    assert_eq!(app.state().fetched, "hello from a button");
}

#[test]
fn captures_publish_view_effects_and_availability_without_running_handlers() {
    let mut live = TestApp::<MailApp>::new().unwrap();
    let mut capture = TestApp::<MailApp>::capture(None).unwrap();
    let live_surface = live.agent_surface();
    let captured_surface = capture.agent_surface();
    for id in ["count", "fetch", "connect", "stall", "note", "show-sent"] {
        let live_spec = effect(&live_surface, id).unwrap();
        let captured_spec = effect(&captured_surface, id).unwrap();
        assert_eq!(
            captured_spec, live_spec,
            "capture lost effect metadata for {id}"
        );
    }
    assert_eq!(
        effect(&captured_surface, "connect").unwrap()["label"],
        "Connect (offline)"
    );
    let audit = capture.harness().native_app_audit();
    assert!(audit.is_certified(), "{:?}", audit.issues);

    // Both effect and view handlers stay inert, even through the real door.
    assert!(capture.effect(&MailEffect::Count).is_err());
    assert!(
        capture
            .effect(&MailEffect::Fetch {
                from: "capture".into()
            })
            .is_err()
    );
    assert!(capture.effect(&MailEffect::ShowSent).is_err());
    capture.settle();
    assert_eq!(capture.state(), Mailbox::default());
    assert!(capture.effect_progress().is_empty());
    assert!(
        capture
            .harness()
            .inspection_json()
            .unwrap()
            .contains("Showing inbox"),
        "the inert view handler must not change local presentation"
    );
}

#[test]
fn history_renders_effect_controls_without_replaying_outside_consequences() {
    DELIVERED.with(|delivered| delivered.borrow_mut().clear());
    let mut app = TestApp::<MailApp>::new().unwrap();
    app.invoke(Mailbox::FETCHED.with("historical".into()))
        .unwrap();
    let version = app.versions().last().unwrap().hash;
    app.invoke(Mailbox::SEND.with("delivered once".into()))
        .unwrap();
    app.settle();
    let past = app.past_version(&[version]).unwrap();
    assert_eq!(past.state().fetched, "historical");
    let texts = past.texts();
    assert!(texts.iter().any(|text| text == "Fetch"), "{texts:?}");
    assert!(
        texts.iter().any(|text| text == "Showing inbox"),
        "{texts:?}"
    );
    assert_eq!(
        DELIVERED.with(|delivered| delivered.borrow().clone()),
        ["delivered once"],
        "History must not replay an external drain"
    );
    assert!(app.state().outbox.is_empty());
}

#[test]
fn a_projection_drains_each_queued_entry_once_on_every_change() {
    DELIVERED.with(|delivered| delivered.borrow_mut().clear());
    let mut app = TestApp::<MailApp>::new().unwrap();

    app.invoke(Mailbox::SEND.with("first".into())).unwrap();
    app.settle();
    let delivered = || DELIVERED.with(|delivered| delivered.borrow().clone());
    assert_eq!(delivered(), ["first"]);
    let state = app.state();
    assert!(state.outbox.is_empty(), "the drain acknowledged its entry");
    assert_eq!(state.sent.values().collect::<Vec<_>>(), ["first"]);

    // A collaborator's queued message is drained here too.
    app.merge_peer_edit(|doc| {
        Mailbox::SEND
            .with("from a peer".into())
            .build(0)
            .unwrap()
            .apply(doc)
            .unwrap();
    });
    app.settle();
    assert_eq!(delivered(), ["first", "from a peer"]);
    assert!(app.state().outbox.is_empty());

    // Unrelated changes deliver nothing again.
    app.invoke(Mailbox::FETCHED.with("x".into())).unwrap();
    app.settle();
    assert_eq!(delivered().len(), 2);

    // History renders a past version without running the drain again.
    let versions = app.versions();
    let first = versions.first().unwrap().hash;
    app.past_version(&[first]).unwrap();
    assert_eq!(delivered().len(), 2);
}

#[test]
fn a_viewer_invokes_read_effects_only_and_sees_its_refused_edits() {
    let mut app = TestApp::<MailApp>::with_role(tilo_app::Role::Viewer).unwrap();
    assert_eq!(
        app.effect(&MailEffect::Count).unwrap().value,
        Some(serde_json::json!(0))
    );
    assert!(
        app.effect(&MailEffect::Fetch {
            from: "a viewer".into()
        })
        .is_err()
    );

    // The effect runs; the edit it submits is refused, and says so.
    app.effect(&MailEffect::Note {
        text: "seen".into(),
    })
    .unwrap();
    app.settle();
    assert_eq!(app.state().fetched, "");
    let tree = app.harness().inspection_json().unwrap();
    assert!(
        tree.contains("Refused apply: this mailbox is open to view only")
            || tree.contains("Refused fetched: this mailbox is open to view only"),
        "{tree}"
    );
}

#[test]
fn an_effect_lends_its_invoker_no_more_than_they_were_admitted_for() {
    let mut app = TestApp::<MailApp>::new().unwrap();
    // A viewer may take a read effect, but not the edit it submits.
    let refused = app
        .effect_by(
            "agent:viewer",
            &MailEffect::Note {
                text: "seen".into(),
            },
        )
        .unwrap_err();
    assert!(
        matches!(
            &refused,
            tilo_actions::DoorError::Unavailable { code, .. } if code == "permission_denied"
        ),
        "{refused:?}"
    );
    app.settle();
    assert_eq!(app.state().fetched, "", "nothing was written");

    // Someone admitted to write gets the edit their effect submits, as
    // theirs.
    app.effect_by(
        "agent:writer",
        &MailEffect::Fetch {
            from: "a writer".into(),
        },
    )
    .unwrap();
    assert_eq!(app.state().fetched, "hello from a writer");
    let invoker = app
        .versions()
        .last()
        .and_then(|version| version.context.clone())
        .and_then(|context| context.invoker)
        .map(|invoker| invoker.actor);
    assert_eq!(invoker.as_deref(), Some("agent:writer"));
}

fn start_receipt(app: &mut TestApp<MailApp>) -> tilo_actions::InvocationId {
    start_named_receipt(app, "receipt")
}

fn start_named_receipt(app: &mut TestApp<MailApp>, action: &str) -> tilo_actions::InvocationId {
    start_named_receipt_for(
        app,
        action,
        Some(tilo_actions::ActionInvoker::actor("agent:receipt-writer")),
    )
}

fn start_named_receipt_for(
    app: &mut TestApp<MailApp>,
    action: &str,
    invoker: Option<tilo_actions::ActionInvoker>,
) -> tilo_actions::InvocationId {
    let address = tilo_app::bound_dispatch::BoundActionAddress {
        instance_id: app.harness().component_instance_id().unwrap(),
        address: tilo_actions::ActionAddress::new("test.mailbox", action),
    };
    let (invocation, started) = app
        .harness()
        .start_bound_job_for(invoker, &address, &serde_json::Map::new(), true)
        .unwrap();
    assert!(started.completed().is_none());
    invocation
}

#[test]
fn effect_source_invocation_is_separate_and_retained_by_continuations() {
    RECEIPT_CALLERS.with(|callers| callers.borrow_mut().clear());
    RECEIPT_SOURCES.with(|sources| sources.borrow_mut().clear());
    RECEIPT_REPLIES.with(|replies| replies.borrow_mut().clear());
    let mut app = TestApp::<MailApp>::new().unwrap();

    let local = start_named_receipt_for(&mut app, "receipt", None);
    assert_eq!(
        RECEIPT_SOURCES.with(|sources| sources.borrow().clone()),
        [None]
    );
    RECEIPT_REPLIES
        .with(|replies| replies.borrow_mut().pop().unwrap())
        .answer(Ok(tilo_actions::ActionOutput {
            value: Some(serde_json::json!("local")),
            resources: Vec::new(),
        }));
    app.settle();
    assert_eq!(
        RECEIPT_SOURCES.with(|sources| sources.borrow().clone()),
        [None, None]
    );
    assert_eq!(
        RECEIPT_CALLERS.with(|callers| callers.borrow().clone()),
        [local]
    );

    RECEIPT_CALLERS.with(|callers| callers.borrow_mut().clear());
    RECEIPT_SOURCES.with(|sources| sources.borrow_mut().clear());
    let admitted = start_named_receipt(&mut app, "receipt");
    assert_eq!(
        RECEIPT_SOURCES.with(|sources| sources.borrow().clone()),
        [Some(admitted)]
    );
    RECEIPT_REPLIES
        .with(|replies| replies.borrow_mut().pop().unwrap())
        .answer(Ok(tilo_actions::ActionOutput {
            value: Some(serde_json::json!("admitted")),
            resources: Vec::new(),
        }));
    app.settle();
    assert_eq!(
        RECEIPT_SOURCES.with(|sources| sources.borrow().clone()),
        [Some(admitted), Some(admitted)]
    );
    assert_eq!(
        RECEIPT_CALLERS.with(|callers| callers.borrow().clone()),
        [admitted]
    );
}

#[test]
fn delayed_receipt_completion_applies_history_as_its_original_invoker() {
    let mut app = TestApp::<MailApp>::new().unwrap();
    let invocation = start_receipt(&mut app);
    app.settle();
    assert!(app.state().fetched.is_empty());
    assert!(RECEIPT_CALLERS.with(|callers| callers.borrow().is_empty()));
    RECEIPT_REPLIES
        .with(|replies| replies.borrow_mut().pop().unwrap())
        .answer(Ok(tilo_actions::ActionOutput {
            value: Some(serde_json::json!("settled receipt")),
            resources: Vec::new(),
        }));
    app.settle();
    assert_eq!(
        RECEIPT_CALLERS.with(|callers| callers.borrow().clone()),
        [invocation]
    );
    assert_eq!(app.state().fetched, "settled receipt");
    assert_eq!(
        app.versions()
            .last()
            .unwrap()
            .context
            .as_ref()
            .unwrap()
            .invoker
            .as_ref()
            .unwrap()
            .actor,
        "agent:receipt-writer"
    );
    assert!(
        app.harness()
            .job_updates(invocation)
            .iter()
            .any(|update| matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }))
    );
}

#[test]
fn delayed_receipt_refusal_preserves_failure_and_skips_history_completion() {
    let mut app = TestApp::<MailApp>::new().unwrap();
    let invocation = start_receipt(&mut app);
    let refused = tilo_actions::ActionFailure {
        code: "clipboard_refused".into(),
        message: "destination grant revoked".into(),
        details: Some(serde_json::json!({"receipt": "opaque"})),
        retryable: true,
    };
    RECEIPT_REPLIES
        .with(|replies| replies.borrow_mut().pop().unwrap())
        .answer(Err(refused.clone()));
    app.settle();
    assert!(RECEIPT_CALLERS.with(|callers| callers.borrow().is_empty()));
    assert!(app.state().fetched.is_empty());
    assert!(app.harness().job_updates(invocation).iter().any(|update|
        matches!(update, tilo_actions::InvocationUpdate::Failed { error, .. } if *error == refused)));
}

#[test]
fn cancelled_delayed_receipt_cannot_run_its_history_completion() {
    let mut app = TestApp::<MailApp>::new().unwrap();
    let invocation = start_receipt(&mut app);
    assert!(app.harness().cancel_job(invocation).is_some());
    let reply = RECEIPT_REPLIES.with(|replies| replies.borrow_mut().pop().unwrap());
    assert!(reply.is_cancelled());
    reply.answer(Ok(tilo_actions::ActionOutput {
        value: Some(serde_json::json!("cancelled receipt")),
        resources: Vec::new(),
    }));
    app.settle();
    assert!(RECEIPT_CALLERS.with(|callers| callers.borrow().is_empty()));
    assert!(app.state().fetched.is_empty());
    assert!(
        !app.harness()
            .job_updates(invocation)
            .iter()
            .any(|update| matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }))
    );
}

#[test]
fn delayed_receipt_completion_cannot_exceed_its_original_read_admission() {
    let mut app = TestApp::<MailApp>::new().unwrap();
    let invocation = start_named_receipt(&mut app, "read-receipt");
    RECEIPT_REPLIES
        .with(|replies| replies.borrow_mut().pop().unwrap())
        .answer(Ok(tilo_actions::ActionOutput {
            value: Some(serde_json::json!("unadmitted edit")),
            resources: Vec::new(),
        }));
    app.settle();
    assert_eq!(
        RECEIPT_CALLERS.with(|callers| callers.borrow().clone()),
        [invocation]
    );
    assert!(app.state().fetched.is_empty());
    assert!(app.harness().job_updates(invocation).iter().any(
        |update| matches!(update, tilo_actions::InvocationUpdate::Failed { error, .. }
            if error.code == "permission_denied")
    ));
    assert!(
        !app.harness()
            .job_updates(invocation)
            .iter()
            .any(|update| matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }))
    );
}

/// A surface's own effects: answered with `&mut self`, completed with it.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "test.tally.effects")]
pub enum TallyEffect {
    /// Tally.
    #[effect(job)]
    Tally,
    /// Count a row.
    Count {
        #[param("How much the row counts")]
        by: u32,
    },
}

#[derive(Default)]
struct Tally {
    rect: Rect,
    counted: u32,
}

impl Widget for Tally {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.new_leaf(tilo_app::taffy::Style::default()).unwrap()
    }

    fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, Size::ZERO);
    }

    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
    }

    fn name(&self) -> &'static str {
        "Tally"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ResourceSurface<Mailbox, TallyEffect> for Tally {
    fn present(&mut self, _: &std::rc::Rc<Mailbox>) {}

    fn effect(
        &mut self,
        effect: TallyEffect,
        cx: &mut EffectCx<Mailbox, Self>,
    ) -> Result<Effected, String> {
        match effect {
            TallyEffect::Count { by } => {
                self.counted = self.counted.saturating_add(by);
                Ok(Effected::done())
            }
            TallyEffect::Tally => Ok(cx.thread(
                |_| 1,
                |tally: &mut Self, step: u32, cx| {
                    tally.counted = tally.counted.saturating_add(step);
                    cx.invoke(Mailbox::FETCHED.with(format!("tally {}", tally.counted)));
                    Ok(Effected::value(tally.counted))
                },
            )),
        }
    }

    fn effects(&self) -> Vec<EffectStatus> {
        vec![EffectStatus::labelled(
            TallyEffect::TALLY,
            format!("Tally ({})", self.counted),
        )]
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        vec![LiveRow::new("row:5", "test.tally.row", "Five").effect(&TallyEffect::Count { by: 5 })]
    }
}

struct TallyApp;

impl ResourceApp for TallyApp {
    type Resource = Mailbox;
    const ID: &'static str = "so.tilo.test-tally";
    const TITLE: &'static str = "Tally";

    fn view(cx: &mut ViewCx<Mailbox>) -> Element {
        cx.surface(Tally::default())
    }
}

#[test]
fn a_surface_answers_its_effects_and_completes_them_with_its_own_state() {
    let mut app = TestApp::<TallyApp>::new().unwrap();
    assert_eq!(
        app.effect(&TallyEffect::Tally).unwrap().value,
        Some(serde_json::json!(1))
    );
    assert_eq!(
        app.effect(&TallyEffect::Tally).unwrap().value,
        Some(serde_json::json!(2))
    );
    app.settle();
    assert_eq!(app.state().fetched, "tally 2");
    let surface = app.agent_surface();
    assert_eq!(effect(&surface, "tally").unwrap()["label"], "Tally (2)");

    // A live row offers the effect bound to its own arguments.
    app.call_on("row:5", "count", serde_json::json!({}))
        .unwrap();
    assert_eq!(
        app.effect(&TallyEffect::Tally).unwrap().value,
        Some(serde_json::json!(8))
    );

    // A past version shows the surface without running or publishing it.
    let first = app.versions().first().unwrap().hash;
    assert!(app.past_version(&[first]).is_ok());
}

thread_local! {
    /// What a service reports it holds: the outside a mirror drain compares
    /// the document's sent messages against.
    static MIRRORED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// The mirror drain's handle, as the view registered it.
    static MIRROR: RefCell<Option<tilo_app::framework::Projection>> = const { RefCell::new(None) };
}

struct MirrorApp;

impl ResourceApp for MirrorApp {
    type Resource = Mailbox;
    const ID: &'static str = "so.tilo.test-mirror";
    const TITLE: &'static str = "Mirror";

    fn view(cx: &mut ViewCx<Mailbox>) -> Element {
        let projection = cx.drain(
            |mail: &Mailbox| {
                let mirrored = MIRRORED.with(|mirrored| mirrored.borrow().clone());
                mail.sent
                    .iter()
                    .filter(|(_, text)| !mirrored.contains(text))
                    .map(|(_, text)| (text.clone(), text.clone()))
                    .collect()
            },
            |text, _| {
                MIRRORED.with(|mirrored| mirrored.borrow_mut().push(text));
                Ok(Effected::done())
            },
        );
        MIRROR.with(|mirror| *mirror.borrow_mut() = Some(projection));
        Box::new(Column::new())
    }
}

#[test]
fn a_projection_looks_again_when_the_outside_it_compares_against_changed() {
    MIRRORED.with(|mirrored| mirrored.borrow_mut().clear());
    let mirrored = || MIRRORED.with(|mirrored| mirrored.borrow().clone());
    let mut app = TestApp::<MirrorApp>::new().unwrap();
    app.invoke(Mailbox::SEND.with("hello".into())).unwrap();
    let entry = app.state().outbox.iter().next().unwrap().0.clone();
    app.invoke(Mailbox::DELIVERED.with(entry)).unwrap();
    app.settle();
    assert_eq!(mirrored(), ["hello"]);

    // The service lost what it held; the document did not move.
    MIRRORED.with(|mirrored| mirrored.borrow_mut().clear());
    app.settle();
    assert!(
        mirrored().is_empty(),
        "nothing tells the drain to look again"
    );

    MIRROR.with(|mirror| mirror.borrow().as_ref().unwrap().outside_changed());
    app.settle();
    assert_eq!(mirrored(), ["hello"]);
}

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "test.mailbox.files")]
pub enum FileEffect {
    /// Import a message from a file.
    #[effect(job)]
    Import,
    /// Export what was fetched to a file.
    #[effect(job, returns)]
    Export,
    /// Sync with a device that is busy.
    #[effect(job)]
    SyncDevice,
}

thread_local! {
    /// The identity of the file last imported.
    static CHOSEN: RefCell<Option<RecordId>> = const { RefCell::new(None) };
}

/// Imports and exports through the host's file chooser.
struct FilesApp;

impl ResourceApp for FilesApp {
    type Resource = Mailbox;
    const ID: &'static str = "so.tilo.test-mailbox-files";
    const TITLE: &'static str = "Mailbox files";

    fn view(cx: &mut ViewCx<Mailbox>) -> Element {
        cx.effects(move |effect: FileEffect, cx| match effect {
            FileEffect::Import => Ok(cx.choose_file(
                PickRequest::open("Import a message", "Import"),
                |(), chosen, cx| {
                    let Some(file) = chosen else {
                        return Err("no file was chosen".into());
                    };
                    CHOSEN.with(|chosen| chosen.borrow_mut().clone_from(&file.identity));
                    cx.invoke(Mailbox::FETCHED.with(String::from_utf8_lossy(&file.bytes).into()));
                    Ok(Effected::done())
                },
            )),
            FileEffect::Export => {
                let fetched = cx.live().with(|mail| mail.fetched.clone());
                Ok(cx.save_file(
                    PickRequest::save("Export the message", "Export", "message.txt"),
                    fetched.into_bytes(),
                    |(), saved, _| Ok(Effected::value(saved)),
                ))
            }
            FileEffect::SyncDevice => {
                cx.reply()
                    .answer(Err(tilo_app::tilo_actions::ActionFailure {
                        code: "device_busy".into(),
                        message: "The device is busy".into(),
                        details: None,
                        retryable: true,
                    }));
                Ok(Effected::Pending)
            }
        });
        Box::new(cx.text(|mail| mail.fetched.clone()))
    }
}

#[test]
fn an_effect_imports_and_exports_through_the_hosts_file_chooser() {
    let directory = tempfile::tempdir().unwrap();
    let incoming = directory.path().join("incoming.txt");
    std::fs::write(&incoming, "from a file").unwrap();
    let (services, recorder) = tilo_app::host::HostServices::recording();
    let _host = tilo_app::host::enter(&services);
    let mut app = TestApp::<FilesApp>::new().unwrap();

    recorder.respond_to_pick(PortalOutcome::Opened(incoming.clone()));
    app.effect(&FileEffect::Import).unwrap();
    app.settle();
    assert_eq!(app.state().fetched, "from a file");
    assert_eq!(
        CHOSEN.with(|chosen| chosen.borrow_mut().take()),
        Some(RecordId::identify_file(&incoming)),
        "the chosen file is known by its durable identity"
    );
    assert_eq!(recorder.pick_requests()[0].title(), "Import a message");

    let outgoing = directory.path().join("outgoing.txt");
    recorder.respond_to_pick(PortalOutcome::Opened(outgoing.clone()));
    let saved = app.effect(&FileEffect::Export).unwrap();
    assert_eq!(saved.value, Some(serde_json::json!(true)));
    assert_eq!(std::fs::read_to_string(&outgoing).unwrap(), "from a file");

    recorder.respond_to_pick(PortalOutcome::Cancelled);
    let cancelled = app.effect(&FileEffect::Import).unwrap_err();
    app.settle();
    assert!(
        cancelled.to_string().contains("no file was chosen"),
        "{cancelled}"
    );
    assert_eq!(
        app.state().fetched,
        "from a file",
        "a cancelled import changes nothing"
    );
}

/// A surface that fetches on its own as soon as it is attached.
#[derive(Default)]
struct Fetcher {
    tally: Tally,
    fetches: u32,
}

impl Widget for Fetcher {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.tally.build_layout(taffy, theme, nodes)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.tally.apply_layout(taffy, node, origin);
    }

    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
    }

    fn name(&self) -> &'static str {
        "Fetcher"
    }

    fn get_rect(&self) -> Rect {
        self.tally.get_rect()
    }
}

impl ResourceSurface<Mailbox> for Fetcher {
    fn present(&mut self, _: &std::rc::Rc<Mailbox>) {}

    fn attach(&mut self, cx: SurfaceCx<Mailbox>) {
        let jobs = cx.jobs();
        // Two requests under one key: the later one wins.
        for text in ["stale", "fresh"] {
            jobs.start::<Self>("fetch", move |cx| {
                Ok(cx.thread(
                    move |_| format!("{text} by the surface"),
                    |fetcher: &mut Self, text, cx| {
                        fetcher.fetches = fetcher.fetches.saturating_add(1);
                        cx.invoke(Mailbox::FETCHED.with(format!("{text} ({})", fetcher.fetches)));
                        Ok(Effected::done())
                    },
                ))
            });
        }
        assert!(jobs.is_running("fetch") || cx.mode() != RunMode::Live);
    }
}

struct FetcherApp;

impl ResourceApp for FetcherApp {
    type Resource = Mailbox;
    const ID: &'static str = "so.tilo.test-fetcher";
    const TITLE: &'static str = "Fetcher";

    fn view(cx: &mut ViewCx<Mailbox>) -> Element {
        cx.jobs().start::<()>("outbox", |cx| {
            Ok(cx.thread(
                |_| "queued by the view".to_owned(),
                |(), text, cx| {
                    cx.invoke(Mailbox::SEND.with(text));
                    Ok(Effected::done())
                },
            ))
        });
        cx.surface(Fetcher::default())
    }
}

#[test]
fn a_view_and_a_surface_start_jobs_nobody_invoked_and_the_latest_wins() {
    let mut app = TestApp::<FetcherApp>::new().unwrap();
    for _ in 0..500 {
        app.settle();
        let state = app.state();
        if !state.fetched.is_empty() && !state.outbox.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let state = app.state();
    assert_eq!(
        state.fetched, "fresh by the surface (1)",
        "the stale fetch was stopped"
    );
    assert_eq!(
        state.outbox.values().cloned().collect::<Vec<_>>(),
        ["queued by the view"]
    );

    // A capture starts no job: what it shows is its fixture.
    let capture = TestApp::<FetcherApp>::capture(None).unwrap();
    assert_eq!(capture.state().fetched, "");
    assert!(capture.state().outbox.is_empty());
}

thread_local! {
    /// The jobs of the running [`ProgressApp`], for the test to read.
    static PROGRESS_JOBS: RefCell<Option<tilo_app::framework::Jobs<Mailbox>>> =
        const { RefCell::new(None) };
}

/// Lets the reporting job finish.
static RELEASE_SYNC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A view whose sync job says how far it is, then waits to be let go.
struct ProgressApp;

impl ResourceApp for ProgressApp {
    type Resource = Mailbox;
    const ID: &'static str = "so.tilo.test-progress";
    const TITLE: &'static str = "Progress";

    fn view(cx: &mut ViewCx<Mailbox>) -> Element {
        let jobs = cx.jobs();
        jobs.start::<()>("sync", |cx| {
            Ok(cx.thread(
                |progress| {
                    progress.report("syncing", Some(1), Some(2));
                    while !RELEASE_SYNC.load(std::sync::atomic::Ordering::Acquire) {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                },
                |(), (), _| Ok(Effected::done()),
            ))
        });
        PROGRESS_JOBS.with(|slot| *slot.borrow_mut() = Some(jobs));
        Box::new(cx.text(|mail| mail.fetched.clone()))
    }
}

#[test]
fn a_job_nobody_invoked_says_how_far_it_is_until_it_ends() {
    let mut app = TestApp::<ProgressApp>::new().unwrap();
    let progress = || PROGRESS_JOBS.with(|slot| slot.borrow().as_ref().unwrap().progress("sync"));
    for _ in 0..500 {
        app.settle();
        if progress().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        progress(),
        Some(tilo_app::framework::JobProgress {
            phase: "syncing".into(),
            completed: Some(1),
            total: Some(2),
        })
    );
    RELEASE_SYNC.store(true, std::sync::atomic::Ordering::Release);
    for _ in 0..500 {
        app.settle();
        if !PROGRESS_JOBS.with(|slot| slot.borrow().as_ref().unwrap().is_running("sync")) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(progress(), None, "an ended job reports nothing");
}

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "test.mailbox.device")]
pub enum DeviceEffect {
    /// Read the mail folder on this computer.
    #[effect(device_only)]
    ReadLocalFolder,
    /// Empty the mail folder on this computer.
    #[effect(device_only = true)]
    EmptyLocalFolder,
    /// Count the messages.
    Count,
}

#[test]
fn a_device_only_effect_is_published_as_device_only() {
    let decl = DeviceEffect::declarations()
        .into_iter()
        .map(|decl| (decl.id, decl.spec("test.mailbox.device")))
        .collect::<Vec<_>>();
    let tagged = |id: &str| {
        decl.iter()
            .find(|(declared, _)| *declared == id)
            .unwrap()
            .1
            .contract
            .tags
            .iter()
            .any(|tag| tag == tilo_app::tilo_actions::LOCAL_DEVICE_ONLY)
    };
    assert!(tagged("read-local-folder"));
    assert!(tagged("empty-local-folder"));
    assert!(!tagged("count"));
    assert!(
        LocalFolders::declarations()
            .iter()
            .all(|decl| decl.device_only),
        "a vocabulary marked device-only marks every effect"
    );
    let exempt = |id: &str| {
        FolderView::declarations()
            .into_iter()
            .find(|decl| decl.id == id)
            .unwrap()
            .spec("test.mailbox.folder-view")
            .contract
            .tags
            .iter()
            .any(|tag| tag == tilo_app::tilo_actions::LOCAL_DEVICE_ONLY)
    };
    assert!(exempt("open-folder"), "the vocabulary's marking stands");
    assert!(
        !exempt("sort-by-date"),
        "an effect exempted from its device-only vocabulary is not"
    );
    assert!(
        !FolderView::declarations()
            .iter()
            .find(|decl| decl.id == "sort-by-date")
            .unwrap()
            .device_only
    );
}

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "test.mailbox.folders", device_only)]
pub enum LocalFolders {
    /// Watch a folder.
    Watch { path: String },
    /// Forget a folder.
    Forget { path: String },
}

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "test.mailbox.folder-view", device_only)]
pub enum FolderView {
    /// Open a folder on this computer.
    OpenFolder,
    /// Sort by date, a preference of the person.
    #[effect(device_only = false)]
    SortByDate,
}

/// The mailbox, opened on a folder: `mailbox sent` shows the sent folder.
struct FolderApp;

impl ResourceApp for FolderApp {
    type Resource = Mailbox;
    const ID: &'static str = "so.tilo.test-folder";
    const TITLE: &'static str = "Folder";
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Mailbox>) -> Element {
        MailApp::view(cx)
    }

    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Mailbox>> {
        match input.argument() {
            Some("sent") => vec![Invoke::effect(&MailEffect::ShowSent)],
            Some("fetch") => vec![Invoke::effect(&MailEffect::Fetch {
                from: "a launch".into(),
            })],
            _ => Vec::new(),
        }
    }
}

/// A launch that shows something invokes a view effect through the same
/// door as a control: a viewer, who may not edit, still gets the folder it
/// opened, while an effect that needs write access is refused to them.
#[test]
fn a_launch_invokes_a_view_effect_even_for_a_viewer() {
    let shown = |app: &mut TestApp<FolderApp>| app.harness().inspection_json().unwrap();

    let mut viewer = TestApp::<FolderApp>::with_role(tilo_app::Role::Viewer)
        .unwrap()
        .relaunch(&["sent"])
        .unwrap();
    viewer.settle();
    let seen = shown(&mut viewer);
    assert!(seen.contains("Showing sent"), "{seen}");
    assert!(!seen.contains("Refused"), "{seen}");

    let mut fetcher = TestApp::<FolderApp>::with_role(tilo_app::Role::Viewer)
        .unwrap()
        .relaunch(&["fetch"])
        .unwrap();
    fetcher.settle();
    assert_eq!(
        fetcher.state().fetched,
        "",
        "a viewer's launch fetches nothing"
    );
}

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "test.mailbox.disks")]
pub enum DiskEffect {
    /// Erase a disk.
    #[effect(risk = "erase_risk")]
    Erase { disk: String },
}

/// Erasing the system disk is destructive; any other disk is not.
fn erase_risk(effect: &DiskEffect) -> tilo_app::tilo_actions::RiskLevel {
    let DiskEffect::Erase { disk } = effect;
    if disk == "system" {
        tilo_app::tilo_actions::RiskLevel::Destructive
    } else {
        tilo_app::tilo_actions::RiskLevel::Low
    }
}

/// A surface recording right now, so sending is not available.
#[derive(Default)]
struct Recorder {
    tally: Tally,
}

impl Widget for Recorder {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.tally.build_layout(taffy, theme, nodes)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.tally.apply_layout(taffy, node, origin);
    }

    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
    }

    fn name(&self) -> &'static str {
        "Recorder"
    }

    fn get_rect(&self) -> Rect {
        self.tally.get_rect()
    }
}

impl ResourceSurface<Mailbox> for Recorder {
    fn present(&mut self, _: &std::rc::Rc<Mailbox>) {}

    fn availability(&self) -> Vec<EffectStatus> {
        vec![EffectStatus::unavailable(
            Mailbox::SEND,
            "recording",
            "Nothing is sent while recording",
        )]
    }
}

struct DiskApp;

impl ResourceApp for DiskApp {
    type Resource = Mailbox;
    const ID: &'static str = "so.tilo.test-disks";
    const TITLE: &'static str = "Disks";

    fn view(cx: &mut ViewCx<Mailbox>) -> Element {
        cx.effects(|effect: DiskEffect, cx| {
            let DiskEffect::Erase { disk } = effect;
            cx.invoke(Mailbox::FETCHED.with(format!("erased {disk}")));
            Ok(Effected::done())
        });
        cx.surface(Recorder::default())
    }
}

#[test]
fn confirmation_follows_the_arguments_and_a_surface_withholds_an_action() {
    let mut app = TestApp::<DiskApp>::new().unwrap();
    app.effect(&DiskEffect::Erase { disk: "usb".into() })
        .expect("erasing a stick needs no confirmation");
    let unconfirmed = app.effect(&DiskEffect::Erase {
        disk: "system".into(),
    });
    assert!(unconfirmed.is_err(), "the system disk asks first");
    app.effect_confirmed(&DiskEffect::Erase {
        disk: "system".into(),
    })
    .expect("confirmed, it runs");
    app.settle();
    assert_eq!(app.state().fetched, "erased system");

    let refused = app
        .invoke(Mailbox::SEND.with("hello".into()))
        .expect_err("sending is unavailable while recording");
    assert!(format!("{refused:?}").contains("recording"), "{refused:?}");
    assert!(app.state().outbox.is_empty());
}

#[test]
fn background_effect_return_values_keep_their_declared_result_contract() {
    use tilo_resource::Effect as _;
    let export = FileEffect::Export.bound().contract;
    assert!(matches!(
        export.execution,
        tilo_actions::ExecutionMode::Background { cancellable: false }
    ));
    assert!(export.validate_declaration().is_empty());
    let output = tilo_actions::ActionOutput {
        value: Some(serde_json::json!({ "file": "exported.eml" })),
        resources: Vec::new(),
    };
    export.result.validate(&output).unwrap();
    let import = FileEffect::Import.bound().contract;
    assert!(import.validate_declaration().is_empty());
    assert!(import.result.validate(&output).is_err());
    import
        .result
        .validate(&tilo_actions::ActionOutput::default())
        .unwrap();
}
