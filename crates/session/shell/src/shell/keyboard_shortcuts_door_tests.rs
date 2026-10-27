//! The desktop's keyboard shortcuts, listed through the action door and
//! rebound in the person's `shortcuts` section.
//!
//! The production broker publishes the shortcut catalogue on a workspace's
//! own provider, to read. Rebinding Quick Settings in the section saves the
//! new chord; a compositor session reading the saved table (as it does
//! whenever the settings file changes) opens Quick Settings on the new chord
//! and leaves the old one to the focused application, and `list` reports
//! the change. A caller without the Settings grant is shown none of it.

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
        let mut workspace = crate::shell::state::Workspace::in_room(
            crate::shell::state::WorkspaceId(1),
            crate::shell::state::RoomId::parse(&ROOM.to_string()).expect("room id"),
            "Desk",
        );
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
                appearance: None,
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
    use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
    use ui::widget::Widget as _;

    let theme = ui::theme::Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn ui::widget::Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size::from_lengths(1280.0, 800.0),
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
    let event = ui::synthetic_input::keyboard_input(event);
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
fn a_rebind_in_the_section_moves_what_the_chord_triggers() {
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
    assert!(
        !top.iter().any(|(_, _, action)| action == "rebind"),
        "a shortcut is rebound in the shortcuts section, not here: {top:?}"
    );

    let shortcuts = ui::storage::Settings::<settings_sections::shortcuts::ShortcutPrefs>::for_app(
        crate::shell::keyboard_shortcuts::APP_ID,
    );
    let mut section = shortcuts.get();
    section
        .rebind("quick-settings".into(), "Super+J".into())
        .expect("Super+J is free");
    assert!(
        section
            .rebind("quick-settings".into(), "Super+Space".into())
            .expect_err("Super+Space opens search")
            .0
            .contains("search")
    );
    shortcuts.set(&section).expect("saved");

    let listed = door.output("keyboard-shortcuts", "list", &json!({}));
    let row = quick_settings_row(&listed);
    assert_eq!(row["chords"], json!(["Super+J"]));
    assert_eq!(row["defaults"], json!(["Super+Q"]));
    assert_eq!(row["rebindable"], true);
    assert_eq!(row["category"], "system");

    // The compositor re-reads the saved table whenever the settings file
    // changes; this is that handoff.
    let (mut host, chrome) =
        crate::shell::build_tilo_overlay_host_with_handle(door.membership, false);
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
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the door owns the broker and its socket directory for the whole test"
)]
fn the_door_changes_no_shortcut() {
    let config = tempfile::tempdir().expect("temporary config directory");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", config.path());
    let dir = tempfile::tempdir().expect("socket directory");
    let socket = dir.path().join("collab.sock");
    let door = Door::open(&socket, dir);
    let agent = Caller::local_agent(AGENT);
    for (caller, resource, action) in [
        (&door.owner, "keyboard-shortcuts", "reset-all"),
        (&door.owner, "keyboard-shortcut:quick-settings", "rebind"),
        (&agent, "keyboard-shortcut:quick-settings", "rebind"),
    ] {
        assert!(
            matches!(
                door.invoke(caller, resource, action, &json!({"chord": "Super+J"})),
                Err(ActionHostError::NotPermitted | ActionHostError::UnknownAction)
            ),
            "{resource}#{action} is the shortcuts section's, not this door's"
        );
    }
    assert_eq!(
        settings_store::hardware::ShortcutConfig::load(),
        settings_store::hardware::ShortcutConfig::default(),
        "nothing was saved"
    );
}
