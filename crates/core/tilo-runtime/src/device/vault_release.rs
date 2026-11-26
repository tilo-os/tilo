//! The vault's release on an app process's own connection
//! ([`tilo_workspace_service::workspaces::vault_release`]), and the logins a window of the
//! app's own asks for its page ([`tilo_workspace_service::workspaces::page_logins`]).
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
    /// The process's own records ([`super::store_lane`]), kept for the connection's life: a
    /// change sent in parts is gathered there.
    store: Arc<super::store_lane::StoreLane>,
}

impl OwnProcess {
    pub(crate) fn new(daemon: &Arc<Daemon>, pid: u32, app: &str, workspace: &str) -> Self {
        Self {
            daemon: Arc::downgrade(daemon),
            pid,
            app: app.to_owned(),
            workspace: workspace.to_owned(),
            store: Arc::new(super::store_lane::StoreLane::new(daemon, app, workspace)),
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
            ProcessOf::Started {
                app,
                workspace,
                own,
            } if app == self.app && workspace == self.workspace => Ok(if own {
                AppProcess::OwnTilolang
            } else {
                AppProcess::Tilolang
            }),
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
        let find = tilo_actions::remote::person_vault::is_find(&request.target);
        let call = tilo_actions::remote::person_vault::is_web_call(&request.target);
        let keep = tilo_actions::remote::person_vault::is_keep(&request.target);
        let device_key = tilo_actions::remote::person_vault::is_device_key(&request.target);
        let (completer, answer) = pending();
        tilo_rt::app().spawn(async move {
            let invocation = request.invocation;
            let answered = if tilo_actions::remote::person_vault::is_page_login(&request.target) {
                workspaces
                    .page_login((&app, &workspace), &process, &request)
                    .await
            } else if find {
                workspaces
                    .find_tagged((&app, &workspace), &process, &request)
                    .await
                    .and_then(|found| {
                        serde_json::to_value(&found).map_err(|_| ActionCallError::Unavailable {
                            reason: "the found item could not be answered".into(),
                        })
                    })
            } else if call {
                workspaces
                    .web_call_to((&app, &workspace), &process, &request)
                    .await
            } else if device_key {
                workspaces
                    .device_key_for((&app, &workspace), &process, &request)
                    .await
                    .and_then(|key| {
                        serde_json::to_value(&key).map_err(|_| ActionCallError::Unavailable {
                            reason: "the device key could not be answered".into(),
                        })
                    })
            } else if keep {
                workspaces
                    .keep_for((&app, &workspace), &process, &request)
                    .await
                    .and_then(|kept| {
                        serde_json::to_value(&kept).map_err(|_| ActionCallError::Unavailable {
                            reason: "the kept item could not be answered".into(),
                        })
                    })
            } else {
                workspaces
                    .release_to((&app, &workspace), &process, &request)
                    .await
                    .and_then(|released| {
                        serde_json::to_value(&released).map_err(|_| ActionCallError::Unavailable {
                            reason: "the released item could not be answered".into(),
                        })
                    })
            };
            completer.complete(answered.map(|value| InvocationReceipt {
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

    /// Only while the connection's process is still the one the daemon started for the app.
    fn store_commands(&self) -> Option<Arc<dyn tilo_actions::remote::StoreCommands>> {
        let daemon = self.daemon.upgrade()?;
        Some(match self.process(&daemon) {
            Ok(_) => Arc::clone(&self.store) as Arc<dyn tilo_actions::remote::StoreCommands>,
            Err(gone) => Arc::new(super::store_lane::Refused(gone)),
        })
    }
}

/// Whether `frame` is a release, a find, a web call, a keep or a device key this connection's own
/// process asks for, the ways an app's held connection reaches its vault items, one of the page
/// login actions a window of the app's own asks for its page
/// ([`tilo_actions::remote::person_vault::is_page_login`]), or a command to its app's records
/// ([`super::store_lane`]).
pub(crate) fn is_answered_for_this_process(
    frame: &tilo_actions::remote::RemoteActionFrame,
) -> bool {
    matches!(
        &frame.message,
        tilo_actions::remote::RemoteActionMessage::StoreRequest { .. }
    ) || matches!(
        &frame.message,
        tilo_actions::remote::RemoteActionMessage::Invoke(request)
            if tilo_actions::remote::person_vault::is_release(&request.target)
                || tilo_actions::remote::person_vault::is_find(&request.target)
                || tilo_actions::remote::person_vault::is_web_call(&request.target)
                || tilo_actions::remote::person_vault::is_keep(&request.target)
                || tilo_actions::remote::person_vault::is_device_key(&request.target)
                || tilo_actions::remote::person_vault::is_page_login(&request.target)
    )
}
