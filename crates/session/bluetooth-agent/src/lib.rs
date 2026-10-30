//! The session's Bluetooth pairing agent.
//!
//! `BlueZ` asks the desktop a question mid-pairing — "these six digits, do
//! they match what the headphones are showing?" — and holds the pairing
//! open until the answer comes back. Whoever answers decides whether a
//! nearby device gets to inject keystrokes and read audio.
//!
//! [`PairingService`] keeps `tilo-bluez`'s agent registered as `BlueZ`'s
//! default agent and asks each of its questions through the session's
//! questions ([`tilo_service::Questions`]): one question of kind
//! [`KIND`] at a time, listed under the device's name, whose details read
//! as [`PairingDetails`]. The session's sheet answers it:
//!
//! - choice 0 confirms or accepts; for a typed passkey or PIN, `values[0]`
//!   is what was typed, checked as [`reply`] says (a passkey up to
//!   [`MAX_PASSKEY`], a PIN of at most [`tilo_bluez::MAX_PIN_CODE_BYTES`]).
//!   Anything else, a blank or unreadable entry included, refuses.
//! - Declining refuses; for a code on show ([`Method::DisplayPasskey`],
//!   [`Method::DisplayPin`]), which has no reply to refuse, it stops the
//!   pairing on the device.
//!
//! Confirming is for the person at the device only: a question's answer
//! action is attended, so an agent acting for them may decline a pairing
//! but never confirm one.
//!
//! A question goes away when it is answered, when `BlueZ` withdraws it
//! (`Cancel`, `Release`), when a device showing a code finishes pairing,
//! when another question takes its place (refusing it), or when nobody
//! answers within the agent's timeout — never by being silently accepted.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_bluez::agent::{AgentEvent, AgentEvents, AgentRegistration, PairingAgent};
use tilo_bluez::{
    BluezClient, MAX_PIN_CODE_BYTES, PairingPrompt, PairingReply, PairingReplySender,
    PairingRequest,
};
use tilo_service::{Answer, Asked, Question, Questions};
use tracing::{info, warn};

/// The kind of a pairing question.
pub const KIND: &str = "bluetooth.pair";

/// The largest passkey Bluetooth has: six decimal digits.
pub const MAX_PASSKEY: u32 = 999_999;

/// How long after the agent gives up on a question the question goes, so
/// `BlueZ` hears "nobody answered" (`Canceled`) rather than a refusal.
const EXPIRY_GRACE: Duration = Duration::from_millis(500);

/// What a handle starts with.
const HANDLE_PREFIX: &str = "bluetooth:";

/// How a pairing is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// Both ends show [`PairingDetails::code`]; the person confirms they
    /// match.
    ConfirmPasskey,
    /// Nothing to compare ("just works"): the person confirms they asked
    /// for this pairing.
    ConfirmPairing,
    /// The person types the passkey the device shows.
    EnterPasskey,
    /// The person types a PIN (often `0000`).
    EnterPin,
    /// The person types [`PairingDetails::code`] on the device.
    DisplayPasskey,
    /// The person types the PIN [`PairingDetails::code`] on the device.
    DisplayPin,
    /// A paired device wants a service it was not allowed yet.
    AuthorizeService,
}

impl Method {
    #[must_use]
    pub const fn of(prompt: &PairingPrompt) -> Self {
        match prompt {
            PairingPrompt::ConfirmPasskey { .. } => Self::ConfirmPasskey,
            PairingPrompt::ConfirmPairing => Self::ConfirmPairing,
            PairingPrompt::RequestPasskey => Self::EnterPasskey,
            PairingPrompt::RequestPinCode => Self::EnterPin,
            PairingPrompt::DisplayPasskey { .. } => Self::DisplayPasskey,
            PairingPrompt::DisplayPinCode { .. } => Self::DisplayPin,
            PairingPrompt::AuthorizeService { .. } => Self::AuthorizeService,
        }
    }
}

