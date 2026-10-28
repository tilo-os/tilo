//! The shell's side of the session's `NetworkManager` secret agent.
//!
//! The agent itself — `NetworkManager`'s D-Bus interface, the decision
//! between a remembered answer, a sheet and a refusal, and the sealed vault —
//! is [`tilo_network_secret_agent`]. The shell supplies where it asks: a
//! sheet the compositor draws ([`crate::shell::portal_dialog`]), never a
//! window an application could imitate or read keystrokes from, worded in
//! the shell's catalogue. It also supplies the person's roaming Wi-Fi
//! credentials once peer sync has started.
//!
//! A SIM's PIN is asked as one, with the attempts `ModemManager` says the
//! SIM has left. When wrong PINs have locked the SIM, the sheet asks for its
//! PUK and a new PIN instead and hands them to `ModemManager` itself
//! ([`ModemManagerUnlock`]): `NetworkManager` only ever asks for the PIN.
//! The connection is then started again on the unlocked SIM.

use std::sync::{Arc, LazyLock};

use tilo_network_secret_agent::service::{AgentHandle, SheetReply, Sheets};
use tilo_network_secret_agent::store::{RoamingWifi, RoamingWifiSlot};
use tilo_network_secret_agent::{
    Line, Message, RequestKind, SecretField, SecretRequest, SheetAnswer,
};
use zeroize::Zeroizing;

