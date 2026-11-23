//! The layouts of the workspaces this device keeps, as the agents acting here put views on them.
//!
//! A layout is read as the device's own read of the workspace's document, and a window opened or
//! closed on it is written as the device's own write: signed as the device and committed through
//! the workspace service's one commit path, so it is kept and carried to the workspace's other
//! devices as any of its writes is ([`tilo_workspace_service::device_documents`]).
//!
//! An agent's "show this" (its app's `open` on the application, or on a record of it) puts the
//! view on its person's layout here, where every device of theirs shows it, and starts nothing.

use std::sync::Arc;

use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, ActionCallError, Caller, InvocationReceipt, InvokeRequest, Pending,
    ReceiptStatus, application_provider, application_provider_resource,
};
use tilo_desktop_model::WorkspaceId;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
use tilo_peer_state::workspace_documents::{
    DeviceDocuments as _, DeviceWrites as _, DocumentRead, DocumentWriting,
};
use tilo_workspace_service::device_documents::{ServiceDocuments, ServiceWrites};

use super::workspaces::{ServiceWorkspaces, block_on_owned};

/// The workspace's layout as this device holds it.
enum HeldLayout {
    /// The layout document.
    Held(automerge::AutoCommit),
    /// This device keeps the workspace and holds no layout: none was written yet.
    Unwritten,
    /// This device does not hold it (yet): another device keeps it.
    Elsewhere,
}

/// The layouts of the workspaces `workspaces` keeps, read and written as this device.
pub(crate) struct DeviceLayouts {
    workspaces: Arc<ServiceWorkspaces>,
}

impl DeviceLayouts {
    pub(crate) const fn new(workspaces: Arc<ServiceWorkspaces>) -> Self {
        Self { workspaces }
    }

    /// The person this device is, as a layout names whose a window is.
    fn person(&self) -> String {
        hex::encode(self.workspaces.device_cert().user_public_key)
    }

    /// `workspace`'s shared layout as this device holds it now.
    fn layout(&self, workspace: &str) -> HeldLayout {
        let copies = self.workspaces.copies().clone();
        let read = ServiceDocuments::of(copies.clone()).read(
            workspace,
            DESKTOP_APP,
            DESKTOP_DOCUMENT,
            None,
        );
        match read {
            Ok(DocumentRead::Whole { bytes, .. }) => {
                automerge::AutoCommit::load(&bytes).map_or(HeldLayout::Elsewhere, HeldLayout::Held)
            }
            // A workspace this device keeps holds every layout written in it: none here is none.
            Ok(DocumentRead::Absent) => {
                let Some(id) = WorkspaceId::parse(workspace) else {
                    return HeldLayout::Elsewhere;
                };
                if block_on_owned(async move { copies.governed_copy(id).await.is_some() }) {
                    HeldLayout::Unwritten
                } else {
                    HeldLayout::Elsewhere
                }
            }
            _ => HeldLayout::Elsewhere,
        }
    }

    /// Write `changes` into `workspace`'s layout as this device: the whole document when
    /// `founding`, a change of the one held otherwise.
    fn write(&self, workspace: &str, changes: &[u8], founding: bool) -> Result<(), String> {
        let written = ServiceWrites::of(self.workspaces.copies().clone()).write(
            workspace,
            changes,
            DocumentWriting {
                app: DESKTOP_APP,
                document: DESKTOP_DOCUMENT,
                readers: None,
                founding,
                snapshot: founding,
                sealed: None,
            },
        );
        block_on_owned(written)
            .map(drop)
            .map_err(|refusal| format!("the layout was not written: {refusal}"))
    }

    /// Put a window of `app` (`(id, title)`) showing `document` (empty: the app's primary record)
    /// on `workspace`'s layout, as this device's person's, opened by the agent session `opened_by`
    /// names. A record a window shows already is left where the people put it. A workspace this
    /// device keeps with no layout yet gets one, written as a page first writes it; where another
    /// device keeps the layout, the view is asked for in its document
    /// ([`tilo_desktop_model::asked`]) and that device places it.
    ///
    /// # Errors
    /// The layout has no place for the window, or the write was refused.
    pub(crate) fn place(
        &self,
        workspace: &str,
        (app, title, document): (&str, &str, &str),
        opened_by: Option<tilo_desktop_model::ownership::OpenedBy>,
    ) -> Result<(), String> {
        let person = self.person();
        let held = match self.layout(workspace) {
            HeldLayout::Held(held) => held,
            HeldLayout::Unwritten => {
                let name = WorkspaceId::parse(workspace)
                    .and_then(|id| self.workspaces.workspace(id))
                    .map(|kept| kept.name)
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| "Workspace".to_owned());
                let founded = founded_layout(workspace, &name);
                self.write(workspace, &founded.clone().save(), true)?;
                founded
            }
            HeldLayout::Elsewhere => {
                let view = tilo_desktop_model::asked::AskedView {
                    app_id: app.to_owned(),
                    title: title.to_owned(),
                    document: document.to_owned(),
                    person: person.clone(),
                    opened_by,
                };
                let mut ask = automerge::AutoCommit::new();
                tilo_desktop_model::asked::ask(
                    &mut ask,
                    &format!("{person}:{app}:{document}"),
                    &view,
                )
                .map_err(|error| error.to_string())?;
                ask.commit();
                return self.write(workspace, &ask.save(), true);
            }
        };
        let view = tilo_desktop_model::place::View {
            app_id: app,
            title,
            document,
            beside: None,
        };
        let opener = tilo_desktop_model::place::Opener {
            person: &person,
            opened_by,
        };
        let change = tilo_desktop_model::place::place_view(&held, &opener, &view)
            .map_err(|why| format!("{title}'s window is not on the person's layout: {why}"))?;
        match change {
            Some(change) => self.write(workspace, &change, false),
            None => Ok(()),
        }
    }
}

