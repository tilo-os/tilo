//! The session's `NetworkManager` secret agent.
//!
//! `NetworkManager` keeps connection profiles but asks a registered agent in
//! the user's session whenever a connection needs a secret it does not hold:
//! an 802.1X user name and password, a Wi-Fi password that changed, a VPN
//! password, a SIM PIN. Without an agent nothing asks and the connection
//! simply fails. The shell registers itself ([`service`]) and asks in a sheet
//! the compositor draws ([`crate::shell::portal_dialog`]) — never in a window
//! an application could imitate or read keystrokes from.
//!
//! Secrets go back to `NetworkManager` only. "Remember on this device" keeps
//! them sealed ([`store`]): a WPA passphrase as the person's roaming Wi-Fi
//! credential when their devices share one, anything else in the network
//! secrets vault. Nothing here writes a secret into settings or a log.
//!
//! [`request_secrets`] is the decision — stored answer, sheet, or refusal —
//! written against [`SecretPrompter`] and [`store::SecretStore`] so it can be
//! tested without a bus or a screen.

pub mod service;
pub mod store;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use zbus::zvariant::{OwnedValue, Value};
use zeroize::Zeroizing;

use crate::shell::i18n::{t, t_args};
use crate::shell::portal_dialog::{SecretFieldRule, SecretFieldSpec};
use store::{SecretStore, StoreKey};

/// `GetSecrets` flags (`NMSecretAgentGetSecretsFlags`).
pub const FLAG_ALLOW_INTERACTION: u32 = 0x1;
pub const FLAG_REQUEST_NEW: u32 = 0x2;
pub const FLAG_USER_REQUESTED: u32 = 0x4;
pub const FLAG_WPS_PBC_ACTIVE: u32 = 0x8;

/// A secret property's `<key>-flags` (`NMSettingSecretFlags`).
pub const SECRET_AGENT_OWNED: u32 = 0x1;
pub const SECRET_NOT_SAVED: u32 = 0x2;
pub const SECRET_NOT_REQUIRED: u32 = 0x4;

/// A connection as `NetworkManager` sends it: setting name → key → value.
pub type ConnectionDict = HashMap<String, HashMap<String, OwnedValue>>;

/// Why a secrets request ended without secrets, as `NetworkManager`'s
/// `SecretAgent` errors.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.NetworkManager.SecretAgent")]
pub enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    /// The caller is not `NetworkManager`.
    PermissionDenied(String),
    /// The connection or setting is not one this agent can answer for.
    InvalidConnection(String),
    /// The person dismissed the sheet.
    UserCanceled(String),
    /// `NetworkManager` withdrew the request.
    AgentCanceled(String),
    /// Nothing stored, and asking was not allowed.
    NoSecrets(String),
}

/// The secrets for one setting, key → value. Wiped on drop; `Debug`
/// shows the keys only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SettingSecrets(pub BTreeMap<String, Zeroizing<String>>);

impl std::fmt::Debug for SettingSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

impl SettingSecrets {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|value| value.as_str())
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.insert(key.into(), Zeroizing::new(value.into()));
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// How the answer is laid out for `NetworkManager`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponseShape {
    /// `{setting: {key: value}}`.
    Flat,
    /// `{"vpn": {"secrets": {key: value}}}`.
    VpnSecrets,
}

/// One `GetSecrets` call, understood: what to ask and how to answer.
#[derive(Clone, Debug)]
pub struct SecretRequest {
    pub connection_path: String,
    pub connection_uuid: String,
    pub connection_id: String,
    pub setting_name: String,
    pub flags: u32,
    pub title: String,
    pub detail: String,
    pub fields: Vec<SecretFieldSpec>,
    /// Whether "Remember on this device" starts ticked.
    pub remember_default: bool,
    /// The network and its security, when the answer is a WPA passphrase
    /// the person's devices may share.
    pub roaming_wifi: Option<(String, os_actions::WifiSecurity)>,
    shape: ResponseShape,
}

fn prop<'a>(connection: &'a ConnectionDict, setting: &str, key: &str) -> Option<&'a OwnedValue> {
    connection.get(setting)?.get(key)
}

fn prop_str(connection: &ConnectionDict, setting: &str, key: &str) -> Option<String> {
    match &**prop(connection, setting, key)? {
        Value::Str(value) => Some(value.to_string()),
        _ => None,
    }
}