use crate::shell::i18n::{t, t_args};
use crate::shell::portal_dialog::{
    NetworkSecretsDecision, NetworkSecretsRequest, PortalDialogQueue, PukRefusal, SecretFieldSpec,
    SimUnlock, SimUnlockStep, SimUnlocker,
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
/// `sim` is what `ModemManager` says about the SIM of a SIM PIN request
/// (`None` when it could not be asked): a SIM its PUK locks is asked for
/// the PUK and a new PIN instead.
pub fn sheet_request(
    handle: &str,
    request: &SecretRequest,
    sim: Option<SimUnlock>,
) -> NetworkSecretsRequest {
    let mut sheet = NetworkSecretsRequest {
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
        sim: None,
    };
    if request.kind != RequestKind::SimPin {
        return sheet;
    }
    // The SIM keeps its PIN and counts the wrong ones: nothing to remember.
    sheet.remember = None;
    let sim = sim.unwrap_or(SimUnlock {
        step: SimUnlockStep::Pin,
        attempts_left: None,
        sim_path: None,
    });
    if sim.step == SimUnlockStep::Puk {
        let network = if request.connection_id.is_empty() {
            t("portal-network-this-network")
        } else {
            request.connection_id.clone()
        };
        sheet.title = t("portal-sim-puk-title");
        sheet.detail = t_args("portal-sim-puk-lead", &[("network", network.into())]);
    }
    sheet.sim = Some(sim);
    sheet
}

/// What `reading` says about the SIM a PIN is asked for: the modem whose
/// SIM asks for its PIN or PUK (else the first modem), what it asks for and
/// the attempts left. `None` without a modem.
pub fn sim_unlock(reading: &os_actions::MobileReading) -> Option<SimUnlock> {
    let modem = reading
        .modems
        .iter()
        .find(|modem| {
            matches!(
                modem.lock,
                os_actions::SimLock::Pin | os_actions::SimLock::Puk
            )
        })
        .or_else(|| reading.modems.first())?;
    Some(SimUnlock::of(
        modem.lock,
        modem.retries,
        modem.active_sim().map(|sim| sim.path.clone()),
    ))
}

impl SimUnlock {
    /// A SIM that asks for `lock`, with `retries` left, at `sim_path`.
    pub fn of(
        lock: os_actions::SimLock,
        retries: os_actions::UnlockRetries,
        sim_path: Option<String>,
    ) -> Self {
        let (step, attempts_left) = match lock {
            os_actions::SimLock::Puk => (SimUnlockStep::Puk, retries.puk),
            os_actions::SimLock::Pin | os_actions::SimLock::None | os_actions::SimLock::Other => {
                (SimUnlockStep::Pin, retries.pin)
            }
        };
        Self {
            step,
            attempts_left,
            sim_path,
        }
    }
}

/// What `ModemManager` says about the SIM now; `None` when it cannot be
/// asked, and the sheet asks for the PIN without the attempts.
async fn read_sim() -> Option<SimUnlock> {
    let manager = match os_actions::modem::ModemManager::system().await {
        Ok(manager) => manager,
        Err(error) => {
            tracing::info!(target: "shell::network_secrets", %error, "no modem state for the SIM PIN sheet");
            return None;
        }
    };
    match manager.read().await {
        Ok(reading) => sim_unlock(&reading),
        Err(error) => {
            tracing::info!(target: "shell::network_secrets", %error, "no modem state for the SIM PIN sheet");
            None
        }
    }
}

/// Unlocks a SIM with its PUK through `ModemManager` on the system bus.
pub struct ModemManagerUnlock;

impl SimUnlocker for ModemManagerUnlock {
    fn send_puk(
        &self,
        sim_path: &str,
        puk: Zeroizing<String>,
        new_pin: Zeroizing<String>,
    ) -> Result<(), PukRefusal> {
        let change = os_actions::ModemChange::SendPuk {
            sim: sim_path.to_owned(),
            puk,
            new_pin,
        };
        match os_actions::apply_modem_change(&change) {
            Ok(()) => Ok(()),
            Err(os_actions::ModemError::WrongCode) => {
                // The SIM counted it: say how many are left now.
                let attempts_left = os_actions::read_mobile()
                    .ok()
                    .and_then(|reading| {
                        reading
                            .modems
                            .into_iter()
                            .find(|modem| modem.sims.iter().any(|sim| sim.path == sim_path))
                    })
                    .and_then(|modem| modem.retries.puk);
                Err(PukRefusal::Wrong { attempts_left })
            }
            Err(error) => {
                tracing::warn!(target: "shell::network_secrets", %error, "the SIM could not be unlocked with its PUK");
                Err(PukRefusal::Failed(t("portal-sim-unlock-failed")))
            }
        }
    }
}

/// What the sheets need of the modem beside `NetworkManager`: the SIM's
/// state as its PIN is asked, and starting the connection again once the
/// SIM was unlocked with its PUK.
pub trait ModemSide: Send + Sync {
    /// What `ModemManager` says about the SIM now; `None` when it cannot be
    /// asked.
    fn read_sim(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<SimUnlock>> + Send>>;
    /// Start the connection at `connection_path` again, as the person.
    fn rejoin(&self, connection_path: &str);
}

/// `ModemManager` and `NetworkManager` on the system bus.
pub struct SystemModem;

impl ModemSide for SystemModem {
    fn read_sim(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<SimUnlock>> + Send>> {
        Box::pin(read_sim())
    }

    fn rejoin(&self, connection_path: &str) {
        join(connection_path);
    }
}

/// Network-secrets sheets in the compositor's dialog queue.
pub struct DialogSheets {
    queue: PortalDialogQueue,
    modem: Arc<dyn ModemSide>,
}

impl DialogSheets {
    pub fn new(queue: PortalDialogQueue) -> Self {
        Self::with_modem(queue, Arc::new(SystemModem))
    }

    /// Sheets that learn about the SIM from `modem`.
    pub fn with_modem(queue: PortalDialogQueue, modem: Arc<dyn ModemSide>) -> Self {
        Self { queue, modem }
    }
}

impl Sheets for DialogSheets {
    fn ask(&self, handle: &str, request: &SecretRequest) -> SheetReply {
        let queue = self.queue.clone();
        let modem = Arc::clone(&self.modem);
        let handle = handle.to_owned();
        let request = request.clone();
        Box::pin(async move {
            // A SIM's state is read as the PIN is asked: how many attempts
            // are left, and whether wrong PINs have locked it to its PUK.
            let sim = if request.kind == RequestKind::SimPin {
                modem.read_sim().await
            } else {
                None
            };
            let receiver = queue.enqueue_network_secrets(sheet_request(&handle, &request, sim));
            match receiver.ok()?.await.ok()? {
                NetworkSecretsDecision::Answered { values, remember } => {
                    Some(SheetAnswer { values, remember })
                }
                NetworkSecretsDecision::UnlockedWithPuk => {
                    // NetworkManager asked for a PIN the SIM no longer
                    // needs: this request ends without one, and the
                    // connection starts again on the unlocked SIM.
                    modem.rejoin(&request.connection_path);
                    None
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
        drop(told);
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
