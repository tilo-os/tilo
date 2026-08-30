//! The compositor's Bluetooth pairing agent.
//!
//! `BlueZ` asks the desktop a question mid-pairing — "these six digits, do
//! they match what the headphones are showing?" — and holds the pairing
//! open until the answer comes back. Whoever answers decides whether a
//! nearby device gets to inject keystrokes and read audio.
//!
//! That makes it the compositor's job and nobody else's: an application
//! must not be able to draw a pairing dialog, or answer one. `qos-bluez`
//! provides the agent object; this module owns it, keeps it registered
//! for the session's life, and turns each question into something the OS chrome can render and reply to.
//!
//! The Settings app can *start* a pairing (`Device1.Pair`), but the prompt that results belongs here.

use std::sync::{Arc, Mutex};

use qos_bluez::agent::{AgentEvent, AgentRegistration, PairingAgent};
use qos_bluez::{BluezClient, PairingReply, PairingRequest};
use tracing::{info, warn};

/// The prompt currently on screen, if any, and the channel that answers it.
///
/// One at a time by construction: `BlueZ` pairs one device at a time, and
/// a second dialog stacking on the first would be a way to trick someone into answering the wrong question.
#[derive(Default)]
pub struct PairingState {
    current: Option<Pending>,
}

struct Pending {
    request: PairingRequest,
    reply: tokio::sync::oneshot::Sender<PairingReply>,
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

    /// The question to render, or `None` when there is nothing to ask.
    pub fn current(&self) -> Option<PairingRequest> {
        self.inner
            .lock()
            .ok()
            .and_then(|s| s.current.as_ref().map(|p| p.request.clone()))
    }

    pub fn is_prompting(&self) -> bool {
        self.current().is_some()
    }

    /// Post a question. Any prompt already up is refused rather than
    /// silently replaced — dropping its reply channel is read as a decline by the agent, so an abandoned pairing fails closed.
    fn post(&self, request: PairingRequest, reply: tokio::sync::oneshot::Sender<PairingReply>) {
        if let Ok(mut s) = self.inner.lock() {
            if let Some(prev) = s.current.take() {
                warn!(
                    target: "qos::bluetooth",
                    device = %prev.request.device_path,
                    "a second pairing question arrived; declining the first"
                );
                let _ = prev.reply.send(PairingReply::Reject);
            }
            s.current = Some(Pending { request, reply });
        }
    }

    /// Answer the prompt and clear it. Returns false when there was nothing to answer, so a stray click cannot invent a consent.
    pub fn answer(&self, reply: PairingReply) -> bool {
        let Ok(mut s) = self.inner.lock() else {
            return false;
        };
        let Some(pending) = s.current.take() else {
            return false;
        };
        info!(
            target: "qos::bluetooth",
            device = %pending.request.device_path,
            accepted = !matches!(reply, PairingReply::Reject),
            "pairing prompt answered"
        );
        let _ = pending.reply.send(reply);
        true
    }

    /// Dismiss without answering — the user closed it, or `BlueZ` withdrew the
    /// request. Dropping the channel makes the agent refuse, the only safe reading of "no answer".
    pub fn dismiss(&self) -> bool {
        let Ok(mut s) = self.inner.lock() else {
            return false;
        };
        s.current.take().is_some()
    }
}

/// A registered agent plus the task pumping its events. Dropping this stops the pump; call [`Self::shutdown`] to also unregister from `BlueZ`.
pub struct PairingService {
    registration: AgentRegistration,
    pump: tokio::task::JoinHandle<()>,
    client: BluezClient,
}

