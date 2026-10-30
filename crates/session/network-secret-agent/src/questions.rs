//! The agent's sheets as the session's questions.
//!
//! The native session has no dialog queue of its own: each secrets request
//! is asked as a question ([`tilo_service::Questions`]) of kind
//! [`KIND`], under the handle the agent names its sheet by, listed under
//! the network's name. The session's sheet reads its details as
//! [`SecretsDetails`] — the agent's words as Fluent keys and arguments
//! ([`Text`]), never resolved prose — and answers through
//! [`Questions::answer`] directly, so typed secrets never travel as an
//! action's arguments:
//!
//! - `values` holds one value per field, in [`SecretsDetails::fields`]
//!   order; for a SIM its PUK locks ([`SimUnlockStep::Puk`]) they are the
//!   PUK and the new PIN instead.
//! - [`Answer::remember`] keeps the secrets on this device, when
//!   [`SecretsDetails::remember`] offers it.
//!
//! The agent checks the answer as the sheet should have: a required field
//! left empty, or a Wi-Fi password `NetworkManager` would refuse, asks the
//! question again with a [`Refusal`] naming the field. A PUK goes to
//! `ModemManager` from here ([`Modem::send_puk`]); a wrong one asks again
//! with the attempts the SIM has left, a right one starts the connection
//! again on the unlocked SIM, since `NetworkManager` only ever asks for the
//! PIN.
//!
//! When `NetworkManager` connected on its own and nothing usable was
//! remembered ([`Sheets::cannot_ask`]), the person is told once per network
//! with a question of kind [`CANNOT_JOIN_KIND`] ([`CannotJoinDetails`]):
//! answering it with choice 0 joins the network as them, which asks for its
//! password as a [`KIND`] question; declining dismisses it.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tilo_service::{Answer, Question, Questions};
use zeroize::Zeroizing;

use crate::service::{AgentHandle, NM_BUS_NAME, SheetReply, Sheets};
use crate::store::{RoamingWifiSlot, SecretStore};
use crate::{Line, Message, RequestKind, SecretField, SecretFieldRule, SecretRequest, SheetAnswer};

/// The kind of a secrets question.
pub const KIND: &str = "network.secrets";
/// The kind of the notice that a network could not be joined on its own.
pub const CANNOT_JOIN_KIND: &str = "network.cannot-join";

/// What the handle of a cannot-join notice starts with, before the
/// connection's path.
const CANNOT_JOIN_PREFIX: &str = "nm-cannot-join:";

/// A required field was left empty.
pub const REFUSAL_REQUIRED: &str = "portal-secret-required";
/// A Wi-Fi password is not 8–63 characters or 64 hexadecimal digits.
pub const REFUSAL_WIFI_LENGTH: &str = "portal-secret-wifi-length";
/// `ModemManager` refused the PUK.
pub const REFUSAL_PUK_WRONG: &str = "portal-sim-puk-wrong";
/// The SIM could not be unlocked for another reason.
pub const REFUSAL_UNLOCK_FAILED: &str = "portal-sim-unlock-failed";

const SIM_PUK_TITLE: &str = "portal-sim-puk-title";
const SIM_PUK_LEAD: &str = "portal-sim-puk-lead";
const THIS_NETWORK: &str = "portal-network-this-network";
const CANNOT_JOIN_TITLE: &str = "network-cannot-join-title";
const CANNOT_JOIN_BODY: &str = "network-cannot-join-body";
const CANNOT_JOIN_ACTION: &str = "network-cannot-join-action";

/// Words on a sheet: a Fluent message the sheet resolves in its catalogue,
/// its arguments resolved first, or text shown as it came.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Text {
    Message {
        key: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        args: BTreeMap<String, Text>,
    },
    Verbatim(String),
}

impl Text {
    fn key(key: &str) -> Self {
        Self::Message {
            key: key.to_owned(),
            args: BTreeMap::new(),
        }
    }

    fn with(mut self, name: &str, value: Self) -> Self {
        if let Self::Message { args, .. } = &mut self {
            args.insert(name.to_owned(), value);
        }
        self
    }
}

