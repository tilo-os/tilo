//! The desktop's keyboard shortcuts, listed and rebound through the action
//! door.
//!
//! The production broker publishes the shortcut catalogue on a workspace's
//! own provider. Rebinding Quick Settings there saves the new chord; a
//! compositor session reading the saved table (as it does whenever the
//! settings file changes) opens Quick Settings on the new chord and leaves
//! the old one to the focused application, and `list` reports the change. A
//! rebind onto a chord another shortcut holds fails naming it, and a caller
//! without the Settings grant is shown none of this.

use std::path::Path;

use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CatalogueQuery, InvokeArgument, InvokeRequest,
    MAX_CATALOGUE_PAGE, Pending, ReceiptStatus, RemoteActionTarget, workspace_provider,
};
use tilo_actions::{ActionAddress, InvocationId};

const ROOM: uuid::Uuid = uuid::Uuid::from_u128(0x6b3d_5e70_0000_4000_8000_0000_0000_0001);
const OWNER: &str = "person:keyboard-owner";
const AGENT: &str = "helper";

fn answer<T>(pending: Pending<T>) -> T {
    match pending.wait_timeout(tilo_test_env::hang_bound()) {
        Ok(Some(value)) => value,
        _ => panic!("the broker did not answer"),
    }
}

struct Door {
    _broker: crate::shell::collab_broker::BrokerHandle,
    membership: crate::shell::state::WorkspaceTreeHandle,
    host: crate::shell::remote_actions::BrokerActionHost,
    owner: Caller,
    _dir: tempfile::TempDir,
}

impl Door {
    fn open(socket: &Path, dir: tempfile::TempDir) -> Self {
        // The broker binds only inside an owner-only directory.
        std::fs::set_permissions(
            dir.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .expect("owner-only socket directory");
        let mut workspace =
            crate::shell::state::Workspace::new(crate::shell::state::WorkspaceId(1), "Desk");
        workspace.set_room(crate::shell::state::RoomId::parse(&ROOM.to_string()).expect("room id"));
        workspace.set_owner(OWNER.to_string());
        // A helper the owner admits to the room and lets change Notes, and
        // holds no Settings grant.
        let owner = tilo_action_policy::Subject::person(OWNER.to_string());
        workspace.admit_agent_preset(
            AGENT,
            tilo_action_policy::roles::AgentPreset::Observer,
            &owner,
        );
        let notes = workspace.node().app("so.tilo.notes");
        workspace.grant(tilo_action_policy::Grant::new(
            "helper-notes",
            tilo_action_policy::Subject::agent(AGENT),
            notes,
            tilo_action_policy::KindSet::UNDOABLE,
            tilo_action_policy::Answer::Allow,
            owner,
        ));
        let mut tree = crate::shell::state::WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        let membership = tree.shared();
        let broker = crate::shell::collab_broker::spawn_native_harness(
            socket.to_path_buf(),
            membership.clone(),
            Vec::new(),
        )
        .expect("bind the collaboration service");
        let host = crate::shell::remote_actions::BrokerActionHost::new(
            broker.app_action_broker().expect("the action broker"),
        );
        Self {
            _broker: broker,
            membership,
            host,
            owner: Caller::local_person(OWNER),
            _dir: dir,
        }
    }

    /// Every row `caller` is offered under `parent`.
    fn rows(&self, caller: &Caller, parent: Option<&str>) -> Vec<(String, String, String)> {
        answer(self.host.query(
            caller,
            CatalogueQuery {
                max_results: MAX_CATALOGUE_PAGE,
                parent_resource: parent.map(str::to_string),
                ..CatalogueQuery::default()
            },
        ))
        .expect("catalogue")
        .rows
        .into_iter()
        .filter_map(|row| {
            let action = row.action?;
            Some((row.app_id, action.address.resource, action.address.action))
        })
        .collect()
    }

    fn invoke(
        &self,
        caller: &Caller,
        resource: &str,
        action: &str,
        arguments: &Value,
    ) -> Result<ReceiptStatus, ActionHostError> {
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        answer(self.host.invoke(
            caller,
            InvokeRequest {
                on_behalf_of: None,
                invocation: InvocationId::new(),
                target: RemoteActionTarget {
                    provider: workspace_provider(&ROOM.to_string()),
                    address: ActionAddress::new(resource, action),
                },
                argument: if arguments.is_empty() {
                    InvokeArgument::None
                } else {
                    InvokeArgument::Json(arguments)
                },
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
            },
        ))
        .map(|receipt| receipt.status)
    }

    fn output(&self, resource: &str, action: &str, arguments: &Value) -> Value {
        match self.invoke(&self.owner, resource, action, arguments) {
            Ok(ReceiptStatus::Completed { output }) => output.value.expect("a JSON result"),
            other => panic!("{resource}#{action} did not complete: {other:?}"),
        }
    }
}

/// Press `key` with `modifiers` in `host` after one layout pass; whether
/// the shell took the press.
fn press(
    host: &mut crate::shell::overlay_host::ShellOverlayHost,
    key: winit::keyboard::Key,
    modifiers: winit::keyboard::ModifiersState,
) -> bool {
    use std::collections::HashMap;
    use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
    use ui::widget::Widget as _;

    let theme = ui::theme::Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn ui::widget::Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(1280.0),
                    height: Dimension::length(800.0),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut text = ui::text_render::TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(1280.0),
                height: AvailableSpace::Definite(800.0),
            },
            |known, available, node_id, _, _| match node_map.get(&node_id) {
                Some(&widget) => unsafe { (*widget).measure(known, available, &mut text) },
                None => Size::ZERO,
            },
        )
        .unwrap();
    host.apply_layout(&taffy, child, vello::kurbo::Point::ORIGIN);

    let event = ui::synthetic_input::key_event(key, None, winit::event::ElementState::Pressed);
    let event = winit::event::WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event,
        is_synthetic: false,
    };
    ui::testing::Dispatch::new()
        .modifiers(modifiers)
        .layout(&taffy, child)
        .send(host, &event)
        .handled
}

