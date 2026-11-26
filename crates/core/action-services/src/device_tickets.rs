use serde::{Deserialize, Serialize};

use tilo_actions::{
    ActionContract, ActionKind, ActionParam, ConfirmationPolicy, OperationKind, ParamType,
    ResultSpec, RiskLevel,
};
use {tilo_actions::ActionAddress, tilo_actions::remote::RemoteActionTarget};

/// The provider the device's daemon answers tickets under.
pub const PROVIDER: &str = "tilo.device";
/// The resource the actions act on: the person's devices.
pub const RESOURCE: &str = "devices";
/// Serve a ticket for a new computer.
pub const ISSUE: &str = "issue";
/// Name one of the person's devices (`device`, `name`) in every list.
pub const RENAME: &str = "rename";
/// End one of the person's devices' logins (`device`, `reason`), signed by their root on the
/// device that holds it.
pub const REVOKE: &str = "revoke";
/// The person's identities this device keeps, without secrets.
pub const IDENTITIES: &str = "identities";
/// Sign new apps in as identity `id` (`confirm`).
pub const USE_IDENTITY: &str = "useIdentity";
/// Remove identity `id` from this device (`confirm`).
pub const REMOVE_IDENTITY: &str = "removeIdentity";
/// Move identity `id`'s key from a plain file into protected storage (`confirm`).
pub const PROTECT_IDENTITY: &str = "protectIdentity";
/// The identity steps, in the order a catalogue lists them. The daemon keeps the identity
/// store and takes every step, so no application reads a secret or signs with the root.
pub const IDENTITY_STEPS: [&str; 4] = [IDENTITIES, USE_IDENTITY, REMOVE_IDENTITY, PROTECT_IDENTITY];

#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// The device actions a desktop hands on to its daemon.
pub const HANDED_ON: [&str; 7] = [
    ISSUE,
    RENAME,
    REVOKE,
    IDENTITIES,
    USE_IDENTITY,
    REMOVE_IDENTITY,
    PROTECT_IDENTITY,
];

/// How each of [`HANDED_ON`] is listed, wherever it is: the one description of each.
#[must_use]
pub fn descriptor(action: &str) -> Option<tilo_actions::ActionDescriptor> {
    let (label, description, kind, mut contract, params) = listed(action)?;
    // Only the person's root signs a revocation, and issues the ticket that lets a computer
    // in: a device that keeps none hands it on, and an agent that asks for one waits for
    // their yes.
    if matches!(action, ISSUE | REVOKE) {
        contract.tags.push(tilo_actions::ROOT_ONLY_TAG.to_owned());
    }
    Some(tilo_actions::ActionDescriptor {
        did: None,
        semantic_identity: None,
        address: ActionAddress::new(RESOURCE, action),
        resource_kind: "device".to_owned(),
        resource_label: "Devices".to_owned(),
        action_label: label.to_owned(),
        keywords: Vec::new(),
        state: tilo_actions::ActionState::default(),
        icon: None,
        description: Some(description.to_owned()),
        locus: tilo_actions::ExecutionLocus::ClientLocal,
        kind,
        contract,
        input: tilo_actions::InputSpec::None,
        params,
        view: None,
    })
}

/// What one of [`HANDED_ON`] says of itself: its label, its description, its kind, its
/// contract and its parameters.
type Listed = (
    &'static str,
    &'static str,
    OperationKind,
    ActionContract,
    Vec<ActionParam>,
);

const DEVICE: ActionParam =
    ActionParam::required("device", ParamType::Text, "The device's key, in hex.");

fn object(required: &[&str]) -> ResultSpec {
    ResultSpec::Json {
        schema: serde_json::json!({ "type": "object", "required": required }),
    }
}

fn effect(result: ResultSpec) -> ActionContract {
    ActionContract {
        kind: ActionKind::Outside,
        result,
        ..ActionContract::default()
    }
}

/// What changes an identity, or takes a key back for good, the person confirms.
fn confirmed(result: ResultSpec) -> ActionContract {
    ActionContract {
        risk: RiskLevel::Privileged,
        confirmation: ConfirmationPolicy::Confirm,
        ..effect(result)
    }
}

fn identity(label: &'static str, description: &'static str) -> Listed {
    (
        label,
        description,
        OperationKind::Effect,
        confirmed(object(&["id"])),
        vec![
            ActionParam::required("id", ParamType::Text, "The identity's id, as listed."),
            ActionParam::required("confirm", ParamType::Boolean, "You confirm the change."),
        ],
    )
}

