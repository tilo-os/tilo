//! The compositor's Bluetooth pairing agent.
//!
//! `BlueZ` asks the desktop a question mid-pairing — "these six digits, do
//! they match what the headphones are showing?" — and holds the pairing
//! open until the answer comes back. Whoever answers decides whether a
//! nearby device gets to inject keystrokes and read audio.
//!
//! That makes it the compositor's job and nobody else's: an application
//! must not be able to draw a pairing dialog, or answer one. `tilo-bluez`
//! provides the agent object; this module owns it, keeps it registered as
//! `BlueZ`'s default agent for the session's life, and turns each question
//! into something the OS chrome can render and reply to.
//!
//! The agent is served on the process's one `BlueZ` connection
//! ([`os_actions::bluez::shared_client`]), the same one quick settings and
//! handoff pair through, so the process never holds a second connection.
//! Settings and every other client that starts a pairing reach it as the
//! default agent.
//!
//! A question on screen goes away when it is answered, when `BlueZ`
//! withdraws it (`Cancel`, `Release`), when the device finishes pairing
//! while a code is displayed, or when nobody answers within the agent's
//! timeout — never by being silently accepted.

use std::sync::{Arc, Mutex, PoisonError};

#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use tilo_bluez::BluezClient;
#[cfg(target_os = "linux")]
use tilo_bluez::agent::{AgentEvent, AgentRegistration, PairingAgent};
use tilo_bluez::{PairingReply, PairingReplySender, PairingRequest};
use tracing::{info, warn};

/// How long after the agent gives up on a question the dialog comes down.
#[cfg(target_os = "linux")]
const EXPIRY_GRACE: Duration = Duration::from_millis(500);

/// Wakes whoever draws the prompt when the question on screen changes.
pub type PromptWake = Arc<dyn Fn() + Send + Sync>;

