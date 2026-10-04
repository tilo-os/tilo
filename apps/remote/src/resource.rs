//! Remote Desktop as a [`tilo_resource::Resource`]: saved connections sync;
//! connect / disconnect / SSH host-key prompts are effects of the live
//! surface, and a connection's credentials stay in this device's secret store.

use std::rc::Rc;

use ui::framework::{Fixture, LiveRow, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionParam, ActionSpec, OperationKind, ParamType};
use ui::tilo_resource::items::{ItemAction, ItemCollection, item_children, item_resource_id};
use ui::tilo_resource::{ActionDecl, Doc, Id, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;
use uuid::Uuid;

use crate::actions::{
    REMOTE_ACTION_TYPE, RemoteAction, delete_action, register, rename_action, set_favorite_action,
};
use crate::app::RemoteApp;
use crate::doc::{CONNECTIONS, RemoteDoc};
use crate::model::{AuthMethod, Connection, ConnectionId, ConnectionKind};

/// The remote connections list resource.
pub struct Remote;

/// The item parameter of every per-connection action.
const CONNECTION: &str = "connection";

/// The published id of a saved connection.
pub fn connection_resource_id(id: ConnectionId) -> String {
    item_resource_id::<Remote>(CONNECTIONS, &Id::from_raw(id.to_string()))
}

fn connection_id(args: &tilo_actions::ActionArgs) -> Result<ConnectionId, String> {
    args.text(CONNECTION)
        .and_then(|id| id.parse().ok())
        .ok_or_else(|| "`connection` names no saved connection".into())
}

fn text(args: &tilo_actions::ActionArgs, name: &str) -> String {
    args.text(name).unwrap_or_default().trim().to_owned()
}

impl Resource for Remote {
    const KIND: &'static str = "remote.connection-list";
    const LABEL: &'static str = "Remote connections";
    const ACTION_TYPE: &'static str = REMOTE_ACTION_TYPE;

    type State = Vec<Connection>;
    type Action = RemoteAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<Vec<Connection>, ModelError> {
        Ok(RemoteDoc::list(doc))
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        let connection = || ActionParam::required(CONNECTION, ParamType::Text, "Connection");
        vec![
            ActionDecl::new(
                "add-connection",
                "Add connection",
                vec![
                    ActionParam::required("name", ParamType::Text, "Name"),
                    ActionParam::required("host", ParamType::Text, "Host, or a Tilo endpoint id"),
                    ActionParam::required("kind", ParamType::Text, "SSH, VNC or Tilo"),
                    ActionParam::optional("port", ParamType::Integer, "Port"),
                    ActionParam::optional("user", ParamType::Text, "User"),
                    ActionParam::optional("auth", ParamType::Text, "Password or Public key"),
                    ActionParam::optional("group", ParamType::Text, "Group"),
                ],
                |args, _| {
                    let kind = ConnectionKind::all()
                        .into_iter()
                        .find(|kind| kind.tag().eq_ignore_ascii_case(&text(args, "kind")))
                        .filter(|kind| kind.is_supported())
                        .ok_or("`kind` is SSH, VNC or Tilo")?;
                    let auth = match text(args, "auth") {
                        auth if auth.is_empty() => AuthMethod::available_for(kind)
                            .first()
                            .copied()
                            .unwrap_or(AuthMethod::Password),
                        auth => AuthMethod::from_label(&auth),
                    };
                    let port = match args.integer("port") {
                        Some(port) => u16::try_from(port).map_err(|_| "`port` is 1–65535")?,
                        None if kind == ConnectionKind::Tilo => 0,
                        None => kind.default_port(),
                    };
                    let draft = crate::model::ConnectionDraft {
                        name: text(args, "name"),
                        host: text(args, "host"),
                        port: port.to_string(),
                        user: text(args, "user"),
                        kind,
                        auth,
                        group: text(args, "group"),
                    };
                    if let Some(problem) = draft.problems().into_iter().next() {
                        return Err(problem);
                    }
                    Ok(crate::actions::upsert_action(&draft.build(Uuid::new_v4())))
                },
            )
            .describe("Save a new connection to another machine."),
            ActionDecl::new(
                "rename-connection",
                "Rename connection",
                vec![
                    connection(),
                    ActionParam::required("name", ParamType::Text, "New name"),
                ],
                |args, _| Ok(rename_action(connection_id(args)?, &text(args, "name"))),
            )
            .describe("Rename a saved connection."),
            ActionDecl::new(
                "set-favorite",
                "Set favourite",
                vec![
                    connection(),
                    ActionParam::required("favorite", ParamType::Boolean, "Favourite"),
                ],
                |args, _| {
                    let favorite = args.boolean("favorite").unwrap_or(true);
                    Ok(set_favorite_action(connection_id(args)?, favorite))
                },
            )
            .describe("Pin a saved connection to the top of the list, or unpin it."),
            ActionDecl::new(
                "delete-connection",
                "Delete connection",
                vec![connection()],
                |args, _| Ok(delete_action(connection_id(args)?)),
            )
            .describe("Remove a saved connection from every device.")
            .destructive(),
        ]
    }

    fn local_operations() -> Vec<ActionSpec> {
        crate::actions::list_view_specs()
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    /// Every saved connection, with its settings view, its connect effect
    /// and the actions bound to it.
    fn children(state: &Vec<Connection>) -> Vec<tilo_actions::Resource> {
        let collection = ItemCollection {
            field: CONNECTIONS,
            items: state
                .iter()
                .map(|connection| {
                    let label = if connection.name.trim().is_empty() {
                        connection.host.clone()
                    } else {
                        connection.name.clone()
                    };
                    (Id::from_raw(connection.id.to_string()), label)
                })
                .collect(),
        };
        let actions =
            ["rename-connection", "set-favorite", "delete-connection"].map(|action| ItemAction {
                action,
                param: CONNECTION,
                collection: Some(CONNECTIONS),
            });
        let mut children = item_children::<Self>(&[collection], &actions);
        for (child, connection) in children.iter_mut().zip(state) {
            child
                .actions
                .insert(0, crate::actions::open_spec(connection.id));
            child
                .actions
                .insert(1, crate::actions::connect_spec(connection));
        }
        children
    }

    fn search(state: &Vec<Connection>) -> Vec<SearchEntry> {
        state
            .iter()
            .map(|connection| SearchEntry {
                anchor: Some(connection_resource_id(connection.id)),
                title: connection.name.clone(),
                text: connection.host.clone(),
                action: None,
            })
            .collect()
    }
}

/// Framework app: the remote widget over [`Remote`].
pub struct RemoteShell;

/// The screenshot scenarios; a plain `--shots` is the saved-connection list.
const SCENARIOS: &[&str] = &[
    "narrow",
    "narrow-open",
    "form",
    "form-tilo",
    "session-ssh",
    "session-vnc",
    "session-rdp",
    "session-tilo",
    "session-connecting",
    "session-failed",
    "confirm-connect",
    "confirm-delete",
    "confirm-ssh-trust",
];

impl ResourceApp for RemoteShell {
    type Resource = Remote;
    const ID: &'static str = "so.tilo.remote";
    const TITLE: &'static str = "Remote Desktop";
    const ICON: Option<&'static str> = Some("remote");

    fn view(cx: &mut ViewCx<Remote>) -> Element {
        cx.surface(RemoteApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario.is_some_and(|name| name.starts_with("narrow")) {
            (560, 720)
        } else {
            (1100, 720)
        }
    }

    /// What a capture shows that the document does not hold: the time "last
    /// used" is read against, which machines answered, the Tilo devices
    /// nearby.
    fn fixture(fixture: &mut Fixture<Remote>) {
        fixture.provide(crate::app::Sample::new());
    }

    /// A lived-in list of saved machines.
    fn scenario(_name: Option<&str>) -> Vec<Invoke<Remote>> {
        crate::app::sample_connections()
            .iter()
            .flat_map(|connection| {
                let mut actions = vec![crate::actions::upsert_action(connection)];
                if connection.favorite {
                    actions.push(set_favorite_action(connection.id, true));
                }
                if let Some(at_ms) = connection.last_used_ms {
                    actions.push(crate::actions::mark_used_action(connection.id, at_ms));
                }
                actions
            })
            .filter_map(|action| Invoke::apply(&action).ok())
            .collect()
    }
}

impl ResourceSurface<Remote> for RemoteApp {
    fn present(&mut self, state: &Rc<Vec<Connection>>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Remote>) {
        self.attach_framework(&cx);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.live_session_rows()
    }
}
