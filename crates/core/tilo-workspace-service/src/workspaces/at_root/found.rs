//! A workspace another device of the person's asks to have made, run at a device that keeps
//! their root: the one founding step for every device of theirs that keeps no root.
//!
//! The asking device keeps the make ([`super::found_request`]) as its record in the person's own
//! workspace. The runner takes it in ([`super::AtRoot::kept_changed`]) and decides it by the
//! person's grants for that device ([`Workspaces::found_decides`]): run at once where they allow
//! it, asked of the person here where they leave it to them, refused otherwise. It runs only for
//! the person themselves on another device of theirs, listed and not revoked: no agent, app or
//! program acting for them, and no one else. It founds the workspace here, places it at the
//! person's server and lists it with their others ([`Workspaces::found_asked`]).

use serde_json::{Value, json};
use tilo_action_policy::roles::person_workspace_id;
use tilo_action_policy::{Answer, Context, TreePath};
use tilo_action_types::ActionKind;
use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest};
use tilo_actions::{ActionOutput, InvocationUpdate};
use tilo_server_protocol::workspaces::WorkspaceLabels;

use super::{Decided, FOUND_ACTION, FOUND_PROVIDER, FOUND_RESOURCE, failed};
use crate::workspaces::Workspaces;
use crate::workspaces::person_workspace::devices;

/// The application whose grants decide a make: the person's workspaces are their settings'.
const APP: &str = tilo_app_id::apps::SETTINGS;

/// What a device that founds nothing hears when it asks to have one made and may not.
pub const NOT_FOR_THIS_DEVICE: &str = "it does not make workspaces for this device";

/// Whether `request` asks to make a workspace of the person's ([`super::found_request`]).
#[must_use]
pub fn is_found(request: &InvokeRequest) -> bool {
    found_target(&request.target)
}

/// Whether `target` is the make of one of the person's workspaces ([`super::found_request`]).
#[must_use]
pub fn found_target(target: &tilo_actions::remote::RemoteActionTarget) -> bool {
    target.provider == FOUND_PROVIDER
        && target.address.resource == FOUND_RESOURCE
        && target.address.action == FOUND_ACTION
}

/// What a make asks for, as its arguments name it.
#[derive(serde::Deserialize)]
struct Making {
    name: String,
    #[serde(default)]
    labels: WorkspaceLabels,
    #[serde(default)]
    id: Option<String>,
}

impl Workspaces {
    /// Whether `caller`, which kept a call in `workspace`, asks as the person themselves from a
    /// device of theirs other than this one, in their own workspace.
    fn asks_as_the_person(&self, caller: &Caller, workspace: &str) -> bool {
        let own = self.device_cert();
        let person = own.user_public_key;
        let acting = caller.acting();
        !acting.is_agent()
            && acting.is_person()
            && acting.app().is_none()
            && acting.acted_for_person().is_none()
            && caller.launched_app().is_none()
            && caller.user_public_key() == Some(&person)
            && caller.device_public_key() != Some(&own.device_public_key)
            && workspace == person_workspace_id(&person)
    }

    /// What the person's grants in `workspace` answer `device` making one of their workspaces:
    /// it reaches out beyond the device, so a device on an operating system Tilo does not
    /// control is asked about. A device the workspace does not admit is refused.
    fn found_granted(&self, workspace: &str, device: [u8; 32]) -> Answer {
        let now = tilo_clock::wall();
        self.read_now(workspace, |peer| {
            peer.permissions(device, now).map(|access| {
                let node = TreePath::workspace(access.workspace())
                    .app(APP)
                    .rest()
                    .resource(FOUND_RESOURCE)
                    .rest()
                    .action(FOUND_ACTION);
                access.answer(&node, ActionKind::Outside, &Context::at_time(now))
            })
        })
        .and_then(|(_, answer)| answer.ok())
        .unwrap_or(Answer::Deny)
    }

    /// How this device, which keeps the person's root, takes in the make `caller` kept in
    /// `workspace`: asked of the person where their grants leave it to them, run at once where
    /// they allow it, and refused for anyone but the person on another device of theirs.
    #[must_use]
    pub fn found_decides(&self, caller: &Caller, workspace: &str) -> Decided {
        let Some(device) = caller
            .device_public_key()
            .filter(|_| self.asks_as_the_person(caller, workspace))
        else {
            return Decided::Refuse(NOT_FOR_THIS_DEVICE.to_owned());
        };
        match self.found_granted(workspace, *device) {
            Answer::Allow => Decided::Run,
            Answer::Ask => Decided::Ask,
            Answer::Deny => Decided::Refuse(NOT_FOR_THIS_DEVICE.to_owned()),
        }
    }

    /// Whether the device `caller` asks from is one of the person's, listed in their devices
    /// record, not revoked, certified now, and no agent's.
    async fn listed_device(&self, caller: &Caller) -> bool {
        let person = self.device_cert().user_public_key;
        let Some(device) = caller.device_public_key().copied() else {
            return false;
        };
        let now = tilo_clock::unix_now_ns();
        devices::of(self, &person)
            .await
            .listed(&person)
            .into_iter()
            .any(|listed| {
                listed.cert.device_public_key == device
                    && listed.cert.capabilities.agent.is_none()
                    && !listed.device.revoked(&listed.cert)
                    && tilo_user_identity::verify_device_cert_at(&listed.cert, now).is_ok()
            })
    }

    /// Make the workspace `request` asks for here, on this device that keeps the person's root,
    /// as the device `caller` asked it from in `workspace`: placed at the person's server and
    /// listed with their others. How it ended, for the record it was kept in.
    pub async fn found_asked(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        workspace: &str,
    ) -> InvocationUpdate {
        let invocation = request.invocation;
        if !self.asks_as_the_person(caller, workspace) || !self.listed_device(caller).await {
            return failed(invocation, NOT_FOR_THIS_DEVICE);
        }
        let InvokeArgument::Json(fields) = &request.argument else {
            return failed(invocation, "a make names the workspace to make");
        };
        let making: Making = match serde_json::from_value(Value::Object(fields.clone())) {
            Ok(making) => making,
            Err(error) => return failed(invocation, &error.to_string()),
        };
        let name = making.name.trim();
        if name.is_empty() {
            return failed(invocation, "a workspace is made with a name");
        }
        // Asked again under the same id, it answers the workspace it made the first time.
        let summary = match making.id.as_deref() {
            Some(id) if self.state(id).await.is_some() => {
                self.summary(id).await.map_err(|error| error.to_string())
            }
            id => self.found(name, id).await,
        };
        let summary = match summary {
            Ok(summary) => summary,
            Err(error) => return failed(invocation, &error),
        };
        if !making.labels.is_empty()
            && let Err(error) = self.label(&summary.workspace, &making.labels).await
        {
            return failed(invocation, &error.to_string());
        }
        InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput {
                value: Some(json!({
                    "workspace": { "id": summary.workspace, "name": summary.name },
                })),
                resources: Vec::new(),
            },
        }
    }
}