/// The layout a workspace's first window is placed on: the workspace by its own id and one strip,
/// in the shape a page keeps its layout in ([`tilo_desktop_model::desktop_doc`]).
fn founded_layout(workspace: &str, name: &str) -> automerge::AutoCommit {
    use tilo_desktop_model::{Layout, Strip, StripId, Workspace};
    let id = WorkspaceId::parse(workspace).unwrap_or_else(|| WorkspaceId::from_u128(1));
    let mut shown = Workspace::new(id, name);
    shown.strips = vec![Strip::new(StripId(1), "Main")];
    let tree = Layout {
        active: id,
        workspaces: vec![shown],
        ..Layout::default()
    };
    let mut doc = automerge::AutoCommit::new();
    tilo_desktop_model::desktop_doc::write_shared(&mut doc, &tree, &[], &[], &Default::default());
    doc.commit();
    doc
}

/// The view an agent's `request` asks to be shown, when it is its app's `open` in `workspace`:
/// placed on the layout of the person the agent acts for, named as the agent session that opened
/// it, answered once it is written. `None` for any other request. `title` names an app by its id.
pub(super) fn show_on_layout(
    workspaces: &Arc<ServiceWorkspaces>,
    workspace: &str,
    (caller, request): (&Caller, &InvokeRequest),
    title: impl FnOnce(&str) -> String,
) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
    if request.target.address.action != APPLICATION_OPEN_ACTION {
        return None;
    }
    let (named, app) = application_provider(&request.target.provider)?;
    if named != workspace {
        return None;
    }
    let document = application_provider_resource(&request.target.provider)
        .unwrap_or_default()
        .to_owned();
    let (app, title) = (app.to_owned(), title(app));
    let opened_by = opened_by(caller, request);
    let layouts = DeviceLayouts::new(Arc::clone(workspaces));
    let (workspace, invocation) = (workspace.to_owned(), request.invocation);
    let (done, answer) = tilo_actions::remote::pending();
    // The layout is read and written as the device, which waits on the workspace service: never
    // on the caller's thread.
    std::thread::spawn(move || {
        let placed = layouts.place(&workspace, (&app, &title, &document), opened_by);
        done.complete(Ok(InvocationReceipt {
            invocation,
            status: match placed {
                Ok(()) => ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                },
                Err(reason) => ReceiptStatus::Rejected {
                    error: ActionCallError::Unavailable { reason },
                },
            },
        }));
    });
    Some(answer)
}

/// The agent session that opens a window for `request`: what the window's mark says beside it.
fn opened_by(
    caller: &Caller,
    request: &InvokeRequest,
) -> Option<tilo_desktop_model::ownership::OpenedBy> {
    let session = caller.agent_session()?;
    // What the agent called the step that opened it, in its own words: never its session's
    // name, which the mark says already.
    let title = request
        .context
        .as_ref()
        .and_then(|context| context.group.as_ref())
        .filter(|group| group.kind == tilo_actions::context::AGENT_SESSION_GROUP)
        .and_then(|group| group.label.as_deref())
        .and_then(|label| {
            tilo_actions::context::one_line(label, tilo_actions::context::MAX_LABEL_CHARS)
        })
        .filter(|title| request.said_session_name().as_deref() != Some(title.as_str()))
        .unwrap_or_default();
    Some(tilo_desktop_model::ownership::OpenedBy {
        run: session.id,
        agent: caller.acting().agent_id().unwrap_or_default().to_owned(),
        label: caller
            .clone()
            .in_session_of(request)
            .agent_called()
            .unwrap_or_default()
            .to_owned(),
        title,
    })
}
