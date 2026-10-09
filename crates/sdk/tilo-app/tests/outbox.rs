//! An outbox on the real component runtime: two devices share a record,
//! only the device an entry names carries it out and records the result,
//! the settlement is nobody's undo step, and undoing the verb that queued
//! an entry queues its compensation for the same device.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::cell::{Cell, RefCell};

use tilo_app::framework::{Effected, Projection};
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

    // The settlement was nobody's step: the person's undo on the phone
    // takes back their note, then their send (from the laptop), never what
    // the phone recorded about the server.
    assert!(phone.undo());
    assert_eq!(phone.state().note, "");
    assert!(phone.undo());
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
