//! Typed semantic requests. Neither this payload nor private transport is a mutation receipt.
use serde::{Deserialize, Serialize};

pub const APP_ID: &str = "dev.qos.desktop";
pub const RESOURCE: &str = "desktop:session";
pub const ACTION: &str = "command";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopCommandError {
    /// Rejected before any canonical state or durable identity changed.
    Rejected(String),
    /// The durable barrier failed or its outcome cannot be established.
    Uncertain(String),
}
impl std::fmt::Display for DesktopCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(message) | Self::Uncertain(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for DesktopCommandError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSource {
    pub room: String,
    /// Exact admitted Automerge heads observed by the initiating desktop.
    pub heads: Vec<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundCommand {
    pub source: CommandSource,
    pub command: DesktopCommand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabLocation {
    pub strip: u64,
    pub group: u64,
    pub tab: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabPlacement {
    /// Native default placement when an admitted workspace has no strips yet.
    EmptyWorkspace,
    InGroup {
        strip: u64,
        group: u64,
        after: Option<u64>,
    },
    NewGroup {
        strip: u64,
        after: Option<u64>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DesktopCommand {
    OpenTab {
        app: String,
        placement: TabPlacement,
    },
    CloseTab {
        location: TabLocation,
    },
    DuplicateTab {
        location: TabLocation,
    },
    ReorderTab {
        location: TabLocation,
        before: Option<u64>,
    },
    ReorderGroup {
        strip: u64,
        group: u64,
        before: Option<u64>,
    },
    MoveTab {
        location: TabLocation,
        placement: TabPlacement,
    },
    SplitTab {
        location: TabLocation,
        before: Option<u64>,
    },
    CreateStrip {
        name: String,
    },
    RenameStrip {
        strip: u64,
        name: String,
    },
    RemoveStrip {
        strip: u64,
    },
    RenameWorkspace {
        name: String,
    },
    /// Open `app` in a new window: the same result as a person opening it
    /// from the launcher, for callers that do not hold a desktop layout (an
    /// agent, the command palette). The owner places it in its own turn, as
    /// a new group at the end of its last strip, or as the first strip of an
    /// empty workspace. Placement-independent, so a stale source revision
    /// does not refuse it. Appended last: existing encodings are unchanged.
    OpenApp {
        app: String,
    },
}

/// Failure code of every definite desktop command refusal.
pub const REJECTED_CODE: &str = "desktop_command_rejected";
/// `kind` of a desktop command receipt value.
pub const RECEIPT_KIND: &str = "desktop.command";

/// Identity of one desktop command (optimistic open, close, reorder): the
/// certified issuer device and the durable invocation UUID the issuer minted.
/// A retry reuses it. It is only an identifier: it never grants anything,
/// and the device comes from authenticated admission, never from arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DesktopCommandId {
    pub device: [u8; 32],
    pub invocation: uuid::Uuid,
}
impl DesktopCommandId {
    /// The id of an owner desktop request, from its authenticated caller.
    pub fn of(request: &crate::owner_commit::OwnerCommitRequest) -> Option<Self> {
        match &request.operation {
            crate::owner_commit::CommitOperation::DesktopCommand {
                invocation, caller, ..
            } => Some(Self {
                device: caller.device_public_key,
                invocation: *invocation,
            }),
            _ => None,
        }
    }
}
/// `<64 hex device>:<uuid>`: the Cloud Store's receipt scope.
impl std::fmt::Display for DesktopCommandId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", hex::encode(self.device), self.invocation)
    }
}
impl std::str::FromStr for DesktopCommandId {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, String> {
        let (hex_device, invocation) = text.split_once(':').ok_or("desktop command id")?;
        let device: [u8; 32] = hex::decode(hex_device)
            .map_err(|e| e.to_string())?
            .try_into()
            .map_err(|_| "desktop command device")?;
        let invocation = uuid::Uuid::parse_str(invocation).map_err(|e| e.to_string())?;
        if invocation.is_nil()
            || invocation.to_string() != text[65..]
            || hex::encode(device) != hex_device
        {
            return Err("desktop command id must be canonical".into());
        }
        Ok(Self { device, invocation })
    }
}

/// u64 desktop ids cross to JavaScript as decimal strings.
mod decimal {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// One window a committed command created.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedWindow {
    pub app_id: String,
    pub record_id: String,
    #[serde(with = "decimal")]
    pub strip: u64,
    #[serde(with = "decimal")]
    pub group: u64,
    #[serde(with = "decimal")]
    pub tab: u64,
}

/// The value of a desktop command's `Succeeded` update: the receipt the
/// issuer (person or agent) matches its command id against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopCommandReceipt {
    pub kind: String,
    /// [`DesktopCommandId`] text form.
    pub command: String,
    /// Owner generation holding the change.
    pub generation: u64,
    /// Windows created, in creation order; empty for close and reorder, and
    /// on owners that do not yet report them.
    pub opened: Vec<OpenedWindow>,
}
impl DesktopCommandReceipt {
    pub fn new(id: &DesktopCommandId, generation: u64, opened: Vec<OpenedWindow>) -> Self {
        Self {
            kind: RECEIPT_KIND.into(),
            command: id.to_string(),
            generation,
            opened,
        }
    }
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    /// JSON Schema of the receipt value: the declared result of every
    /// published desktop command action (open and the layout command).
    pub fn schema() -> serde_json::Value {
        let id = serde_json::json!({"type": "string", "pattern": "^[0-9]+$"});
        serde_json::json!({
            "type": "object",
            "required": ["kind", "command", "generation", "opened"],
            "properties": {
                "kind": {"const": RECEIPT_KIND},
                "command": {"type": "string"},
                "generation": {"type": "integer", "minimum": 1},
                "opened": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["appId", "recordId", "strip", "group", "tab"],
                        "properties": {
                            "appId": {"type": "string"},
                            "recordId": {"type": "string"},
                            "strip": id,
                            "group": id,
                            "tab": id,
                        },
                    },
                },
            },
        })
    }
}

