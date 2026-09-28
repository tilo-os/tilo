//! Saying something about an app, end to end over the production broker.
//!
//! Mail runs as a real app connection in a room; Chat runs in the direct
//! room this person shares with the Tilo team. Everything goes through the
//! shell's own `remark.*` actions, as tilo-control or an agent drives them:
//! pick Mail's Subject field, record pressing Send until Mail shows why it
//! failed, review what will leave (hiding one more word by hand), and send
//! it privately. The team's room then holds a remark about that field with
//! the redacted recording attached, and neither the remark nor the recording
//! carries the draft's personal recipient or the hidden word.
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use std::os::fd::IntoRawFd as _;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use automerge::AutoCommit;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Value, json};
use shell::compositor::foreign;
use shell::compositor::frame::FrameBuilder;
use shell::compositor::tab_binding;
use shell::shell::remark::{Directory, fixture};
use shell::shell::state::{RoomId, Workspace, WorkspaceId, WorkspaceTree};

const MAIL_ROOM: uuid::Uuid = uuid::Uuid::from_u128(0x4e3a_4c00_0000_4c0d_9a11_0000_0000_0051);
const SURFACE: u64 = 0x4E3A_4C00_0051;
const CHAT: &str = "so.tilo.chat";
const MAIL_VIEWPORT: (u32, u32) = (720, 480);
/// A recipient Mail declares personal: it must not leave the device.
const PERSONAL: &str = "Lovelace";
/// A word the person hides by hand in review: the invoice's month.
const HIDDEN: &str = "March";

struct Person {
    root: SigningKey,
    device: SigningKey,
}

impl Person {
    fn new(root: u8, device: u8) -> Self {
        Self {
            root: SigningKey::from_bytes(&[root; 32]),
            device: SigningKey::from_bytes(&[device; 32]),
        }
    }

    fn principal(&self) -> String {
        let collab_types::Principal::Person(principal) = collab_types::Principal::person_from_key(
            collab_types::Ed25519PublicKey::from_bytes(self.root.verifying_key().to_bytes()),
        ) else {
            unreachable!("a person key names a person")
        };
        principal
    }

    fn certificate(&self) -> tilo_user_identity::DeviceCert {
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: self.root.verifying_key().to_bytes(),
            device_public_key: self.device.verifying_key().to_bytes(),
            device_label: "laptop".to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &self.root)
            .expect("root signs the device");
        certificate
    }

    /// This device's identity where the session reads it, as first boot
    /// leaves it.
    fn provision(&self) {
        let paths = shell::firstboot::collaboration_identity_paths();
        for path in [
            &paths.user_pubkey,
            &paths.device_privkey,
            &paths.device_cert,
        ] {
            std::fs::create_dir_all(path.parent().expect("a directory")).expect("identity dirs");
        }
        let persist = |result: std::io::Result<()>| result.expect("write the identity");
        persist(tilo_user_identity::persist::write_public_key(
            &paths.user_pubkey,
            &self.root.verifying_key().to_bytes(),
        ));
        persist(tilo_user_identity::persist::write_device_key(
            &paths.device_privkey,
            &self.device,
        ));
        tilo_user_identity::persist::write_device_cert(&paths.device_cert, &self.certificate())
            .expect("write the device certificate");
    }

    fn identity(&self, room: &str) -> ui::collaboration::CertifiedEndpointIdentity {
        let certificate = self.certificate();
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.to_owned(),
            device_certificate: serde_json::to_vec(&certificate).expect("certificate JSON"),
            proof: self
                .device
                .sign(&runtime_identity::collaboration_join_signing_bytes(room))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .expect("certified join encodes");
        ui::collaboration::CertifiedEndpointIdentity::new(presentation, self.device.clone())
    }

    fn connect(
        &self,
        socket: &Path,
        app: &str,
        room: &str,
    ) -> ui::collaboration::CollaborationManager {
        let stream = UnixStream::connect(socket).expect("connect to the broker");
        ui::collaboration::CollaborationManager::for_route_fd_with_identity(
            stream.into_raw_fd(),
            app,
            uuid::Uuid::parse_str(room).expect("a room is a uuid"),
            None,
            Some("laptop".to_owned()),
            self.identity(room),
        )
        .expect("start the collaboration client")
    }
}

