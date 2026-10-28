//! Messages on the web: two people's Messages windows, each lent the
//! page's messaging service (`tilo_messaging::room`) over its own HTTPS
//! session with the host that governs their room, send to each other and
//! see what arrives without asking again. The governor, the HTTPS door and
//! Messages are the real ones; only the window is headless.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

// This binary re-execs as the automerge worker (support is shared with crates that lack the dep).
tilo_automerge_isolate::test_worker!();

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use messages::{MessagesEffect, MessagesShell};
use tilo_app::framework::testing::TestApp;
use tilo_app::prelude::Id;
use tilo_browser_peer::invite::{ask_to_join, guest_certificate};
use tilo_browser_peer::{HttpClient, HttpsLink};
use tilo_contact_directory::ContactDirectory;
use tilo_messaging::PersonAccounts;
use tilo_messaging::room::{PrivateAnswer, PrivateDoor, RoomMessaging};
use tilo_relay_client::https as carrier;
use tilo_relay_protocol::rooms::private::{PrivateCall, PrivateOperation};
use tilo_relay_protocol::rooms::{JoinReply, Room, RoomRole, RoomsReply, RoomsRequest};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use support::{TestHost, enrol, start_room_host};

/// The Contacts person each window writes to.
const PERSON: &str = "00000000-0000-4000-8000-0000000000aa";

/// A page's door: its HTTPS session with the room's host.
struct SessionDoor {
    client: Arc<dyn HttpClient>,
    session: String,
    room: String,
    runtime: tokio::runtime::Handle,
}

impl PrivateDoor for SessionDoor {
    fn ask(&self, operation: PrivateOperation) -> PrivateAnswer {
        let (client, session) = (self.client.clone(), self.session.clone());
        let call = PrivateCall {
            room: self.room.clone(),
            operation,
        };
        // The window polls its work off the host's runtime; the request runs on it.
        let asked = self.runtime.spawn(async move {
            carrier::private(client.as_ref(), &session, &call)
                .await
                .map_err(|error| error.to_string())
        });
        Box::pin(async move { asked.await.map_err(|error| error.to_string())? })
    }

    fn spawn(&self, work: Pin<Box<dyn Future<Output = ()> + Send>>) {
        self.runtime.spawn(work);
    }
}

/// Enter the room's workspace on `link`, as a page does once it joined.
async fn subscribe(link: &mut HttpsLink, workspace_id: u64) {
    link.send(tilo_relay_protocol::Envelope::Subscribe { workspace_id })
        .unwrap();
    tilo_test_env::within("the host answers the subscription", async {
        loop {
            match link.recv().await {
                Some(tilo_relay_protocol::Envelope::Ready { .. }) => return,
                Some(tilo_relay_protocol::Envelope::NotAuthorised { reason, .. }) => {
                    panic!("not authorised: {reason}")
                }
                Some(_) => {}
                None => panic!("session ended"),
            }
        }
    })
    .await;
}

fn room(reply: RoomsReply) -> Room {
    match reply {
        RoomsReply::Room(room) => room,
        other => panic!("expected a room, got {other:?}"),
    }
}

struct AccountsOwner;
impl tilo_app::framework::ResourceApp for AccountsOwner {
    type Resource = PersonAccounts;
    const ID: &'static str = "so.tilo.settings";
    const TITLE: &'static str = "Accounts owner";
    fn view(cx: &mut tilo_app::framework::ViewCx<Self::Resource>) -> tilo_app::widget::Element {
        let _ = cx.shared_person::<PersonAccounts>();
        Box::new(tilo_app::widget::Column::new())
    }
}

/// `person`'s Messages window, lent `door`'s service, with their Tilo
/// account (as the page's sign-up publishes it) and `other` in Contacts as
/// reachable on Tilo. Returns the window and the conversation's choice.
fn window(person: &str, other: &str, door: SessionDoor) -> (TestApp<MessagesShell>, String) {
    let account = uuid::Uuid::new_v4().to_string();
    let scope = tilo_messaging::room::scope(&account);
    let mut settings = TestApp::<AccountsOwner>::for_person(person).unwrap();
    let owned = settings.shared_person::<PersonAccounts>().unwrap();
    owned
        .invoke(&PersonAccounts::PUBLISH_TILO_ACCOUNT.with((
            account.clone(),
            "Tilo".into(),
            scope.clone(),
            1,
        )))
        .unwrap();
    let service = RoomMessaging::new(person, Arc::new(door)).client(person);
    let mut app = TestApp::<MessagesShell>::for_person_with_lent(person, service).unwrap();
    let accounts = app.shared_person::<PersonAccounts>().unwrap();
    accounts
        .record()
        .merge_peer_document(owned.record().document().unwrap())
        .unwrap();
    drop(owned);
    drop(settings);
    let directory = app.shared_person::<ContactDirectory>().unwrap();
    directory
        .invoke(&ContactDirectory::PROJECT_CONTACT.with((Id::from_raw(PERSON), "Them".into())))
        .unwrap();
    app.settle();
    app.effect(&MessagesEffect::OpenPerson {
        person: PERSON.into(),
    })
    .unwrap();
    let choice = serde_json::json!({
        "service": "tilo",
        "account": account,
        "recipient": other,
        "association": "tilo",
        "scope": scope,
        "revision": "room-member",
    })
    .to_string();
    // Messages' own review of who the person is on Tilo.
    app.effect_confirmed(&MessagesEffect::ConfirmAccount {
        person: PERSON.into(),
        choice: choice.clone(),
    })
    .unwrap();
    app.settle();
    app.effect(&MessagesEffect::SelectRoute {
        choice: choice.clone(),
    })
    .unwrap();
    (app, choice)
}

