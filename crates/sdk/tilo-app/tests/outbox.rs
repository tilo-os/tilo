//! An outbox on the real component runtime: two devices share a record,
//! only the device an entry names carries it out and records the result,
//! the settlement is nobody's undo step, and undoing the verb that queued
//! an entry queues its compensation for the same device.
#![allow(clippy::unwrap_used, reason = "test assertions")]

use std::cell::{Cell, RefCell};

use tilo_app::framework::{Effected, Projection};
use tilo_app::prelude::tilo_actions::Action as _;
use tilo_app::prelude::*;

thread_local! {
    /// What each device carried out, in order: the outside world.
    static CARRIED: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    /// Whether the outside is reachable.
    static OFFLINE: Cell<bool> = const { Cell::new(false) };
    static PROJECTION: RefCell<Option<Projection>> = const { RefCell::new(None) };
}

fn carried() -> Vec<(String, String)> {
    CARRIED.with(|carried| carried.borrow().clone())
}

/// A message for the device that holds the account it goes out through.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Delivery {
    pub device: String,
    pub text: String,
    /// The delivery this one takes back.
    pub recalls: Option<Id>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Post {
    /// Settled with the server's receipt.
    pub outbox: Outbox<Delivery, String>,
    pub note: String,
}

#[resource(kind = "test.post", label = "Post")]
impl Post {
    /// Send a message through the account on `device`.
    #[action(undo = Self::unsend)]
    pub fn send(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Delivery, String>,
        device: String,
        text: String,
    ) {
        outbox.push(
            cx.id(),
            Delivery {
                device,
                text,
                recalls: None,
            },
        );
    }

    fn unsend(undoing: Undoing<'_, Self, post::Send>) -> Option<Invoke<Self>> {
        Some(Self::RECALL.with(undoing.minted.first()?.clone()))
    }

    /// Take a sent message back.
    #[action(item = outbox)]
    pub fn recall(
        &mut self,
        cx: &mut ActionCx,
        outbox: &mut Outbox<Delivery, String>,
        entry: Id,
    ) -> Result<(), Refusal> {
        let sent = outbox.get(&entry).ok_or("no such message")?;
        let Some(settled) = &sent.settled else {
            outbox.forget(&entry);
            return Ok(());
        };
        let recall = Delivery {
            device: sent.value.device.clone(),
            text: format!("recall {}", settled.result()),
            recalls: Some(entry),
        };
        outbox.push(cx.id(), recall);
        Ok(())
    }

    /// Write a note.
    #[action]
    pub fn write(&mut self, note: String) {
        self.note = note;
    }
}

struct PostApp;

impl ResourceApp for PostApp {
    type Resource = Post;
    const ID: &'static str = "so.tilo.test-post";
    const TITLE: &'static str = "Post";

    fn view(cx: &mut ViewCx<Post>) -> Element {
        let me = cx.device().unwrap_or_default().to_owned();
        let here = me.clone();
        let projection = cx.outbox(
            Post::paths().outbox,
            move |_, delivery: &Delivery| delivery.device == here,
            move |_, delivery, _| {
                if OFFLINE.with(Cell::get) {
                    return Err("the server is unreachable".into());
                }
                CARRIED.with(|carried| {
                    carried
                        .borrow_mut()
                        .push((me.clone(), delivery.text.clone()));
                });
                Ok(Effected::value(format!("receipt:{}", delivery.text)))
            },
        );
        PROJECTION.with(|slot| *slot.borrow_mut() = Some(projection));
        Box::new(Column::new())
    }
}

/// Exchange what each device has, as sync does.
fn sync(a: &mut TestApp<PostApp>, b: &mut TestApp<PostApp>) {
    let mut doc = b.peer_document();
    a.merge_peer(&mut doc).unwrap();
    b.merge_peer(&mut doc).unwrap();
    a.settle();
    b.settle();
}