fn quick_settings_row(listed: &Value) -> Value {
    listed["shortcuts"]
        .as_array()
        .expect("the shortcuts")
        .iter()
        .find(|row| row["id"] == "quick-settings")
        .cloned()
        .expect("Quick Settings is listed")
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the door owns the broker and its socket directory for the whole test"
)]
fn rebinding_through_the_door_moves_what_the_chord_triggers() {
    use winit::keyboard::{Key, ModifiersState};

    let config = tempfile::tempdir().expect("temporary config directory");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", config.path());
    let dir = tempfile::tempdir().expect("socket directory");
    let socket = dir.path().join("collab.sock");
    let door = Door::open(&socket, dir);

    let top = door.rows(&door.owner, None);
    assert!(
        top.contains(&(
            "so.tilo.settings".into(),
            "keyboard-shortcuts".into(),
            "list".into()
        )),
        "the catalogue is published as a Settings resource: {top:?}"
    );
    let entries = door.rows(&door.owner, Some("keyboard-shortcuts"));
    assert!(entries.contains(&(
        "so.tilo.settings".into(),
        "keyboard-shortcut:quick-settings".into(),
        "rebind".into()
    )));
    assert!(
        !entries
            .iter()
            .any(|(_, resource, _)| resource == "keyboard-shortcut:fixed-close-tab"),
        "a fixed chord offers nothing to change"
    );

    let rebound = door.output(
        "keyboard-shortcut:quick-settings",
        "rebind",
        &json!({"chord": "Super+J"}),
    );
    assert_eq!(rebound["chords"], json!(["Super+J"]));

    match door.invoke(
        &door.owner,
        "keyboard-shortcut:quick-settings",
        "rebind",
        &json!({"chord": "Super+Space"}),
    ) {
        Ok(ReceiptStatus::Failed { error }) => {
            assert_eq!(error.code, "shortcut_conflict");
            let details = error.details.expect("the holder is named");
            assert_eq!(details["conflict"]["id"], "search");
            assert_eq!(details["conflict"]["kind"], "shortcut");
        }
        other => panic!("taking search's chord must fail naming search: {other:?}"),
    }

    let listed = door.output("keyboard-shortcuts", "list", &json!({}));
    let row = quick_settings_row(&listed);
    assert_eq!(row["chords"], json!(["Super+J"]));
    assert_eq!(row["defaults"], json!(["Super+Q"]));
    assert_eq!(row["rebindable"], true);
    assert_eq!(row["category"], "system");

    // The compositor re-reads the saved table whenever the settings file
    // changes; this is that handoff.
    let (mut host, chrome) =
        crate::shell::build_tilo_overlay_host_with_handle(door.membership.clone(), false);
    host.set_shortcut_config(settings_store::hardware::ShortcutConfig::load())
        .expect("the saved table is valid");
    assert!(
        !press(&mut host, Key::Character("q".into()), ModifiersState::SUPER),
        "the old chord goes back to the focused application"
    );
    assert!(!chrome.borrow().overlays.quick_settings);
    assert!(press(
        &mut host,
        Key::Character("j".into()),
        ModifiersState::SUPER
    ));
    assert!(
        chrome.borrow().overlays.quick_settings,
        "the new chord opens Quick Settings"
    );

    let reset = door.output("keyboard-shortcut:quick-settings", "reset", &json!({}));
    assert_eq!(reset["chords"], json!(["Super+Q"]));
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the door owns the broker and its socket directory for the whole test"
)]
fn a_caller_without_the_settings_grant_cannot_rebind() {
    let config = tempfile::tempdir().expect("temporary config directory");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", config.path());
    let dir = tempfile::tempdir().expect("socket directory");
    let socket = dir.path().join("collab.sock");
    let door = Door::open(&socket, dir);
    let agent = Caller::local_agent(AGENT);

    assert!(
        !door
            .rows(&agent, Some("keyboard-shortcuts"))
            .iter()
            .any(|(_, _, action)| action == "rebind"),
        "an agent without the Settings grant is shown no rebind"
    );
    // Refused either way: not permitted, or (for a caller the room does
    // not admit at all) not there.
    assert!(matches!(
        door.invoke(
            &agent,
            "keyboard-shortcut:quick-settings",
            "rebind",
            &json!({"chord": "Super+J"}),
        ),
        Err(ActionHostError::NotPermitted | ActionHostError::UnknownAction)
    ));
    assert_eq!(
        settings_store::hardware::ShortcutConfig::load(),
        settings_store::hardware::ShortcutConfig::default(),
        "nothing was saved"
    );
}