fn prop_u32(connection: &ConnectionDict, setting: &str, key: &str) -> Option<u32> {
    match &**prop(connection, setting, key)? {
        Value::U32(value) => Some(*value),
        Value::I32(value) => u32::try_from(*value).ok(),
        _ => None,
    }
}

fn prop_bool(connection: &ConnectionDict, setting: &str, key: &str) -> Option<bool> {
    match &**prop(connection, setting, key)? {
        Value::Bool(value) => Some(*value),
        _ => None,
    }
}

fn prop_bytes(connection: &ConnectionDict, setting: &str, key: &str) -> Option<Vec<u8>> {
    Vec::<u8>::try_from(prop(connection, setting, key)?.try_clone().ok()?).ok()
}

fn prop_strings(connection: &ConnectionDict, setting: &str, key: &str) -> Vec<String> {
    prop(connection, setting, key)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| Vec::<String>::try_from(value).ok())
        .unwrap_or_default()
}

fn prop_str_dict(
    connection: &ConnectionDict,
    setting: &str,
    key: &str,
) -> BTreeMap<String, String> {
    prop(connection, setting, key)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| HashMap::<String, String>::try_from(value).ok())
        .map(|map| map.into_iter().collect())
        .unwrap_or_default()
}

fn masked(key: &str, label: &str, required: bool, rule: SecretFieldRule) -> SecretFieldSpec {
    SecretFieldSpec {
        key: key.into(),
        label: label.into(),
        masked: true,
        required,
        initial: String::new(),
        rule,
    }
}

fn visible(key: &str, label: &str, required: bool, initial: Option<String>) -> SecretFieldSpec {
    SecretFieldSpec {
        key: key.into(),
        label: label.into(),
        masked: false,
        required,
        initial: initial.unwrap_or_default(),
        rule: SecretFieldRule::Any,
    }
}

/// A VPN plugin's secret key as a label: `cert-pass` → "Cert pass".
fn humanize(key: &str) -> String {
    let spaced = key.replace(['-', '_'], " ");
    let mut chars = spaced.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// What the sheet says about the server certificate an 802.1X
/// connection will accept.
fn certificate_note(connection: &ConnectionDict) -> String {
    let pinned = prop_bytes(connection, "802-1x", "ca-cert").is_some_and(|ca| !ca.is_empty())
        || prop_str(connection, "802-1x", "ca-path").is_some_and(|path| !path.is_empty());
    let system = prop_bool(connection, "802-1x", "system-ca-certs").unwrap_or(false);
    let domain = prop_str(connection, "802-1x", "domain-suffix-match")
        .or_else(|| prop_str(connection, "802-1x", "domain-match"))
        .filter(|domain| !domain.is_empty());
    let (checked, checked_domain) = if pinned {
        (
            "portal-network-cert-pinned",
            "portal-network-cert-pinned-domain",
        )
    } else if system {
        (
            "portal-network-cert-system",
            "portal-network-cert-system-domain",
        )
    } else {
        return t("portal-network-cert-unchecked");
    };
    domain.map_or_else(
        || t(checked),
        |domain| t_args(checked_domain, &[("domain", domain.into())]),
    )
}

/// What one setting's sheet asks for, and how its answer goes back.
struct Ask {
    title: String,
    lead: String,
    fields: Vec<SecretFieldSpec>,
    notes: Vec<String>,
    remember_default: bool,
    roaming_wifi: Option<(String, os_actions::WifiSecurity)>,
    shape: ResponseShape,
}

impl Ask {
    fn new(title: &str, lead: String, fields: Vec<SecretFieldSpec>) -> Self {
        Self {
            title: title.to_string(),
            lead,
            fields,
            notes: Vec::new(),
            remember_default: true,
            roaming_wifi: None,
            shape: ResponseShape::Flat,
        }
    }
}

/// The facts about a connection the sheets are worded from.
struct Subject<'a> {
    connection: &'a ConnectionDict,
    id: &'a str,
    ssid: Option<&'a str>,
}

impl Subject<'_> {
    /// The name people know the network by: its SSID, else the profile's.
    fn network(&self) -> &str {
        self.ssid.unwrap_or(self.id)
    }
}

