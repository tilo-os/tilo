#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! Effects on the real component runtime: a typed vocabulary published on
//! the resource, answered by the view or surface that registered it, its
//! jobs reporting progress and finishing with actions; and a projection the
//! resource's actions queue, drained after every change.

use std::cell::RefCell;

use tilo_app::framework::{EffectCx, EffectStatus, Effected};
use tilo_app::prelude::tilo_actions::Action as _;
use tilo_app::prelude::*;
use tilo_resource::Effect;

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
    /// Note what was read, as an edit.
    #[effect(permission = "read")]
    Note { text: String },
}

thread_local! {
    /// What the outbox drain delivered, in order: the outside world.
    static DELIVERED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

struct MailApp;

impl ResourceApp for MailApp {
    type Resource = Mailbox;
    const ID: &'static str = "so.tilo.test-mailbox";
    const TITLE: &'static str = "Mailbox";

    fn view(cx: &mut ViewCx<Mailbox>) -> Element {
        let online = cx.local(false);
        let reached = online.clone();
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
            MailEffect::Note { text } => {
                cx.invoke(Mailbox::FETCHED.with(text));
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

/// A surface's own effects: answered with `&mut self`, completed with it.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "test.tally.effects")]
pub enum TallyEffect {
    /// Tally.
    #[effect(job)]
    Tally,
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
            TallyEffect::Tally => Ok(cx.thread(
                |_| 1,
                |tally: &mut Self, step: u32, cx| {
                    tally.counted += step;
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

    // A past version shows the surface without running or publishing it.
    let first = app.versions().first().unwrap().hash;
    assert!(app.past_version(&[first]).is_ok());
}
