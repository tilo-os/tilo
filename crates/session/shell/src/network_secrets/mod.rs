//! The shell's side of the session's `NetworkManager` secret agent.
//!
//! The agent itself — `NetworkManager`'s D-Bus interface, the decision
//! between a remembered answer, a sheet and a refusal, and the sealed vault —
//! is [`tilo_network_secret_agent`]. The shell supplies where it asks: a
//! sheet the compositor draws ([`crate::shell::portal_dialog`]), never a
//! window an application could imitate or read keystrokes from, worded in
//! the shell's catalogue. It also supplies the person's roaming Wi-Fi
//! credentials once peer sync has started.

use std::sync::{Arc, LazyLock};

use tilo_network_secret_agent::service::{AgentHandle, SheetReply, Sheets};
use tilo_network_secret_agent::store::{RoamingWifi, RoamingWifiSlot};
use tilo_network_secret_agent::{Line, Message, SecretField, SecretRequest, SheetAnswer};

use crate::shell::i18n::{t, t_args};
use crate::shell::portal_dialog::{
    NetworkSecretsDecision, NetworkSecretsRequest, PortalDialogQueue, SecretFieldSpec,
};

/// `message` in the shell's words, its arguments resolved first.
fn resolve_message(message: &Message) -> String {
    if message.args.is_empty() {
        return t(message.key);
    }
    let args: Vec<(&str, ui::i18n::ArgValue)> = message
        .args
        .iter()
        .map(|(name, value)| (*name, resolve(value).into()))
        .collect();
    t_args(message.key, &args)
}

/// A line of a sheet in the shell's words.
fn resolve(line: &Line) -> String {
    match line {
        Line::Message(message) => resolve_message(message),
        Line::Verbatim(text) => text.clone(),
    }
}

fn field_spec(field: &SecretField) -> SecretFieldSpec {
    SecretFieldSpec {
        key: field.key.clone(),
        label: resolve(&field.label),
        masked: field.masked,
        required: field.required,
        initial: field.initial.clone(),
        rule: field.rule,
    }
}

/// The sheet the compositor draws for `request`, in the shell's words.
fn sheet_request(handle: &str, request: &SecretRequest) -> NetworkSecretsRequest {
    NetworkSecretsRequest {
        handle: handle.to_owned(),
        title: resolve_message(&request.title),
        detail: request
            .detail
            .iter()
            .map(resolve)
            .collect::<Vec<_>>()
            .join("\n"),
        fields: request.fields.iter().map(field_spec).collect(),
        remember: Some(request.remember_default),
    }
}

/// Network-secrets sheets in the compositor's dialog queue.
pub struct DialogSheets {
    queue: PortalDialogQueue,
}

impl DialogSheets {
    pub fn new(queue: PortalDialogQueue) -> Self {
        Self { queue }
    }
}

impl Sheets for DialogSheets {
    fn ask(&self, handle: &str, request: &SecretRequest) -> SheetReply {
        let receiver = self
            .queue
            .enqueue_network_secrets(sheet_request(handle, request));
        Box::pin(async move {
            match receiver.ok()?.await.ok()? {
                NetworkSecretsDecision::Answered { values, remember } => {
                    Some(SheetAnswer { values, remember })
                }
                NetworkSecretsDecision::Cancelled => None,
            }
        })
    }

    fn cancel(&self, handle: &str) {
        self.queue.cancel(handle);
    }

    fn cannot_ask(&self, request: &SecretRequest) {
        if let Some(notice) = CannotJoin::new(request) {
            notice.queue();
        }
    }
}

/// The notification action that joins a network as the person, so its
/// password is asked for in a sheet: `join-network:<connection path>`.
pub(crate) const JOIN_ACTION_PREFIX: &str = "join-network:";

/// `NetworkManager` connected to a network on its own and had no usable
/// secret for it (none remembered, or the remembered one was rejected): the
/// person is told, once per network in this session, and the notification
/// joins it as them, which asks for the password.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CannotJoin {
    pub(crate) network: String,
    pub(crate) connection_path: String,
}

static CANNOT_JOIN: std::sync::Mutex<Vec<CannotJoin>> = std::sync::Mutex::new(Vec::new());
static TOLD: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

