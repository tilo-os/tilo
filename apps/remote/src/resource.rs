//! Remote Desktop as a [`tilo_resource::Resource`]: saved connections sync;
//! connect / disconnect / SSH host-key prompts are Effects.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::EncodedAction;
use ui::tilo_actions::{ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{REMOTE_ACTION_TYPE, RemoteAction, register};
use crate::app::RemoteApp;
use crate::doc::RemoteDoc;
use crate::model::Connection;

/// The remote connections list resource.
pub struct Remote;

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

    fn local_operations() -> Vec<ActionSpec> {
        // List-level View ops. Per-session connect/disconnect Effects (and SSH
        // host-key prompts) are published on children; there is no outbox for
        // undoing a live session — the connection record stays on the resource.
        crate::actions::list_view_specs()
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn children(state: &Vec<Connection>) -> Vec<tilo_actions::Resource> {
        crate::actions::connection_resources(state)
    }

    fn search(state: &Vec<Connection>) -> Vec<SearchEntry> {
        state
            .iter()
            .map(|connection| SearchEntry {
                anchor: Some(format!("remote.connection:{}", connection.id)),
                title: connection.name.clone(),
                text: format!(
                    "{} {} {}",
                    connection.kind.label(),
                    connection.address(),
                    connection.group
                ),
            })
            .collect()
    }
}

/// Framework app: the remote widget over [`Remote`].
pub struct RemoteShell;

impl ResourceApp for RemoteShell {
    type Resource = Remote;
    const ID: &'static str = "so.tilo.remote";
    const TITLE: &'static str = "Remote Desktop";
    const ICON: Option<&'static str> = Some("remote");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Remote>) -> Element {
        let app = RemoteApp::new();
        // A picked search hit shows its connection in the list.
        let opened = app.open_requests();
        cx.on_open(move |anchor| opened.borrow_mut().push(anchor.to_owned()));
        cx.surface(app)
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "empty",
            "pinned",
            "narrow",
            "form",
            "form-tilo",
            "form-invalid",
            "edit",
            "session-ssh",
            "session-vnc",
            "session-rdp",
            "session-tilo",
            "session-connecting",
            "session-failed",
            "session-closed",
            "confirm-connect",
            "confirm-delete",
            "confirm-ssh-trust",
        ]
    }

    fn scenario_viewport(name: &str) -> Option<(u32, u32)> {
        (name == "narrow").then_some((800, 600))
    }

    /// Every scenario but `empty` shows a lived-in list of saved machines,
    /// written through the same actions the form, the favourite control and
    /// a finished session use.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Remote>> {
        if name == Some("empty") {
            return Vec::new();
        }
        let now = ui::clock::local_now().timestamp();
        crate::app::sample_connections(now)
            .iter()
            .flat_map(|connection| {
                let mut actions = vec![crate::actions::upsert_action(connection)];
                if connection.favorite {
                    actions.push(crate::actions::set_favorite_action(connection.id, true));
                }
                if let Some(at) = connection.last_used {
                    actions.push(crate::actions::record_session_action(
                        connection.id,
                        at,
                        connection.presence,
                    ));
                }
                actions
            })
            .filter_map(|action| Invoke::apply(&action).ok())
            .collect()
    }
}

impl ResourceSurface<Remote> for RemoteApp {
    fn present(&mut self, state: &std::rc::Rc<Vec<Connection>>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Remote>) {
        self.attach_framework(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        RemoteApp::stage(self, scenario);
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Remote>>,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Remote>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &RemoteAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(crate::actions::encode(action));
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }
}

/// Live launch through the framework host.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<RemoteShell>()
}
