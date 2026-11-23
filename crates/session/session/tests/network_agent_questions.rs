//! An agent working on the person's workspace host asks them to allow one action, and the session
//! answers with what they chose.
//! The host is this test's own endpoint, speaking the certification protocol's approver side: it
//! lists pending confirmations and keeps each answer the session sends. The session is a
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
use tilo_device_identity::IdentityPaths;
use tilo_identity::device_fleet::DeviceFleetStore;
use tilo_identity::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_server_client::frame::{read_frame, write_frame};
use tilo_server_protocol::auth::{self, HandshakeTrust, ServerIdentity};
use tilo_server_protocol::certify::CERTIFY_ALPN;
use tilo_server_protocol::certify::{
    AgentAsk, ApproverReply, ApproverRequest, CertifyOpen, ConfirmationAnswer,
    MAX_CERTIFY_FRAME_BYTES, PendingConfirmation, TypedSecret,
};
use tilo_service::Questions;
use tilo_session::Options;
use tilo_sheets::SheetState as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};

/// The person's identity and this device's certificate, in a directory of its own, as the desktop's
/// own start reads it.
struct Person {
    cert: DeviceCert,
    // The person's home server: the host the questions come from.
    server_cert: DeviceCert,
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
    tilo_identity::provisioning::ensure_collaboration_identity(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let registry = tilo_identity::store::IdentityRegistryStore::from_env()
        .expect("the identity registry")
        .load_reconciled_cached()
        .expect("the registry reads");
    let profile = registry
        .certified_identity()
        .expect("this device's person is known");
    let cert = tilo_device_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this device's certificate");
    let store = DeviceFleetStore::from_env().expect("the device fleet");
    let now = tilo_clock::unix_now_ns();
    let host_key = ed25519_dalek::SigningKey::from_bytes(&[0x52; 32]);
    // The person's own root signs the device, through the store whose copy the fleet names.
    let server_cert = store
        .certify_device(
            &profile.id,
            host_key.verifying_key().to_bytes(),
            "The home server",
            DeviceCapabilities::member(),
            now,
        )
        .expect("the second device is enrolled");
    Person {
        cert,
        server_cert,
        host_key,
        env,
        _root: root,
    }
}

/// One question the host puts to the person's devices.
fn question(id: u8, allow_always: bool) -> PendingConfirmation {
    PendingConfirmation {
        id: [id; 32],
        agent_id: "careful".into(),
        agent_label: "Careful".into(),
        title: "Tidy the shell".into(),
        action_label: "Clear the history".into(),
        arguments: vec![("Entries".into(), "All".into())],
        resource_label: "Calculator".into(),
        risk: "destructive".into(),
        allow_always,
        workspace: "The shell".into(),
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(60_000_000_000),
        ..PendingConfirmation::default()
    }
}

/// The host asking for a mail account's password, in its own words, for the person to type into
/// the system's protected field.
fn password_question(id: u8) -> PendingConfirmation {
    PendingConfirmation {
        id: [id; 32],
        agent_id: "scout".into(),
        agent_label: "Scout".into(),
        workspace: "Ada's workspace".into(),
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(60_000_000_000),
        ask: Some(AgentAsk {
            // The words the server asks in for a mail account, which the prompt shows whole.
            question: "Sign in to ada@example.test at imap.example.test and smtp.example.test. \
                       Scout never sees what you type: your vault keeps it for those two servers \
                       alone."
                .into(),
            choices: Vec::new(),
            secret: true,
        }),
        ..PendingConfirmation::default()
    }
}

/// What the person types into the protected field.
const TYPED: &str = "only her mail servers read this";

/// Where this test's captures are written: in the build's own directory, beside its binary.
fn shots_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("SHELL_UI_SHOTS_OUT") {
        return dir.into();
    }
    let binary = std::env::current_exe().expect("the running test's path");
    binary
        .ancestors()
        .nth(3)
        .expect("the build's directory")
        .join("shell-ui-shots")
}