fn listed(action: &str) -> Option<Listed> {
    Some(match action {
        ISSUE => (
            "Add a device",
            "Print a one-use ticket another computer logs in with as one of the person's \
             devices.",
            OperationKind::Effect,
            effect(object(&["ticket"])),
            vec![
                ActionParam::optional(
                    "wait",
                    ParamType::Boolean,
                    "Wait for the computer to log in with the ticket.",
                ),
                ActionParam::optional(
                    "plan",
                    ParamType::Text,
                    "The planned machine's entry in your Servers workspace, which the \
                     computer's certification takes into its own.",
                ),
                ActionParam::optional(
                    "server",
                    ParamType::Boolean,
                    "The computer is a server, which keeps your workspaces: it gets an entry \
                     in your Servers workspace.",
                ),
            ],
        ),
        RENAME => (
            "Rename a device",
            "Name one of the person's devices in every list of them.",
            OperationKind::Effect,
            effect(object(&["device", "name"])),
            vec![
                DEVICE,
                ActionParam::required("name", ParamType::Text, "What the device is called."),
            ],
        ),
        REVOKE => (
            "Revoke a device",
            "Take one of the person's devices back for good: every device of theirs refuses \
             it from now on.",
            OperationKind::Effect,
            confirmed(object(&["device", "revoked"])),
            vec![
                DEVICE,
                ActionParam::optional(
                    "reason",
                    ParamType::Text,
                    "lost, stolen, retired or compromised (retired when none is given).",
                ),
            ],
        ),
        IDENTITIES => (
            "List the identities",
            "The person's identities this device keeps, without secrets, and which one new \
             apps sign in as.",
            OperationKind::View,
            ActionContract {
                result: ResultSpec::Json {
                    schema: serde_json::json!({ "type": "array", "items": { "type": "object" } }),
                },
                ..ActionContract::view()
            },
            Vec::new(),
        ),
        USE_IDENTITY => identity(
            "Use an identity for new apps",
            "Sign apps opened from now on in as this identity.",
        ),
        REMOVE_IDENTITY => identity(
            "Remove an identity",
            "Delete an identity, with its key, from this device.",
        ),
        PROTECT_IDENTITY => identity(
            "Protect an identity's key",
            "Move an identity's key from a plain file into protected storage.",
        ),
        _ => return None,
    })
}

/// What [`REVOKE`] takes back: any key of the person's (a device, an agent's grant, a
/// script's key) by its key in hex, and why (`retired`, `lost`, `stolen` or `compromised`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revoke {
    pub device: String,
    pub reason: String,
}

impl Revoke {
    /// Taking `device` back as retired: what a list's remove asks.
    #[must_use]
    pub fn retired(device: &str) -> Self {
        Self {
            device: device.to_owned(),
            reason: "retired".to_owned(),
        }
    }

    /// The argument [`REVOKE`] is asked with.
    #[must_use]
    pub fn argument(&self) -> tilo_actions::remote::InvokeArgument {
        tilo_actions::remote::workspace_activity::argument(self)
    }
}

/// One of the person's devices, as every list of them shows it: joined from the record of
/// their own workspace and who is here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetDevice {
    /// The name the device was enrolled with.
    pub label: String,
    /// The device's public key, 64 lowercase hex characters.
    pub key_hex: String,
    /// The key's short form people compare between screens.
    pub fingerprint: String,
    pub current: bool,
    /// Whether its trust was revoked.
    pub revoked: bool,
    /// What the key is: a device of the person's, an agent's grant or a
    /// script's key.
    #[serde(default)]
    pub kind: KeyKind,
    /// The browser profile it is a tab of, by the profile's id: a
    /// browser keeps one key per open tab, and the person sees and takes
    /// back all of one browser's keys as one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    /// The agent this key runs, as the sessions it opened on a host named
    /// it ("Claude Code"): a computer set up with `tilo login` is a
    /// device of the person's that an agent works on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// When its certificate was issued, ms since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_ms: Option<i64>,
    /// When it expires, ms since the epoch; `None` for one that does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_ms: Option<i64>,
    /// When a session of this key with one of the person's hosts last
    /// opened or ended, ms since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_ms: Option<i64>,
    /// A session of this key is open with one of their hosts now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub online: bool,
    /// The one workspace an agent's grant reaches, by its name where the
    /// person's record names it, else its id. `None` every workspace of
    /// theirs: a device of theirs, and an agent with the whole account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}
/// What a key the person's root certified is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyKind {
    /// One of their devices.
    #[default]
    Device,
    /// An agent's grant: a key an agent signs with for them.
    Agent,
    /// A key they made for their own script or backend.
    Script,
}

impl KeyKind {
    /// What the key `capabilities` certify is.
    #[must_use]
    pub fn of(agent: bool, server_key: bool) -> Self {
        if server_key {
            Self::Script
        } else if agent {
            Self::Agent
        } else {
            Self::Device
        }
    }

    /// The word a list shows it by, as it is serialized: `device`, `agent` or `script`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::Agent => "agent",
            Self::Script => "script",
        }
    }
}
