//! The installer's operations: the one payload type every door produces.
//!
//! A wizard button, a keyboard shortcut, the Ctrl+K palette, a screen
//! reader, `tilo-control actions invoke`, a headless scenario and the
//! `tilo-vm scenario install` driver all end in the same place: an
//! [`Operation`] encoded under [`OPERATION_TYPE`] by the `ActionSpec` the
//! installer publishes, decoded and applied by `InstallerApp`. There is no
//! second implementation of "Next" or "Begin installation" behind any of
//! them.
//!
//! These are effect operations, not document actions: installer state is
//! local to the privileged live session, never replicated, and several
//! operations (install, reboot) act on the machine rather than on a CRDT.

use serde::{Deserialize, Serialize};

/// `provider` in `tilo-control` terms: the name the console socket answers
/// under.
pub const PROVIDER: &str = "installer";

/// The single protocol type every installer operation is encoded under.
pub const OPERATION_TYPE: &str = "installer.operation";

/// The fixed online-account controls, as the accounts surface publishes them.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnlineControl {
    Add,
    Retry,
    Cancel,
    SkipOffline,
}

impl OnlineControl {
    fn id(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Retry => "retry",
            Self::Cancel => "cancel",
            Self::SkipOffline => "skip-offline",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        match id {
            "add" => Some(Self::Add),
            "retry" => Some(Self::Retry),
            "cancel" => Some(Self::Cancel),
            "skip-offline" => Some(Self::SkipOffline),
            _ => None,
        }
    }
}

/// Everything a door can ask the installer for: one typed vocabulary over the
/// wizard's operations and the accounts surface's fixed controls.
///
/// Not `Debug`: the operation variants carry secrets.
#[derive(Clone)]
pub enum InstallerEffect {
    Install(Operation),
    Online(OnlineControl),
}

/// Resource ids. They name wizard concerns, not screen positions, so an
/// address stays valid across layouts, themes and text scales.
pub const SESSION: &str = "installer";
pub const KEYBOARD: &str = "installer.keyboard";
pub const NETWORK: &str = "installer.network";
pub const STORAGE: &str = "installer.storage";
pub const ACCOUNT: &str = "installer.account";
pub const OPTIONS: &str = "installer.options";
pub const MEDIA: &str = "installer.media";
pub const REVIEW: &str = "installer.review";
pub const RECOVERY: &str = "installer.recovery";
pub const POWER: &str = "installer.power";

/// One discovered disk: `installer.disk:/dev/vdb`.
pub fn disk_resource(device: &str) -> String {
    format!("installer.disk:{device}")
}

/// One unallocated region of the selected disk, when more than one could
/// hold an alongside install.
pub fn region_resource(index: usize) -> String {
    format!("installer.region:{index}")
}

