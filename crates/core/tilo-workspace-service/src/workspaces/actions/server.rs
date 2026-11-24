//! What a person changes of their servers' entries, served on their Servers
//! workspace. What the machine itself is asked to do (its health, a service's
//! journal, a restart, a reboot, an update) is its own daemon's `server` device
//! actions instead, answered by the machine.
//!
//! Naming a server (`server.rename`), choosing which workspaces of the
//! person's list it keeps (`server.keep`) and taking it out of service
//! (`server.retire`) are actions on the same entry. Retiring carries the
//! revocation of the server's device key the person's root signed: the server
//! takes it and lets go of everything it holds, and once its entry is retired
//! every holder of the workspace refuses that key.
//!
//! Every device of the person holds that workspace and asks these, and so does
//! the agent at that device: what a person may ask of their own servers is
//! theirs, not a grant's. Another person reaches none of it.

use super::{
    ActionCallError, ActionKind, ActionParam, Args, CHOOSES_SERVERS, CatalogueRow, Governed, Id,
    ParamType, Verb, Workspaces, roles,
};
use crate::workspaces::servers;
use tilo_actions::remote::InvokeRequest;
use tilo_user_identity::DeviceCert;

/// One thing a person asks of one of their servers, served on their Servers
/// workspace.
pub(super) struct ServerVerb {
    /// The action, as the workspace's governance verbs are declared.
    pub(super) verb: Verb,
    /// The change to its entry: [`RENAME`], [`KEEP`] or [`RETIRE`].
    pub(super) operation: &'static str,
}

/// Naming a server: a change to its entry, which the server takes the name of.
const RENAME: &str = "rename";
/// Taking a server out of service with its root-signed revocation.
const RETIRE: &str = "retire";
/// Having a server keep a workspace of the person's list, or not: a change to
/// its entry, which the server keeps or lets go of the workspace by.
const KEEP: &str = "keep";

/// How long an action that asks a server waits for its answer: longer than a
/// server waits for its own root helper to carry a restart out, so a slow
/// restart still answers, and short of leaving a caller hanging on a server
/// that is away.
const SERVER_ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(90);