/// The desktop: Mail's window in its room, focused, and the direct room
/// with the team, both this person's.
fn desktop(me: &str, direct_room: &str) -> FrameBuilder {
    let mut protocol = WorkspaceTree::bootstrap_demo();
    tab_binding::adopt_toplevel(&mut protocol, SURFACE, Some(fixture::APP.to_string()));
    let mut fb = FrameBuilder::new((1280, 800));
    {
        let chrome = fb.chrome();
        let mut rendered = chrome.tree_mut();
        assert!(foreign::reconcile_foreign_tabs(&protocol, &mut rendered));
        let (index, strip, group) = rendered
            .desktop
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(index, workspace)| {
                workspace.strips.iter().flat_map(move |strip| {
                    strip
                        .window_groups
                        .iter()
                        .map(move |group| (index, strip.id, group))
                })
            })
            .find(|(_, _, group)| {
                group.tabs.iter().any(|tab| {
                    matches!(
                        &tab.tile,
                        shell::shell::state::TileContent::Foreign { surface_id, .. }
                            if *surface_id == SURFACE
                    )
                })
            })
            .map(|(index, strip, group)| (index, strip, group.id))
            .expect("Mail's group is in the rendered tree");
        rendered.desktop.set_active_group(strip, group);
        let workspace = &mut rendered.desktop.workspaces[index];
        workspace.room = RoomId::parse(&MAIL_ROOM.to_string()).expect("room id");
        workspace.set_owner(me.to_string());
        rendered.desktop.active = workspace.id;
        let mut direct = Workspace::new(WorkspaceId(0x7ea7), "Tilo team");
        direct.room = RoomId::parse(direct_room).expect("a direct room is a uuid");
        direct.set_owner(me.to_string());
        rendered.desktop.workspaces.push(direct);
    }
    fb.with_chrome(shell::shell::chrome_state::ChromeState::bump);
    fb
}

struct World {
    fb: FrameBuilder,
    mail: ui::App,
    chat: ui::App,
}

impl World {
    /// One frame of the shell and one turn of each app.
    fn step(&mut self) {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        self.fb.pump_apps(&mut doc, &root);
        let _ = self.fb.paint_settled();
        // Mail draws, as a window would; Chat only needs to keep up.
        let mut scene = vello::Scene::new();
        self.mail
            .render_frame_headless(&mut scene, MAIL_VIEWPORT.0, MAIL_VIEWPORT.1, 1.0);
        self.chat.step_headless();
        std::thread::sleep(Duration::from_millis(5));
    }

    fn until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let started = Instant::now();
        while !done(self) {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "timed out waiting for {what}: {:#}",
                self.status()
            );
            self.step();
        }
    }

    /// Run a shell action as tilo-control does.
    fn act(&mut self, id: &str, args: &Value) {
        let args = args.as_object().cloned().unwrap_or_default();
        let mut outcome = None;
        self.fb.with_chrome(|st| {
            outcome = Some(shell::shell::search::providers::actions::apply(
                st, id, &args,
            ));
        });
        if let Some(Err(error)) = outcome {
            panic!("{id} refused: {error:?}\n{:#}", self.status());
        }
        self.step();
    }

    fn status(&self) -> Value {
        shell::shell::remark::status_json(&self.fb.chrome())
    }
}