impl CannotJoin {
    pub(crate) fn new(request: &SecretRequest) -> Option<Self> {
        let network = request
            .roaming_wifi
            .as_ref()
            .map(|(ssid, _)| ssid.clone())
            .filter(|ssid| !ssid.is_empty())
            .or_else(|| {
                (!request.connection_id.is_empty()).then(|| request.connection_id.clone())
            })?;
        Some(Self {
            network,
            connection_path: request.connection_path.clone(),
        })
    }

    /// Queue the notification unless this network was already told about.
    fn queue(self) {
        use tilo_lock::MutexRecover as _;
        let mut told = TOLD.lock_recover();
        if told.contains(&self.connection_path) {
            return;
        }
        told.push(self.connection_path.clone());
        CANNOT_JOIN.lock_recover().push(self);
    }

    pub(crate) fn notification(&self) -> tilo_desktop::notifications::Notification {
        tilo_desktop::notifications::Notification::new(
            t_args(
                "network-cannot-join-title",
                &[("network", self.network.as_str().into())],
            ),
            t("network-cannot-join-body"),
        )
        .with_severity(tilo_desktop::notifications::NotificationSeverity::Warning)
        .with_app_name(t("network-cannot-join-app-name"))
    }

    /// The action key that joins this network.
    pub(crate) fn action(&self) -> String {
        format!("{JOIN_ACTION_PREFIX}{}", self.connection_path)
    }
}

/// The notifications queued since the last call, each with its join action.
pub(crate) fn drain_cannot_join() -> Vec<tilo_desktop::notifications::Notification> {
    use tilo_lock::MutexRecover as _;
    std::mem::take(&mut *CANNOT_JOIN.lock_recover())
        .into_iter()
        .map(|notice| {
            let mut notification = notice.notification();
            notification.actions = vec![(notice.action(), t("network-cannot-join-action"))];
            notification
        })
        .collect()
}

/// Join the network at `connection_path` as the person: `NetworkManager`
/// activates it on behalf of this session, so this session's agent may ask
/// for its password in a sheet. Off the caller's thread.
pub(crate) fn join(connection_path: &str) {
    let Ok(path) = zbus::zvariant::OwnedObjectPath::try_from(connection_path.to_owned()) else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("network-join".into())
        .spawn(move || {
            let result = zbus::blocking::Connection::system().and_then(|bus| {
                let root = zbus::zvariant::ObjectPath::from_static_str_unchecked("/");
                bus.call_method(
                    Some(tilo_network_secret_agent::service::NM_BUS_NAME),
                    "/org/freedesktop/NetworkManager",
                    Some("org.freedesktop.NetworkManager"),
                    "ActivateConnection",
                    &(&path, &root, &root),
                )
                .map(drop)
            });
            if let Err(error) = result {
                tracing::warn!(target: "shell::network_secrets", %error, "could not join the network");
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(target: "shell::network_secrets", %error, "could not start joining the network");
    }
}

impl RoamingWifi for crate::shell::network::ShellNetwork {
    fn password(&self, ssid: &str, security: os_actions::WifiSecurity) -> Option<String> {
        self.shared_wifi_password(ssid, security)
    }

    fn share(
        &self,
        ssid: &str,
        security: os_actions::WifiSecurity,
        password: &str,
    ) -> Result<(), String> {
        self.share_wifi_credential(ssid, security, password)
    }
}

/// The session's roaming Wi-Fi slot: filled where peer sync starts, read by
/// the agent's vault.
pub fn roaming_wifi_slot() -> RoamingWifiSlot {
    static SLOT: LazyLock<RoamingWifiSlot> = LazyLock::new(RoamingWifiSlot::default);
    #[cfg(test)]
    crate::shell::test_support::ambient_start("global roaming Wi-Fi slot");
    SLOT.clone()
}

/// Serve the agent on the system bus, asking in `queue`'s sheets, and
/// register it with `NetworkManager` for this session.
///
/// # Errors
///
/// When the system bus or `NetworkManager` cannot be reached, or no vault
/// location exists for this session.
pub async fn serve(queue: PortalDialogQueue) -> zbus::Result<AgentHandle> {
    tilo_network_secret_agent::service::serve(
        Arc::new(DialogSheets::new(queue)),
        roaming_wifi_slot(),
    )
    .await
}

#[cfg(test)]
mod tests;