fn entry(app: &TestApp<PostApp>, text: &str) -> Id {
    app.state()
        .outbox
        .entries()
        .find(|(_, entry)| entry.value.text == text)
        .map(|(id, _)| id.clone())
        .unwrap()
}

#[test]
fn only_the_owning_device_carries_out_an_entry_and_both_see_the_result() {
    CARRIED.with(|carried| carried.borrow_mut().clear());
    OFFLINE.with(|offline| offline.set(false));
    let mut laptop = TestApp::<PostApp>::on_device("laptop").unwrap();
    let mut phone = TestApp::<PostApp>::on_device("phone").unwrap();
    sync(&mut laptop, &mut phone);

    laptop
        .invoke(Post::SEND.with(("phone".to_owned(), "hello".to_owned())))
        .unwrap();
    laptop.settle();
    assert!(carried().is_empty(), "the laptop holds no account for it");
    assert_eq!(laptop.state().outbox.pending().count(), 1);

    sync(&mut laptop, &mut phone);
    assert_eq!(carried(), [("phone".to_owned(), "hello".to_owned())]);
    sync(&mut laptop, &mut phone);
    let id = entry(&laptop, "hello");
    assert_eq!(
        laptop.state().outbox.outcome(&id),
        Some(&Settled::Done("receipt:hello".to_owned())),
        "the result reaches every replica"
    );

    // Nothing runs twice: not on another change, not after a sync.
    phone.invoke(Post::WRITE.with("unrelated")).unwrap();
    sync(&mut laptop, &mut phone);
    assert_eq!(carried().len(), 1);

    // The settlement was nobody's step: Ctrl+Z on the phone takes back the
    // note written there, and the person's undo (as the palette and agents
    // reach it, on every device) then their send from the laptop — never
    // what the phone recorded about the server.
    assert!(phone.undo());
    assert_eq!(phone.state().note, "");
    phone.call("undo", serde_json::json!({})).unwrap();
    let state = phone.state();
    assert_eq!(state.outbox.outcome(&id).map(Settled::is_done), Some(true));
    assert!(
        state
            .outbox
            .entries()
            .any(|(_, entry)| entry.value.recalls.as_ref() == Some(&id)),
        "undoing the send queued its recall"
    );
    assert_eq!(
        carried().last(),
        Some(&("phone".to_owned(), "recall receipt:hello".to_owned())),
        "and the phone, which holds the account, carried it out"
    );
}

#[test]
fn undoing_a_send_queues_its_recall_for_the_same_device() {
    CARRIED.with(|carried| carried.borrow_mut().clear());
    OFFLINE.with(|offline| offline.set(false));
    let mut laptop = TestApp::<PostApp>::on_device("laptop").unwrap();
    let mut phone = TestApp::<PostApp>::on_device("phone").unwrap();
    sync(&mut laptop, &mut phone);

    laptop
        .invoke(Post::SEND.with(("phone".to_owned(), "hello".to_owned())))
        .unwrap();
    sync(&mut laptop, &mut phone);
    sync(&mut laptop, &mut phone);
    let sent = entry(&laptop, "hello");
    assert!(laptop.state().outbox.outcome(&sent).is_some());

    assert!(laptop.undo(), "the send is the laptop's step");
    let state = laptop.state();
    let recall = state
        .outbox
        .pending()
        .find(|(_, delivery)| delivery.recalls.as_ref() == Some(&sent))
        .map(|(_, delivery)| delivery.clone())
        .expect("undo queued the recall");
    assert_eq!(recall.device, "phone");
    assert_eq!(recall.text, "recall receipt:hello");
    assert!(
        state.outbox.get(&sent).is_some(),
        "the sent message stays: undo did not erase what went out"
    );

    sync(&mut laptop, &mut phone);
    assert_eq!(
        carried(),
        [
            ("phone".to_owned(), "hello".to_owned()),
            ("phone".to_owned(), "recall receipt:hello".to_owned()),
        ]
    );
}

