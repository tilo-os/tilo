//! Settings' window changes the settings the session keeps: each control of it is bound to another
//! provider's action, and that provider is this one, so the click the window sends is admitted by
//! the workspace's broker and run on the session's settings service.

use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    ActionHostError, Caller, CatalogueQuery, Confirmation, InvokeArgument, InvokeRequest,
    PendingPoll, RemoteActionTarget, settings_records,
};
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_identity_host::provisioning::IdentityStorage;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::{self, testing};
use tilo_user_identity::{DeviceCert, IdentityPaths};

const APPEARANCE: &str = "settings:settings.appearance";
const POWER: &str = "settings:settings.power";
const INPUT: &str = "settings:settings.input";
const NIGHT_LIGHT: &str = "settings:settings.night-light";
/// The workspace this desktop hosts, as the desk names it.
const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";

/// This device's identity and the workspace it hosts, where the session reads them: what makes the
/// person's own click at their desktop one the broker admits.
struct Fixture {
    cert: DeviceCert,
    state: std::path::PathBuf,
    // The identity's environment and files, restored and removed last: the session reads them.
    _env: tilo_test_env::ScopedEnv,
    _root: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("a directory for the identity");
    let xdg = root.path().join("config");
    let system = root.path().join("system");
    let state = root.path().join("state");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", &xdg)
        .set("TILO_IDENTITY_ROOT", &system)
        .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &xdg.join("tilo/identity"),
        &IdentityPaths::under(&system),
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let device = tilo_user_identity::persist::load_device_key(&IdentityPaths::under(&system))
        .expect("this device's key");
    let cert =
        tilo_user_identity::persist::read_device_cert(&IdentityPaths::under(&system).device_cert)
            .expect("this device's certificate");
    hold_workspace(&state, &device, &cert);
    Fixture {
        cert,
        state,
        _env: env,
        _root: root,
    }
}

/// The workspace's owner, as this device holds it in `state` : what the session opens when it
/// starts, with this device the one governing the workspace.
fn hold_workspace(state: &Path, device: &ed25519_dalek::SigningKey, cert: &DeviceCert) {
    let record = state.join("tilo").join("rooms.json");
    std::fs::create_dir_all(record.parent().expect("the state's workspace directory"))
        .expect("this device's state directory");
    let owner = owner::open_file(&owner::owner_file_of(&record))
        .expect("an owner for this device's workspace");
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let governor = testing::enroll_interactive_governor_in(&owner, device, cert, WORKSPACE)
        .expect("this device governs the workspace it hosts");
    let host = governor.workspace_host().device_public_key;
    let frames: Vec<Vec<u8>> = governor
        .outgoing()
        .map(|(bytes, _)| bytes.to_vec())
        .collect();
    for frame in frames {
        owner
            .receive(
                workspace,
                TransportOrigin::RetainedRelayDelta(host),
                postcard::from_bytes(&frame).expect("a publication is a message"),
                std::time::SystemTime::now(),
            )
            .expect("the host admits the workspace's people");
    }
}

/// The person `root` certifies, as a workspace names its owner.
fn principal(root: &[u8; 32]) -> String {
    match collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(*root),
    ) {
        collab_types::caps::Principal::Person(id) => id,
        other => panic!("{other:?} is not a person"),
    }
}

/// A desk showing the workspace, known by its id, owned by the person.
fn desk(fixture: &Fixture) -> DesktopTree {
    let workspace_id = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let slot = WorkspaceSlot::of_workspace(workspace_id);
    let mut workspace = Workspace::in_workspace(slot, workspace_id, "Desk");
    workspace.set_owner(principal(&fixture.cert.user_public_key));
    DesktopTree {
        workspaces: vec![workspace.clone()],
        active: workspace.slot,
        ..DesktopTree::default()
    }
}

/// A headless session keeping the person's settings, its broker on `plane`: what every control of
/// Settings' window reaches.
fn serving(fixture: &Fixture, plane: &Path) -> crate::Session {
    let mut session = crate::Session::headless(
        crate::Options {
            presentation: ui::headless::fixture_runner_inputs,
            services: vec![Box::new(tilo_settings_service::SettingsService::new())],
            person: principal(&fixture.cert.user_public_key),
            collab: false,
            state: Some(fixture.state.clone()),
            action_plane: crate::ActionPlane::At(plane.join("collab.sock")),
            ..crate::Options::default()
        },
        &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("a session");
    session.handle().replace_tree(desk(fixture));
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
    session
}

/// The person at this desktop, in the workspace in front: whose own click a control's action is.
fn seat(session: &crate::Session) -> Caller {
    Caller::local_person(session.shared.person.clone()).in_workspace(WORKSPACE)
}

/// What `ask` answers, run until the session's pass has answered it: a host step's rows and its
/// invocation are read on the loop, which steps here until the answer is in.
fn answered<T: Send + 'static>(
    session: &mut crate::Session,
    ask: impl FnOnce() -> tilo_actions::remote::Pending<Result<T, ActionHostError>>,
) -> Result<T, ActionHostError> {
    let (done, mut answered) = tilo_actions::remote::pending();
    ask().then(move |answer| done.complete(answer));
    let deadline = tilo_test_env::hang_deadline();
    while std::time::Instant::now() < deadline {
        session.step(Duration::from_millis(1)).expect("a frame");
        if let PendingPoll::Ready(answer) = answered.poll_ready() {
            return answer.expect("the broker answers");
        }
    }
    Err(ActionHostError::Refused {
        reason: "the session did not answer".to_owned(),
    })
}