/// Whether the window's conversation says `text`, as a line of it.
fn shows(app: &mut TestApp<MessagesShell>, text: &str) -> bool {
    app.harness()
        .wait_until(tilo_test_env::hang_bound(), |harness| {
            harness.a11y_nodes().iter().any(|(_, node)| {
                node.label()
                    .is_some_and(|label| label == text || label.split(", ").any(|p| p == text))
            })
        })
}

fn send(app: &mut TestApp<MessagesShell>, choice: &str, text: &str) {
    app.effect_confirmed(&MessagesEffect::Send {
        choice: choice.to_owned(),
        operation: uuid::Uuid::new_v4().to_string(),
        text: text.into(),
    })
    .unwrap();
}

#[test]
fn two_people_message_each_other_through_their_pages_sessions() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
    let host = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let page = enrol(&alice, "alice-browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let (doors, people, test_host) = runtime.block_on(async {
        let test_host = start_room_host(&alice, &host, state.path(), false).await;
        let TestHost { web, .. } = &test_host;
        let https: Arc<dyn HttpClient> =
            support::https::DevHttps::new(web.addr, web.certificate_sha256.as_ref().unwrap());
        let connect = |cert, key: SigningKey| {
            let https = https.clone();
            let host = host.cert.device_public_key;
            async move {
                HttpsLink::connect(https, &cert, host, &RevocationList::new(), &key)
                    .await
                    .unwrap()
            }
        };
        let mut owner = connect(page.cert.clone(), page.key.clone()).await;
        let ask = |request: RoomsRequest| {
            let owner = &owner;
            async move { carrier::rooms(owner.client().as_ref(), owner.session(), &request).await }
        };
        let created = room(
            ask(RoomsRequest::Create {
                name: "Messages".into(),
            })
            .await
            .unwrap(),
        );
        room(
            ask(RoomsRequest::Enter {
                room: created.room.clone(),
            })
            .await
            .unwrap(),
        );
        let RoomsReply::Invitation {
            signed_invitation, ..
        } = ask(RoomsRequest::Invite {
            room: created.room.clone(),
            request_id: [1; 32],
            role: RoomRole::Editor,
            expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
            inviter_name: Some("Alice".into()),
            wait_list_entry: None,
        })
        .await
        .unwrap()
        else {
            panic!("no invitation");
        };
        let bob_root = SigningKey::generate(&mut UnwrapErr(SysRng));
        let bob_key = SigningKey::generate(&mut UnwrapErr(SysRng));
        let bob = guest_certificate(
            &bob_root,
            bob_key.verifying_key().to_bytes(),
            "Bob",
            tilo_clock::unix_now_ns(),
        )
        .await
        .unwrap();
        assert_eq!(
            ask_to_join(
                https.as_ref(),
                &signed_invitation,
                &bob,
                &bob_key,
                [2; 32],
                "Bob"
            )
            .await
            .unwrap(),
            JoinReply::Waiting
        );
        room(
            ask(RoomsRequest::Admit {
                room: created.room.clone(),
                request_id: [2; 32],
            })
            .await
            .unwrap(),
        );
        let mut member = connect(bob.clone(), bob_key).await;
        subscribe(&mut member, created.workspace_id).await;
        subscribe(&mut owner, created.workspace_id).await;
        let door = |link: &HttpsLink| SessionDoor {
            client: link.client().clone(),
            session: link.session().to_owned(),
            room: created.room.clone(),
            runtime: tokio::runtime::Handle::current(),
        };
        (
            [door(&owner), door(&member)],
            [
                hex::encode(alice.verifying_key().to_bytes()),
                hex::encode(bob.user_public_key),
            ],
            test_host,
        )
    });
    let [alice_door, bob_door] = doors;
    let [alice_key, bob_key] = people;
    let (mut alice_window, to_bob) = window(&alice_key, &bob_key, alice_door);
    let (mut bob_window, to_alice) = window(&bob_key, &alice_key, bob_door);

    // Bob opens the conversation before anything is in it: what arrives
    // later shows on its own.
    bob_window
        .effect_confirmed(&MessagesEffect::ReadHistory {
            choice: to_alice.clone(),
        })
        .unwrap();
    send(&mut alice_window, &to_bob, "Hello from Alice's page");
    assert!(shows(&mut alice_window, "Hello from Alice's page"));
    assert!(
        shows(&mut bob_window, "Hello from Alice's page"),
        "Bob's open conversation shows what came in without asking again"
    );

    send(&mut bob_window, &to_alice, "Hi Alice from Bob");
    assert!(shows(&mut bob_window, "Hi Alice from Bob"));
    assert!(shows(&mut alice_window, "Hi Alice from Bob"));
    // Nothing of the conversation is in either window's shared record.
    for window in [&mut alice_window, &mut bob_window] {
        let dump = window.harness().dump_json();
        assert!(!dump.contains("Hello from Alice's page") && !dump.contains("from Bob"));
    }

    drop((alice_window, bob_window));
    runtime.block_on(test_host.relay.shutdown());
}
