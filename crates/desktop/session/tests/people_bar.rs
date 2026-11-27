//! What a bar says about the people here, as the tilolang shell draws it: someone arrives and the
//! bar says so on its own, someone waves and the bar says who, someone follows this person and
//! their face wears the mark and says so, and each goes when its moment or its following ends.
//!
//! Presence is handed to the session exactly as the transport hands it
//! (`SessionClient::heard_presence`), so what the session reads of it is the production one: the
//! session's own `session:people` and the shell's `device.people` of it, drawn on the real bar.
//! That presence crosses between two people's devices in one workspace is `chat_2x2`'s; this is
//! what the bar makes of it.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call \
              made into it"
)]

use std::rc::Rc;
use std::time::{Duration, Instant};

use collab_types::presence::{Payload, Presence};
use iroh::EndpointId;
use neighbour_wire::NeighbourMsg;
use serde_json::{Value, json};
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_session::sync::{Client, Event, OwnPresence};
use tilo_session::{Options, Session};

/// The workspace the person's shell shows.
const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";

/// The live publisher of a device from its seed.
fn publisher(seed: u8) -> EndpointId {
    iroh::SecretKey::from_bytes(&[seed; 32]).public()
}

/// A desktop's identity, heard from another device: the certificate says whose device it is, and
/// `label` is what that device calls itself.
fn hello(from: u8, label: &str) -> Event {
    let device_key = publisher(from);
    let root = ed25519_dalek::SigningKey::from_bytes(&[from; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: *device_key.as_bytes(),
        device_label: label.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities {
            can_own_workspaces: false,
            ..tilo_user_identity::DeviceCapabilities::member()
        },
        issued_at_ns: 0,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).expect("a certified device");
    Event::Msg {
        ws: WorkspaceId::parse(WORKSPACE).expect("a workspace id"),
        peer: device_key,
        msg: Box::new(NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert),
            revocations: Vec::new(),
        }),
    }
}

/// One presence the device `from` said on the workspace's live session, as its transport hands it
/// to the session.
fn presence(sync: &Client, from: u8, payload: collab_types::PeerPresencePayload) {
    let payload = match payload {
        collab_types::PeerPresencePayload::Arrived => Payload::Arrived,
        collab_types::PeerPresencePayload::Left => Payload::Left,
        collab_types::PeerPresencePayload::Status { text, automatic } => {
            Payload::Status { text, automatic }
        }
        collab_types::PeerPresencePayload::Person { name } => Payload::Person { name },
        collab_types::PeerPresencePayload::Following { target } => Payload::Following { target },
        collab_types::PeerPresencePayload::Wave { seq, target } => Payload::Wave { seq, target },
        other => panic!("no test says {other:?} as a desktop"),
    };
    sync.heard_presence(
        WORKSPACE,
        publisher(from),
        &Presence::desktop(WORKSPACE, payload),
    );
}

/// `from` arrives where this person is, under the name everyone here sees.
fn arrived_as(sync: &Client, from: u8, name: &str) {
    presence(
        sync,
        from,
        collab_types::PeerPresencePayload::Person {
            name: name.to_owned(),
        },
    );
    presence(sync, from, collab_types::PeerPresencePayload::Arrived);
}

/// A shell showing the one workspace this session takes part in.
fn shell() -> Layout {
    let workspace = Workspace::new(
        WorkspaceId::parse(WORKSPACE).expect("a workspace id"),
        "Session",
    );
    Layout {
        workspaces: vec![workspace.clone()],
        active: workspace.id(),
        ..Layout::default()
    }
}

/// A private home for the settings the shell reads its style from.
fn settings_home() -> (tempfile::TempDir, tilo_test_env::ScopedEnv) {
    let root = tempfile::tempdir().expect("a directory for the settings");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", root.path().join("config"))
        .set("XDG_DATA_HOME", root.path().join("data"))
        .set("XDG_STATE_HOME", root.path().join("state"));
    (root, env)
}

/// A shell taking part in the workspace through `sync`, drawing what the session publishes of who
/// is here. The person's own settings are beside it, so a run may choose the light theme.
fn session_over(sync: &Rc<Client>) -> Session {
    session_on(sync, 1280, 800)
}