#[test]
fn an_agents_question_reaches_the_person_and_its_answer_the_agent() {
    let mut person = person();
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let asked = Arc::new(Mutex::new(Vec::<ConfirmationAnswer>::new()));
    let questions = Questions::new();

    // The host answers the person's approver as a server serves it: the certification handshake,
    // then questions listed one at a time until each is answered.
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
                    cert: person.server_cert.clone(),
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

    for (index, (id, expected)) in [
        (0x21, ConfirmationAnswer::AllowOnce),
        (0x22, ConfirmationAnswer::AllowAlways),
        (
            0x23,
            ConfirmationAnswer::Secret(TypedSecret::new(TYPED.to_owned())),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let allow_always = expected == ConfirmationAnswer::AllowAlways;
        let handle = format!("agent-question:{}", hex::encode([id; 32]));
        let deadline = tilo_test_env::hang_deadline();
        let shown = loop {
            if let Some(shown) = questions.pending().into_iter().find(|question| {
                question.kind == "network.agent-question" && question.handle == handle
            }) {
                break shown;
            }
            session
                .step(Duration::from_millis(20))
                .expect("the session steps");
            assert!(
                Instant::now() < deadline,
                "the agent's question {handle} reached the person"
            );
        };
        if let ConfirmationAnswer::Secret(_) = &expected {
            // A password: the sheet is the system's protected field, in the host's words, and
            // what is typed stays in the sheet's own buffers until Sign in.
            assert_eq!(shown.label, "Scout");
            assert_eq!(shown.details["ask"]["secret"], true);
            let deadline = tilo_test_env::hang_deadline();
            while !session
                .portal()
                .is_some_and(|sheet| sheet.presentation().is_open())
            {
                session
                    .step(Duration::from_millis(20))
                    .expect("the session shows the sheet");
                assert!(Instant::now() < deadline, "the password sheet opened");
            }
            let dialog = session
                .portal()
                .and_then(tilo_sheets::portal::PortalState::dialog)
                .expect("the sheet asks");
            let tilo_sheets::portal::Body::Secret(prompt) = &dialog.body else {
                panic!(
                    "a password is asked in the protected field: {:?}",
                    dialog.body
                );
            };
            assert!(prompt.fields.iter().all(|field| field.masked));
            assert!(prompt.resource.contains("ada@example.test"), "{prompt:?}");
            assert!(dialog.title.contains("Scout"), "{}", dialog.title);
            for _ in 0..10 {
                session
                    .step(Duration::from_millis(20))
                    .expect("the session draws the sheet");
            }
            let dir = shots_dir();
            std::fs::create_dir_all(&dir).expect("the shots directory");
            let shot = session
                .compositor()
                .screenshot("HEADLESS-1")
                .expect("a frame of the sheet");
            image::save_buffer(
                dir.join("agent-password-ask.png"),
                &shot.rgba,
                shot.width,
                shot.height,
                image::ColorType::Rgba8,
            )
            .expect("the frame is written");
            let labels = session.accessible_labels().join("\n");
            assert!(
                !labels.contains(TYPED),
                "nothing typed is in the accessibility tree"
            );
            let sheet = session.portal().expect("the sheet stays open");
            sheet.give_answer(ui::widget::SecretAnswer {
                username: None,
                secret: zeroize::Zeroizing::new(TYPED.to_owned()),
                new_pin: None,
                values: vec![zeroize::Zeroizing::new(TYPED.to_owned())],
                choice: None,
                option: None,
            });
            sheet.press(tilo_sheets::portal::PortalEffect::Submit { handle });
        } else {
            assert_eq!(shown.label, "Careful");
            let details = shown.details;
            assert_eq!(details["agent"], "Careful");
            assert_eq!(details["action"], "Clear the history");
            assert_eq!(details["title"], "Tidy the shell");
            assert_eq!(
                details["arguments"],
                serde_json::json!([["Entries", "All"]])
            );
            assert_eq!(details["resource"], "Calculator");
            assert_eq!(details["workspace"], "The shell");
            assert_eq!(details["risk"], "destructive");
            assert_eq!(details["allow_always"], allow_always);
            if allow_always {
                session
                    .step(Duration::from_millis(20))
                    .expect("the session shows the sheet");
                let sheet = session.portal().expect("the system approval sheet");
                assert!(sheet.presentation().is_open());
                sheet.press(tilo_sheets::portal::PortalEffect::Answer {
                    handle,
                    index: u32::try_from(tilo_sheets::portal::ALLOW_ALWAYS).unwrap(),
                });
            } else {
                assert!(
                    questions.answer(&handle, tilo_service::Answer::default()),
                    "the person answered the question"
                );
            }
        }

        let deadline = tilo_test_env::hang_deadline();
        while asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
            <= index
        {
            session
                .step(Duration::from_millis(20))
                .expect("the session steps");
            assert!(
                Instant::now() < deadline,
                "the answer reached the agent's server"
            );
        }
        assert_eq!(
            asked
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)[index],
            expected
        );
    }
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
/// listing its questions in order and keeping each answer.
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
    let questions = [
        question(0x21, false),
        question(0x22, true),
        password_question(0x23),
    ];
    while let Ok(frame) = read_frame(&mut recv, MAX_CERTIFY_FRAME_BYTES).await {
        let Ok(request) = postcard::from_bytes::<ApproverRequest>(&frame) else {
            return;
        };
        let reply = match request {
            ApproverRequest::Confirmations => {
                let answered = asked
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .len();
                let next = questions.get(answered).cloned().into_iter().collect();
                ApproverReply::Confirmations(next)
            }
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
        let Ok(reply) = reply.encode() else {
            return;
        };
        if write_frame(&mut send, &reply).await.is_err() {
            return;
        }
    }
}