/// A helper whose grant on Settings asks first is offered the rebind; it
/// does not rebind on its own word, and does once the person confirms on
/// their device.
#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the door owns the broker and its socket directory for the whole test"
)]
fn a_helper_asked_first_rebinds_on_the_persons_confirmation() {
    use tilo_action_broker::app_actions::{
        AppActionConsent, AppActionConsentPrompter, AppActionConsentRequest,
    };
    struct Person;
    impl AppActionConsentPrompter for Person {
        fn ask(
            &self,
            request: AppActionConsentRequest,
        ) -> Option<tokio::sync::oneshot::Receiver<AppActionConsent>> {
            assert!(
                request.confirming,
                "the person confirms one call: {request:?}"
            );
            let (answer, asked) = tokio::sync::oneshot::channel();
            let _ = answer.send(AppActionConsent::Once);
            Some(asked)
        }
    }
    let config = tempfile::tempdir().expect("temporary config directory");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", config.path());
    let dir = tempfile::tempdir().expect("socket directory");
    let socket = dir.path().join("collab.sock");
    let door = Door::open(&socket, dir);
    door.membership
        .write()
        .desktop
        .workspaces
        .first_mut()
        .map(|workspace| {
            let settings = workspace
                .node()
                .app(crate::shell::keyboard_shortcuts::APP_ID);
            workspace.grant(tilo_action_policy::Grant::new(
                "helper-settings-asks",
                tilo_action_policy::Subject::agent(AGENT),
                settings,
                tilo_action_policy::KindSet::ALL,
                tilo_action_policy::Answer::Ask,
                tilo_action_policy::Subject::person(OWNER.to_string()),
            ))
        })
        .expect("the room");
    door._broker
        .app_action_broker()
        .expect("the action broker")
        .set_app_action_consent(std::sync::Arc::new(Person));
    let agent = Caller::local_agent(AGENT)
        .in_room(ROOM.to_string())
        .acting_for(OWNER);
    assert!(
        door.rows(&agent, Some("keyboard-shortcuts"))
            .iter()
            .any(|(_, _, action)| action == "rebind"),
        "an action asking first is offered"
    );
    let request = || InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: workspace_provider(&ROOM.to_string()),
            address: ActionAddress::new("keyboard-shortcut:quick-settings", "rebind"),
        },
        argument: InvokeArgument::Json(
            json!({"chord": "Super+J"})
                .as_object()
                .cloned()
                .expect("an object"),
        ),
        context: None,
        confirmation: Some(tilo_actions::remote::Confirmation::Confirmed),
        revision: None,
        wait_ms: None,
    };
    assert_eq!(
        answer(door.host.invoke(&agent, request())).err(),
        Some(ActionHostError::ConfirmationRequired),
        "the helper's own word confirms nothing"
    );
    let mut asked = request();
    let witnessed = answer(door.host.confirm(&agent, &asked)).expect("the person confirmed");
    asked.confirmation = Some(witnessed);
    let receipt = answer(door.host.invoke(&agent, asked)).expect("admitted");
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "{receipt:?}"
    );
}