/// [`session_over`] on a display `width` by `height` : a phone's, where the bar is compact.
fn session_on(sync: &Rc<Client>, width: i32, height: i32) -> Session {
    let mut session = crate::common::headless_on(
        &[tilo_compositor::OutputConfig::new(
            "HEADLESS-1",
            width,
            height,
        )],
        Options {
            person: "Quinten".to_owned(),
            sync: Some(Rc::clone(sync)),
            services: vec![Box::new(settings_sections::service::SettingsService::new())],
            ..Options::default()
        },
    );
    session.handle().replace_tree(shell());
    crate::common::until(&mut session, "the shell to show the workspace", |session| {
        session.shell_control("Top bar").is_some()
    });
    session
}

/// Whether this run takes its pictures in the light theme.
fn light() -> bool {
    std::env::var_os("TILO_PEOPLE_LIGHT").is_some()
}

/// Choose the light theme, as Settings' Appearance page does: the appearance section's own
/// setters, which every device of the person reads.
#[track_caller]
fn choose_light(session: &mut Session) {
    // The settings service publishes its sections on a pass of its own.
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let set = |session: &mut Session, field: &str, value: Value| {
        let arguments: serde_json::Map<String, Value> = json!({ "value": value })
            .as_object()
            .cloned()
            .expect("an object");
        let action = format!("set-{}", field.replace('_', "-"));
        session
            .invoke("settings/appearance", &action, &arguments)
            .unwrap_or_else(|error| panic!("the appearance's {action}: {error:?}"));
    };
    set(session, "light_theme", json!("paper-light"));
    set(session, "mode", json!("light"));
    // The shell draws the choice once the section holds it.
    for _ in 0..8 {
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// Keep the screen, and the band of the bar across its top, as `name.png` and `name-bar.png` where
/// `TL_SHELL_SHOTS` names: what the bar looks like with what it is saying now. A relative directory
/// is read from the package, so a run elsewhere can name one beside the checkout.
#[track_caller]
fn keep_bar(session: &mut Session, name: &str) {
    let Some(directory) = std::env::var_os("TL_SHELL_SHOTS") else {
        return;
    };
    let theme = if light() { "light" } else { "dark" };
    let mut directory = std::path::PathBuf::from(directory);
    if directory.is_relative() {
        directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(directory);
    }
    std::fs::create_dir_all(&directory).expect("the captures' directory");
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a picture of the display");
    let width = usize::try_from(shot.width).expect("a display's width");
    image::save_buffer(
        directory.join(format!("{name}-{theme}.png")),
        &shot.rgba,
        shot.width,
        shot.height,
        image::ColorType::Rgba8,
    )
    .expect("the screen");
    let band = width.saturating_mul(56 * 4).min(shot.rgba.len());
    image::save_buffer(
        directory.join(format!("{name}-{theme}-bar.png")),
        &shot.rgba[..band],
        shot.width,
        56,
        image::ColorType::Rgba8,
    )
    .expect("the bar's band");
}

/// Whether the bar of `session` says `words`.
fn bar_says(session: &Session, words: &str) -> bool {
    session.shell_says(words)
}

/// Where the control named `name` stands on the bar `session` drew: `(x, y, width, height)`.
#[track_caller]
fn control(session: &Session, name: &str) -> (f64, f64, f64, f64) {
    session
        .shell_control(name)
        .unwrap_or_else(|| panic!("the bar shows {name:?}: {:?}", session.accessible_labels()))
}

/// Run frames until the bar stops saying `words`, failing with everything it does say when
/// `within` runs out: a notice that outlives its moment is the bug this names.
#[track_caller]
fn bar_gone(session: &mut Session, words: &str, within: Duration) {
    let deadline = Instant::now()
        .checked_add(within)
        .expect("the bound fits in an Instant");
    while bar_says(session, words) {
        assert!(
            Instant::now() < deadline,
            "the bar still says {words:?} after {within:?}: {:?}",
            session.accessible_labels(),
        );
        session.step(Duration::from_millis(20)).expect("step");
    }
}

/// Someone arrives, waves and follows the person here, and each goes when it is over: the bar says
/// it all from what the transport hands the session and nothing else.
#[test]
fn the_bar_says_who_arrived_who_waved_and_who_follows_this_person() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (_home, _settings) = settings_home();
    let own = OwnPresence {
        label: "Quinten".to_owned(),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(publisher(0xf0), own));
    let mut session = session_over(&sync);
    if light() {
        // The theme before anyone arrives, so every picture this run takes is the same bar.
        choose_light(&mut session);
    }
    // Nobody else is here yet.
    assert!(bar_says(&session, "People"));

    // Ada arrives at her own device: the bar says so on its own, naming the first word of her
    // name, where the standing label beside the faces names the whole of it.
    sync.hear(hello(0xaa, "Framework 16"));
    arrived_as(&sync, 0xaa, "Ada Lovelace");
    crate::common::until(&mut session, "Ada's arrival", |session| {
        session.shell_control("Ada is here").is_some()
    });
    keep_bar(&mut session, "people-bar-arrival");
    let (_, arrival_y, _, _) = control(&session, "Ada is here");
    assert!(arrival_y < 80.0, "the arrival stands in the bar");
    // The arrival is a notice, not a row: no hand takes it away, and the session gives it up when
    // its four seconds are over.
    bar_gone(&mut session, "Ada is here", Duration::from_secs(8));

    // Where the faces and the invite stand while the bar says nothing: a notice must not move
    // what a person may be aiming at.
    let faces = control(&session, "Ada Lovelace");
    let invite = control(&session, "Invite someone");

    // Ada waves at this person: the bar says who.
    presence(
        &sync,
        0xaa,
        collab_types::PeerPresencePayload::Wave {
            seq: 1,
            target: publisher(0xf0).to_string(),
        },
    );
    crate::common::until(&mut session, "Ada's wave", |session| {
        bar_says(session, "Ada Lovelace waved at you")
    });
    // The news stands where the clock stood, and the controls of who is here keep their places:
    // nothing a person aims at is shoved aside by a word that goes by itself.
    assert_eq!(
        control(&session, "Ada Lovelace"),
        faces,
        "the faces stay where they were while the bar says who waved"
    );
    assert_eq!(
        control(&session, "Invite someone"),
        invite,
        "the invite stays where it was while the bar says who waved"
    );
    keep_bar(&mut session, "people-bar-wave");
    // And it goes by itself, after its six seconds, with nothing pressed since it was heard.
    bar_gone(
        &mut session,
        "Ada Lovelace waved at you",
        Duration::from_secs(10),
    );

    // Ada follows this person: her face on the bar wears the mark and says so.
    presence(
        &sync,
        0xaa,
        collab_types::PeerPresencePayload::Following {
            target: Some(publisher(0xf0).to_string()),
        },
    );
    crate::common::until(&mut session, "Ada to follow", |session| {
        bar_says(session, "Ada Lovelace, follows you")
    });
    keep_bar(&mut session, "people-bar-follower");

    // She stops: nothing on the bar says she follows any more.
    presence(
        &sync,
        0xaa,
        collab_types::PeerPresencePayload::Following { target: None },
    );
    crate::common::until(&mut session, "Ada to stop following", |session| {
        !bar_says(session, "Ada Lovelace, follows you")
    });
}

/// A phone's bar has room for one thing where the clock stands: someone arrives and the bar says
/// who, taking the time's place, and gives it back when the notice's moment is over.
#[test]
fn the_bar_on_a_phone_says_who_arrived_in_the_clock_s_place() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (_home, _settings) = settings_home();
    let own = OwnPresence {
        label: "Quinten".to_owned(),
        ..OwnPresence::default()
    };
    let sync = Rc::new(Client::untethered(publisher(0xf0), own));
    let mut session = session_on(&sync, 480, 900);
    // The time stands where the bar's glance is while nobody has come.
    assert!(
        session.shell_says(":"),
        "the bar shows the time: {:?}",
        session.accessible_labels()
    );

    sync.hear(hello(0xaa, "Framework 16"));
    arrived_as(&sync, 0xaa, "Ada Lovelace");
    crate::common::until(&mut session, "Ada's arrival on a phone", |session| {
        session.shell_control("Ada is here").is_some()
    });
    // The news takes the time's place, and gives it back when its moment is over.
    bar_gone(&mut session, "Ada is here", Duration::from_secs(8));
    crate::common::until(&mut session, "the time back", |session| {
        session.shell_says(":")
    });
}