impl PairingService {
    /// Connect to `BlueZ`, register the pairing agent, and start feeding
    /// prompts to `handle`.
    ///
    /// Failing to reach `BlueZ` is not fatal — a desktop with no Bluetooth is
    /// a desktop, so the caller logs and continues. What must never happen
    /// is the agent appearing registered when it isn't, since then pairings would silently fail with no prompt.
    pub async fn start(handle: PairingPromptHandle) -> Result<Self, String> {
        let client = BluezClient::connect()
            .await
            .map_err(|e| format!("cannot reach org.bluez: {e}"))?;
        // Prime the name cache so the first prompt can title itself without calling back into a daemon blocked waiting on us.
        if let Err(e) = client.devices().await {
            warn!(target: "qos::bluetooth", error = %e, "could not preload device names");
        }
        let (agent, mut events) = PairingAgent::new(client.names());
        let registration = AgentRegistration::register(client.connection(), agent)
            .await
            .map_err(|e| format!("bluetoothd refused the pairing agent: {e}"))?;
        info!(target: "qos::bluetooth", path = registration.path(), "pairing agent registered");

        let pump_handle = handle.clone();
        let pump = tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                match ev {
                    AgentEvent::Ask { request, reply } => pump_handle.post(request, reply),
                    AgentEvent::Cancelled => {
                        // BlueZ withdrew it — take the prompt down rather than leaving a dead dialog the user can answer.
                        pump_handle.dismiss();
                    }
                    AgentEvent::Released => {
                        pump_handle.dismiss();
                        break;
                    }
                }
            }
        });

        Ok(Self {
            registration,
            pump,
            client,
        })
    }

    pub fn client(&self) -> &BluezClient {
        &self.client
    }

    /// Unregister cleanly, so the daemon isn't left holding a dead object path across a compositor restart.
    pub async fn shutdown(self) {
        self.pump.abort();
        if let Err(e) = self.registration.unregister().await {
            warn!(target: "qos::bluetooth", error = %e, "could not unregister the pairing agent");
        }
    }
}

/// Convenience for the compositor boot path: start the service on `rt`, logging rather than failing when there's no Bluetooth on this machine.
pub fn spawn_on(
    rt: &tokio::runtime::Handle,
    handle: PairingPromptHandle,
) -> tokio::task::JoinHandle<Option<PairingService>> {
    rt.spawn(async move {
        match PairingService::start(handle).await {
            Ok(svc) => Some(svc),
            Err(e) => {
                info!(target: "qos::bluetooth", "no pairing agent this session: {e}");
                None
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_bluez::PairingPrompt;

    fn request(name: &str) -> PairingRequest {
        PairingRequest {
            device_path: format!("/org/bluez/hci0/dev_{name}"),
            device_name: name.to_string(),
            prompt: PairingPrompt::ConfirmPasskey { passkey: 123_456 },
        }
    }

    #[tokio::test]
    async fn a_posted_prompt_is_readable_and_answerable() {
        let h = PairingPromptHandle::new();
        assert!(!h.is_prompting());
        let (tx, rx) = tokio::sync::oneshot::channel();
        h.post(request("A"), tx);
        assert!(h.is_prompting());
        assert_eq!(h.current().unwrap().device_name, "A");
        assert!(h.answer(PairingReply::Accept));
        assert_eq!(rx.await.unwrap(), PairingReply::Accept);
        assert!(!h.is_prompting(), "answering clears the prompt");
    }

    /// A click with nothing on screen must not manufacture a consent.
    #[tokio::test]
    async fn answering_nothing_does_nothing() {
        let h = PairingPromptHandle::new();
        assert!(!h.answer(PairingReply::Accept));
        assert!(!h.dismiss());
    }

    /// Two questions must not stack. The older one is declined rather than left behind a new dialog where the user might answer the wrong one.
    #[tokio::test]
    async fn a_second_question_declines_the_first() {
        let h = PairingPromptHandle::new();
        let (tx1, rx1) = tokio::sync::oneshot::channel();
        let (tx2, _rx2) = tokio::sync::oneshot::channel();
        h.post(request("first"), tx1);
        h.post(request("second"), tx2);
        assert_eq!(rx1.await.unwrap(), PairingReply::Reject);
        assert_eq!(h.current().unwrap().device_name, "second");
    }

    /// Closing the dialog without answering must refuse, never accept.
    #[tokio::test]
    async fn dismissing_refuses_rather_than_accepting() {
        let h = PairingPromptHandle::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        h.post(request("A"), tx);
        assert!(h.dismiss());
        // The sender was dropped, so the agent sees a closed channel — which it reads as a refusal.
        assert!(rx.await.is_err(), "no answer was sent");
    }

    #[tokio::test]
    async fn declining_is_carried_through_verbatim() {
        let h = PairingPromptHandle::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        h.post(request("A"), tx);
        assert!(h.answer(PairingReply::Reject));
        assert_eq!(rx.await.unwrap(), PairingReply::Reject);
    }

    #[tokio::test]
    async fn a_typed_passkey_is_carried_through_verbatim() {
        let h = PairingPromptHandle::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        h.post(
            PairingRequest {
                device_path: "/org/bluez/hci0/dev_X".into(),
                device_name: "X".into(),
                prompt: PairingPrompt::RequestPasskey,
            },
            tx,
        );
        assert!(h.answer(PairingReply::Passkey(987_654)));
        assert_eq!(rx.await.unwrap(), PairingReply::Passkey(987_654));
    }
}