fn ask_wifi(subject: &Subject<'_>) -> Result<Ask, AgentError> {
    const SETTING: &str = "802-11-wireless-security";
    let key_mgmt = prop_str(subject.connection, SETTING, "key-mgmt").unwrap_or_default();
    let auth_alg = prop_str(subject.connection, SETTING, "auth-alg").unwrap_or_default();
    let (field, roaming) = match key_mgmt.as_str() {
        "wpa-psk" => (
            masked(
                "psk",
                &t("portal-network-password"),
                true,
                SecretFieldRule::WpaPassphrase,
            ),
            Some(os_actions::WifiSecurity::Wpa2),
        ),
        "sae" => (
            masked(
                "psk",
                &t("portal-network-password"),
                true,
                SecretFieldRule::Any,
            ),
            Some(os_actions::WifiSecurity::Wpa3),
        ),
        "none" => {
            let index = prop_u32(subject.connection, SETTING, "wep-tx-keyidx").unwrap_or(0);
            let key = format!("wep-key{}", index.min(3));
            (
                masked(&key, &t("portal-network-key"), true, SecretFieldRule::Any),
                None,
            )
        }
        "ieee8021x" if auth_alg == "leap" => (
            masked(
                "leap-password",
                &t("portal-network-password"),
                true,
                SecretFieldRule::Any,
            ),
            None,
        ),
        other => {
            return Err(AgentError::InvalidConnection(format!(
                "Wi-Fi key management {other:?} has no secret to ask for"
            )));
        }
    };
    let mut ask = Ask::new(
        &t("portal-network-wifi-title"),
        t_args(
            "portal-network-enter-password",
            &[("network", subject.network().into())],
        ),
        vec![field],
    );
    ask.roaming_wifi = roaming
        .zip(subject.ssid.map(str::to_string))
        .map(|(security, ssid)| (ssid, security));
    Ok(ask)
}

fn ask_8021x(subject: &Subject<'_>) -> Ask {
    const SETTING: &str = "802-1x";
    let method = prop_strings(subject.connection, SETTING, "eap")
        .into_iter()
        .next()
        .unwrap_or_default();
    let identity = prop_str(subject.connection, SETTING, "identity");
    let mut fields = vec![visible(
        "identity",
        &t("portal-network-user-name"),
        true,
        identity,
    )];
    let password = || {
        masked(
            "password",
            &t("portal-network-password"),
            true,
            SecretFieldRule::Any,
        )
    };
    match method.as_str() {
        "tls" => fields.push(masked(
            "private-key-password",
            &t("portal-network-private-key-password"),
            true,
            SecretFieldRule::Any,
        )),
        "peap" | "ttls" | "fast" => {
            fields.push(visible(
                "anonymous-identity",
                &t("portal-network-anonymous-identity"),
                false,
                prop_str(subject.connection, SETTING, "anonymous-identity"),
            ));
            fields.push(password());
        }
        _ => fields.push(password()),
    }
    let wired = prop_str(subject.connection, "connection", "type")
        .is_some_and(|kind| kind == "802-3-ethernet");
    let mut ask = Ask::new(
        &t(if wired {
            "portal-network-wired-sign-in-title"
        } else {
            "portal-network-sign-in-title"
        }),
        t_args(
            "portal-network-sign-in-lead",
            &[("network", subject.network().into())],
        ),
        fields,
    );
    ask.notes.push(certificate_note(subject.connection));
    ask
}

fn ask_vpn(subject: &Subject<'_>, hinted: &[&str]) -> Ask {
    let mut fields: Vec<SecretFieldSpec> = hinted
        .iter()
        .map(|key| masked(key, &humanize(key), true, SecretFieldRule::Any))
        .collect();
    if fields.is_empty() {
        fields = prop_str_dict(subject.connection, "vpn", "data")
            .iter()
            .filter_map(|(key, value)| {
                let key = key.strip_suffix("-flags")?;
                let flags = value.parse::<u32>().ok()?;
                (flags & SECRET_NOT_REQUIRED == 0
                    && flags & (SECRET_AGENT_OWNED | SECRET_NOT_SAVED) != 0)
                    .then(|| masked(key, &humanize(key), false, SecretFieldRule::Any))
            })
            .collect();
    }
    if fields.is_empty() {
        fields.push(masked(
            "password",
            &t("portal-network-password"),
            true,
            SecretFieldRule::Any,
        ));
    }
    let mut ask = Ask::new(
        &t("portal-network-vpn-title"),
        t_args("portal-network-vpn-lead", &[("network", subject.id.into())]),
        fields,
    );
    ask.shape = ResponseShape::VpnSecrets;
    // A one-time code must not be replayed; let the person opt in.
    ask.remember_default = false;
    ask
}

fn ask_password(title: &str, subject: &Subject<'_>) -> Ask {
    Ask::new(
        title,
        t_args(
            "portal-network-enter-password",
            &[("network", subject.id.into())],
        ),
        vec![masked(
            "password",
            &t("portal-network-password"),
            true,
            SecretFieldRule::Any,
        )],
    )
}

