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

use tilo_action_services::environment_device::ItemUse;
use tilo_action_services::environment_window::ItemLent;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, ItemRef, Pending, ReceiptStatus, pending,
};
use tilo_actions::{ActionOutput, InvocationId};
use tilo_person_services::vault_release::AppProcess;
use tilo_workspace_records::vault as workspace_vault;

use super::services::Daemon;
use super::vm_environment::HostAsks;
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
        // Whoever made the running call of the app's action a page's login step continues: the
        // fill is decided as them as well as the app.
        let asked_by = request.on_behalf_of.and_then(|running| {
            daemon
                .app_broker()?
                .running_caller(&self.app, &self.workspace, running)
                .inspect_err(|why| tracing::info!(app = %self.app, %why, "a page login step continues no running call"))
                .ok()
        });
        // A fill the host that placed this workspace VM handed its Browser for a turn it carries
        // out is lent by that host, which keeps what decides it; every other fill, the person's
        // own, is decided by the vault here.
        if asked == Some(workspace_vault::PAGE_FILL)
            && let Some(host) = daemon.host_asks()
            && lent_by_host(asked_by.as_ref(), daemon.placing_host())
        {
            return from_the_host(host, &self.workspace, &request);
        }
        let (completer, answer) = pending();
        tilo_rt::app().spawn(async move {
            let invocation = request.invocation;
            let at = (app.as_str(), workspace.as_str());
            let services = PersonServices::of(&workspaces);
            let answered = match asked {
                Some(action) if workspace_vault::PAGE_LOGIN_ACTIONS.contains(&action) => {
                    services
                        .page_login(at, &process, &request, asked_by.as_ref())
                        .await
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

/// A page fill (`request`) a workspace VM's Browser asks for in `workspace`, asked of the host
/// that placed the VM ([`HostAsks::lend`]): for the running call it continues, the item it names
/// and the site its engine says the page is at. The host decides whether it hands the text; this
/// machine only opens what the host sealed to it for that one fill and site.
fn from_the_host(
    host: &HostAsks,
    workspace: &str,
    request: &InvokeRequest,
) -> Pending<Result<InvocationReceipt, ActionCallError>> {
    let Some(running) = request.on_behalf_of else {
        return Pending::ready(Err(ActionCallError::refused(
            tilo_person_services::page_logins::ASKED_BY_NOBODY,
        )));
    };
    let Some(item) = ItemRef::addressed(&request.target, workspace_vault::PAGE_FILL) else {
        return Pending::ready(Err(ActionCallError::refused(
            "That is no login of the vault's",
        )));
    };
    let site = match &request.argument {
        InvokeArgument::Json(arguments) => arguments
            .get("site")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        _ => None,
    };
    let Some(origin) = site else {
        return Pending::ready(Err(ActionCallError::InvalidRequest {
            reason: "a page's fill names the site its page is at".to_owned(),
        }));
    };
    let invocation = request.invocation;
    let used = ItemUse {
        invocation: running,
        item: item.to_string(),
        origin,
    };
    host.lend(workspace, used).map(move |lent| match lent {
        ItemLent::Text { text, .. } => Ok(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(serde_json::json!({"username": "", "secret": text})),
                    resources: Vec::new(),
                },
            },
        }),
        ItemLent::Asked { words } => Err(ActionCallError::refused(words)),
        ItemLent::Refused { reason } => Err(ActionCallError::refused(reason)),
    })
}

/// Whether a page fill asked for the running call whose caller is `asked_by` is lent by the host
/// that placed this machine (`placing`): a call that host made itself, for a turn it carries out.
/// A call any other device made, the person's own, is not, and neither is one on a machine no
/// host placed.
fn lent_by_host(asked_by: Option<&Caller>, placing: Option<[u8; 32]>) -> bool {
    asked_by
        .and_then(Caller::device_public_key)
        .zip(placing)
        .is_some_and(|(device, host)| *device == host)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the call the placing host made itself is lent by it: the person's own call on their
    /// device is decided by the vault on this machine, and a machine no host placed asks no host.
    #[test]
    fn a_fill_is_lent_by_the_host_only_for_a_call_the_host_made() {
        let (host, persons_device, person) = ([1; 32], [2; 32], [3; 32]);
        let by_host = Caller::authenticated(person, host);
        let by_person = Caller::authenticated(person, persons_device);
        assert!(lent_by_host(Some(&by_host), Some(host)));
        assert!(!lent_by_host(Some(&by_person), Some(host)));
        assert!(!lent_by_host(Some(&by_host), None));
        assert!(!lent_by_host(None, Some(host)));
    }
}