impl From<&Message> for Text {
    fn from(message: &Message) -> Self {
        Self::Message {
            key: message.key.to_owned(),
            args: message
                .args
                .iter()
                .map(|(name, value)| ((*name).to_owned(), Self::from(value)))
                .collect(),
        }
    }
}

impl From<&Line> for Text {
    fn from(line: &Line) -> Self {
        match line {
            Line::Message(message) => Self::from(message),
            Line::Verbatim(text) => Self::Verbatim(text.clone()),
        }
    }
}

/// How a field checks its value ([`SecretFieldRule`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldRule {
    Any,
    /// 8–63 characters, or 64 hexadecimal digits.
    WpaPassphrase,
}

impl From<SecretFieldRule> for FieldRule {
    fn from(rule: SecretFieldRule) -> Self {
        match rule {
            SecretFieldRule::Any => Self::Any,
            SecretFieldRule::WpaPassphrase => Self::WpaPassphrase,
        }
    }
}

/// One field a secrets question asks for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDetails {
    /// `NetworkManager`'s key for the value, e.g. `psk` or `identity`.
    pub key: String,
    pub label: Text,
    /// Drawn as bullets.
    pub masked: bool,
    pub required: bool,
    /// What the field starts with; always empty for a masked field.
    pub initial: String,
    pub rule: FieldRule,
}

impl From<&SecretField> for FieldDetails {
    fn from(field: &SecretField) -> Self {
        Self {
            key: field.key.clone(),
            label: Text::from(&field.label),
            masked: field.masked,
            required: field.required,
            initial: field.initial.clone(),
            rule: field.rule.into(),
        }
    }
}

/// What a SIM asks for before its modem can be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimUnlockStep {
    /// Its PIN, which goes back to `NetworkManager`.
    Pin,
    /// Its PUK and a new PIN, after too many wrong PINs; the agent hands
    /// them to `ModemManager` itself.
    Puk,
}

/// A SIM PIN request's SIM, as a question shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimDetails {
    pub step: SimUnlockStep,
    /// The attempts the SIM has left for that step, when the modem says.
    pub attempts_left: Option<u32>,
}

/// Why the last answer was not taken: the field it is about, counted from
/// zero, and the Fluent message saying why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub field: usize,
    pub key: String,
}

impl Refusal {
    fn at(field: usize, key: &str) -> Self {
        Self {
            field,
            key: key.to_owned(),
        }
    }
}

/// The details of a [`KIND`] question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretsDetails {
    /// The network's name as `NetworkManager` or the SSID gave it.
    pub network: Option<String>,
    pub title: Text,
    /// What the sheet says, one line each. A line keyed
    /// `portal-network-retry` says the last secrets did not work.
    pub detail: Vec<Text>,
    pub fields: Vec<FieldDetails>,
    /// `Some(default)` offers to remember the answer on this device,
    /// starting ticked or not; `None` when it cannot be kept.
    pub remember: Option<bool>,
    /// The request is for a SIM's PIN, or its PUK once wrong PINs locked
    /// it; `None` for every other secret.
    pub sim: Option<SimDetails>,
    /// Why the last answer was not taken.
    pub refusal: Option<Refusal>,
}

/// The details of a [`CANNOT_JOIN_KIND`] question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CannotJoinDetails {
    /// The network's name.
    pub network: String,
    pub title: Text,
    pub body: Text,
    /// What choice 0 says: joining the network, which asks for its
    /// password.
    pub join: Text,
}

/// What `ModemManager` reports about the SIM a PIN is asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimUnlock {
    pub step: SimUnlockStep,
    /// The attempts the SIM has left for that step, when the modem says.
    pub attempts_left: Option<u32>,
    /// `ModemManager`'s object path of the SIM, which a PUK is sent to;
    /// `None` when `ModemManager` could not be asked.
    pub sim_path: Option<String>,
}

impl SimUnlock {
    /// A SIM that asks for `lock`, with `retries` left, at `sim_path`.
    #[must_use]
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

    /// A SIM nothing is known about: its PIN is asked without attempts.
    const fn unknown() -> Self {
        Self {
            step: SimUnlockStep::Pin,
            attempts_left: None,
            sim_path: None,
        }
    }
}

