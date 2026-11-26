//! Machines a person prepared to install: the plan and its waiting entry in
//! their Servers workspace, read and written by the session where their own
//! workspace service keeps it. The command and the installer's window name the
//! same steps, so one list is read and written in one place.

use tilo_actions::ActionParam;
use tilo_actions::ParamType;

use {tilo_actions::ActionAddress, tilo_actions::remote::RemoteActionTarget};

/// The provider that answers a person's install machines.
pub const PROVIDER: &str = tilo_actions::remote::MACHINES_PROVIDER;
/// The resource the steps are actions of.
pub const RESOURCE: &str = "machines";
/// Plan a machine from its plan document (`plan`, `label`, `token_file`,
/// `installer_url`); answers what the person is told.
pub const PLAN: &str = "machine.plan";
/// The machines the person planned and has not installed.
pub const LIST: &str = "machine.list";
/// One planned machine (`plan`).
pub const SHOW: &str = "machine.show";
/// The person's servers in service, as the document of their workspace
/// names them: a list of `tilo_server_protocol::servers::PersonServer`, each
/// one's device key, name and address.
pub const SERVERS: &str = "machine.servers";
/// Take a machine that enrolled with a plan's token into its entry (`device`,
/// `nonce`, and what it reported: `drives`, `ask`).
pub const ADMIT: &str = "machine.admit";
/// The disk recovery keys of the person's machines, as the vault of their Servers workspace
/// keeps them: a list of `tilo_server_protocol::servers::RecoveryKey`. The person's alone, never
/// an app's or an agent's.
pub const RECOVERY_KEYS: &str = "machine.recovery-keys";
/// Keep the disk recovery key (`key`) of the machine that asks, in the vault of the person's
/// Servers workspace: the machine asks its host this itself, as it installs, and keeps no copy.
pub const KEEP_RECOVERY_KEY: &str = "machine.keep-recovery-key";

/// The steps, as the command and the window reach them.
pub const STEPS: [&str; 6] = [PLAN, LIST, SHOW, SERVERS, ADMIT, RECOVERY_KEYS];

/// The parameters the step `step` takes.
#[must_use]
pub fn params(step: &str) -> Vec<ActionParam> {
    match step {
        PLAN => vec![
            ActionParam::required(
                "plan",
                ParamType::Text,
                "The plan document, as the command or the window wrote it (JSON).",
            ),
            ActionParam::required(
                "label",
                ParamType::Text,
                "The name you see the machine by, 1 to 80 bytes.",
            ),
            ActionParam::required(
                "token_file",
                ParamType::Text,
                "Where the token file the medium carries is, which the answer names.",
            ),
            ActionParam::optional(
                "installer_url",
                ParamType::Text,
                "Where the installer is served, for the command the answer prints.",
            ),
        ],
        SHOW => vec![ActionParam::required(
            "plan",
            ParamType::Text,
            "The plan's id, as listing the machines prints it.",
        )],
        ADMIT => vec![
            ActionParam::required(
                "device",
                ParamType::Text,
                "The machine's device key, 64 lowercase hex characters.",
            ),
            ActionParam::required(
                "nonce",
                ParamType::Text,
                "The token's nonce, 32 lowercase hex characters.",
            ),
            ActionParam::optional(
                "drives",
                ParamType::Text,
                "The drives the machine reported, as JSON; none when it has not reported.",
            ),
            ActionParam::optional(
                "ask",
                ParamType::Text,
                "The choices the machine asks about, choice to reason, as JSON.",
            ),
        ],
        _ => Vec::new(),
    }
}

/// The address of the step `step`.
#[must_use]
pub fn target(step: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, step),
    }
}