/// The prompt currently on screen, if any, and the channel that answers it.
///
/// One at a time by construction: `BlueZ` pairs one device at a time, and
/// a second dialog stacking on the first would be a way to trick someone into answering the wrong question.
#[derive(Default)]
pub struct PairingState {
    current: Option<Pending>,
    next_token: u64,
    wake: Option<PromptWake>,
    /// Where a display-only prompt the person cancels becomes a
    /// `CancelPairing` on the device: that prompt has no reply to refuse.
    withdrawals: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

struct Pending {
    request: PairingRequest,
    reply: PairingReplySender,
    token: u64,
}

/// Cheap-clone handle the chrome holds.
#[derive(Clone, Default)]
pub struct PairingPromptHandle {
    inner: Arc<Mutex<PairingState>>,
}

impl PairingPromptHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// The session's handle: the one the compositor's agent posts to and
    /// its chrome draws from.
    pub fn global() -> Self {
        static HANDLE: std::sync::LazyLock<PairingPromptHandle> =
            std::sync::LazyLock::new(PairingPromptHandle::default);
        #[cfg(test)]
        crate::shell::test_support::ambient_start("global Bluetooth pairing prompt");
        HANDLE.clone()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, PairingState> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wake `wake` whenever a question appears, changes or goes away.
    pub fn set_wake(&self, wake: PromptWake) {
        self.state().wake = Some(wake);
    }

    #[cfg(any(test, target_os = "linux"))]
    fn set_withdrawals(&self, withdrawals: tokio::sync::mpsc::UnboundedSender<String>) {
        self.state().withdrawals = Some(withdrawals);
    }

    /// Run the wake outside the lock, so a wake that reads the handle
    /// cannot deadlock.
    fn wake(&self) {
        let wake = self.state().wake.clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    /// The question to render, or `None` when there is nothing to ask.
    pub fn current(&self) -> Option<PairingRequest> {
        self.state().current.as_ref().map(|p| p.request.clone())
    }

    pub fn is_prompting(&self) -> bool {
        self.current().is_some()
    }

    /// Post a question and return its token, which [`Self::expire`] takes.
    ///
    /// A display-only update for the device already showing a code (the
    /// remote typed another digit) replaces it quietly. Any other prompt
    /// already up is refused rather than silently replaced, so an abandoned
    /// pairing fails closed.
    pub fn post(&self, request: PairingRequest, reply: PairingReplySender) -> u64 {
        let token = {
            let mut s = self.state();
            if let Some(prev) = s.current.take() {
                let progress = !prev.request.prompt.expects_answer()
                    && !request.prompt.expects_answer()
                    && prev.request.device_path == request.device_path;
                if !progress {
                    warn!(
                        target: "tilo::bluetooth",
                        device = %prev.request.device_path,
                        "a second pairing question arrived; declining the first"
                    );
                    let _ = prev.reply.send(PairingReply::Reject);
                }
            }
            s.next_token = s.next_token.wrapping_add(1);
            let token = s.next_token;
            s.current = Some(Pending {
                request,
                reply,
                token,
            });
            token
        };
        self.wake();
        token
    }

    /// Take down the question `token` names if it is still the one on
    /// screen: nobody answered in time. A later question keeps its place.
    pub fn expire(&self, token: u64) -> bool {
        let is_current = self
            .state()
            .current
            .as_ref()
            .is_some_and(|pending| pending.token == token);
        is_current && self.withdraw()
    }

    /// Take down a displayed code once `device` has finished pairing.
    pub fn finish_display(&self, device: &str) -> bool {
        let displayed = self.state().current.as_ref().is_some_and(|pending| {
            !pending.request.prompt.expects_answer() && pending.request.device_path == device
        });
        displayed && self.withdraw()
    }

    /// Answer the prompt and clear it. Returns false when there was nothing
    /// to answer, so a stray click cannot invent a consent.
    pub fn answer(&self, reply: PairingReply) -> bool {
        let Some(pending) = self.state().current.take() else {
            return false;
        };
        info!(
            target: "tilo::bluetooth",
            device = %pending.request.device_path,
            accepted = !matches!(reply, PairingReply::Reject),
            "pairing prompt answered"
        );
        let _ = pending.reply.send(reply);
        self.wake();
        true
    }

    /// The person closed the prompt without agreeing. A question is refused;
    /// a displayed code stops the pairing on the device, since there is no
    /// reply left to refuse.
    ///
    /// Send an explicit rejection because the agent retains a cancellation
    /// handle for every active request; merely dropping the UI's handle
    /// would otherwise wait for the timeout.
    pub fn dismiss(&self) -> bool {
        let (pending, withdrawals) = {
            let mut s = self.state();
            let withdrawals = s.withdrawals.clone();
            (s.current.take(), withdrawals)
        };
        let Some(pending) = pending else {
            return false;
        };
        if !pending.request.prompt.expects_answer()
            && let Some(withdrawals) = withdrawals
        {
            let _ = withdrawals.send(pending.request.device_path.clone());
        }
        let _ = pending.reply.send(PairingReply::Reject);
        self.wake();
        true
    }

    /// `BlueZ` or the clock took the question away: refuse it if it is
    /// still waiting, and clear the screen.
    fn withdraw(&self) -> bool {
        let Some(pending) = self.state().current.take() else {
            return false;
        };
        let _ = pending.reply.send(PairingReply::Reject);
        self.wake();
        true
    }
}

/// A registered agent plus the task pumping its events. Dropping this stops the pump; call [`Self::shutdown`] to also unregister from `BlueZ`.
#[cfg(target_os = "linux")]
pub struct PairingService {
    registration: AgentRegistration,
    pump: tokio::task::JoinHandle<()>,
    client: BluezClient,
    handle: PairingPromptHandle,
}

#[cfg(target_os = "linux")]
impl PairingService {
    /// Register the pairing agent on the process's `BlueZ` connection and
    /// start feeding prompts to `handle`.
    ///
    /// Failing to reach `BlueZ` is not fatal — a desktop with no Bluetooth is
    /// a desktop, so the caller logs and continues. What must never happen
    /// is the agent appearing registered when it isn't, since then pairings would silently fail with no prompt.
    ///
    /// # Errors
    ///
    /// Returns an error if connecting to `org.bluez` fails, or if `bluetoothd`
    /// refuses to register the pairing agent.
    pub async fn start(handle: PairingPromptHandle) -> Result<Self, String> {
        let client = tokio::task::spawn_blocking(os_actions::bluez::shared_client)
            .await
            .map_err(|e| format!("cannot reach org.bluez: {e}"))??;
        Self::start_with(client, handle, tilo_bluez::DEFAULT_ANSWER_TIMEOUT).await
    }

    /// [`Self::start`] on a given client. `timeout` is how long a question
    /// waits for an answer, and how long a displayed code stays up without
    /// news from the device.
    ///
    /// # Errors
    ///
    /// Returns an error if `bluetoothd` refuses to register the agent.
    pub async fn start_with(
        client: BluezClient,
        handle: PairingPromptHandle,
        timeout: Duration,
    ) -> Result<Self, String> {
        // Prime the name cache so the first prompt can title itself without calling back into a daemon blocked waiting on us.
        if let Err(e) = client.devices().await {
            warn!(target: "tilo::bluetooth", error = %e, "could not preload device names");
        }
        let (agent, mut events) = PairingAgent::with_timeout(client.names(), timeout);
        let registration = AgentRegistration::register(client.connection(), agent)
            .await
            .map_err(|e| format!("bluetoothd refused the pairing agent: {e}"))?;
        info!(target: "tilo::bluetooth", path = registration.path(), "pairing agent registered");

        let (withdraw_tx, mut withdrawals) = tokio::sync::mpsc::unbounded_channel();
        handle.set_withdrawals(withdraw_tx);
        let pump_handle = handle.clone();
        let pump_client = client.clone();
        let pump = tokio::spawn(async move {
            let mut watcher: Option<tokio::task::JoinHandle<()>> = None;
            loop {
                tokio::select! {
                    ev = events.recv() => {
                        let Some(ev) = ev else { break };
                        match ev {
                            AgentEvent::Ask { request, reply } => {
                                if let Some(previous) = watcher.take() {
                                    previous.abort();
                                }
                                let displayed = (!request.prompt.expects_answer())
                                    .then(|| request.device_path.clone());
                                let token = pump_handle.post(request, reply);
                                let expiry = pump_handle.clone();
                                tokio::spawn(async move {
                                    // Just after the agent's own deadline, so
                                    // `BlueZ` hears "nobody answered"
                                    // (Canceled) rather than a refusal.
                                    tokio::time::sleep(timeout.saturating_add(EXPIRY_GRACE))
                                        .await;
                                    if expiry.expire(token) {
                                        info!(target: "tilo::bluetooth", "pairing prompt timed out");
                                    }
                                });
                                if let Some(device) = displayed {
                                    watcher = Some(tokio::spawn(finish_display_when_paired(
                                        pump_client.clone(),
                                        pump_handle.clone(),
                                        device,
                                    )));
                                }
                            }
                            AgentEvent::Cancelled => {
                                // BlueZ withdrew it — take the prompt down rather than leaving a dead dialog the user can answer.
                                pump_handle.withdraw();
                            }
                            AgentEvent::Released => {
                                pump_handle.withdraw();
                                break;
                            }
                        }
                    }
                    Some(device) = withdrawals.recv() => {
                        if let Err(e) = pump_client.cancel_pairing(&device).await {
                            warn!(target: "tilo::bluetooth", device, error = %e, "could not cancel pairing");
                        }
                    }
                }
            }
            if let Some(watcher) = watcher {
                watcher.abort();
            }
        });

        Ok(Self {
            registration,
            pump,
            client,
            handle,
        })
    }

    pub fn client(&self) -> &BluezClient {
        &self.client
    }

    /// Unregister cleanly, so the daemon isn't left holding a dead object path across a compositor restart.
    pub async fn shutdown(self) {
        self.handle.withdraw();
        self.pump.abort();
        if let Err(e) = self.registration.unregister().await {
            warn!(target: "tilo::bluetooth", error = %e, "could not unregister the pairing agent");
        }
    }
}

/// Take a displayed code down when the device reports it is paired. The
/// agent learns nothing when a display-only pairing succeeds, so the
/// device's own `Paired` property is the only signal.
#[cfg(target_os = "linux")]
async fn finish_display_when_paired(
    client: BluezClient,
    handle: PairingPromptHandle,
    device: String,
) {
    use futures_util::StreamExt;

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
                handle.finish_display(&device);
                break;
            }
        }
        Ok::<(), zbus::Error>(())
    };
    if let Err(e) = watch.await {
        warn!(target: "tilo::bluetooth", device, error = %e, "cannot follow the pairing device");
    }
}

