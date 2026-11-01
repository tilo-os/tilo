//! A private message reaches the person it was sent to: a text written on
//! one person's device arrives on the other's session, over the two devices'
//! own collaboration endpoints, and both sides keep the conversation they
//! exchanged. Sending is the published action, so what a composer does is
//! what this test does.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_compositor::OutputConfig;
use tilo_network_actor::private_messages;
use tilo_service::{LoopWake, Service};
use tilo_session::messages::{MESSAGES, Messages};
use tilo_session::sync::{Client, OwnPresence};
use tilo_session::{Options, Session};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

/// How long the two devices have to carry what they carry.
const DEADLINE: Duration = Duration::from_secs(60);
/// One turn of the receiving session's loop.
const PASS: Duration = Duration::from_millis(20);

/// A person's root key.
fn person(seed: u8) -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
}

/// One of a person's devices: its certificate, and the identity a session
/// runs its endpoint on.
fn certified(
    person: &ed25519_dalek::SigningKey,
    seed: u8,
    label: &str,
) -> tilo_identity_host::custody::HostIdentity {
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, person).expect("the person signs their own device");
    tilo_identity_host::custody::HostIdentity {
        device_cert: cert,
        device_signing_key: key,
        workspace_device_certs: HashMap::new(),
        workspace_revocations: HashMap::new(),
        identity_paths: None,
    }
}

/// The other person's device, on an endpoint of its own, with the private
/// texts it sends and keeps published as its own service.
struct Other {
    client: Client,
    messages: Messages,
}

impl Other {
    /// Start the device and take the lane its private texts travel on.
    fn start(identity: tilo_identity_host::custody::HostIdentity) -> Self {
        let client = Client::start(
            Some(identity.clone()),
            None,
            OwnPresence::default(),
            Vec::new(),
            tilo_session::fabric::MediaContinuity::default(),
            Arc::new(|| {}),
        )
        .expect("an endpoint binds");
        let messages = Messages::new(&LoopWake::default()).expect("private messages start");
        messages.act_as(identity, hex::encode(client.publisher().as_bytes()));
        let transport: private_messages::Transport = client
            .private_messages()
            .expect("the endpoint carries private texts")
            .blocking_recv()
            .expect("the endpoint answers");
        messages.bind(transport).expect("this device answers peers");
        Self { client, messages }
    }

    /// Send `text` to the person `at` as the action does.
    fn say(&mut self, at: &Answer, text: &str) {
        let arguments = serde_json::json!({
            "peer": at.address,
            "person": at.person,
            "text": text,
        })
        .as_object()
        .cloned()
        .expect("an object");
        Service::invoke(&mut self.messages, MESSAGES, "send", &arguments)
            .expect("the text is sent");
    }

    /// Whether the conversation with `root` holds `text` now.
    fn holds(&self, root: &str, text: &str) -> bool {
        let reading = Service::read(&self.messages, MESSAGES).expect("the conversations");
        conversation_holds(&reading, root, text)
    }
}

/// What the `messages` resource reads: who this device is for private texts,
/// and where it is reached.
#[derive(Clone, Debug)]
struct Answer {
    address: String,
    person: String,
    at: serde_json::Value,
}

/// The answer of the session's own `messages` resource.
fn answer(reading: &serde_json::Value) -> Answer {
    Answer {
        address: reading["address"].as_str().unwrap_or_default().to_owned(),
        person: reading["person"].as_str().unwrap_or_default().to_owned(),
        at: reading["at"].clone(),
    }
}

/// Whether the conversations `reading` holds a line `text` with `root`.
fn conversation_holds(reading: &serde_json::Value, root: &str, text: &str) -> bool {
    reading["conversations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|conversation| conversation["person"].as_str() == Some(root))
        .any(|conversation| {
            conversation["texts"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|line| line["body"].as_str() == Some(text))
        })
}

/// A session whose person has an identity of their own on this machine, so
/// its endpoint runs on a certified device of theirs.
fn this_computers_session(home: &mut tilo_test_env::TestHome) -> (Session, tempfile::TempDir) {
    let root = tempfile::tempdir().expect("a directory for this computer's identity");
    let identity_dir = home.config().join("tilo").join("identity");
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &identity_dir,
        &tilo_user_identity::IdentityPaths::under(root.path()),
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .expect("this computer's identity");
    home.env().set("TILO_IDENTITY_ROOT", root.path());
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: true,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
    )
    .expect("session");
    (session, root)
}

/// The session's own `messages` reading, once the endpoint is bound.
#[track_caller]
fn reachable(session: &mut Session) -> Answer {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(reading) = session.read(MESSAGES) {
            let answer = answer(&reading);
            if !answer.address.is_empty() && !answer.person.is_empty() {
                return answer;
            }
        }
        assert!(Instant::now() < deadline, "the endpoint binds");
        session.step(PASS).expect("the session runs");
    }
}

#[test]
fn a_private_message_reaches_the_person_it_was_sent_to() {
    let mut home = tilo_test_env::TestHome::new().expect("a home of this machine's own");
    let (mut session, _identity) = this_computers_session(&mut home);
    let theirs = reachable(&mut session);

    // The other person at another computer of their own.
    let them = person(21);
    let mut other = Other::start(certified(&them, 22, "Their laptop"));
    let other_computer: iroh::EndpointAddr =
        serde_json::from_value(theirs.at.clone()).expect("the other computer's address");
    // The other computer learns where this one is, as the person's routes
    // tell it.
    other.client.connect(other_computer);

    let written = "See you at seven?";
    other.say(&theirs, written);
    // A conversation is kept under the person at the other end: here, them.
    let them_root = hex::encode(them.verifying_key().to_bytes());
    let ours_root = theirs.person.clone();

    let deadline = Instant::now() + DEADLINE;
    let mut arrived = false;
    while Instant::now() < deadline {
        session.step(PASS).expect("the session runs");
        if session
            .read(MESSAGES)
            .is_some_and(|reading| conversation_holds(&reading, &them_root, written))
        {
            arrived = true;
            break;
        }
    }
    assert!(arrived, "the text reached the person it was sent to");

    // And the person here answers on the same action: it reaches the other
    // computer, and this one keeps what it said.
    let reply = "Seven it is";
    let arguments = serde_json::json!({
        "peer": hex::encode(other.client.publisher().as_bytes()),
        "person": hex::encode(them.verifying_key().to_bytes()),
        "text": reply,
    })
    .as_object()
    .cloned()
    .expect("an object");
    session
        .invoke(MESSAGES, "send", &arguments)
        .expect("the reply is sent");

    let deadline = Instant::now() + DEADLINE;
    let mut landed = false;
    let mut kept = false;
    while Instant::now() < deadline && !(landed && kept) {
        other.client.follow();
        session.step(PASS).expect("the session runs");
        landed |= other.holds(&ours_root, reply);
        kept |= session
            .read(MESSAGES)
            .is_some_and(|reading| conversation_holds(&reading, &them_root, reply));
    }
    assert!(landed, "the reply reached the person it was sent to");
    assert!(kept, "this computer keeps what it said");
}