/// Stable reason of a definite refusal, for presentation only: every
/// refusal snaps an optimistic command back, whatever its reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopRefusal {
    NotPermitted,
    StaleSource,
    UnknownApp,
    ForeignTarget,
    TargetMissing,
    IdConflict,
    Invalid,
}
impl DesktopRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotPermitted => "not_permitted",
            Self::StaleSource => "stale_source",
            Self::UnknownApp => "unknown_app",
            Self::ForeignTarget => "foreign_target",
            Self::TargetMissing => "target_missing",
            Self::IdConflict => "id_conflict",
            Self::Invalid => "invalid",
        }
    }
    /// Classify an owner's [`DesktopCommandError::Rejected`] message.
    pub fn classify(message: &str) -> Self {
        let m = message.to_ascii_lowercase();
        let any = |needles: &[&str]| needles.iter().any(|needle| m.contains(needle));
        if any(&[
            "reused",
            "identity conflict",
            "substitution",
            "already reserved",
        ]) {
            Self::IdConflict
        } else if any(&["another topology owner", "foreign"]) {
            Self::ForeignTarget
        } else if any(&[
            "cannot be opened",
            "checked record factory",
            "canonical application identity",
        ]) {
            Self::UnknownApp
        } else if any(&[
            "cannot change",
            "no longer admitted",
            "no longer current",
            "blocked",
            "may only open",
            "require certified",
            "not a member",
            "permission",
            "denied",
            "refused",
        ]) {
            Self::NotPermitted
        } else if any(&["stale", "no admitted source revision"]) {
            Self::StaleSource
        } else if any(&["was removed", "no longer present", "removed or moved"]) {
            Self::TargetMissing
        } else {
            Self::Invalid
        }
    }
    /// `ActionFailure.details` of a refusal: the command id and the reason.
    pub fn details(self, id: &DesktopCommandId) -> serde_json::Value {
        serde_json::json!({"command": id.to_string(), "reason": self.as_str()})
    }
}

#[cfg(test)]
mod command_id_tests {
    use super::*;

    #[test]
    fn a_command_id_round_trips_through_its_text_form() {
        let id = DesktopCommandId {
            device: [0xab; 32],
            invocation: uuid::Uuid::from_u128(0x1234_5678_9abc_4def_8123_456789abcdef),
        };
        let text = id.to_string();
        assert_eq!(text.len(), 64 + 1 + 36);
        assert_eq!(text.parse::<DesktopCommandId>().unwrap(), id);
        assert!("ab:not-a-uuid".parse::<DesktopCommandId>().is_err());
        let nil = format!("{}:{}", hex::encode([1; 32]), uuid::Uuid::nil());
        assert!(nil.parse::<DesktopCommandId>().is_err());
        let upper = text.to_ascii_uppercase();
        assert!(
            upper.parse::<DesktopCommandId>().is_err(),
            "one canonical form"
        );
    }

    #[test]
    fn a_receipt_carries_the_id_and_window_ids_as_decimal_strings() {
        let id = DesktopCommandId {
            device: [7; 32],
            invocation: uuid::Uuid::from_u128(9),
        };
        let receipt = DesktopCommandReceipt::new(
            &id,
            5,
            vec![OpenedWindow {
                app_id: "so.tilo.notes".into(),
                record_id: "r".into(),
                strip: u64::MAX,
                group: 2,
                tab: 3,
            }],
        );
        let value = receipt.to_value();
        assert_eq!(value["kind"], RECEIPT_KIND);
        assert_eq!(value["command"], id.to_string());
        assert_eq!(value["opened"][0]["strip"], u64::MAX.to_string());
        assert_eq!(value["opened"][0]["appId"], "so.tilo.notes");
        let back: DesktopCommandReceipt = serde_json::from_value(value).unwrap();
        assert_eq!(back, receipt);
    }

    #[test]
    fn refusal_messages_map_to_stable_reasons() {
        use DesktopRefusal::*;
        for (message, reason) in [
            ("caller cannot change this desktop", NotPermitted),
            ("agent tasks may only open applications", NotPermitted),
            ("desktop source revision is stale", StaleSource),
            ("application cannot be opened here", UnknownApp),
            ("target belongs to another topology owner", ForeignTarget),
            ("tab was removed or moved", TargetMissing),
            (
                "invocation identity reused with a different desktop command",
                IdConflict,
            ),
            ("desktop command exceeds bounded size", Invalid),
        ] {
            assert_eq!(DesktopRefusal::classify(message), reason, "{message}");
        }
        let id = DesktopCommandId {
            device: [1; 32],
            invocation: uuid::Uuid::from_u128(2),
        };
        assert_eq!(
            StaleSource.details(&id),
            serde_json::json!({"command": id.to_string(), "reason": "stale_source"})
        );
    }
}
