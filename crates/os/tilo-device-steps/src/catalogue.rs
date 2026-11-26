//! The steps the device offers on `devices/<device>/vm`, one action each, its parameters the
//! fields of the step's argument struct in `tilo-device-protocol`. `tilo-device-step catalogue`
//! prints them, and the device's daemon publishes what it printed.

use serde::Serialize;
use tilo_actions::{ActionParam, ParamType};

use ParamType::{Array, Boolean, Integer, Object, Secret, Text};

/// A parameter: name, type, whether the step needs it, whether it may be `null`.
type Param = (&'static str, ParamType, bool, bool);

const R: bool = true;
const O: bool = false;
const NULL: bool = true;
const SET: bool = false;

/// Every step: its action id, its label, and its parameters.
const STEPS: &[(&str, &str, &[Param])] = &[
    ("status", "Device status", &[("account", Text, O, NULL)]),
    (
        "account-create",
        "Create an account",
        &[
            ("name", Text, R, SET),
            ("kind", Text, R, SET),
            ("owner", Object, O, SET),
            ("outputs", Array, O, SET),
            ("identity", Boolean, O, SET),
            ("device_identity", Boolean, O, SET),
        ],
    ),
    ("account-list", "List accounts", &[]),
    (
        "account-restart",
        "Restart an account",
        &[("name", Text, R, SET)],
    ),
    ("account-rm", "Remove an account", &[("name", Text, R, SET)]),
    ("account-show", "Show an account", &[("name", Text, R, SET)]),
    (
        "layer-import",
        "Import a layer",
        &[("source", Object, R, SET), ("provenance", Object, R, SET)],
    ),
    ("layer-list", "List layers", &[]),
    ("layer-verify", "Verify a layer", &[("hash", Text, R, SET)]),
    (
        "swap",
        "Swap a component",
        &[
            ("account", Text, R, SET),
            ("component", Text, R, SET),
            ("layer", Text, R, SET),
            ("info", Object, O, NULL),
            ("partner", Object, O, NULL),
        ],
    ),
    (
        "userspace-apply",
        "Apply a userspace",
        &[
            ("account", Text, R, SET),
            ("expected_generation", Integer, R, SET),
            ("expected_session", Object, R, SET),
            ("source", Object, R, SET),
            ("source_inventory", Text, R, SET),
            ("members", Array, R, SET),
        ],
    ),
    (
        "rollback",
        "Roll an account back",
        &[("account", Text, R, SET), ("to", Integer, O, NULL)],
    ),
    (
        "logs",
        "Read an account's logs",
        &[("account", Text, R, SET), ("lines", Integer, R, SET)],
    ),
    (
        "seat-claim",
        "Claim the physical seat",
        &[
            ("account", Text, R, SET),
            ("owner", Object, O, SET),
            ("steal", Boolean, O, SET),
        ],
    ),
    (
        "seat-release",
        "Release the physical seat",
        &[("account", Text, R, SET)],
    ),
    ("shot", "Take a screenshot", &[("account", Text, R, SET)]),
    (
        "control",
        "Control a session",
        &[("account", Text, R, SET), ("args", Array, R, SET)],
    ),
    (
        "input",
        "Send input",
        &[("account", Text, R, SET), ("actions", Array, R, SET)],
    ),
    (
        "gc",
        "Collect unused layers",
        &[
            ("keep", Integer, R, SET),
            ("dry_run", Boolean, R, SET),
            ("keep_targets", Integer, O, NULL),
            ("free_goal_bytes", Integer, O, NULL),
            ("min_age_seconds", Integer, O, NULL),
        ],
    ),
    (
        "app-install",
        "Install an app",
        &[("account", Text, R, SET), ("source", Object, R, SET)],
    ),
    (
        "boot-confirm",
        "Confirm the booted kernel",
        &[("expect_version", Text, O, NULL)],
    ),
    (
        "awake",
        "Keep the device awake",
        &[("seconds", Integer, O, NULL), ("off", Boolean, O, SET)],
    ),
    (
        "network-wake",
        "Wake on network",
        &[("profile", Text, R, SET), ("enabled", Boolean, O, NULL)],
    ),
    (
        "device-unlock-enrol",
        "Unlock the disk from sibling devices",
        &[
            ("passphrase", Secret, R, SET),
            ("networks", Array, O, SET),
            ("allow_unsealed", Boolean, O, SET),
        ],
    ),
    (
        "build",
        "Build from a worktree",
        &[
            ("worktree", Text, R, SET),
            ("target", Object, R, SET),
            ("provenance", Object, R, SET),
            ("owner", Object, O, SET),
            ("color", Boolean, O, SET),
        ],
    ),
    (
        "ci-layer",
        "Make a layer from a published image",
        &[
            ("image", Text, R, SET),
            ("component", Text, R, SET),
            ("paths", Array, R, SET),
            ("provenance", Object, R, SET),
        ],
    ),
    (
        "exec",
        "Run a command in an account",
        &[
            ("account", Text, R, SET),
            ("command", Array, R, SET),
            ("timeout", Integer, O, SET),
            ("max_bytes", Integer, O, SET),
        ],
    ),
    (
        "install-packages",
        "Install distribution packages",
        &[("packages", Array, R, SET), ("timeout", Integer, O, SET)],
    ),
    (
        "tree-sync",
        "Lay a worktree out",
        &[("worktree", Text, R, SET), ("manifest", Text, R, SET)],
    ),
    (
        "upgrade",
        "Stage another OS image",
        &[("source", Object, R, SET), ("expected", Text, O, NULL)],
    ),
    ("reboot", "Restart the device", &[]),
    ("poweroff", "Turn the device off", &[]),
];

/// One step as the daemon publishes it: its action id, its label, its parameters, and whether a
/// person confirms it before it runs.
#[derive(Serialize)]
struct Declared {
    id: &'static str,
    label: &'static str,
    params: Vec<ActionParam>,
    confirm: bool,
}

/// Every step, as the JSON `tilo-device-step catalogue` prints.
///
/// # Errors
/// What encoding the steps as JSON refused.
pub fn declared() -> serde_json::Result<String> {
    let steps: Vec<Declared> = STEPS
        .iter()
        .map(|&(id, label, params)| Declared {
            id,
            label,
            params: params
                .iter()
                .map(|&(name, ty, required, nullable)| {
                    let mut param = if required {
                        ActionParam::required(name, ty, "")
                    } else {
                        ActionParam::optional(name, ty, "")
                    };
                    param.nullable = nullable;
                    param
                })
                .collect(),
            // The disk passphrase is the one step a person confirms on their own device.
            confirm: id == "device-unlock-enrol",
        })
        .collect();
    serde_json::to_string(&steps)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    /// Unlocking the disk from other devices declares its passphrase a secret and asks the person to confirm it,
    /// which the daemon admits it by; no other step asks.
    #[test]
    fn only_disk_unlock_is_confirmed_and_its_passphrase_is_a_secret() {
        let declared: Vec<Value> =
            serde_json::from_str(&super::declared().expect("encoded")).expect("JSON");
        let confirmed: Vec<&Value> = declared
            .iter()
            .filter(|step| step["confirm"] == true)
            .collect();
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0]["id"], "device-unlock-enrol");
        let passphrase = confirmed[0]["params"]
            .as_array()
            .and_then(|params| params.iter().find(|param| param["name"] == "passphrase"))
            .expect("it asks for the passphrase");
        assert_eq!(
            serde_json::from_value::<tilo_actions::ParamType>(passphrase["ty"].clone()).ok(),
            Some(tilo_actions::ParamType::Secret)
        );
    }
}
