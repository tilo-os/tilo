//! An agent working on the person's workspace host asks them to allow one action, and the session
//! answers with what they chose.
//! The host is this test's own endpoint, speaking the certification protocol's approver side: it
//! lists one pending confirmation and records the answer the session sends. The session is a
//! headless one, started the way the desktop starts it, so the question goes through everything
//! that runs in between: the actor's approver session, the session's question sheet, and the
//! person's answer back to the host.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::endpoint::{Connection, Endpoint};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{EndpointAddr, RelayMode};
use tilo_compositor::OutputConfig;
use tilo_identity_host::device_fleet::DeviceFleetStore;
use tilo_identity_host::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_relay_client::frame::{read_frame, write_frame};
use tilo_relay_protocol::auth::{self, HandshakeTrust, ServerIdentity};
use tilo_relay_protocol::certify::CERTIFY_ALPN;
use tilo_relay_protocol::certify::{
    ApproverReply, ApproverRequest, CertifyOpen, ConfirmationAnswer, MAX_CERTIFY_FRAME_BYTES,
    PendingConfirmation,
};
use tilo_service::Questions;
use tilo_session::Options;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, IdentityPaths, RevocationList};

/// The person's identity and this device's certificate, in a directory of its own, as the desktop's
/// own start reads it.
struct Person {
    cert: DeviceCert,
    // The person's home server: the host the questions come from.
    host_cert: DeviceCert,
    host_key: ed25519_dalek::SigningKey,
    env: tilo_test_env::ScopedEnv,
    _root: tempfile::TempDir,
}

fn person() -> Person {
    let root = tempfile::tempdir().expect("a directory for the identity");
    let xdg = root.path().join("config");
    let system = root.path().join("system");
    let runtime = root.path().join("run");
    std::fs::create_dir_all(&runtime).expect("a runtime directory");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", &xdg)
        .set("TILO_IDENTITY_ROOT", &system)
        .set("XDG_RUNTIME_DIR", &runtime)
        .set(FILE_SECRETS_ENV, "1");
    let personal = xdg.join("tilo/identity");
    let paths = IdentityPaths::under(&system);
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let registry = tilo_identity_host::registry::IdentityRegistryStore::from_env()
        .expect("the identity registry")
        .load_reconciled_cached()
        .expect("the registry reads");
    let profile = registry
        .certified_identity()
        .expect("this device's person is known");
    let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this device's certificate");
    let store = DeviceFleetStore::from_env().expect("the device fleet");
    let now = tilo_clock::unix_now_ns();
    let host_key = ed25519_dalek::SigningKey::from_bytes(&[0x52; 32]);
    // The person's own root signs the device, through the store whose copy the fleet names.
    let token = store
        .mint_enrollment(
            &profile.id,
            DeviceCapabilities::host(),
            now,
            now.saturating_add(3_600_000_000_000),
        )
        .expect("this device may enroll another of the person's");
    let host_cert = store
        .complete_enrollment(
            &profile.id,
            &token,
            tilo_identity_host::enrollment::EnrollmentRole::HomePeer,
            host_key.verifying_key().to_bytes(),
            "The home server",
            now,
        )
        .expect("the second device is enrolled");
    Person {
        cert,
        host_cert,
        host_key,
        env,
        _root: root,
    }
}

/// One question the host puts to the person's devices.
fn question() -> PendingConfirmation {
    PendingConfirmation {
        id: [0x21; 32],
        agent_id: "careful".into(),
        agent_label: "Careful".into(),
        run: "Tidy the desk".into(),
        action_label: "Clear the history".into(),
        arguments: vec![("Entries".into(), "All".into())],
        resource_label: "Calculator".into(),
        risk: "destructive".into(),
        workspace: "The desk".into(),
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(60_000_000_000),
        ..PendingConfirmation::default()
    }
}