/// Everything the installer can be asked to do.
///
/// Not `Debug`: password, disk passphrase and network actions carry secrets, and a
/// derived `Debug` would print them into any log that formats an operation.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    /// Report the wizard's state (never a secret).
    #[serde(rename = "status")]
    Status,
    #[serde(rename = "next")]
    Next,
    #[serde(rename = "back")]
    Back,
    #[serde(rename = "set-layout")]
    SetKeyboardLayout { layout: String },
    #[serde(rename = "refresh-networks")]
    RefreshNetworks,
    #[serde(rename = "select-network")]
    SelectNetwork { ssid: String },
    #[serde(rename = "connect-network")]
    ConnectNetwork {
        #[serde(default)]
        password: Option<String>,
    },
    #[serde(rename = "set-method")]
    SetInstallMethod { method: String },
    #[serde(rename = "refresh-disks")]
    RefreshDisks,
    #[serde(rename = "select-disk")]
    SelectDisk { device: String },
    #[serde(rename = "select-region")]
    SelectFreeRegion { index: usize },
    #[serde(rename = "set-size")]
    SetAlongsideSize {
        gib: i64,
        disk: Box<crate::disks::Disk>,
        extent: crate::disks::GuidedAlongsidePlan,
    },
    #[serde(rename = "set-identity")]
    SetIdentity {
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        full_name: Option<String>,
    },
    #[serde(rename = "set-passphrase")]
    SetPassphrase {
        passphrase: String,
        confirmation: String,
    },
    #[serde(rename = "set-disk-passphrase")]
    SetDiskPassphrase {
        passphrase: String,
        confirmation: String,
        /// Empty keeps the interactive policy: a short TPM PIN only when the
        /// reviewed target proves the measured-boot boundary, otherwise a
        /// strong passphrase. `tpm2` requires that boundary; `passphrase`
        /// forces the independent strong passphrase even on a TPM2 machine.
        #[serde(default)]
        mode: String,
    },
    /// Set an account password that is already a SHA-512 crypt hash, without
    /// keeping or transmitting the plaintext. Only the unattended recipe path
    /// publishes this action.
    #[serde(rename = "set-password-hash")]
    SetPasswordHash { hash: String },
    #[serde(rename = "set-filesystem")]
    SetFilesystem { filesystem: String },
    #[serde(rename = "set-security-key")]
    SetSecurityKey { enabled: bool },
    #[serde(rename = "set-media-codecs")]
    SetMediaCodecs { enabled: bool },
    #[serde(rename = "set-hostname")]
    SetHostname { hostname: String },
    #[serde(rename = "set-locale")]
    SetLocale { locale: String },
    #[serde(rename = "set-timezone")]
    SetTimezone { timezone: String },
    #[serde(rename = "set-recovery-promise")]
    SetRecoveryPromise { acknowledged: bool },
    /// Start the install onto the reviewed target. `target` is the exact
    /// object the published confirmation phrase named; an install whose
    /// target has since changed is refused rather than redirected.
    #[serde(rename = "begin")]
    BeginInstall {
        target: String,
        /// The exact append-only geometry displayed when this action was published.
        #[serde(default)]
        allocation: Option<crate::disks::GuidedAlongsidePlan>,
    },
    #[serde(rename = "copy-recovery-key")]
    CopyRecoveryKey,
    #[serde(rename = "confirm-recovery-saved")]
    ConfirmRecoverySaved { saved: bool },
    #[serde(rename = "reboot")]
    Reboot,
    #[serde(rename = "power-off")]
    PowerOff,
    /// Leave a staged (USB-free) installer and restart into the operating
    /// system it was staged from. Firmware already consumed the one-shot
    /// `BootNext`, so a plain restart lands there.
    #[serde(rename = "return-to-host")]
    ReturnToHost,
}

impl Operation {
    /// A short, secret-free name for logs and errors.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Next => "next",
            Self::Back => "back",
            Self::SetKeyboardLayout { .. } => "set-layout",
            Self::RefreshNetworks => "refresh-networks",
            Self::SelectNetwork { .. } => "select-network",
            Self::ConnectNetwork { .. } => "connect-network",
            Self::SetInstallMethod { .. } => "set-method",
            Self::RefreshDisks => "refresh-disks",
            Self::SelectDisk { .. } => "select-disk",
            Self::SelectFreeRegion { .. } => "select-region",
            Self::SetAlongsideSize { .. } => "set-size",
            Self::SetIdentity { .. } => "set-identity",
            Self::SetPassphrase { .. } => "set-passphrase",
            Self::SetDiskPassphrase { .. } => "set-disk-passphrase",
            Self::SetPasswordHash { .. } => "set-password-hash",
            Self::SetFilesystem { .. } => "set-filesystem",
            Self::SetSecurityKey { .. } => "set-security-key",
            Self::SetMediaCodecs { .. } => "set-media-codecs",
            Self::SetHostname { .. } => "set-hostname",
            Self::SetLocale { .. } => "set-locale",
            Self::SetTimezone { .. } => "set-timezone",
            Self::SetRecoveryPromise { .. } => "set-recovery-promise",
            Self::BeginInstall { .. } => "begin",
            Self::CopyRecoveryKey => "copy-recovery-key",
            Self::ConfirmRecoverySaved { .. } => "confirm-recovery-saved",
            Self::Reboot => "reboot",
            Self::PowerOff => "power-off",
            Self::ReturnToHost => "return-to-host",
        }
    }

    /// Decode a payload produced by one of the installer's own specs.
    ///
    /// # Errors
    ///
    /// Returns a secret-free message when the payload is not an installer
    /// operation.
    pub fn decode(payload: &[u8]) -> Result<Self, String> {
        // Report the category only: serde's message can quote the offending
        // input, and that input may be a passphrase.
        serde_json::from_slice(payload).map_err(|error| payload_error(&error))
    }

    /// The arguments of this operation, as the canonical effect envelope
    /// carries them: its fields without the serde tag.
    pub(crate) fn arguments(&self) -> serde_json::Map<String, serde_json::Value> {
        let serde_json::Value::Object(mut arguments) =
            serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
        else {
            return serde_json::Map::new();
        };
        arguments.remove("op");
        arguments.retain(|_, value| !value.is_null());
        arguments
    }

    /// The operation `id` with `arguments`: the inverse of [`Self::arguments`]
    /// under the canonical effect envelope.
    pub(crate) fn from_arguments(
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, String> {
        let mut invocation = arguments.clone();
        invocation.insert("op".into(), serde_json::Value::String(id.to_owned()));
        serde_json::from_value(serde_json::Value::Object(invocation))
            .map_err(|error| payload_error(&error))
    }
}

