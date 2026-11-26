//! The vault's release on an app process's own connection
//! ([`tilo_person_services::vault_release`]), and the logins a window of the
//! app's own asks for its page ([`tilo_person_services::page_logins`]).
//!
//! The daemon started the process and knows it by its pid ([`crate::apps::ProcessOf`]): whether
//! it is a sandboxed launch of the app's own package, and whether that launch is the app's held
//! connection. A release frame on the
//! connection is answered here, on that connection alone, with what the workspace hands that
//! process: the line it is answered on is written nowhere else.

use std::sync::{Arc, Weak};
use tilo_person_services::PersonServices;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, ReceiptStatus, pending,
};
use tilo_actions::{ActionOutput, InvocationId};
use tilo_person_services::vault_release::AppProcess;
use tilo_workspace_service::workspace_vault;

use super::services::Daemon;
use crate::apps::ProcessOf;

/// `answered` as the value the call is answered with.
fn answer_of<T: serde::Serialize>(
    answered: Result<T, ActionCallError>,
) -> Result<serde_json::Value, ActionCallError> {
    answered.and_then(|value| {
        serde_json::to_value(value).map_err(|_| ActionCallError::Unavailable {
            reason: "the vault's answer could not be written".into(),
        })
    })
}

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
        let asked = workspace_vault::asked_by_app(&request.target);
        let (completer, answer) = pending();
        tilo_rt::app().spawn(async move {
            let invocation = request.invocation;
            let at = (app.as_str(), workspace.as_str());
            let services = PersonServices::of(&workspaces);
            let answered = match asked {
                Some(action) if workspace_vault::PAGE_LOGIN_ACTIONS.contains(&action) => {
                    services.page_login(at, &process, &request).await
                }
                Some(workspace_vault::FIND) => {
                    answer_of(services.find_tagged(at, &process, &request).await)
                }
                Some(workspace_vault::WEB_CALL) => {
                    services.web_call_to(at, &process, &request).await
                }
                Some(workspace_vault::DEVICE_KEY) => {
                    answer_of(services.device_key_for(at, &process, &request).await)
                }
                Some(workspace_vault::KEEP) => {
                    answer_of(services.keep_for(at, &process, &request).await)
                }
                _ => answer_of(services.release_to(at, &process, &request).await),
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
/// ([`workspace_vault::PAGE_LOGIN_ACTIONS`]), or a command to its app's records
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
            if workspace_vault::asked_by_app(&request.target).is_some()
    )
}