/// What `reading` says about the SIM a PIN is asked for: the modem whose
/// SIM asks for its PIN or PUK (else the first modem), what it asks for and
/// the attempts left. `None` without a modem.
#[must_use]
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

/// Why `ModemManager` did not unlock a SIM with its PUK.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PukRefusal {
    /// The PUK was wrong; the attempts the SIM has left for it, when the
    /// modem says.
    Wrong { attempts_left: Option<u32> },
    /// Anything else.
    Failed,
}

/// A future the modem side answers.
pub type ModemFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What the agent needs of the system beside `NetworkManager`'s request:
/// the SIM's state as its PIN is asked, unlocking it with its PUK, and
/// joining a network as the person.
pub trait Modem: Send + Sync {
    /// What `ModemManager` says about the SIM now; `None` when it cannot be
    /// asked.
    fn read_sim(&self) -> ModemFuture<Option<SimUnlock>>;
    /// Unlock the SIM at `sim_path` with `puk`, setting `new_pin`.
    fn send_puk(
        &self,
        sim_path: &str,
        puk: Zeroizing<String>,
        new_pin: Zeroizing<String>,
    ) -> ModemFuture<Result<(), PukRefusal>>;
    /// Start the connection at `connection_path` as the person, so this
    /// session's agent may ask for its secrets.
    fn join(&self, connection_path: &str);
}

/// `ModemManager` and `NetworkManager` on the system bus.
pub struct SystemModem;

impl Modem for SystemModem {
    fn read_sim(&self) -> ModemFuture<Option<SimUnlock>> {
        Box::pin(async {
            let reading = match os_actions::modem::ModemManager::system().await {
                Ok(manager) => manager.read().await,
                Err(error) => Err(error),
            };
            match reading {
                Ok(reading) => sim_unlock(&reading),
                Err(error) => {
                    tracing::info!(target: "shell::network_secrets", %error, "no modem state for the SIM PIN question");
                    None
                }
            }
        })
    }

    fn send_puk(
        &self,
        sim_path: &str,
        puk: Zeroizing<String>,
        new_pin: Zeroizing<String>,
    ) -> ModemFuture<Result<(), PukRefusal>> {
        let sim_path = sim_path.to_owned();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || send_puk_blocking(&sim_path, puk, new_pin))
                .await
                .unwrap_or(Err(PukRefusal::Failed))
        })
    }

    fn join(&self, connection_path: &str) {
        let Ok(path) = zbus::zvariant::OwnedObjectPath::try_from(connection_path.to_owned()) else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(target: "shell::network_secrets", "no runtime to join the network on");
            return;
        };
        drop(runtime.spawn(async move {
            if let Err(error) = activate(&path).await {
                tracing::warn!(target: "shell::network_secrets", %error, "could not join the network");
            }
        }));
    }
}

fn send_puk_blocking(
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
            Err(PukRefusal::Failed)
        }
    }
}

/// `NetworkManager` activates the connection at `path` on behalf of this
/// session.
async fn activate(path: &zbus::zvariant::OwnedObjectPath) -> zbus::Result<()> {
    let bus = zbus::Connection::system().await?;
    let root = zbus::zvariant::ObjectPath::from_static_str_unchecked("/");
    bus.call_method(
        Some(NM_BUS_NAME),
        "/org/freedesktop/NetworkManager",
        Some("org.freedesktop.NetworkManager"),
        "ActivateConnection",
        &(path, &root, &root),
    )
    .await
    .map(drop)
}

/// The network `request` is for, by name: its SSID when it is Wi-Fi, else
/// the profile's name. `None` when it has neither.
#[must_use]
pub fn network_name(request: &SecretRequest) -> Option<String> {
    request
        .roaming_wifi
        .as_ref()
        .map(|(ssid, _)| ssid.clone())
        .filter(|ssid| !ssid.is_empty())
        .or_else(|| (!request.connection_id.is_empty()).then(|| request.connection_id.clone()))
}