impl SecretRequest {
    /// Understand a `GetSecrets` call.
    ///
    /// # Errors
    ///
    /// [`AgentError::InvalidConnection`] for a connection without an id,
    /// or a setting this agent does not know how to ask for.
    pub fn parse(
        connection: &ConnectionDict,
        connection_path: &str,
        setting_name: &str,
        hints: &[String],
        flags: u32,
    ) -> Result<Self, AgentError> {
        let uuid = prop_str(connection, "connection", "uuid").unwrap_or_default();
        if uuid.is_empty() {
            return Err(AgentError::InvalidConnection(
                "the connection has no uuid".into(),
            ));
        }
        let id = prop_str(connection, "connection", "id")
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| t("portal-network-this-network"));
        let ssid = prop_bytes(connection, "802-11-wireless", "ssid")
            .map(|ssid| String::from_utf8_lossy(&ssid).into_owned())
            .filter(|ssid| !ssid.is_empty());
        let subject = Subject {
            connection,
            id: &id,
            ssid: ssid.as_deref(),
        };
        let hinted: Vec<&str> = hints
            .iter()
            .map(String::as_str)
            .filter(|hint| !hint.starts_with("x-vpn-message:"))
            .collect();
        let ask = match setting_name {
            "802-11-wireless-security" => ask_wifi(&subject)?,
            "802-1x" => ask_8021x(&subject),
            "vpn" => ask_vpn(&subject, &hinted),
            "gsm" if hinted.contains(&"pin") => Ask::new(
                &t("portal-network-sim-pin-title"),
                t_args(
                    "portal-network-sim-pin-lead",
                    &[("network", id.as_str().into())],
                ),
                vec![masked(
                    "pin",
                    &t("portal-network-sim-pin"),
                    true,
                    SecretFieldRule::Any,
                )],
            ),
            "gsm" | "cdma" => ask_password(&t("portal-network-mobile-title"), &subject),
            "pppoe" => ask_password(&t("portal-network-dsl-title"), &subject),
            other => {
                return Err(AgentError::InvalidConnection(format!(
                    "no secrets can be asked for setting {other:?}"
                )));
            }
        };

        let mut lines = Vec::new();
        if flags & FLAG_REQUEST_NEW != 0 && flags & FLAG_USER_REQUESTED == 0 {
            lines.push(t("portal-network-retry"));
        }
        lines.push(ask.lead);
        if flags & FLAG_WPS_PBC_ACTIVE != 0 {
            lines.push(t("portal-network-wps"));
        }
        lines.extend(
            hints
                .iter()
                .filter_map(|hint| hint.strip_prefix("x-vpn-message:"))
                .map(str::to_string),
        );
        lines.extend(ask.notes);
        Ok(Self {
            connection_path: connection_path.to_string(),
            connection_uuid: uuid,
            connection_id: id,
            setting_name: setting_name.to_string(),
            flags,
            title: ask.title,
            detail: lines.join("\n"),
            fields: ask.fields,
            remember_default: ask.remember_default,
            roaming_wifi: ask.roaming_wifi,
            shape: ask.shape,
        })
    }

    pub fn store_key(&self) -> StoreKey {
        StoreKey {
            uuid: self.connection_uuid.clone(),
            setting: self.setting_name.clone(),
            roaming_wifi: self.roaming_wifi.clone(),
        }
    }

    /// Whether `secrets` answer every required field.
    pub fn is_answered_by(&self, secrets: &SettingSecrets) -> bool {
        self.fields
            .iter()
            .filter(|field| field.required)
            .all(|field| {
                secrets
                    .get(&field.key)
                    .is_some_and(|value| !value.is_empty())
            })
    }

    /// The reply `NetworkManager` expects for this setting.
    pub fn response(
        &self,
        secrets: &SettingSecrets,
    ) -> HashMap<String, HashMap<String, OwnedValue>> {
        let mut setting = HashMap::new();
        match self.shape {
            ResponseShape::Flat => {
                for (key, value) in &secrets.0 {
                    if let Ok(value) = OwnedValue::try_from(Value::from(value.as_str())) {
                        setting.insert(key.clone(), value);
                    }
                }
            }
            ResponseShape::VpnSecrets => {
                let map: HashMap<String, String> = secrets
                    .0
                    .iter()
                    .map(|(key, value)| (key.clone(), value.to_string()))
                    .collect();
                if let Ok(value) = OwnedValue::try_from(Value::from(map)) {
                    setting.insert("secrets".to_string(), value);
                }
            }
        }
        HashMap::from([(self.setting_name.clone(), setting)])
    }
}

