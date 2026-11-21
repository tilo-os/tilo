//! What a person asks of their servers, served on their Servers workspace.
//!
//! Each operation a server is asked for is one action here (`server.status`,
//! `server.logs`, `server.restart`, `server.reboot`, `server.update`,
//! `server.pause-updates`, `server.resume-updates`, `server.rollback`,
//! `server.drain`, `server.set-default-start`). An action keeps the request in
//! that server's entry in the workspace's document; the server the entry names
//! carries it out and keeps what it found or did; and the action answers with
//! that, read live for the operations that read (its health and a service's
//! journal).
//!
//! Every device of the person holds that workspace and asks these, and so does
//! the agent at that device: what a person may ask of their own servers is
//! theirs, not a grant's. Another person reaches none of it.

use super::{
    ActionCallError, ActionKind, ActionParam, Args, CHOOSES_SERVERS, CatalogueRow, Governed, Id,
    ParamType, Verb, Workspaces, refused, roles,
};
use crate::workspaces::servers;
use tilo_actions::remote::InvokeRequest;
use tilo_user_identity::DeviceCert;

/// One thing a person asks of one of their servers, served on their Servers
/// workspace.
pub(super) struct ServerVerb {
    /// The action, as the workspace's governance verbs are declared.
    pub(super) verb: Verb,
    /// The operation it asks of the server.
    pub(super) operation: &'static str,
}

/// How long an action that asks a server waits for its answer: longer than a
/// server waits for its own root helper to carry a restart out, so a slow
/// restart still answers, and short of leaving a caller hanging on a server
/// that is away.
const SERVER_ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(90);

/// How often a waiting action reads the Servers document again: an answer can
/// reach this device's copy without its write watch hearing it.
const ANSWER_TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// What a person asks of a server, in catalogue order: each is one operation
/// on that server's entry in the workspace.
pub(super) const SERVER_VERBS: [ServerVerb; 10] = [
    asking(
        "server.status",
        "status",
        "Ask a server how it is",
        ActionKind::Read,
        status_params,
    ),
    asking(
        "server.logs",
        "logs",
        "Read a server's journal",
        ActionKind::Read,
        logs_params,
    ),
    asking(
        "server.restart",
        "restart",
        "Restart a service on a server",
        roles::GOVERNANCE_KIND,
        service_params,
    ),
    asking(
        "server.reboot",
        "reboot",
        "Reboot a server",
        roles::GOVERNANCE_KIND,
        status_params,
    ),
    asking(
        "server.update",
        "update",
        "Fetch and stage an update on a server",
        roles::GOVERNANCE_KIND,
        status_params,
    ),
    asking(
        "server.pause-updates",
        "pause-updates",
        "Stop a server taking updates",
        roles::GOVERNANCE_KIND,
        status_params,
    ),
    asking(
        "server.resume-updates",
        "resume-updates",
        "Have a server take updates again",
        roles::GOVERNANCE_KIND,
        status_params,
    ),
    asking(
        "server.rollback",
        "rollback",
        "Have a server start the previous system",
        roles::GOVERNANCE_KIND,
        status_params,
    ),
    asking(
        "server.drain",
        "drain",
        "Drain a server before taking it out of service",
        roles::GOVERNANCE_KIND,
        status_params,
    ),
    asking(
        "server.set-default-start",
        "set-default-start",
        "Have Tilo start by default on a server",
        roles::GOVERNANCE_KIND,
        status_params,
    ),
];

const fn asking(
    id: &'static str,
    operation: &'static str,
    label: &'static str,
    kind: ActionKind,
    params: fn() -> Vec<ActionParam>,
) -> ServerVerb {
    ServerVerb {
        verb: Verb {
            id,
            label,
            check: CHOOSES_SERVERS,
            kind,
            params,
        },
        operation,
    }
}

/// The action `action` is, when it asks a server an operation.
pub(super) fn verb(action: &str) -> Option<&'static ServerVerb> {
    SERVER_VERBS.iter().find(|verb| verb.verb.id == action)
}

/// The rows of what a person asks of their servers, published on `workspace`.
pub(super) fn rows(workspace: &crate::workspaces::GovernedWorkspace) -> Vec<CatalogueRow> {
    SERVER_VERBS
        .iter()
        .map(|verb| {
            verb.verb
                .governance_row(workspace.workspace(), &workspace.name())
        })
        .collect()
}

fn server_param() -> ActionParam {
    ActionParam::required(
        "server",
        ParamType::Text,
        "The server: the device key of its entry in this workspace, 64 hex characters.",
    )
}

fn status_params() -> Vec<ActionParam> {
    vec![server_param()]
}

fn service_params() -> Vec<ActionParam> {
    vec![
        server_param(),
        ActionParam::required(
            "service",
            ParamType::Text,
            "The systemd unit, e.g. tilo-server.service.",
        ),
    ]
}

fn logs_params() -> Vec<ActionParam> {
    vec![
        server_param(),
        ActionParam::required(
            "service",
            ParamType::Text,
            "The systemd unit, e.g. tilo-server.service.",
        ),
        ActionParam::optional(
            "lines",
            ParamType::Integer,
            "How many lines back to read, at most 500; 200 when omitted.",
        ),
    ]
}