/// The first field `values` does not answer as `fields` ask, and why: a
/// required field left empty, or a value its rule refuses. Values missing
/// at the end are empty.
#[must_use]
pub fn refusal(fields: &[SecretField], values: &[Zeroizing<String>]) -> Option<Refusal> {
    fields.iter().enumerate().find_map(|(index, field)| {
        let value = values.get(index).map_or("", |value| value.as_str());
        if value.is_empty() {
            field.required.then(|| Refusal::at(index, REFUSAL_REQUIRED))
        } else if field.rule.accepts(value) {
            None
        } else {
            Some(Refusal::at(index, REFUSAL_WIFI_LENGTH))
        }
    })
}

/// The details of the question for `request`, its SIM as `sim` says.
#[must_use]
pub fn details(
    request: &SecretRequest,
    sim: Option<&SimUnlock>,
    refusal: Option<Refusal>,
) -> SecretsDetails {
    let mut details = SecretsDetails {
        network: network_name(request),
        title: Text::from(&request.title),
        detail: request.detail.iter().map(Text::from).collect(),
        fields: request.fields.iter().map(FieldDetails::from).collect(),
        remember: Some(request.remember_default),
        sim: None,
        refusal,
    };
    if request.kind != RequestKind::SimPin {
        return details;
    }
    // The SIM keeps its PIN and counts the wrong ones: nothing to remember.
    details.remember = None;
    let sim = sim.cloned().unwrap_or(SimUnlock::unknown());
    if sim.step == SimUnlockStep::Puk {
        let network = if request.connection_id.is_empty() {
            Text::key(THIS_NETWORK)
        } else {
            Text::Verbatim(request.connection_id.clone())
        };
        details.title = Text::key(SIM_PUK_TITLE);
        details.detail = vec![Text::key(SIM_PUK_LEAD).with("network", network)];
    }
    details.sim = Some(SimDetails {
        step: sim.step,
        attempts_left: sim.attempts_left,
    });
    details
}

/// The PUK and the new PIN from an answer to a PUK question.
fn puk_answer(mut answer: Answer) -> Result<(Zeroizing<String>, Zeroizing<String>), Refusal> {
    let mut values = answer.values.drain(..);
    let puk = values.next().filter(|puk| !puk.is_empty());
    let new_pin = values.next().filter(|pin| !pin.is_empty());
    match (puk, new_pin) {
        (Some(puk), Some(new_pin)) => Ok((puk, new_pin)),
        (None, _) => Err(Refusal::at(0, REFUSAL_REQUIRED)),
        (Some(_), None) => Err(Refusal::at(1, REFUSAL_REQUIRED)),
    }
}

/// The agent's sheets as questions in `questions`.
pub struct QuestionSheets {
    questions: Questions,
    modem: Arc<dyn Modem>,
    /// The connections already told they could not be joined.
    told: Arc<Mutex<Vec<String>>>,
}

impl QuestionSheets {
    /// Sheets that learn about SIMs from `ModemManager` on the system bus.
    #[must_use]
    pub fn new(questions: Questions) -> Self {
        Self::with_modem(questions, Arc::new(SystemModem))
    }

    /// Sheets that learn about SIMs from `modem`.
    #[must_use]
    pub fn with_modem(questions: Questions, modem: Arc<dyn Modem>) -> Self {
        Self {
            questions,
            modem,
            told: Arc::default(),
        }
    }
}