#[test]
fn an_entry_that_could_not_go_out_stays_pending_until_the_outside_changed() {
    CARRIED.with(|carried| carried.borrow_mut().clear());
    OFFLINE.with(|offline| offline.set(true));
    let mut app = TestApp::<PostApp>::on_device("laptop").unwrap();
    app.invoke(Post::SEND.with(("laptop".to_owned(), "later".to_owned())))
        .unwrap();
    app.settle();
    assert!(carried().is_empty());
    assert_eq!(app.state().outbox.pending().count(), 1);

    // Back online, but nothing tells the drain.
    OFFLINE.with(|offline| offline.set(false));
    app.settle();
    assert!(carried().is_empty());

    PROJECTION.with(|slot| slot.borrow().as_ref().unwrap().outside_changed());
    app.settle();
    app.settle();
    assert_eq!(carried(), [("laptop".to_owned(), "later".to_owned())]);
    assert_eq!(app.state().outbox.pending().count(), 0);

    // History shows a past version without carrying anything out again.
    let first = app.versions().first().unwrap().hash;
    app.past_version(&[first]).unwrap();
    assert_eq!(carried().len(), 1);
}

thread_local! {
    /// The invocation each awaited entry ran under, and its pending reply.
    static AWAITED: RefCell<Vec<(Option<tilo_actions::InvocationId>, String)>> =
        const { RefCell::new(Vec::new()) };
    static REPLIES: RefCell<Vec<tilo_app::framework::EffectReply>> = const { RefCell::new(Vec::new()) };
}

/// Carries each entry out under the invocation that queued it, answering
/// when the outside confirms.
struct AwaitedApp;

impl ResourceApp for AwaitedApp {
    type Resource = Post;
    const ID: &'static str = "so.tilo.test-post-awaited";
    const TITLE: &'static str = "Post";

    fn view(cx: &mut ViewCx<Post>) -> Element {
        cx.outbox_awaited(
            Post::paths().outbox,
            |_, _: &Delivery| true,
            |_, delivery, cx| {
                AWAITED.with(|awaited| {
                    awaited
                        .borrow_mut()
                        .push((cx.invocation(), delivery.text.clone()));
                });
                REPLIES.with(|replies| replies.borrow_mut().push(cx.reply()));
                Ok(Effected::Pending)
            },
        );
        Box::new(Column::new())
    }
}

fn start(
    app: &mut TestApp<AwaitedApp>,
    action: &str,
    arguments: &serde_json::Value,
) -> tilo_actions::InvocationId {
    let instance_id = app.harness().component_instance_id().unwrap();
    let address = tilo_app::bound_dispatch::BoundActionAddress {
        instance_id,
        address: tilo_actions::ActionAddress::new("test.post", action),
    };
    let (invocation, started) = app
        .harness()
        .start_bound_job_for(None, &address, arguments.as_object().unwrap(), true)
        .unwrap();
    assert!(
        started.completed().is_none(),
        "the entry is still going out"
    );
    invocation
}

fn succeeded(app: &mut TestApp<AwaitedApp>, invocation: tilo_actions::InvocationId) -> bool {
    app.harness().job_updates(invocation).iter().any(|update| {
        matches!(update, tilo_actions::InvocationUpdate::Succeeded { invocation: actual, .. } if *actual == invocation)
    })
}

fn reply(result: Result<tilo_actions::ActionOutput, String>) {
    REPLIES
        .with(|replies| replies.borrow_mut().pop().unwrap())
        .finish(result);
}