impl Workspaces {
    /// The Servers workspace `workspace_id` carries and the certificate of
    /// `caller`'s device, when `caller` may ask what its person's servers are
    /// to do: one of their devices speaking for themselves, or the agent their
    /// device, and the workspace their own
    /// ([`person_workspace_id`](tilo_server_protocol::servers::person_workspace_id)).
    /// Another person's device, or an agent of theirs, reaches no server of
    /// this person's. Unknown and forbidden are the same answer.
    pub(super) async fn server_operator(
        &self,
        workspace_id: tilo_server_protocol::WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        certificate: Option<&DeviceCert>,
    ) -> Option<(Governed, DeviceCert)> {
        let certificate = certificate?;
        let person = caller.user_public_key()?;
        if certificate.user_public_key != *person
            || caller.device_public_key() != Some(&certificate.device_public_key)
            || workspace_id.to_string() != tilo_server_protocol::servers::person_workspace_id(person)
            || !(caller.acting().is_person() || caller.is_session_delegated())
        {
            return None;
        }
        Some((self.held(workspace_id).await?, certificate.clone()))
    }

    /// Ask the server `verb` names an operation of in the person's Servers
    /// workspace, and answer what it did: the operation, its result and
    /// whether it is done, as the server recorded them, read live for the ones
    /// that read (its health and a service's journal).
    pub(super) async fn server_operation_invoke(
        &self,
        workspace_id: tilo_server_protocol::WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        certificate: Option<&DeviceCert>,
        verb: &'static ServerVerb,
        request: &InvokeRequest,
    ) -> Result<serde_json::Value, ActionCallError> {
        let (governed, _device) = self
            .server_operator(workspace_id, caller, certificate)
            .await
            .ok_or(ActionCallError::UnknownAction)?;
        let workspace = governed.lock().await.workspace().to_owned();
        let args = Args::of(&request.argument)?;
        let server = args.required_text("server")?.trim().to_owned();
        let service = args.text("service")?.unwrap_or_default().to_owned();
        let lines = u32::try_from(args.integer("lines")?.unwrap_or(200)).unwrap_or(200);
        let request_id = uuid::Uuid::new_v4().simple().to_string();
        // Its answer comes as another write to the same document: listening
        // before the ask is what keeps it from being missed.
        let mut writes = self.document_writes();
        servers::apply_on(
            self,
            &workspace,
            servers::Servers::ASK_OPERATION.with((
                Id::from_raw(server.clone()),
                request_id.clone(),
                verb.operation.to_owned(),
                service,
                lines,
            )),
        )
        .await
        .map_err(|error| refused(&error))?;
        let asked = self
            .wait_for_answer(&workspace, &server, &request_id, &mut writes)
            .await?;
        let result = asked
            .result()
            .ok_or_else(|| refused(&"the server's answer does not read"))?;
        serde_json::to_value(result).map_err(|error| refused(&error))
    }

    /// The answer the server `server` recorded to the request `request` in
    /// the Servers workspace `workspace`, once it did, or why none came.
    async fn wait_for_answer(
        &self,
        workspace: &str,
        server: &str,
        request: &str,
        writes: &mut tokio::sync::broadcast::Receiver<crate::workspaces::ResourceWrite>,
    ) -> Result<servers::Asked, ActionCallError> {
        let waiting = async {
            loop {
                let answered = servers::read_on(self, workspace)
                    .await
                    .ok()
                    .and_then(|servers| {
                        servers
                            .servers
                            .get(&Id::from_raw(server.to_owned()))
                            .and_then(|entry| entry.asked.get(&Id::from_raw(request.to_owned())))
                            .filter(|asked| !asked.answer.is_empty())
                            .cloned()
                    });
                if let Some(asked) = answered {
                    return Ok(asked);
                }
                tokio::select! {
                    heard = writes.recv() => match heard {
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            return Err(refused(&"the server did not answer"));
                        }
                    },
                    // An answer reaches this copy through a holder's, which
                    // the local write watch does not always hear.
                    () = tokio::time::sleep(ANSWER_TICK) => {}
                }
            }
        };
        tokio::time::timeout(SERVER_ANSWER_WAIT, waiting)
            .await
            .unwrap_or_else(|_| {
                Err(refused(
                    &"the server did not answer; it may be away, and does it when it is back",
                ))
            })
    }
}

/// Take a machine that enrolled with a plan's token into its entry, under its
/// device key: what the person's device that approves the machine does, so the
/// holders hand it their Servers workspace and it reads its plan there. The
/// drives and questions it reported ride along when the caller has them; the
/// machine's own report updates the same entry once it holds the workspace.
pub(super) const MACHINE_ADMIT: &str = "machine.admit";

impl Workspaces {
    /// [`MACHINE_ADMIT`]: one of the person's own devices admits a machine it
    /// approved into the machine's waiting entry in their Servers workspace.
    pub(super) async fn machine_admit(
        &self,
        call: super::GovernanceCall<'_>,
    ) -> Result<serde_json::Value, ActionCallError> {
        // The Servers workspace is the person's own: no other member admits a
        // machine into it.
        if call.device.user_public_key != self.inner.host.cert.user_public_key {
            return Err(ActionCallError::UnknownAction);
        }
        let device = call.args.required_text("device")?.to_owned();
        let nonce = call.args.required_text("nonce")?.to_owned();
        let drives = call.args.text("drives")?.unwrap_or("[]").to_owned();
        let ask = call
            .args
            .text("ask")?
            .map_or_else(String::new, str::to_owned);
        Box::pin(servers::apply_on(
            self,
            call.workspace,
            servers::Servers::BEGIN_INSTALL.with((device, nonce, drives, ask)),
        ))
        .await
        .map_err(|error| refused(&error))?;
        Ok(serde_json::json!({ "admitted": true }))
    }
}

/// The governance check `action` is held to, when it is one this file serves:
/// taking a planned machine in is the person's own devices' own, decided in
/// its handler, so it is no verb the catalogue offers.
pub(super) fn check_of(action: &str) -> Option<&'static str> {
    (action == MACHINE_ADMIT)
        .then_some("")
        .or_else(|| super::check_of(action))
}