/// Ask `request` until it is answered as its fields ask, declined, or its
/// SIM unlocked with the PUK.
async fn ask(
    questions: Questions,
    modem: Arc<dyn Modem>,
    handle: String,
    request: SecretRequest,
) -> Option<SheetAnswer> {
    // A SIM's state is read as its PIN is asked: how many attempts are
    // left, and whether wrong PINs have locked it to its PUK.
    let mut sim = if request.kind == RequestKind::SimPin {
        Some(modem.read_sim().await.unwrap_or(SimUnlock::unknown()))
    } else {
        None
    };
    let label = network_name(&request).unwrap_or_default();
    let mut refused = None;
    loop {
        let details = details(&request, sim.as_ref(), refused.take());
        let remember_offered = details.remember.is_some();
        let answer = questions
            .ask(Question {
                handle: handle.clone(),
                kind: KIND.to_owned(),
                app_id: None,
                label: label.clone(),
                details: serde_json::to_value(details).unwrap_or(serde_json::Value::Null),
            })
            .await?;
        let Some(locked) = sim.as_mut().filter(|sim| sim.step == SimUnlockStep::Puk) else {
            if let Some(refusal) = refusal(&request.fields, &answer.values) {
                refused = Some(refusal);
                continue;
            }
            return Some(SheetAnswer {
                remember: remember_offered && answer.remember,
                values: answer.values,
            });
        };
        let (puk, new_pin) = match puk_answer(answer) {
            Ok(codes) => codes,
            Err(refusal) => {
                refused = Some(refusal);
                continue;
            }
        };
        let Some(sim_path) = locked.sim_path.clone() else {
            refused = Some(Refusal::at(0, REFUSAL_UNLOCK_FAILED));
            continue;
        };
        match modem.send_puk(&sim_path, puk, new_pin).await {
            Ok(()) => {
                // NetworkManager asked for a PIN the SIM no longer needs:
                // this request ends without one, and the connection starts
                // again on the unlocked SIM.
                modem.join(&request.connection_path);
                return None;
            }
            Err(PukRefusal::Wrong { attempts_left }) => {
                locked.attempts_left = attempts_left;
                refused = Some(Refusal::at(0, REFUSAL_PUK_WRONG));
            }
            Err(PukRefusal::Failed) => refused = Some(Refusal::at(0, REFUSAL_UNLOCK_FAILED)),
        }
    }
}

impl Sheets for QuestionSheets {
    fn ask(&self, handle: &str, request: &SecretRequest) -> SheetReply {
        Box::pin(ask(
            self.questions.clone(),
            Arc::clone(&self.modem),
            handle.to_owned(),
            request.clone(),
        ))
    }

    fn cancel(&self, handle: &str) {
        self.questions.decline(handle);
    }

    fn cannot_ask(&self, request: &SecretRequest) {
        let Some(network) = network_name(request) else {
            return;
        };
        let path = request.connection_path.clone();
        {
            let mut told = self.told.lock_recover();
            if told.contains(&path) {
                return;
            }
            told.push(path.clone());
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(target: "shell::network_secrets", "no runtime to tell that a network could not be joined");
            return;
        };
        let details = CannotJoinDetails {
            network: network.clone(),
            title: Text::key(CANNOT_JOIN_TITLE).with("network", Text::Verbatim(network.clone())),
            body: Text::key(CANNOT_JOIN_BODY),
            join: Text::key(CANNOT_JOIN_ACTION),
        };
        let asked = self.questions.ask(Question {
            handle: format!("{CANNOT_JOIN_PREFIX}{path}"),
            kind: CANNOT_JOIN_KIND.to_owned(),
            app_id: None,
            label: network,
            details: serde_json::to_value(details).unwrap_or(serde_json::Value::Null),
        });
        let modem = Arc::clone(&self.modem);
        drop(runtime.spawn(async move {
            if asked.await.is_some_and(|answer| answer.choice == Some(0)) {
                modem.join(&path);
            }
        }));
    }
}

/// Serve the agent on the system bus, asking in `questions`, and register
/// it with `NetworkManager` for this session.
///
/// # Errors
///
/// When the system bus or `NetworkManager` cannot be reached, or no vault
/// location exists for this session.
pub async fn serve(questions: Questions, roaming: RoamingWifiSlot) -> zbus::Result<AgentHandle> {
    crate::service::serve(Arc::new(QuestionSheets::new(questions)), roaming).await
}

/// Serve the agent on `connection`, asking in `sheets` and remembering in
/// `store`, and register it with `NetworkManager` there.
///
/// # Errors
///
/// When the object cannot be served or `NetworkManager` refuses the agent.
pub async fn serve_on(
    connection: zbus::Connection,
    sheets: QuestionSheets,
    store: Arc<dyn SecretStore>,
) -> zbus::Result<AgentHandle> {
    crate::service::serve_on(connection, Arc::new(sheets), store).await
}

#[cfg(test)]
mod tests;