/// The details of a [`KIND`] question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingDetails {
    /// The device's name, or its address when it has none.
    pub device: String,
    /// `BlueZ`'s object path of the device.
    pub device_path: String,
    pub method: Method,
    /// The code to show or compare: a passkey as six digits, or a PIN.
    pub code: Option<String>,
    /// How many digits of a shown passkey the device has had typed.
    pub entered: Option<u16>,
    /// The service a paired device asks for, as `BlueZ` names it (a UUID).
    pub service: Option<String>,
    /// A Fluent key naming that service by what it does, for the services
    /// a person would recognise; `None` for any other.
    pub service_key: Option<String>,
}

impl From<&PairingRequest> for PairingDetails {
    fn from(request: &PairingRequest) -> Self {
        let (entered, service) = match &request.prompt {
            PairingPrompt::DisplayPasskey { entered, .. } => (Some(*entered), None),
            PairingPrompt::AuthorizeService { uuid } => (None, Some(uuid.clone())),
            _ => (None, None),
        };
        Self {
            device: request.device_name.clone(),
            device_path: request.device_path.clone(),
            method: Method::of(&request.prompt),
            code: request.prompt.passkey_text(),
            entered,
            service_key: service.as_deref().and_then(service_key).map(str::to_owned),
            service,
        }
    }
}

/// The catalogue key naming a Bluetooth service by what it does, for the
/// assigned 16-bit UUIDs a person would recognise. `uuid` is the full
/// 128-bit form on the Bluetooth base UUID.
#[must_use]
pub fn service_key(uuid: &str) -> Option<&'static str> {
    let uuid = uuid.to_ascii_lowercase();
    let short = uuid
        .strip_suffix("-0000-1000-8000-00805f9b34fb")
        .and_then(|head| head.strip_prefix("0000"))
        .and_then(|short| u16::from_str_radix(short, 16).ok())?;
    Some(match short {
        0x110a | 0x110b | 0x110d => "bluetooth-service-audio",
        0x1108 | 0x1112 | 0x111e | 0x111f => "bluetooth-service-calls",
        0x110c | 0x110e | 0x110f => "bluetooth-service-remote-control",
        0x1124 | 0x1812 => "bluetooth-service-input",
        0x112f | 0x1130 => "bluetooth-service-contacts",
        0x1132..=0x1134 => "bluetooth-service-messages",
        0x1105 | 0x1106 => "bluetooth-service-files",
        0x1115..=0x1117 => "bluetooth-service-network",
        _ => return None,
    })
}

/// What the person's answer to `prompt` tells `BlueZ`; `None` is a decline.
///
/// Only choice 0 agrees. The typed prompts answer with what was typed — a
/// blank or unreadable entry is a refusal, not a zero passkey, which the
/// device would reject anyway and read as "pairing mysteriously failed". A
/// code on show has nothing to agree to.
#[must_use]
pub fn reply(prompt: &PairingPrompt, answer: Option<&Answer>) -> PairingReply {
    let Some(answer) = answer.filter(|answer| answer.choice == Some(0)) else {
        return PairingReply::Reject;
    };
    let entry = answer.values.first().map_or("", |value| value.trim());
    match prompt {
        PairingPrompt::RequestPasskey => match entry.parse::<u32>() {
            Ok(passkey) if passkey <= MAX_PASSKEY => PairingReply::Passkey(passkey),
            _ => PairingReply::Reject,
        },
        PairingPrompt::RequestPinCode => {
            if entry.is_empty() || entry.len() > MAX_PIN_CODE_BYTES {
                PairingReply::Reject
            } else {
                PairingReply::PinCode(entry.to_owned())
            }
        }
        PairingPrompt::DisplayPasskey { .. } | PairingPrompt::DisplayPinCode { .. } => {
            PairingReply::Reject
        }
        PairingPrompt::ConfirmPasskey { .. }
        | PairingPrompt::ConfirmPairing
        | PairingPrompt::AuthorizeService { .. } => PairingReply::Accept,
    }
}