/// Report a payload's category only: serde's message can quote the offending
/// input, and that input may be a passphrase.
fn payload_error(error: &serde_json::Error) -> String {
    let category = match error.classify() {
        serde_json::error::Category::Io => "unreadable payload",
        serde_json::error::Category::Syntax => "malformed payload",
        serde_json::error::Category::Data => "unexpected fields or values",
        serde_json::error::Category::Eof => "truncated payload",
    };
    format!("not an installer operation: {category}")
}

impl ui::tilo_resource::Effect for InstallerEffect {
    const OPERATION: &'static str = OPERATION_TYPE;

    fn declarations() -> Vec<ui::tilo_resource::effect::EffectDecl> {
        installer_declarations()
    }

    fn id(&self) -> &'static str {
        match self {
            Self::Install(operation) => operation.name(),
            Self::Online(control) => control.id(),
        }
    }

    fn arguments(&self) -> serde_json::Map<String, serde_json::Value> {
        match self {
            Self::Install(operation) => operation.arguments(),
            Self::Online(_) => serde_json::Map::new(),
        }
    }

    fn decode(
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, String> {
        if let Some(control) = OnlineControl::from_id(id) {
            return Ok(Self::Online(control));
        }
        Operation::from_arguments(id, arguments).map(Self::Install)
    }
}

fn installer_declaration(
    id: &'static str,
    label: &'static str,
    params: Vec<tilo_actions::ActionParam>,
    kind: tilo_actions::OperationKind,
    risk: tilo_actions::RiskLevel,
    answers: ui::tilo_resource::effect::Answers,
) -> ui::tilo_resource::effect::EffectDecl {
    let view = kind == tilo_actions::OperationKind::View;
    ui::tilo_resource::effect::EffectDecl {
        id,
        label,
        description: "",
        params,
        kind,
        row: false,
        answers,
        risk,
        idempotent: false,
        action_kind: if view {
            tilo_actions::ActionKind::Read
        } else {
            tilo_actions::ActionKind::Outside
        },
        own_device: false,
        attended: false,
        risk_of: None,
        audience: tilo_actions::Audience::Everyone,
        untrusted: false,
        opens: None,
        participant: false,
        edits: false,
        member: false,
        item: None,
        offload: None,
        tags: &[],
        outside_with: None,
    }
}

