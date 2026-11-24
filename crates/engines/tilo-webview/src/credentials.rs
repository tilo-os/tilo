//! The engine's side of a page's logins: the forms it finds, what a person typed into one, and
//! filling one. The logins themselves are kept in the workspace's vault, never by the engine: a
//! password crosses this protocol only to fill a form ([`CredentialAction::Fill`]) and to take
//! what a form sent for the vault to keep ([`CredentialAction::Take`]), each held once and wiped
//! once written ([`ReleasedText`]).
use crate::ReleasedText;
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
    /// Read what the person typed into a login form: the engine keeps the password and answers
    /// the capture's number and the name.
    Capture {
        form: String,
    },
    /// Hand over the password of a capture, once its form still reads as it did, for the vault
    /// to keep: the reply carries it ([`CredentialReply::password`]), and nothing else does.
    Take {
        capture: String,
    },
    /// Fill a login form with a name and a password the vault handed over for this site.
    Fill {
        form: String,
        username: String,
        password: ReleasedText,
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
            Self::Capture { .. } => "capture",
            Self::Take { .. } => "take",
            Self::Fill { .. } => "fill",
            Self::FillAddress { .. } => "fill_address",
        }
    }
    fn valid(&self) -> bool {
        match self {
            Self::Capture { form } => counter(form),
            Self::Take { capture } => counter(capture),
            Self::Fill {
                form,
                username,
                password,
            } => {
                counter(form)
                    && username.len() <= 1024
                    && password
                        .held_bytes()
                        .is_some_and(|bytes| bytes > 0 && bytes <= 4096)
            }
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
            CredentialAction::Take { capture } => {
                value.insert("capture".into(), json!(capture));
            }
            CredentialAction::Fill {
                form,
                username,
                password,
            } => {
                // Written once: whoever writes the command wipes it after ([`wipe`]).
                let password = password.take()?;
                value.insert("form".into(), json!(form));
                value.insert("username".into(), json!(username));
                value.insert("password".into(), json!(password.as_str()));
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
    pub capture: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub late: bool,
    /// The password a [`CredentialAction::Take`] handed over, held once for the vault to keep.
    #[serde(skip)]
    pub password: Option<ReleasedText>,
}

/// Wipe the password of a credential command once it was written to the engine.
pub(crate) fn wipe(value: &mut Value) {
    if value.get("op").and_then(Value::as_str) == Some("credential_action")
        && let Some(Value::String(password)) = value.get_mut("password")
    {
        zeroize::Zeroize::zeroize(password);
    }
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
                        | "capture"
                        | "password"
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
        if let Some(items) = value.get("forms") {
            let items = items.as_array()?;
            if items.len() > 32 {
                return None;
            }
            for item in items {
                {
                    let entry = item.as_object()?;
                    if entry
                        .keys()
                        .any(|k| k != "id" && k != "username" && k != "kind")
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
            ("password", 4096),
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
        fields.remove("password");
        Some(fields)
    }

    pub(crate) fn decode(value: &Value) -> Option<Self> {
        let fields = Self::bounded_fields(value)?;
        let mut reply: Self = serde_json::from_value(Value::Object(fields)).ok()?;
        reply.password = value
            .get("password")
            .and_then(Value::as_str)
            .map(|password| ReleasedText::new(password.to_owned()));
        if reply.request_id == 0
            || reply.request_id > i32::MAX as u32
            || (!reply.document.is_empty() && !counter(&reply.document))
            || !matches!(
                reply.action.as_str(),
                "status"
                    | "cancel"
                    | "discover"
                    | "discover_addresses"
                    | "capture"
                    | "take"
                    | "fill"
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
        // A password crosses only as what a take handed over, with the name it signs in as.
        if reply.password.is_some()
            && !(reply.success && reply.action == "take" && reply.username.is_some())
        {
            return None;
        }
        if reply.success
            && reply.action == "take"
            && (reply.password.is_none() || reply.username.is_none())
        {
            return None;
        }
        if (!reply.forms.is_empty()
            && !matches!(reply.action.as_str(), "discover" | "discover_addresses"))
            || (reply.capture.is_some() && reply.action != "capture")
            || (reply.username.is_some() && !matches!(reply.action.as_str(), "capture" | "take"))
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
        Some(reply)
    }
}