/// The question asked for `request` under `handle`.
fn question(handle: String, request: &PairingRequest) -> Question {
    Question {
        handle,
        kind: KIND.to_owned(),
        app_id: None,
        label: request.device_name.clone(),
        details: serde_json::to_value(PairingDetails::from(request))
            .unwrap_or(serde_json::Value::Null),
    }
}

/// The question asked now.
struct Current {
    request: PairingRequest,
    reply: PairingReplySender,
    asked: Asked,
    deadline: tokio::time::Instant,
}

impl Current {
    fn shows_code(&self) -> bool {
        !self.request.prompt.expects_answer()
    }

    /// `BlueZ` or the clock took the question away: refuse it if it is
    /// still waiting. Dropping it withdraws the question.
    fn withdraw(self) {
        let _ = self.reply.send(PairingReply::Reject);
    }
}

/// When a question asked now expires: just after the agent's own
/// deadline `timeout`.
fn deadline_after(timeout: Duration) -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    let wait = timeout.saturating_add(EXPIRY_GRACE);
    now.checked_add(wait).unwrap_or(now)
}

type Watch = Pin<Box<dyn Future<Output = bool> + Send>>;

async fn answered(current: &mut Option<Current>) -> Option<Answer> {
    match current {
        Some(current) => (&mut current.asked).await,
        None => std::future::pending().await,
    }
}