#[test]
fn an_entry_runs_under_the_invocation_that_queued_it_which_waits_for_it() {
    AWAITED.with(|awaited| awaited.borrow_mut().clear());
    REPLIES.with(|replies| replies.borrow_mut().clear());
    let mut app = TestApp::<AwaitedApp>::new().unwrap();
    let send = start(
        &mut app,
        "send",
        &serde_json::json!({ "device": "test-device", "text": "hello" }),
    );
    assert_eq!(
        AWAITED.with(|awaited| awaited.borrow().clone()),
        [(Some(send), "hello".to_owned())]
    );
    app.settle();
    assert!(
        !succeeded(&mut app, send),
        "it waits for the entry to settle"
    );
    reply(Ok(tilo_actions::ActionOutput {
        value: Some(serde_json::json!("receipt:hello")),
        resources: Vec::new(),
    }));
    app.settle();
    assert!(succeeded(&mut app, send));
    let hello = entry_of(&app, "hello");
    assert_eq!(
        app.state().outbox.outcome(&hello),
        Some(&Settled::Done("receipt:hello".to_owned()))
    );

    // Undo is an invocation of its own; the recall it queues runs under it,
    // and a refused recall fails it.
    let undo = start(&mut app, "undo", &serde_json::json!({}));
    assert_ne!(undo, send);
    assert_eq!(
        AWAITED.with(|awaited| awaited.borrow().last().cloned()),
        Some((Some(undo), "recall receipt:hello".to_owned()))
    );
    reply(Err("the server refused the recall".into()));
    app.settle();
    assert!(app.harness().job_updates(undo).iter().any(
        |update| matches!(update, tilo_actions::InvocationUpdate::Failed { invocation, error }
            if *invocation == undo && error.message.contains("refused the recall"))
    ));

    // What a peer queued inherits no invocation.
    app.merge_peer_edit(|doc| {
        Post::SEND
            .with(("test-device".to_owned(), "from a peer".to_owned()))
            .build(0)
            .unwrap()
            .apply(doc)
            .unwrap();
    });
    app.settle();
    assert_eq!(
        AWAITED.with(|awaited| awaited.borrow().last().cloned()),
        Some((None, "from a peer".to_owned()))
    );
}

fn entry_of(app: &TestApp<AwaitedApp>, text: &str) -> Id {
    app.state()
        .outbox
        .entries()
        .find(|(_, entry)| entry.value.text == text)
        .map(|(id, _)| id.clone())
        .unwrap()
}

/// A meeting on a server's calendar.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Meeting {
    pub title: String,
    pub room: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Agenda {
    pub meeting: Meeting,
    /// Each version of the meeting the server is to get.
    pub uploads: Outbox<Meeting>,
}

#[resource(kind = "test.agenda", label = "Agenda", queue = Agenda::queue)]
impl Agenda {
    /// Move the meeting and rename it at once.
    #[action]
    pub fn reschedule(&mut self, title: String, room: String) {
        self.meeting = Meeting { title, room };
    }

    fn queue(&mut self, before: &Self, cx: &mut ActionCx) {
        if self.meeting != before.meeting {
            self.uploads.push(cx.id(), self.meeting.clone());
        }
    }
}

struct AgendaApp;

impl ResourceApp for AgendaApp {
    type Resource = Agenda;
    const ID: &'static str = "so.tilo.test-agenda";
    const TITLE: &'static str = "Agenda";

    fn view(cx: &mut ViewCx<Agenda>) -> Element {
        // The server takes every upload at once.
        cx.outbox(
            Agenda::paths().uploads,
            |_, _: &Meeting| true,
            |_, _, _| Ok(Effected::done()),
        );
        Box::new(Column::new())
    }
}

fn sent(app: &TestApp<AgendaApp>) -> Vec<(String, String)> {
    app.state()
        .uploads
        .entries()
        .map(|(_, entry)| (entry.value.title.clone(), entry.value.room.clone()))
        .collect()
}