#[test]
fn an_agents_question_reaches_the_person_and_its_answer_the_agent() {
    let mut person = person();
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let asked = Arc::new(Mutex::new(Vec::<ConfirmationAnswer>::new()));
    let questions = Questions::new();

    // The host answers the person's approver as a relay serves it: the certification handshake,
    // then one listed question until it is answered.
    let (router, host_addr) = runtime.block_on(async {
        let endpoint = Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback address")
            .bind()
            .await
            .expect("the host's endpoint binds");
        let port = endpoint
            .bound_sockets()
            .first()
            .expect("a bound socket")
            .port();
        let addr = EndpointAddr::new(endpoint.id())
            .with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
        let router = Router::builder(endpoint)
            .accept(
                CERTIFY_ALPN,
                Asking {
                    user: person.cert.user_public_key,
                    cert: person.host_cert.clone(),
                    key: person.host_key.clone(),
                    asked: Arc::clone(&asked),
                },
            )
            .spawn();
        (router, addr)
    });
    let _router = router;

    // The session dials the host it was configured with, as the launcher's configuration names it,
    // and asks its person on the sheet it was given.
    person.env.set(
        "TILO_COLLAB_HOSTS",
        serde_json::to_string(&vec![host_addr]).expect("the host's address encodes"),
    );
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
        Options {
            collab: true,
            questions: Some(questions.clone()),
            ..Options::default()
        },
    );

    let deadline = tilo_test_env::hang_deadline();
    let shown = loop {
        if let Some(shown) = questions
            .pending()
            .into_iter()
            .find(|question| question.kind == "network.agent-question")
        {
            break shown;
        }
        session
            .step(Duration::from_millis(20))
            .expect("the session steps");
        assert!(
            Instant::now() < deadline,
            "the agent's question reached the person"
        );
    };
    assert_eq!(shown.label, "Careful");
    let details: serde_json::Value = shown.details.clone();
    assert_eq!(details["agent"], "Careful");
    assert_eq!(details["action"], "Clear the history");
    assert_eq!(details["run"], "Tidy the desk");
    assert_eq!(
        details["arguments"],
        serde_json::json!([["Entries", "All"]])
    );
    assert_eq!(details["resource"], "Calculator");
    assert_eq!(details["workspace"], "The desk");
    assert_eq!(details["risk"], "destructive");

    assert!(
        questions.answer(&shown.handle, tilo_service::Answer::default()),
        "the person answered the question"
    );

    let deadline = tilo_test_env::hang_deadline();
    while asked
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_empty()
    {
        session
            .step(Duration::from_millis(20))
            .expect("the session steps");
        assert!(
            Instant::now() < deadline,
            "the answer reached the agent's host"
        );
    }
    assert_eq!(
        asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[0],
        ConfirmationAnswer::AllowOnce
    );
}

/// The person's workspace host, as far as the certification protocol sees it: it answers approver
/// sessions with one question.
#[derive(Debug)]
struct Asking {
    user: [u8; 32],
    cert: DeviceCert,
    key: ed25519_dalek::SigningKey,
    asked: Arc<Mutex<Vec<ConfirmationAnswer>>>,
}

impl ProtocolHandler for Asking {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        serve_approver(
            connection,
            self.user,
            self.cert.clone(),
            self.key.clone(),
            Arc::clone(&self.asked),
        )
        .await;
        Ok(())
    }
}

/// Serve the approver side of the certification protocol for one connection,
/// listing one question and recording what the person answers.
async fn serve_approver(
    connection: Connection,
    user: [u8; 32],
    cert: DeviceCert,
    key: ed25519_dalek::SigningKey,
    asked: Arc<Mutex<Vec<ConfirmationAnswer>>>,
) {
    let Some((mut send, mut recv)) = connection.accept_bi().await.ok() else {
        return;
    };
    let Some(frame) = read_frame(&mut recv, MAX_CERTIFY_FRAME_BYTES).await.ok() else {
        return;
    };
    let Ok(CertifyOpen::Approver(hello)) = postcard::from_bytes::<CertifyOpen>(&frame) else {
        return;
    };
    let identity = ServerIdentity {
        own_user: &user,
        cert: &cert,
        key: &key,
    };
    let Ok((pending, ack)) = auth::server_begin(
        &identity,
        HandshakeTrust::OwnerDeviceAdmitters,
        &RevocationList::new(),
        &hello,
        tilo_clock::unix_now_ns(),
        [0x5a; 32],
    ) else {
        return;
    };
    let write_ack = |ack: &auth::HelloAck| postcard::to_allocvec(ack);
    let Ok(ack) = write_ack(&ack) else {
        return;
    };
    if write_frame(&mut send, &ack).await.is_err() {
        return;
    }
    let Some(frame) = read_frame(&mut recv, MAX_CERTIFY_FRAME_BYTES).await.ok() else {
        return;
    };
    let Ok(ack2) = postcard::from_bytes::<auth::HelloAck2>(&frame) else {
        return;
    };
    if pending.finish(&ack2).is_err() {
        return;
    }
    let question = question();
    while let Ok(frame) = read_frame(&mut recv, MAX_CERTIFY_FRAME_BYTES).await {
        let Ok(request) = postcard::from_bytes::<ApproverRequest>(&frame) else {
            return;
        };
        let reply = match request {
            ApproverRequest::Confirmations => ApproverReply::Confirmations(vec![question.clone()]),
            ApproverRequest::Answer { answer, .. } => {
                asked
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(answer);
                ApproverReply::Decided
            }
            _ => ApproverReply::Refused {
                reason: "this host asks nothing else".into(),
            },
        };
        let Ok(reply) = postcard::to_allocvec(&reply) else {
            return;
        };
        if write_frame(&mut send, &reply).await.is_err() {
            return;
        }
    }
}
