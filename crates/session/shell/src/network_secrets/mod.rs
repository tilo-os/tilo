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