#[test]
fn undoing_a_field_edit_that_queued_queues_the_restored_value() {
    let mut app = TestApp::<AgendaApp>::new().unwrap();
    app.invoke(Invoke::edit(&Agenda::paths().meeting.title.replace("Review")).unwrap())
        .unwrap();
    assert_eq!(sent(&app), [("Review".to_owned(), String::new())]);
    app.invoke(Agenda::RESCHEDULE.with(("Retro".to_owned(), "Attic".to_owned())))
        .unwrap();
    assert_eq!(sent(&app).len(), 2);

    // Ctrl+Z puts both fields back in one step, through apply: the server
    // gets the restored meeting.
    assert!(app.undo());
    app.settle();
    assert_eq!(app.state().meeting.title, "Review");
    assert_eq!(app.state().meeting.room, "");
    assert_eq!(
        sent(&app).last(),
        Some(&("Review".to_owned(), String::new()))
    );
    assert!(app.undo());
    app.settle();
    assert_eq!(app.state().meeting.title, "");
    assert_eq!(sent(&app).last(), Some(&(String::new(), String::new())));
    assert!(app.redo());
    app.settle();
    assert_eq!(app.state().meeting.title, "Review");
    assert_eq!(
        sent(&app).last(),
        Some(&("Review".to_owned(), String::new()))
    );
}

fn pending_steps(app: &TestApp<AwaitedApp>) -> usize {
    app.read_document(|doc| history_source::steps::pending_compensations(doc).len())
}

#[test]
fn an_undo_that_queues_counts_once_its_entry_settled_done() {
    AWAITED.with(|awaited| awaited.borrow_mut().clear());
    REPLIES.with(|replies| replies.borrow_mut().clear());
    let mut app = TestApp::<AwaitedApp>::new().unwrap();
    let send = start(
        &mut app,
        "send",
        &serde_json::json!({ "device": "test-device", "text": "hello" }),
    );
    reply(Ok(tilo_actions::ActionOutput::default()));
    app.settle();
    assert!(succeeded(&mut app, send));

    // Ctrl+Z queues the recall; until the server confirms it the send is
    // still in effect.
    assert!(app.undo());
    app.settle();
    app.settle();
    assert_eq!(
        AWAITED.with(|awaited| awaited.borrow().last().map(|(_, text)| text.clone())),
        Some("recall ".to_owned())
    );
    assert_eq!(pending_steps(&app), 1, "the undo waits for its recall");

    // The server refuses the recall for good: the send stays in effect.
    reply(Ok(tilo_actions::ActionOutput {
        value: serde_json::to_value(Settled::<String>::failed("too late")).ok(),
        resources: Vec::new(),
    }));
    app.settle();
    assert_eq!(
        pending_steps(&app),
        1,
        "a failed recall leaves the send in effect"
    );

    // Undo again: this recall goes through, and the send is taken back.
    assert!(app.undo());
    app.settle();
    app.settle();
    reply(Ok(tilo_actions::ActionOutput::default()));
    app.settle();
    assert_eq!(
        pending_steps(&app),
        0,
        "the confirmed recall completes the undo"
    );
    assert!(!app.undo(), "nothing more to take back");
}

#[test]
fn a_host_without_the_apps_types_settles_an_entry_through_a_field_edit() {
    CARRIED.with(|carried| carried.borrow_mut().clear());
    OFFLINE.with(|offline| offline.set(false));
    let mut app = TestApp::<PostApp>::on_device("laptop").unwrap();
    app.invoke(Post::SEND.with(("bot-host".to_owned(), "hi".to_owned())))
        .unwrap();
    app.settle();
    let id = entry(&app, "hi");
    let outcome = serde_json::to_value(Settled::Done("receipt:hi".to_owned())).unwrap();
    let arguments =
        tilo_app::tilo_resource::outbox_settle_arguments("outbox", id.as_str(), &outcome);
    app.call(
        tilo_app::tilo_resource::FIELD_EDIT,
        serde_json::Value::Object(arguments),
    )
    .unwrap();
    app.settle();
    assert_eq!(
        app.state().outbox.outcome(&id),
        Some(&Settled::Done("receipt:hi".to_owned()))
    );
    assert!(
        carried().is_empty(),
        "the laptop carried nothing out itself"
    );
}