/// The invocation Settings' window sends when the control bound to the field `field` of the
/// section `section` is pressed: the provider's field edit of that section, with the edit the
/// control fixes.
fn field_edit(section: &str, field: &str, value: &Value, confirmed: bool) -> InvokeRequest {
    let edit = json!({ "path": [{ "field": field }], "op": "set", "value": value });
    InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: settings_records::PROVIDER.to_owned(),
            address: ActionAddress::new(section, "field-edit"),
        },
        argument: InvokeArgument::Json(Map::from_iter([("edit".to_owned(), edit)])),
        context: None,
        confirmation: confirmed.then_some(Confirmation::Confirmed),
        revision: None,
        wait_ms: None,
        appearance: None,
    }
}

/// The setting `section` holds now, as the session publishes it.
fn stored(session: &crate::Session, section: &str, field: &str) -> Value {
    session.read(section).expect("the section is published")[field].clone()
}

/// Every control of Settings' window changes the setting it shows: the provider the window names
/// offers each section's field edit, and pressing one changes the settings the session publishes,
/// whichever section it is of.
#[test]
fn settings_controls_change_the_settings_the_session_keeps() {
    let _settled = ui::motion::testing::settled();
    let fixture = fixture();
    let plane = tempfile::tempdir().expect("a directory to serve the broker in");
    std::fs::set_permissions(
        plane.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("the broker's directory is this user's");
    let mut session = serving(&fixture, plane.path());
    let broker = session
        .action_broker()
        .expect("the session serves a broker");
    let person = seat(&session);

    // The sections Settings' panes show stand under the provider its controls name, each with the
    // field edit they bind to: the same resources, and the same action, as the settings the
    // session keeps.
    let page = answered(&mut session, || {
        broker.action_query(
            &person,
            &CatalogueQuery {
                provider: Some(settings_records::PROVIDER.to_owned()),
                max_results: 1024,
                ..CatalogueQuery::default()
            },
        )
    })
    .expect("the person sees the settings' catalogue");
    for section in [APPEARANCE, POWER, INPUT, NIGHT_LIGHT] {
        assert!(
            page.rows
                .iter()
                .any(|row| row.action.as_ref().is_some_and(|action| {
                    action.address.resource == section && action.address.action == "field-edit"
                })),
            "{section} offers the field edit its controls bind to: {:?}",
            page.rows
                .iter()
                .filter_map(|row| row.action.as_ref())
                .map(|action| { format!("{}/{}", action.address.resource, action.address.action) })
                .collect::<Vec<_>>()
        );
    }

    // The appearance theme, the accent, the text size, the power lock, the pointer speed and the
    // night light: a control of each kind, and each change lands in the settings the session keeps.
    let changes = [
        (APPEARANCE, "mode", json!("light")),
        (
            APPEARANCE,
            "dark_theme",
            json!(ui::theme::Preset::DarkCool.id()),
        ),
        (APPEARANCE, "accent", json!("violet")),
        (APPEARANCE, "text_scale", json!(1.25)),
        (APPEARANCE, "high_contrast", json!(true)),
        // Locking sooner than now weakens nothing, so it asks nothing.
        (POWER, "lock_minutes", json!(1)),
        (INPUT, "pointer_speed", json!(0.7)),
        (NIGHT_LIGHT, "night_light", json!(true)),
    ];
    for (section, field, value) in changes {
        answered(&mut session, || {
            broker.action_invoke_local_person(&person, &field_edit(section, field, &value, false))
        })
        .unwrap_or_else(|error| panic!("the broker runs a change of {field}: {error}"));
        assert_eq!(
            stored(&session, section, field),
            value,
            "{field} is what the person set"
        );
    }

    // A change that weakens the person's security asks them first: none of an agent's own answers
    // stands for it, and their own answer runs it.
    let later = field_edit(POWER, "lock_minutes", &json!(30), false);
    assert!(
        matches!(
            answered(&mut session, || broker
                .action_invoke_local_person(&person, &later)),
            Err(ActionHostError::ConfirmationRequired { .. })
        ),
        "locking later waits for the person's answer"
    );
    assert_eq!(
        stored(&session, POWER, "lock_minutes"),
        json!(1),
        "nothing weakened without them"
    );
    answered(&mut session, || {
        broker.action_invoke_local_person(
            &person,
            &field_edit(POWER, "lock_minutes", &json!(30), true),
        )
    })
    .expect("their own answer runs it");
    assert_eq!(stored(&session, POWER, "lock_minutes"), json!(30));
}
