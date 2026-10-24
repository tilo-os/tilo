//! The room's own changes as governance actions ([`ROOM_GOVERNANCE_PROVIDER`]
//! on `workspace.permissions:<room>`): renaming the workspace, its saved
//! grant sets and their assignments, the actions its apps may use of each
//! other, policy grants and sandbox capabilities. Each runs
//! [`Rooms::apply_owner_command`], which decides it by the caller's own
//! standing; the catalogue offers only what that decision would allow
//! ([`Rooms::may_offer`]), and invoking anything else is the same
//! `UnknownAction` as invoking what does not exist.
//!
//! [`ROOM_GOVERNANCE_PROVIDER`]: tilo_actions::remote::ROOM_GOVERNANCE_PROVIDER

use serde_json::{Value, json};
use tilo_action_policy::roles::{self, governance};
use tilo_action_policy::{Grant, GrantId, Lifetime, Preset, Subject};
use tilo_actions::remote::{
    ActionHostError, Caller, CatalogueRow, InvokeRequest, room_governance_resource,
};
use tilo_actions::{ActionParam, ParamType};
use tilo_user_identity::DeviceCert;

use super::{Args, GOVERNANCE, Verb, invalid, refused, request_id};
use crate::rooms::owner_commands::Offer;
use crate::rooms::{AppAction, GovernedRoom, OwnerCommand, Rooms, app_action_lifetime};

pub(crate) use tilo_action_policy::roles::owner_commands::{
    ASSIGN_PRESET, GRANT_APP_ACTION, GRANT_SANDBOX, RENAME_WORKSPACE as RENAME, REVOKE_APP_ACTION,
    REVOKE_SANDBOX, SAVE_PRESET,
};

/// The room's own changes, in catalogue order. `gate` names the room
/// governance action each is offered by; renaming and assigning are
/// decided by [`offer_of`].
const OWNER_VERBS: [Verb; 9] = [
    Verb {
        id: RENAME,
        label: "Rename workspace",
        gate: governance::SETTINGS,
        kind: roles::GOVERNANCE_KIND,
        params: rename_params,
    },
    Verb {
        id: SAVE_PRESET,
        label: "Save grant set",
        gate: governance::SETTINGS,
        kind: roles::GOVERNANCE_KIND,
        params: save_preset_params,
    },
    Verb {
        id: ASSIGN_PRESET,
        label: "Assign grant set",
        gate: governance::GRANT,
        kind: roles::GOVERNANCE_KIND,
        params: assign_preset_params,
    },
    Verb {
        id: GRANT_APP_ACTION,
        label: "Let an app use an action",
        gate: governance::GRANT,
        kind: roles::GOVERNANCE_KIND,
        params: grant_app_action_params,
    },
    Verb {
        id: REVOKE_APP_ACTION,
        label: "Stop an app using an action",
        gate: governance::REVOKE,
        kind: roles::GOVERNANCE_KIND,
        params: app_action_params,
    },
    Verb {
        id: governance::GRANT,
        label: "Grant",
        gate: governance::GRANT,
        kind: roles::GOVERNANCE_KIND,
        params: grant_params,
    },
    Verb {
        id: governance::REVOKE,
        label: "Revoke grant",
        gate: governance::REVOKE,
        kind: roles::GOVERNANCE_KIND,
        params: revoke_params,
    },
    Verb {
        id: GRANT_SANDBOX,
        label: "Grant sandbox capability",
        gate: governance::GRANT,
        kind: roles::GOVERNANCE_KIND,
        params: grant_sandbox_params,
    },
    Verb {
        id: REVOKE_SANDBOX,
        label: "Revoke sandbox capability",
        gate: governance::REVOKE,
        kind: roles::GOVERNANCE_KIND,
        params: revoke_sandbox_params,
    },
];

fn rename_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "name",
        ParamType::Text,
        "The workspace's new name, 1 to 80 characters.",
    )]
}

fn save_preset_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "preset",
        ParamType::Object,
        "The grant set: its name, description and rules, as an object or its JSON text.",
    )]
}

fn assign_preset_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("preset", ParamType::Text, "The saved grant set's name."),
        ActionParam::required(
            "subject_kind",
            ParamType::Text,
            "Who gets the grants: a person, an app or an agent.",
        )
        .with_choices(["person", "app", "agent"]),
        ActionParam::required(
            "subject",
            ParamType::Text,
            "The person's user root key (64 hex characters), the app's id or the agent's id.",
        ),
        ActionParam::optional(
            "expires_unix",
            ParamType::Integer,
            "When the grants end, in Unix seconds; never when omitted or 0.",
        ),
    ]
}

fn app_action_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("app", ParamType::Text, "The app that uses the action."),
        ActionParam::required(
            "target_app",
            ParamType::Text,
            "The app whose action it uses.",
        ),
        ActionParam::required(
            "resource",
            ParamType::Text,
            "The other app's resource the action is on.",
        ),
        ActionParam::required("action", ParamType::Text, "The action."),
    ]
}

fn grant_app_action_params() -> Vec<ActionParam> {
    let mut params = app_action_params();
    params.push(
        ActionParam::required(
            "lifetime",
            ParamType::Text,
            "How long the app may: always, until taken back.",
        )
        .with_choices(["always"]),
    );
    params
}

fn grant_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "grant",
        ParamType::Object,
        "The grant: its id, subject, node in this room, kinds, answer and lifetime. Recorded as given by the caller.",
    )]
}

fn revoke_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "grant",
        ParamType::Text,
        "The id of the grant to take back.",
    )]
}