/// What the person typed: one value per field, in order.
pub struct SheetAnswer {
    pub values: Vec<Zeroizing<String>>,
    pub remember: bool,
}

/// Asks the person; `None` when they dismissed the sheet.
pub trait SecretPrompter: Send + Sync {
    fn ask(
        &self,
        request: &SecretRequest,
    ) -> impl std::future::Future<Output = Option<SheetAnswer>> + Send;
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, AgentError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| AgentError::NoSecrets("the secret store stopped".into()))
}

/// Answer one request: from the store when `NetworkManager` allows it and
/// nothing new is wanted, else from the sheet when asking is allowed.
///
/// # Errors
///
/// [`AgentError::NoSecrets`] when nothing is stored and asking is not
/// allowed, [`AgentError::UserCanceled`] when the person dismissed the sheet.
pub async fn request_secrets(
    request: &SecretRequest,
    store: &Arc<dyn SecretStore>,
    prompter: &impl SecretPrompter,
) -> Result<SettingSecrets, AgentError> {
    let key = request.store_key();
    if request.flags & FLAG_REQUEST_NEW == 0 {
        let lookup_store = Arc::clone(store);
        let lookup_key = key.clone();
        let stored = blocking(move || lookup_store.load(&lookup_key)).await?;
        if let Some(stored) = stored.filter(|stored| request.is_answered_by(stored)) {
            tracing::debug!(
                target: "shell::network_secrets",
                connection = %request.connection_id,
                setting = %request.setting_name,
                "answered from remembered secrets"
            );
            return Ok(stored);
        }
    }
    if request.flags & FLAG_ALLOW_INTERACTION == 0 {
        return Err(AgentError::NoSecrets(
            "nothing is remembered and asking is not allowed".into(),
        ));
    }
    let Some(answer) = prompter.ask(request).await else {
        return Err(AgentError::UserCanceled("dismissed".into()));
    };
    let mut secrets = SettingSecrets::default();
    for (field, value) in request.fields.iter().zip(answer.values) {
        if !value.is_empty() {
            secrets.0.insert(field.key.clone(), value);
        }
    }
    let keep_store = Arc::clone(store);
    let kept = secrets.clone();
    let remember = answer.remember;
    let outcome = blocking(move || {
        if remember {
            keep_store.keep(&key, &kept)
        } else {
            keep_store.forget(&key)
        }
    })
    .await?;
    if let Err(error) = outcome {
        tracing::warn!(
            target: "shell::network_secrets",
            connection = %request.connection_id,
            setting = %request.setting_name,
            %error,
            "could not update remembered secrets"
        );
    }
    Ok(secrets)
}

/// The agent-owned secrets in a connection `NetworkManager` hands over to
/// keep (`SaveSecrets`), by setting.
pub fn agent_owned_secrets(connection: &ConnectionDict) -> BTreeMap<String, SettingSecrets> {
    let mut found = BTreeMap::new();
    for (setting, properties) in connection {
        let mut secrets = SettingSecrets::default();
        if setting == "vpn" {
            let data = prop_str_dict(connection, setting, "data");
            for (key, value) in prop_str_dict(connection, setting, "secrets") {
                let flags = data
                    .get(&format!("{key}-flags"))
                    .and_then(|flags| flags.parse::<u32>().ok())
                    .unwrap_or(0);
                if flags & SECRET_AGENT_OWNED != 0 && flags & SECRET_NOT_SAVED == 0 {
                    secrets.insert(key, value);
                }
            }
        } else {
            for (key, value) in properties {
                let Value::Str(value) = &**value else {
                    continue;
                };
                let flags = prop_u32(connection, setting, &format!("{key}-flags")).unwrap_or(0);
                if flags & SECRET_AGENT_OWNED != 0 && flags & SECRET_NOT_SAVED == 0 {
                    secrets.insert(key.clone(), value.as_str());
                }
            }
        }
        if !secrets.is_empty() {
            found.insert(setting.clone(), secrets);
        }
    }
    found
}

/// The connection's uuid, if it has one.
pub fn connection_uuid(connection: &ConnectionDict) -> Option<String> {
    prop_str(connection, "connection", "uuid").filter(|uuid| !uuid.is_empty())
}

#[cfg(test)]
mod tests;
