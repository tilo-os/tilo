//! Secret-free browser credential protocol. Passwords stay in native CEF IPC.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) fn counter(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 16
        && !value.starts_with('0')
        && value.bytes().all(|b| b.is_ascii_digit())
        && value
            .parse::<u64>()
            .is_ok_and(|n| n <= 9_007_199_254_740_991)
}

#[derive(Clone, Default, PartialEq, Eq, Serialize)]
pub struct AddressFields {
    pub full_name: String,
    pub given_name: String,
    pub family_name: String,
    pub street: String,
    pub city: String,
    pub region: String,
    pub postal_code: String,
    pub country: String,
}
impl std::fmt::Debug for AddressFields {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AddressFields(<personal data>)")
    }
}
impl AddressFields {
    fn valid(&self) -> bool {
        [
            &self.full_name,
            &self.given_name,
            &self.family_name,
            &self.street,
            &self.city,
            &self.region,
            &self.postal_code,
            &self.country,
        ]
        .iter()
        .all(|s| s.len() <= 1024)
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum CredentialAction {
    Status,
    Cancel,
    Discover,
    DiscoverAddresses,
    List,
    Capture {
        form: String,
    },
    Save {
        capture: String,
    },
    Update {
        capture: String,
        account: String,
    },
    Fill {
        form: String,
        account: String,
    },
    Delete {
        account: String,
    },
    FillAddress {
        form: String,
        profile: AddressFields,
    },
}
impl CredentialAction {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Cancel => "cancel",
            Self::Discover => "discover",
            Self::DiscoverAddresses => "discover_addresses",
            Self::List => "list",
            Self::Capture { .. } => "capture",
            Self::Save { .. } => "save",
            Self::Update { .. } => "update",
            Self::Fill { .. } => "fill",
            Self::Delete { .. } => "delete",
            Self::FillAddress { .. } => "fill_address",
        }
    }
    fn valid(&self) -> bool {
        let account = |s: &str| !s.is_empty() && s.len() <= 128;
        match self {
            Self::Capture { form } => counter(form),
            Self::Save { capture } => counter(capture),
            Self::Update {
                capture,
                account: id,
            } => counter(capture) && account(id),
            Self::Fill { form, account: id } => counter(form) && account(id),
            Self::Delete { account: id } => account(id),
            Self::FillAddress { form, profile } => counter(form) && profile.valid(),
            _ => true,
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct CredentialRequest {
    pub request_id: u32,
    pub origin: String,
    pub document: String,
    pub action: CredentialAction,
}
impl CredentialRequest {
    pub(crate) fn valid(&self) -> bool {
        self.request_id > 0
            && i32::try_from(self.request_id).is_ok()
            && !self.origin.is_empty()
            && self.origin.len() <= 8192
            && (matches!(
                self.action,
                CredentialAction::Status | CredentialAction::Cancel
            ) || counter(&self.document))
            && self.document.len() <= 16
            && self.action.valid()
    }
    pub(crate) fn encode(&self) -> Option<Value> {
        if !self.valid() {
            return None;
        }
        let mut value = serde_json::Map::from_iter([
            ("op".into(), json!("credential_action")),
            ("request_id".into(), json!(self.request_id)),
            ("origin".into(), json!(self.origin)),
            ("document".into(), json!(self.document)),
            ("action".into(), json!(self.action.name())),
        ]);
        match &self.action {
            CredentialAction::Capture { form } => {
                value.insert("form".into(), json!(form));
            }
            CredentialAction::Save { capture } => {
                value.insert("capture".into(), json!(capture));
            }
            CredentialAction::Update { capture, account } => {
                value.insert("capture".into(), json!(capture));
                value.insert("account".into(), json!(account));
            }
            CredentialAction::Fill { form, account } => {
                value.insert("form".into(), json!(form));
                value.insert("account".into(), json!(account));
            }
            CredentialAction::Delete { account } => {
                value.insert("account".into(), json!(account));
            }
            CredentialAction::FillAddress { form, profile } => {
                value.insert("form".into(), json!(form));
                value.insert("profile".into(), json!(profile));
            }
            _ => {}
        }
        Some(Value::Object(value))
    }
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialForm {
    pub id: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub kind: String,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialAccount {
    pub id: String,
    pub username: String,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialReply {
    pub request_id: u32,
    pub action: String,
    pub origin: String,
    pub document: String,
    pub success: bool,
    pub message: String,
    #[serde(default)]
    pub forms: Vec<CredentialForm>,
    #[serde(default)]
    pub accounts: Vec<CredentialAccount>,
    #[serde(default)]
    pub capture: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub late: bool,
}
impl CredentialReply {
    fn bounded_fields(value: &Value) -> Option<serde_json::Map<String, Value>> {
        let object = value.as_object()?;
        if value["event"].as_str()? != "credential_result"
            || object.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "event"
                        | "tab"
                        | "request_id"
                        | "action"
                        | "origin"
                        | "document"
                        | "success"
                        | "message"
                        | "forms"
                        | "accounts"
                        | "capture"
                        | "username"
                        | "late"
                )
            })
        {
            return None;
        }
        if value.get("tab").is_some_and(|v| {
            !v.as_u64()
                .is_some_and(|n| n > 0 && i32::try_from(n).is_ok())
        }) {
            return None;
        }
        // Bound collections before cloning/deserializing. Never keep unknown
        // fields from a native reply in widget state or diagnostics.
        for (key, limit) in [("forms", 32), ("accounts", 64)] {
            if let Some(items) = value.get(key) {
                let items = items.as_array()?;
                if items.len() > limit {
                    return None;
                }
                for item in items {
                    let entry = item.as_object()?;
                    if entry
                        .keys()
                        .any(|k| k != "id" && k != "username" && !(key == "forms" && k == "kind"))
                    {
                        return None;
                    }
                    if !entry
                        .get("id")?
                        .as_str()
                        .is_some_and(|s| !s.is_empty() && s.len() <= 128)
                    {
                        return None;
                    }
                    if entry
                        .get("username")
                        .is_some_and(|v| v.as_str().is_none_or(|s| s.len() > 1024))
                    {
                        return None;
                    }
                }
            }
        }
        for (key, limit) in [
            ("origin", 8192),
            ("document", 16),
            ("message", 1024),
            ("action", 32),
            ("username", 1024),
            ("capture", 16),
        ] {
            if value
                .get(key)
                .is_some_and(|v| v.as_str().is_none_or(|s| s.len() > limit))
            {
                return None;
            }
        }
        let mut fields = object.clone();
        fields.remove("event");
        fields.remove("tab");
        Some(fields)
    }

    pub(crate) fn decode(value: &Value) -> Option<Self> {
        let fields = Self::bounded_fields(value)?;
        let reply: Self = serde_json::from_value(Value::Object(fields)).ok()?;
        if reply.request_id == 0
            || reply.request_id > i32::MAX as u32
            || (!reply.document.is_empty() && !counter(&reply.document))
            || !matches!(
                reply.action.as_str(),
                "status"
                    | "cancel"
                    | "discover"
                    | "discover_addresses"
                    | "list"
                    | "capture"
                    | "save"
                    | "update"
                    | "fill"
                    | "delete"
                    | "fill_address"
            )
            || reply.capture.as_ref().is_some_and(|s| !counter(s))
        {
            return None;
        }
        if reply.success
            && reply.action != "cancel"
            && (reply.origin.is_empty() || !counter(&reply.document))
        {
            return None;
        }
        if reply.success
            && reply.action == "capture"
            && (reply.capture.is_none() || reply.username.is_none())
        {
            return None;
        }
        if (!reply.forms.is_empty()
            && !matches!(reply.action.as_str(), "discover" | "discover_addresses"))
            || (!reply.accounts.is_empty() && reply.action != "list")
            || ((reply.capture.is_some() || reply.username.is_some()) && reply.action != "capture")
        {
            return None;
        }
        let mut ids = std::collections::BTreeSet::new();
        if !reply.forms.iter().all(|f| {
            counter(&f.id)
                && ids.insert(&f.id)
                && if reply.action == "discover" {
                    matches!(f.kind.as_str(), "current-password" | "new-password")
                } else {
                    f.kind.is_empty()
                }
        }) {
            return None;
        }
        let mut ids = std::collections::BTreeSet::new();
        if !reply
            .accounts
            .iter()
            .all(|a| !a.id.is_empty() && ids.insert(&a.id))
        {
            return None;
        }
        Some(reply)
    }
}