/// Start the service on the session runtime for the compositor boot path,
/// logging rather than failing when there's no Bluetooth on this machine.
#[cfg(target_os = "linux")]
pub fn spawn_on(
    rt: &tokio::runtime::Handle,
    handle: PairingPromptHandle,
) -> tokio::task::JoinHandle<Option<PairingService>> {
    rt.spawn(async move {
        match PairingService::start(handle).await {
            Ok(svc) => Some(svc),
            Err(e) => {
                info!(target: "tilo::bluetooth", "no pairing agent this session: {e}");
                None
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tilo_bluez::PairingPrompt;

    fn request(name: &str) -> PairingRequest {
        with_prompt(name, PairingPrompt::ConfirmPasskey { passkey: 123_456 })
    }

    fn with_prompt(name: &str, prompt: PairingPrompt) -> PairingRequest {
        PairingRequest {
            device_path: format!("/org/bluez/hci0/dev_{name}"),
            device_name: name.to_string(),
            prompt,
        }
    }

    #[tokio::test]
    async fn pairing_prompts_carry_answers_verbatim_and_refuse_on_dismiss() {
        // Phase `a_posted_prompt_is_readable_and_answerable`.
        {
            let h = PairingPromptHandle::new();
            assert!(!h.is_prompting());
            let (tx, rx) = PairingReplySender::channel();
            h.post(request("A"), tx);
            assert!(h.is_prompting());
            assert_eq!(h.current().unwrap().device_name, "A");
            assert!(h.answer(PairingReply::Accept));
            assert_eq!(rx.await.unwrap(), PairingReply::Accept);
            assert!(!h.is_prompting(), "answering clears the prompt");
        }
        // Phase `answering_nothing_does_nothing`.
        // A click with nothing on screen must not manufacture a consent.
        {
            let h = PairingPromptHandle::new();
            assert!(!h.answer(PairingReply::Accept));
            assert!(!h.dismiss());
        }
        // Phase `a_second_question_declines_the_first`.
        // Two questions must not stack. The older one is declined rather than left behind a new dialog where the user might answer the wrong one.
        {
            let h = PairingPromptHandle::new();
            let (tx1, rx1) = PairingReplySender::channel();
            let (tx2, _rx2) = PairingReplySender::channel();
            h.post(request("first"), tx1);
            h.post(request("second"), tx2);
            assert_eq!(rx1.await.unwrap(), PairingReply::Reject);
            assert_eq!(h.current().unwrap().device_name, "second");
        }
        // Phase `dismissing_refuses_rather_than_accepting`.
        // Closing the dialog without answering must refuse, never accept.
        {
            let h = PairingPromptHandle::new();
            let (tx, rx) = PairingReplySender::channel();
            h.post(request("A"), tx);
            assert!(h.dismiss());
            assert_eq!(rx.await.unwrap(), PairingReply::Reject);
        }
        // Phase `declining_is_carried_through_verbatim`.
        {
            let h = PairingPromptHandle::new();
            let (tx, rx) = PairingReplySender::channel();
            h.post(request("A"), tx);
            assert!(h.answer(PairingReply::Reject));
            assert_eq!(rx.await.unwrap(), PairingReply::Reject);
        }
        // Phase `a_typed_passkey_is_carried_through_verbatim`.
        {
            let h = PairingPromptHandle::new();
            let (tx, rx) = PairingReplySender::channel();
            h.post(with_prompt("X", PairingPrompt::RequestPasskey), tx);
            assert!(h.answer(PairingReply::Passkey(987_654)));
            assert_eq!(rx.await.unwrap(), PairingReply::Passkey(987_654));
        }
    }

    #[tokio::test]
    async fn prompts_expire_update_and_withdraw_without_consenting() {
        // Phase `an_unanswered_question_expires_as_a_refusal`.
        {
            let h = PairingPromptHandle::new();
            let (tx, rx) = PairingReplySender::channel();
            let token = h.post(request("A"), tx);
            assert!(h.expire(token));
            assert!(!h.is_prompting());
            assert_eq!(rx.await.unwrap(), PairingReply::Reject);
        }
        // Phase `an_old_timer_never_takes_down_a_newer_question`.
        {
            let h = PairingPromptHandle::new();
            let (tx1, _rx1) = PairingReplySender::channel();
            let first = h.post(request("A"), tx1);
            let (tx2, _rx2) = PairingReplySender::channel();
            let _second = h.post(request("B"), tx2);
            assert!(!h.expire(first));
            assert_eq!(h.current().unwrap().device_name, "B");
        }
        // Phase `typed_digits_update_a_displayed_code_in_place`.
        // Each key typed on the device re-sends the code with one more digit;
        // the dialog follows it without refusing anything.
        {
            let h = PairingPromptHandle::new();
            for entered in 0..=3 {
                let (tx, _rx) = PairingReplySender::channel();
                h.post(
                    with_prompt(
                        "K",
                        PairingPrompt::DisplayPasskey {
                            passkey: 42,
                            entered,
                        },
                    ),
                    tx,
                );
            }
            assert_eq!(
                h.current().unwrap().prompt,
                PairingPrompt::DisplayPasskey {
                    passkey: 42,
                    entered: 3
                }
            );
        }
        // Phase `cancelling_a_displayed_code_cancels_the_pairing`.
        // A displayed code has no reply to refuse; closing it stops the
        // pairing on the device instead.
        {
            let h = PairingPromptHandle::new();
            let (withdraw_tx, mut withdrawals) = tokio::sync::mpsc::unbounded_channel();
            h.set_withdrawals(withdraw_tx);
            let (tx, _rx) = PairingReplySender::channel();
            h.post(
                with_prompt("K", PairingPrompt::DisplayPinCode { pin: "0000".into() }),
                tx,
            );
            assert!(h.dismiss());
            assert_eq!(
                withdrawals.recv().await.as_deref(),
                Some("/org/bluez/hci0/dev_K")
            );
        }
        // Phase `a_finished_pairing_takes_down_only_its_own_code`.
        {
            let h = PairingPromptHandle::new();
            let (tx, _rx) = PairingReplySender::channel();
            h.post(
                with_prompt(
                    "K",
                    PairingPrompt::DisplayPasskey {
                        passkey: 1,
                        entered: 6,
                    },
                ),
                tx,
            );
            assert!(!h.finish_display("/org/bluez/hci0/dev_OTHER"));
            assert!(h.finish_display("/org/bluez/hci0/dev_K"));
            assert!(!h.is_prompting());
            let (tx, _rx) = PairingReplySender::channel();
            h.post(request("K"), tx);
            assert!(
                !h.finish_display("/org/bluez/hci0/dev_K"),
                "a question is answered, never finished for the person"
            );
        }
        // Phase `every_change_wakes_the_chrome`.
        {
            let h = PairingPromptHandle::new();
            let wakes = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&wakes);
            h.set_wake(Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }));
            let (tx, _rx) = PairingReplySender::channel();
            let token = h.post(request("A"), tx);
            h.expire(token);
            assert_eq!(wakes.load(Ordering::SeqCst), 2);
        }
    }
}