/// The declarations of the installer's one vocabulary. The accounts controls
/// are ordinary effects here too, so the catalogue and the toolkit's effect
/// host see the same operation type.
#[expect(
    clippy::too_many_lines,
    reason = "one table entry per effect keeps the installer's vocabulary together"
)]
fn installer_declarations() -> Vec<ui::tilo_resource::effect::EffectDecl> {
    use tilo_actions::{ActionParam, OperationKind, ParamType, RiskLevel};
    use ui::tilo_resource::effect::Answers;

    let text = |name: &'static str, description: &'static str| {
        ActionParam::required(name, ParamType::Text, description)
    };
    let secret = |name: &'static str, description: &'static str| {
        ActionParam::required(name, ParamType::Secret, description)
    };
    let boolean = |name: &'static str, description: &'static str| {
        ActionParam::required(name, ParamType::Boolean, description)
    };
    let effect = |id, label, params| {
        installer_declaration(
            id,
            label,
            params,
            OperationKind::Effect,
            RiskLevel::Low,
            Answers::Nothing,
        )
    };

    vec![
        installer_declaration(
            "status",
            "Installer status",
            Vec::new(),
            OperationKind::View,
            RiskLevel::None,
            Answers::Value,
        ),
        effect("next", "Next", Vec::new()),
        effect("back", "Back", Vec::new()),
        effect(
            "return-to-host",
            "Return to the existing system",
            Vec::new(),
        ),
        effect(
            "set-layout",
            "Choose keyboard layout",
            vec![text("layout", "Keyboard layout")],
        ),
        effect("refresh-networks", "Refresh networks", Vec::new()),
        effect(
            "select-network",
            "Choose Wi-Fi network",
            vec![text("ssid", "Network name")],
        ),
        effect(
            "connect-network",
            "Connect",
            vec![ActionParam::optional(
                "password",
                ParamType::Secret,
                "Wi-Fi password (secured networks only)",
            )],
        ),
        effect("add", "Sign in using private form", Vec::new()),
        effect("retry", "Retry using private form", Vec::new()),
        effect("cancel", "Cancel sign-in", Vec::new()),
        effect("skip-offline", "Continue offline", Vec::new()),
        effect(
            "set-method",
            "Choose install method",
            vec![text(
                "method",
                "erase: use the whole disk; alongside: keep existing partitions",
            )],
        ),
        effect("refresh-disks", "Refresh disks", Vec::new()),
        effect("select-disk", "Use this disk", Vec::new()),
        effect("select-region", "Use this region", Vec::new()),
        effect(
            "set-size",
            "Use N GiB for Tilo",
            vec![ActionParam::required(
                "gib",
                ParamType::Integer,
                "Total Tilo capacity in whole GiB (at least 35)",
            )],
        ),
        effect(
            "set-identity",
            "Set username and name",
            vec![
                ActionParam::optional(
                    "username",
                    ParamType::Text,
                    "Up to 32 lowercase letters, digits, _ or -; starts with a letter or _",
                ),
                ActionParam::optional("full_name", ParamType::Text, "Full name (optional)"),
            ],
        ),
        effect(
            "set-passphrase",
            "Set account and default startup secret",
            vec![
                secret("passphrase", "Startup and unlock passphrase"),
                secret("confirmation", "The same secret again"),
            ],
        ),
        effect(
            "set-disk-passphrase",
            "Set a separate disk startup secret",
            vec![
                secret("passphrase", "Startup and unlock passphrase"),
                secret("confirmation", "The same disk secret again"),
                ActionParam::optional(
                    "mode",
                    ParamType::Text,
                    "`tpm2` requires the measured-boot boundary; `passphrase` forces an independent strong passphrase. Omit for the reviewed target's normal policy.",
                ),
            ],
        ),
        effect(
            "set-password-hash",
            "Set a pre-hashed account password",
            vec![secret("hash", "A canonical SHA-512 crypt hash")],
        ),
        effect(
            "set-filesystem",
            "Choose filesystem",
            vec![text("filesystem", "Root filesystem")],
        ),
        effect(
            "set-security-key",
            "Enroll a security key",
            vec![boolean(
                "enabled",
                "Enroll a FIDO2 security key as another unlock factor",
            )],
        ),
        effect(
            "set-hostname",
            "Set hostname",
            vec![text(
                "hostname",
                "Letters, digits and hyphens; no leading or trailing hyphen; at most 63",
            )],
        ),
        effect(
            "set-locale",
            "Choose language and locale",
            vec![text("locale", "Language and locale")],
        ),
        effect(
            "set-timezone",
            "Choose time zone",
            vec![text("timezone", "IANA time zone")],
        ),
        effect(
            "set-media-codecs",
            "Install media codecs",
            vec![boolean("enabled", "Install more video and audio formats")],
        ),
        effect(
            "set-recovery-promise",
            "I will save my recovery key",
            vec![boolean(
                "acknowledged",
                "A recovery key is shown once during install; it is the only backup if the TPM or passphrase is lost",
            )],
        ),
        installer_declaration(
            "begin",
            "Begin installation",
            Vec::new(),
            OperationKind::Effect,
            RiskLevel::Destructive,
            Answers::Nothing,
        ),
        effect("copy-recovery-key", "Copy recovery key", Vec::new()),
        effect(
            "confirm-recovery-saved",
            "I saved this key off this computer",
            vec![boolean(
                "saved",
                "The recovery key is stored somewhere other than this computer",
            )],
        ),
        effect("reboot", "Reboot now", Vec::new()),
        effect("power-off", "Power off", Vec::new()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_round_trip_and_refusals_never_echo_secrets() {
        {
            for operation in [
                Operation::Next,
                Operation::SelectDisk {
                    device: "/dev/vdb".into(),
                },
                Operation::SetIdentity {
                    username: Some("ada".into()),
                    full_name: None,
                },
                Operation::BeginInstall {
                    target: "/dev/vdb".into(),
                    allocation: None,
                },
            ] {
                let bytes = serde_json::to_vec(&operation).unwrap();
                assert!(Operation::decode(&bytes).unwrap() == operation);
            }
            assert!(
                Operation::decode(br#"{"op":"select-disk","device":"/dev/vdb","extra":1}"#)
                    .is_err()
            );
            assert!(Operation::decode(br#"{"op":"format-everything"}"#).is_err());
        }
        {
            let error = Operation::decode(
                br#"{"op":"set-passphrase","passphrase":"hunter2-correct-horse","confirmation":5}"#,
            )
            .err()
            .unwrap();
            assert!(!error.contains("hunter2"), "{error}");
        }
    }
}
