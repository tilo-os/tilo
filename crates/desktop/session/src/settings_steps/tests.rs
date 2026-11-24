//! Settings' window changes the settings the session keeps: each control of it is bound to another
//! provider's action, and that provider is this one, so the click the window sends is admitted by
//! the workspace's broker and run on the session's settings service.

use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueQuery, Confirmation, InvokeArgument, InvokeRequest,
    PendingPoll, RemoteActionTarget, settings_resources,
};
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_device_identity::IdentityPaths;
use tilo_identity::provisioning::IdentityStorage;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::{self, testing};
use tilo_user_identity::DeviceCert;

const APPEARANCE: &str = "settings.appearance";
const POWER: &str = "settings.power";
const INPUT: &str = "settings.input";
const NIGHT_LIGHT: &str = "settings.night-light";
/// The workspace this desktop hosts, as the shell names it.
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
    // This device's settings documents live in the document store under the
    // data home: a fresh one per fixture keeps a click here out of another
    // case's settings.
    env.set("XDG_CONFIG_HOME", &xdg)
        .set("XDG_DATA_HOME", root.path().join("data"))
        .set("TILO_IDENTITY_ROOT", &system)
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    // The account's own store below the identity directory, where every reader looks.
    tilo_identity::sources::test_trust::install();
    tilo_identity::provisioning::ensure_collaboration_identity(
        &tilo_identity::store::account_identity_dir(&xdg.join("tilo/identity")),
        &IdentityPaths::under(&system),
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let device = tilo_device_identity::persist::load_device_key(&IdentityPaths::under(&system))
        .expect("this device's key");
    let cert =
        tilo_device_identity::persist::read_device_cert(&IdentityPaths::under(&system).device_cert)
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
    let owner = owner::open_file(
        &tilo_session_client::engine::owner_path(state).expect("this device's state directory"),
    )
    .expect("an owner for this device's workspace");
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let governor = testing::add_interactive_device_in(&owner, device, cert, WORKSPACE)
        .expect("this device governs the workspace it hosts");
    let host = governor.device().device_public_key;
    let frames: Vec<Vec<u8>> = governor.outgoing().map(|(bytes, _)| bytes).collect();
    for frame in frames {
        owner
            .receive(
                workspace,
                TransportOrigin::RetainedServerDelta(host),
                postcard::from_bytes(&frame).expect("a publication is a message"),
                std::time::SystemTime::now(),
            )
            .expect("the host admits the workspace's people");
    }
}

/// The person `root` certifies, as a workspace names its owner.
fn person_id(root: &[u8; 32]) -> String {
    hex::encode(root)
}

/// A shell showing the workspace, known by its id, owned by the person.
fn shell(fixture: &Fixture) -> Layout {
    let workspace_id = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let mut workspace = Workspace::new(workspace_id, "Shell");
    workspace
        .policy
        .set_owner(person_id(&fixture.cert.user_public_key));
    Layout {
        workspaces: vec![workspace.clone()],
        active: workspace.id(),
        ..Layout::default()
    }
}

/// A headless session keeping the person's settings, its broker on `app_wire`: what every control of
/// Settings' window reaches.
fn serving(fixture: &Fixture, app_wire: &Path) -> crate::Session {
    let mut session = crate::Session::headless(
        crate::Options {
            presentation: ui::headless::fixture_runner_inputs,
            services: vec![Box::new(settings_sections::service::SettingsService::new())],
            person: person_id(&fixture.cert.user_public_key),
            collab: false,
            state: Some(fixture.state.clone()),
            app_socket: crate::AppSocket::At(app_wire.join("collab.sock")),
            ..crate::Options::default()
        },
        &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("a session");
    session.handle().replace_tree(shell(fixture));
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
    session
}

/// The person at this desktop, in the workspace in front: whose own click a control's action is.
fn local_person(session: &crate::Session) -> Caller {
    Caller::local_person(session.shared.person.clone()).in_workspace(WORKSPACE)
}

/// What `ask` answers, run until the session's pass has answered it: a host step's rows and its
/// invocation are read on the loop, which steps here until the answer is in.
fn answered<T: Send + 'static>(
    session: &mut crate::Session,
    ask: impl FnOnce() -> tilo_actions::remote::Pending<Result<T, ActionCallError>>,
) -> Result<T, ActionCallError> {
    let (done, mut answered) = tilo_actions::remote::pending();
    ask().then(move |answer| done.complete(answer));
    let deadline = tilo_test_env::hang_deadline();
    while std::time::Instant::now() < deadline {
        session.step(Duration::from_millis(1)).expect("a frame");
        if let PendingPoll::Ready(answer) = answered.poll_ready() {
            return answer.expect("the broker answers");
        }
    }
    Err(ActionCallError::Refused {
        reason: "the session did not answer".to_owned(),
    })
}

/// The section of kind `kind`, where the session publishes it.
fn section(kind: &str) -> String {
    settings_sections::address(kind).expect("a section the session keeps")
}

/// The invocation Settings' window sends when the control bound to the field `field` of the
/// section of kind `kind` is pressed: the setter of that field, with the value the control fixes.
fn field_edit(kind: &str, field: &str, value: &Value, confirmed: bool) -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: settings_resources::PROVIDER.to_owned(),
            address: ActionAddress::new(section(kind), ui::setting_verb(field)),
        },
        argument: InvokeArgument::Json(Map::from_iter([("value".to_owned(), value.clone())])),
        context: None,
        confirmation: confirmed.then_some(Confirmation::Confirmed),
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The setting the section of kind `kind` holds now, as the session publishes it.
fn stored(session: &crate::Session, kind: &str, field: &str) -> Value {
    session
        .read(&section(kind))
        .expect("the section is published")[field]
        .clone()
}

/// Every control of Settings' window changes the setting it shows: the provider the window names
/// offers each section's setters, and pressing one changes the settings the session publishes,
/// whichever section it is of.
#[test]
fn settings_controls_change_the_settings_the_session_keeps() {
    let _settled = ui::motion::testing::settled();
    let fixture = fixture();
    let app_wire = tempfile::tempdir().expect("a directory to serve the broker in");
    std::fs::set_permissions(
        app_wire.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("the broker's directory is this user's");
    let mut session = serving(&fixture, app_wire.path());
    let broker = session
        .action_broker()
        .expect("the session serves a broker");
    let person = local_person(&session);

    // The sections Settings' panes show stand under the provider its controls name, each with the
    // setters they bind to: the same resources, and the same action, as the settings the
    // session keeps.
    let page = answered(&mut session, || {
        broker.action_query(
            &person,
            &CatalogueQuery {
                provider: Some(settings_resources::PROVIDER.to_owned()),
                max_results: 1024,
                ..CatalogueQuery::default()
            },
        )
    })
    .expect("the person sees the settings' catalogue");
    for (kind, field) in [
        (APPEARANCE, "mode"),
        (POWER, "lock_minutes"),
        (INPUT, "pointer_speed"),
        (NIGHT_LIGHT, "night_light"),
    ] {
        let section = section(kind);
        let setter = ui::setting_verb(field);
        assert!(
            page.rows
                .iter()
                .any(|row| row.action.as_ref().is_some_and(|action| {
                    action.address.resource == section && action.address.action == setter
                })),
            "{section} offers the setter {setter} its control binds to: {:?}",
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
            json!(ui::theme::Preset::MossDark.id()),
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
            Err(ActionCallError::ConfirmationRequired { .. })
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