/// Where Mail's control labelled `label` is, in Mail's own surface, as
/// pick mode sees it.
fn locate(status: &Value, label: &str) -> (f64, f64) {
    status["pick"]["surfaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|surface| surface["app"] == fixture::APP)
        .find_map(|surface| {
            let origin = &surface["screen"];
            surface["nodes"]
                .as_array()?
                .iter()
                .find(|node| node["role"] == "button" && node["label"] == label)
                .map(|node| {
                    let at = &node["screen"];
                    let number = |value: &Value| value.as_f64().unwrap_or_default();
                    (
                        number(&at["x"]) - number(&origin["x"]) + number(&at["width"]) / 2.0,
                        number(&at["y"]) - number(&origin["y"]) + number(&at["height"]) / 2.0,
                    )
                })
        })
        .unwrap_or_else(|| panic!("Mail shows {label}: {status:#}"))
}

/// Click Mail at `(x, y)` of its surface, as the person at the device does.
fn click(world: &mut World, (x, y): (f64, f64)) {
    let device = winit::event::DeviceId::dummy();
    for event in [
        winit::event::WindowEvent::CursorMoved {
            device_id: device,
            position: winit::dpi::PhysicalPosition::new(x, y),
        },
        winit::event::WindowEvent::MouseInput {
            device_id: device,
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
        },
        winit::event::WindowEvent::MouseInput {
            device_id: device,
            state: winit::event::ElementState::Released,
            button: winit::event::MouseButton::Left,
        },
    ] {
        world.mail.handle_event_headless(&event, 1.0);
    }
    world.step();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one person's remark from pick to delivery, read top to bottom"
)]
fn a_remark_about_a_field_with_a_redacted_recording_reaches_the_team_privately() {
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let me = Person::new(51, 52);
    let team = Person::new(61, 62);
    let identity_root = home.root().join("identity");
    home.env()
        .set("TILO_USER_ID", me.principal())
        .set("TILO_IDENTITY_ROOT", &identity_root);
    me.provision();
    let direct = tilo_reference::DirectRoom::between(&me.principal(), &team.principal())
        .expect("two people share a direct room");
    let direct_room = direct.room_id();

    foreign::store().forget(SURFACE);
    let mut fb = desktop(&me.principal(), &direct_room);
    // The session's own durable records, where the redacted recording is kept.
    fb.with_chrome(|st| st.set_records(shell::shell::session_record_store()));
    let socket = home.runtime().join("collab.sock");
    let broker = shell::shell::collab_broker::spawn_native_harness(
        socket.clone(),
        fb.chrome().tree_handle(),
        vec![me.device.verifying_key().to_bytes()],
    )
    .expect("bind the broker");

    let mail = ui::App::with_collaboration_manager(
        ui::State::new(),
        Box::new(fixture::mail_window()),
        me.connect(&socket, fixture::APP, &MAIL_ROOM.to_string()),
    );
    let comments = ui::record::RecordOwners::new(home.state().join("chat"))
        .open(CHAT, &direct_room)
        .expect("a record store in the direct room");
    let conversation = chat::ChatApp::for_participant(&me.principal(), "me", comments);
    let mut chat = ui::App::with_collaboration_manager(
        ui::State::new(),
        Box::new(conversation),
        me.connect(&socket, CHAT, &direct_room),
    );
    chat::actions::register(chat.action_registry_mut());
    let chat = chat.with_records(
        ui::RecordSet::in_memory().with_home(direct_room),
        AutoCommit::new(),
    );
    let mut world = World { fb, mail, chat };
    world.fb.host_mut().attach_collab_broker(&broker);
    world.fb.host_mut().seed_remark_directory(Directory {
        team: Some((team.principal(), "Tilo team".into())),
        community: None,
        people: Vec::new(),
        rooms: Vec::new(),
    });
    world.until("the conversation to exist", |world| {
        chat::doc::ChatDoc::is_seeded(&world.chat.state_mut_for_test().doc)
    });

    // Pick Mail's Subject field in pick mode.
    world.act("remark.open", &json!({}));
    world.act("remark.pick.start", &json!({}));
    let mut subject = None;
    world.until("pick mode to see Mail's fields", |world| {
        let status = world.status();
        subject = status["pick"]["surfaces"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|surface| surface["app"] == fixture::APP)
            .find_map(|surface| {
                surface["nodes"]
                    .as_array()?
                    .iter()
                    .find(|node| node["id"] == "t:subject")
                    .inspect(|node| {
                        assert_eq!(node["label"], "Subject", "a field goes by its name");
                    })
                    .map(|node| (surface["surface"].clone(), node["id"].clone()))
            });
        subject.is_some()
    });
    let (surface, node) = subject.expect("found");
    let send_button = locate(&world.status(), "Send");
    world.act(
        "remark.pick.node",
        &json!({ "surface": surface, "node": node }),
    );
    world.act("remark.pick.done", &json!({}));
    assert_eq!(world.status()["stage"], "compose", "{:#}", world.status());
    world.act(
        "remark.text.set",
        &json!({ "text": "Send fails with an attachment and only says why afterwards" }),
    );

    // Record pressing Send until Mail says it failed.
    world.act("remark.record.start", &json!({}));
    let recorder = world
        .fb
        .host()
        .session_services()
        .recorder()
        .cloned()
        .expect("a recorder");
    world.until("the recorder to observe Mail", |_| {
        recorder.status_json()["recording"]["events"]
            .as_u64()
            .is_some_and(|events| events > 0)
    });
    let events = |recorder: &shell::shell::recorder::Recorder| {
        recorder.status_json()["recording"]["events"]
            .as_u64()
            .unwrap_or(0)
    };
    let before = events(&recorder);
    click(&mut world, send_button);
    // The press, the banner it brought up, and the keyframe showing it.
    world.until("the recording to see the press and the banner", |_| {
        events(&recorder) >= before + 3
    });
    world.act("remark.record.stop", &json!({}));

    // Review: the walk-through ends where Mail said why, and one more
    // word is hidden by hand.
    world.act("remark.review.open", &json!({}));
    world.until("review to walk through the recording", |world| {
        world.status()["review"]["steps"]
            .as_array()
            .is_some_and(|steps| !steps.is_empty())
    });
    let review = world.status()["review"].clone();
    let steps = review["steps"].as_array().cloned().unwrap_or_default();
    assert!(
        steps.iter().any(|step| step["title"]
            .as_str()
            .is_some_and(|title| title.contains("Send"))),
        "pressing Send is a step: {review:#}"
    );
    let (step, body, at) = steps
        .iter()
        .find_map(|step| {
            step["texts"].as_array()?.iter().find_map(|text| {
                let sent = text["sent"].as_str()?;
                let at = sent.find(HIDDEN)?;
                Some((step["step"].clone(), text["node"].clone(), at))
            })
        })
        .unwrap_or_else(|| panic!("the body is reviewed: {review:#}"));
    world.act(
        "remark.review.redact-word",
        &json!({ "step": step, "node": body, "at": at }),
    );
    world.act("remark.review.close", &json!({}));

    // Send it to the team, privately.
    world.act("remark.destination.set", &json!({ "id": "team" }));
    world.act("remark.send", &json!({}));
    world.until("the remark to be delivered", |world| {
        let status = world.status();
        assert_ne!(status["sending"]["state"], "failed", "{status:#}");
        status["last"]["state"] == "sent"
    });

    // The team's room holds the remark about Subject, with the recording.
    let messages =
        chat::doc::ChatDoc::list_messages(&world.chat.state_mut_for_test().doc, "c_general");
    let remark = messages
        .iter()
        .find(|message| message.text.contains("only says why"))
        .expect("the remark is in the direct room's conversation");
    let reference = remark.reference.as_ref().expect("it says what it is about");
    assert_eq!(
        (reference.kind.as_str(), reference.id.as_str()),
        ("tilo.ui.control", "ui:t%3Asubject"),
        "the remark is about the picked field"
    );
    let Some(chat::model::Attachment::Record(attachment)) = &remark.attachment else {
        panic!("the recording is attached: {:?}", remark.attachment);
    };
    let record = ui::record::RecordId::parse(&attachment.record).expect("a record id");
    let sent = world
        .fb
        .chrome()
        .records()
        .read(&record, session_trace::read)
        .expect("the redacted recording is stored")
        .expect("it is a session");
    let sent = serde_json::to_string(&sent).expect("a session serializes");
    let remark = serde_json::to_string(&(remark.text.clone(), reference, attachment))
        .expect("the remark serializes");
    for (what, text) in [("the recording", &sent), ("the remark", &remark)] {
        assert!(
            !text.contains(PERSONAL),
            "{what} carries the personal recipient"
        );
        assert!(
            !text.contains(HIDDEN),
            "{what} carries the word hidden in review"
        );
    }
    assert!(
        sent.contains("Couldn't send"),
        "the recording still shows why it failed"
    );
    drop(broker);
}

/// The session's own record store mints records, not only opens them: a
/// remark's reviewed recording is reserved and written there.
#[test]
fn the_session_record_store_mints_a_reviewed_recording() {
    let _home = tilo_test_env::TestHome::new().expect("a test home");
    let store = shell::shell::session_record_store();
    let checkout = ui::record::RecordCheckout::new(
        ui::record::RecordId::minted(),
        ui::record::RecordKind::new(session_trace::SESSION_RECORD_KIND),
    );
    let mut reservation = store
        .reserve_record(&checkout)
        .expect("the session store reserves a record");
    let trace = fixture::sample_recording(&ui::Theme::dark()).trace;
    reservation
        .initialize(|doc| session_trace::write(doc, &trace).map_err(|error| error.to_string()))
        .expect("the recording is written");
    assert_eq!(
        store.read(&checkout.id, |doc| session_trace::read(doc).ok()),
        Some(Some(trace)),
        "the minted recording reads back"
    );
}