/// How often a waiting action reads the Servers document again: an answer can
/// reach this device's copy without its write watch hearing it.
const ANSWER_TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// What a person changes of a server, in catalogue order: each is one change
/// to that server's entry in the workspace.
pub(super) const SERVER_VERBS: [ServerVerb; 3] = [
    asking(
        "server.rename",
        RENAME,
        "Rename a server",
        roles::GOVERNANCE_KIND,
        rename_params,
    ),
    asking(
        "server.keep",
        KEEP,
        "Choose whether a server keeps a workspace",
        roles::GOVERNANCE_KIND,
        keep_params,
    ),
    asking(
        "server.retire",
        RETIRE,
        "Take a server out of service",
        roles::GOVERNANCE_KIND,
        retire_params,
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

/// Whether `caller` asks in `workspace_id` as the workspace of their person that lists their
/// servers, where what they ask of those servers is published.
pub(super) fn where_servers_are_asked(
    workspace_id: tilo_server_protocol::WorkspaceId,
    caller: &tilo_actions::remote::Caller,
) -> bool {
    caller.user_public_key().is_some_and(|person| {
        workspace_id.to_string() == tilo_server_protocol::servers::person_workspace_id(person)
    })
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

fn rename_params() -> Vec<ActionParam> {
    vec![
        server_param(),
        ActionParam::required(
            "name",
            ParamType::Text,
            "The server's new name, one line of 1 to 80 bytes.",
        ),
    ]
}

fn keep_params() -> Vec<ActionParam> {
    vec![
        server_param(),
        ActionParam::required(
            "workspace",
            ParamType::Text,
            "The workspace of your list, by its id.",
        ),
        ActionParam::required(
            "keeps",
            ParamType::Boolean,
            "Whether the server keeps it: true to keep it, false to let go of it.",
        ),
    ]
}

fn retire_params() -> Vec<ActionParam> {
    vec![
        server_param(),
        ActionParam::required(
            "revocation",
            ParamType::Text,
            "The revocation of the server's device key your root signed, as JSON.",
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
            || workspace_id.to_string()
                != tilo_server_protocol::servers::person_workspace_id(person)
            || !(caller.acting().is_person() || caller.is_device_agent())
            || certificate
                .capabilities
                .agent
                .is_some_and(|scope| !scope.reaches(&workspace_id.to_string()))
        {
            return None;
        }
        Some((self.held(workspace_id).await?, certificate.clone()))
    }

    /// Change the entry of the server `verb` names in the person's Servers
    /// workspace, and answer what it became.
    pub(super) async fn server_operation_invoke(
        &self,
        workspace_id: tilo_server_protocol::WorkspaceId,
        caller: &tilo_actions::remote::Caller,
        certificate: Option<&DeviceCert>,
        verb: &'static ServerVerb,
        request: &InvokeRequest,
    ) -> Result<serde_json::Value, ActionCallError> {
        let (governed, device) = self
            .server_operator(workspace_id, caller, certificate)
            .await
            .ok_or(ActionCallError::UnknownAction)?;
        let workspace = governed.lock().await.workspace().to_owned();
        let args = Args::of(&request.argument)?;
        let server = args.required_text("server")?.trim().to_owned();
        match verb.operation {
            RENAME => {
                let name = args.required_text("name")?.to_owned();
                servers::apply_on(
                    self,
                    &workspace,
                    servers::Servers::RENAME_SERVER.with((Id::from_raw(server), name.clone())),
                )
                .await
                .map_err(ActionCallError::refused)?;
                Ok(serde_json::json!({ "name": name.trim() }))
            }
            KEEP => {
                let kept = args.required_text("workspace")?.trim().to_owned();
                let keeps = args.flag("keeps")?;
                servers::apply_on(
                    self,
                    &workspace,
                    servers::Servers::SET_KEEPING.with((Id::from_raw(server), kept.clone(), keeps)),
                )
                .await
                .map_err(ActionCallError::refused)?;
                Ok(serde_json::json!({ "workspace": kept, "keeps": keeps }))
            }
            RETIRE => {
                self.retire(
                    &workspace,
                    &device,
                    server,
                    args.required_text("revocation")?,
                )
                .await
            }
            other => Err(ActionCallError::refused(format!(
                "{other} is no change to a server's entry"
            ))),
        }
    }

    /// Retire the server `server` of the person `device` belongs to in their
    /// Servers workspace `workspace`, with `revocation`, the revocation of its
    /// device key their root signed (JSON). The server is asked first, while
    /// every holder still reaches it: it takes the revocation, retires its own
    /// entry and lets go of everything. One that does not within the wait is
    /// away, and is retired here. This device refuses the key from then on;
    /// every other holder does as it applies the retired entry.
    async fn retire(
        &self,
        workspace: &str,
        device: &DeviceCert,
        server: String,
        revocation: &str,
    ) -> Result<serde_json::Value, ActionCallError> {
        let entry = serde_json::from_str::<tilo_user_identity::RevocationEntry>(revocation)
            .ok()
            .filter(|entry| entry.user_public_key == device.user_public_key)
            .ok_or_else(|| {
                ActionCallError::refused("a server is retired with its person's revocation of it")
            })?;
        let id = Id::from_raw(server.clone());
        let mut writes = self.document_writes();
        servers::apply_on(
            self,
            workspace,
            servers::Servers::ASK_RETIREMENT.with((id.clone(), revocation.to_owned())),
        )
        .await
        .map_err(ActionCallError::refused)?;
        let retired = |servers: &servers::Servers| {
            servers
                .servers
                .get(&id)
                .is_some_and(|server| server.retired)
        };
        let itself = tokio::time::timeout(SERVER_ANSWER_WAIT, async {
            while !servers::read_on(self, workspace)
                .await
                .is_ok_and(|servers| retired(&servers))
            {
                tokio::select! {
                    heard = writes.recv() => if matches!(heard, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                        return;
                    },
                    () = tokio::time::sleep(ANSWER_TICK) => {}
                }
            }
        })
        .await
        .is_ok()
            && servers::read_on(self, workspace)
                .await
                .is_ok_and(|servers| retired(&servers));
        if !itself {
            servers::apply_on(self, workspace, servers::Servers::RETIRE_SERVER.with(id))
                .await
                .map_err(ActionCallError::refused)?;
        }
        self.take_revocations(vec![entry])
            .await
            .map_err(ActionCallError::refused)?;
        Ok(serde_json::json!({ "server": server, "retired": true, "answered": itself }))
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
        .map_err(ActionCallError::refused)?;
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