fn grant_sandbox_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "capability",
        ParamType::Object,
        "The sandbox capability: subject, scope, effect, access and expiry; an id is minted when empty. Recorded as given by the caller.",
    )]
}

fn revoke_sandbox_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "capability",
        ParamType::Text,
        "The id of the sandbox capability to take back.",
    )]
}

/// Whether `action` is one of the room's own changes.
pub(super) fn serves(action: &str) -> bool {
    OWNER_VERBS.iter().any(|verb| verb.id == action)
}

/// What the catalogue offers `verb` by.
fn offer_of(verb: &Verb) -> Offer {
    match verb.id {
        RENAME => Offer::Rename,
        ASSIGN_PRESET => Offer::AssignPreset,
        _ => Offer::Govern(verb.gate),
    }
}

/// The value of argument `name`, decoded from an object or its JSON text.
fn decoded<T: serde::de::DeserializeOwned>(args: &Args, name: &str) -> Result<T, ActionHostError> {
    let value = match args.0.get(name) {
        None | Some(Value::Null) => return Err(invalid(format!("`{name}` is required"))),
        Some(Value::String(text)) => serde_json::from_str(text),
        Some(value) => serde_json::from_value(value.clone()),
    };
    value.map_err(|error| invalid(format!("`{name}` does not decode: {error}")))
}

fn app_action(args: &Args) -> Result<AppAction, ActionHostError> {
    AppAction::new(
        args.required_text("app")?,
        args.required_text("target_app")?,
        args.required_text("resource")?,
        args.required_text("action")?,
    )
    .map_err(|error| invalid(error.to_string()))
}

/// The command `action` asks for with `args`.
fn command_of(action: &str, args: &Args) -> Result<OwnerCommand, ActionHostError> {
    Ok(match action {
        RENAME => OwnerCommand::Rename {
            name: args.required_text("name")?.to_owned(),
        },
        SAVE_PRESET => OwnerCommand::SavePreset {
            preset: decoded::<Preset>(args, "preset")?,
        },
        ASSIGN_PRESET => {
            let subject = args.required_text("subject")?;
            let subject = match args.required_text("subject_kind")? {
                "person" => Subject::person(subject),
                "app" => Subject::app(subject),
                "agent" => Subject::agent(subject),
                other => {
                    return Err(invalid(format!(
                        "a grant set is assigned to a person, an app or an agent, not `{other}`"
                    )));
                }
            };
            let lifetime = match args.integer("expires_unix")?.unwrap_or(0) {
                0 => Lifetime::UntilRevoked,
                seconds => Lifetime::Until(seconds.saturating_mul(1000)),
            };
            OwnerCommand::AssignPreset {
                preset: args.required_text("preset")?.to_owned(),
                subject,
                lifetime,
            }
        }
        GRANT_APP_ACTION => {
            app_action_lifetime(args.required_text("lifetime")?)
                .map_err(|error| invalid(error.to_string()))?;
            OwnerCommand::GrantAppAction(app_action(args)?)
        }
        REVOKE_APP_ACTION => OwnerCommand::RevokeAppAction(app_action(args)?),
        governance::GRANT => OwnerCommand::Grant {
            grant: decoded::<Grant>(args, "grant")?,
        },
        governance::REVOKE => OwnerCommand::Revoke {
            grant: GrantId::new(args.required_text("grant")?),
        },
        GRANT_SANDBOX => OwnerCommand::GrantSandbox {
            capability: decoded(args, "capability")?,
        },
        REVOKE_SANDBOX => OwnerCommand::RevokeSandbox {
            capability: args.required_text("capability")?.to_owned(),
        },
        _ => return Err(ActionHostError::UnknownAction),
    })
}

impl Rooms {
    /// The room's own changes `device` may ask for in `room` now, as rows
    /// on its governance resource `resource`.
    pub(super) fn owner_rows(
        &self,
        room: &GovernedRoom,
        device: &DeviceCert,
        resource: &str,
        now: u64,
    ) -> Vec<CatalogueRow> {
        OWNER_VERBS
            .iter()
            .filter(|verb| self.may_offer(room, device, offer_of(verb), now))
            .map(|verb| verb.row(GOVERNANCE, resource, &room.record.name))
            .collect()
    }

    /// Run `request`, one of the room's own changes, from `caller` against
    /// the room `workspace_id` carries.
    pub(super) async fn owner_invoke(
        &self,
        workspace_id: u64,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Value, ActionHostError> {
        let address = &request.target.address;
        let verb = OWNER_VERBS
            .iter()
            .find(|verb| verb.id == address.action)
            .ok_or(ActionHostError::UnknownAction)?;
        let (governed, device) = self
            .governed_by(workspace_id, caller)
            .await
            .ok_or(ActionHostError::UnknownAction)?;
        let room = {
            let room = governed.lock().await;
            // Unknown and forbidden are the same answer.
            if address.resource != room_governance_resource(room.room())
                || !self.may_offer(&room, &device, offer_of(verb), tilo_clock::unix_now_ns())
            {
                return Err(ActionHostError::UnknownAction);
            }
            room.room().to_owned()
        };
        let args = Args::of(&request.argument)?;
        let command = command_of(verb.id, &args)?;
        let done = self
            .apply_owner_command(
                &room,
                &device,
                request_id(&device.user_public_key, request.invocation),
                command,
            )
            .await
            .map_err(|error| refused(&error))?;
        Ok(json!({ "generation": done.generation }))
    }
}

#[cfg(test)]
mod tests;