async fn expired(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

async fn followed(watch: &mut Option<Watch>) -> bool {
    match watch {
        Some(watch) => watch.await,
        None => std::future::pending().await,
    }
}

/// Whether `device` reports it is paired. The agent learns nothing when a
/// pairing with a code on show succeeds, so the device's own `Paired`
/// property is the only signal; `false` when it cannot be followed.
async fn paired(client: BluezClient, device: String) -> bool {
    use futures_util::StreamExt as _;

    let watch = async {
        let properties = zbus::fdo::PropertiesProxy::builder(client.connection())
            .destination("org.bluez")?
            .path(device.as_str())?
            .build()
            .await?;
        let mut changes = properties.receive_properties_changed().await?;
        while let Some(change) = changes.next().await {
            let Ok(args) = change.args() else { continue };
            if args.interface_name().as_str() == "org.bluez.Device1"
                && args
                    .changed_properties()
                    .get("Paired")
                    .and_then(|value| bool::try_from(value).ok())
                    == Some(true)
            {
                return Ok(true);
            }
        }
        Ok::<bool, zbus::Error>(false)
    };
    watch.await.unwrap_or_else(|error| {
        warn!(target: "tilo::bluetooth", device, %error, "cannot follow the pairing device");
        false
    })
}

/// Ask each of the agent's questions in `questions`, one at a time, until
/// `BlueZ` releases the agent.
async fn pump(
    mut events: AgentEvents,
    questions: Questions,
    client: BluezClient,
    timeout: Duration,
) {
    let mut current: Option<Current> = None;
    let mut watch: Option<Watch> = None;
    let mut asked = 0_u64;
    loop {
        let deadline = current.as_ref().map(|current| current.deadline);
        tokio::select! {
            event = events.recv() => match event {
                Some(AgentEvent::Ask { request, reply }) => {
                    let mut handle = None;
                    if let Some(previous) = current.take() {
                        let progress = previous.shows_code()
                            && !request.prompt.expects_answer()
                            && previous.request.device_path == request.device_path;
                        if progress {
                            // The device had another digit typed: the same
                            // question, shown anew.
                            handle = Some(previous.asked.handle().to_owned());
                        } else {
                            warn!(
                                target: "tilo::bluetooth",
                                device = %previous.request.device_path,
                                "a second pairing question arrived; declining the first"
                            );
                            previous.withdraw();
                        }
                    }
                    let handle = handle.unwrap_or_else(|| {
                        asked = asked.wrapping_add(1);
                        format!("{HANDLE_PREFIX}{asked}")
                    });
                    watch = (!request.prompt.expects_answer()).then(|| {
                        Box::pin(paired(client.clone(), request.device_path.clone())) as Watch
                    });
                    current = Some(Current {
                        asked: questions.ask(question(handle, &request)),
                        request,
                        reply,
                        deadline: deadline_after(timeout),
                    });
                }
                Some(AgentEvent::Cancelled) => {
                    // BlueZ withdrew it: no question left to answer.
                    watch = None;
                    if let Some(current) = current.take() {
                        current.withdraw();
                    }
                }
                Some(AgentEvent::Released) | None => break,
            },
            answer = answered(&mut current) => {
                watch = None;
                let Some(done) = current.take() else { continue };
                if done.shows_code() {
                    if answer.is_none() {
                        // A code on show has no reply to refuse: stop the
                        // pairing on the device instead.
                        let device = done.request.device_path.clone();
                        done.withdraw();
                        if let Err(error) = client.cancel_pairing(&device).await {
                            warn!(target: "tilo::bluetooth", device, %error, "could not cancel pairing");
                        }
                    }
                    continue;
                }
                let reply = reply(&done.request.prompt, answer.as_ref());
                info!(
                    target: "tilo::bluetooth",
                    device = %done.request.device_path,
                    accepted = !matches!(reply, PairingReply::Reject),
                    "pairing question answered"
                );
                let _ = done.reply.send(reply);
            },
            () = expired(deadline) => {
                watch = None;
                if let Some(current) = current.take() {
                    info!(target: "tilo::bluetooth", "pairing question timed out");
                    current.withdraw();
                }
            },
            finished = followed(&mut watch) => {
                watch = None;
                if finished && current.as_ref().is_some_and(Current::shows_code) {
                    current = None;
                }
            },
        }
    }
    if let Some(current) = current.take() {
        current.withdraw();
    }
}

/// Aborts the pump when dropped.
struct Pump(tokio::task::JoinHandle<()>);

impl Drop for Pump {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The registered agent and the task asking its questions. Dropping this
/// stops asking; [`Self::shutdown`] also unregisters from `BlueZ`.
pub struct PairingService {
    registration: AgentRegistration,
    client: BluezClient,
    _pump: Pump,
}

impl PairingService {
    /// Register the pairing agent on the system bus and ask its questions
    /// in `questions`.
    ///
    /// # Errors
    ///
    /// When the system bus or `BlueZ` cannot be reached: a desktop without
    /// Bluetooth, which the caller logs and carries on without.
    pub async fn start(questions: Questions) -> zbus::Result<Self> {
        let client = BluezClient::connect().await?;
        Self::start_with(client, questions, tilo_bluez::DEFAULT_ANSWER_TIMEOUT).await
    }

    /// [`Self::start`] on `client`'s connection. `timeout` is how long a
    /// question waits for an answer, and how long a code stays on show
    /// without news from the device.
    ///
    /// # Errors
    ///
    /// When `BlueZ` refuses the agent.
    pub async fn start_with(
        client: BluezClient,
        questions: Questions,
        timeout: Duration,
    ) -> zbus::Result<Self> {
        // Names first, so the first question can name its device without
        // calling back into a daemon blocked waiting on the agent.
        if let Err(error) = client.devices().await {
            warn!(target: "tilo::bluetooth", %error, "could not preload device names");
        }
        let (agent, events) = PairingAgent::with_timeout(client.names(), timeout);
        let registration = AgentRegistration::register(client.connection(), agent).await?;
        info!(target: "tilo::bluetooth", path = registration.path(), "pairing agent registered");
        let pump = tokio::spawn(pump(events, questions, client.clone(), timeout));
        Ok(Self {
            registration,
            client,
            _pump: Pump(pump),
        })
    }

    #[must_use]
    pub fn client(&self) -> &BluezClient {
        &self.client
    }

    /// Unregister cleanly, so `BlueZ` is not left holding a dead object
    /// path across a session restart. A question still asked is refused.
    pub async fn shutdown(self) {
        let Self {
            registration,
            _pump: pump,
            ..
        } = self;
        drop(pump);
        if let Err(error) = registration.unregister().await {
            warn!(target: "tilo::bluetooth", %error, "could not unregister the pairing agent");
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod copy_lint_tests;
