//! The vault's release on an app process's own connection
//! ([`tilo_workspace_service::workspaces::vault_release`]).
//!
//! The daemon started the process and knows it by its pid ([`crate::apps::ProcessOf`]): whether
//! it is a sandboxed launch of the app's own package, and whether that launch is the app's held
//! connection. A release frame on the
//! connection is answered here, on that connection alone, with what the workspace hands that
//! process: the line it is answered on is written nowhere else.

use std::sync::{Arc, Weak};

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, ReceiptStatus, pending,
};
use tilo_actions::{ActionOutput, InvocationId};
use tilo_workspace_service::workspaces::vault_release::AppProcess;

use super::services::Daemon;
use crate::apps::ProcessOf;

/// The provider a release frame from process `pid`, the app `app` in `workspace`, is answered
/// by.
#[derive(Clone)]
pub(crate) struct OwnProcess {
    daemon: Weak<Daemon>,
    pid: u32,
    app: String,
    workspace: String,
}

impl OwnProcess {
    pub(crate) fn new(daemon: &Arc<Daemon>, pid: u32, app: &str, workspace: &str) -> Self {
        Self {
            daemon: Arc::downgrade(daemon),
            pid,
            app: app.to_owned(),
            workspace: workspace.to_owned(),
        }
    }

    /// What process of the app `pid` is now, as the daemon's launch records say.
    fn process(&self, daemon: &Daemon) -> Result<AppProcess, ActionCallError> {
        let gone = || {
            ActionCallError::refused(format!(
                "This connection's process is no longer the one this computer started for {}, \
                 so it is released nothing.",
                self.app
            ))
        };
        match daemon.apps().process_of(self.pid).ok_or_else(gone)? {
            ProcessOf::Started { app, workspace }
                if app == self.app && workspace == self.workspace =>
            {
                Ok(AppProcess::Tilolang)
            }
            ProcessOf::Launched {
                app,
                workspace,
                connection,
            } if app == self.app && workspace == self.workspace => Ok(if connection {
                AppProcess::connection(&app)
            } else {
                AppProcess::Unplaced
            }),
            _ => Err(gone()),
        }
    }
}

impl ActionProvider for OwnProcess {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::refused(
            "this connection's process lists its actions through the workspace",
        )))
    }

    fn invoke(
        &self,
        _: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let Some(daemon) = self.daemon.upgrade() else {
            return Pending::ready(Err(ActionCallError::Unavailable {
                reason: "this computer's daemon is stopping".into(),
            }));
        };
        let process = match self.process(&daemon) {
            Ok(process) => process,
            Err(refused) => return Pending::ready(Err(refused)),
        };
        let workspaces = daemon.workspaces().clone();
        let (app, workspace) = (self.app.clone(), self.workspace.clone());
        let (completer, answer) = pending();
        tilo_rt::app().spawn(async move {
            let invocation = request.invocation;
            let released = workspaces
                .release_to((&app, &workspace), &process, &request)
                .await
                .and_then(|released| {
                    serde_json::to_value(&released).map_err(|_| ActionCallError::Unavailable {
                        reason: "the released item could not be answered".into(),
                    })
                });
            completer.complete(released.map(|value| InvocationReceipt {
                invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                },
            }));
        });
        answer
    }

    fn observe(&self, _: &Caller, _: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _: &Caller, _: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Ok(()))
    }
}

/// Whether `frame` is a release this connection's own process asks for.
pub(crate) fn is_release(frame: &tilo_actions::remote::RemoteActionFrame) -> bool {
    matches!(
        &frame.message,
        tilo_actions::remote::RemoteActionMessage::Invoke(request)
            if tilo_actions::remote::person_vault::is_release(&request.target)
    )
}
