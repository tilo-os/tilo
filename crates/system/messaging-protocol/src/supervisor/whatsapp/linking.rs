//! Private linking presentation from the exact installed SDK client.
use super::super::{AccountAuthority, AccountSupervisor};
use crate::journal::Owner;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
use tilo_clock::{Duration, Instant};
use tilo_messaging::{Pairing, SendError, ServiceAccount};
use whatsapp_rust::{
    Client,
    wacore::types::events::{Event, EventHandler, EventInterest, EventKind, Subscription},
};
use zeroize::Zeroizing;

struct Presentation {
    attempt: String,
    payload: Zeroizing<Vec<u8>>,
    expires: Instant,
}
struct State {
    presentation: Option<Presentation>,
    verified: Option<String>,
    interrupted: bool,
}
/// Retains the original account, SDK source and credential authority. QR bytes
/// and verified self identity remain private, never in a replicated resource.
pub struct WhatsappLinking {
    owner: Owner,
    source: Arc<Client>,
    authority: Arc<dyn AccountAuthority>,
    state: Mutex<State>,
    resumed: Option<String>,
    started: AtomicBool,
    stopped: AtomicBool,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    subscription: Mutex<Option<Subscription>>,
}
struct Handler(Weak<WhatsappLinking>);
impl EventHandler for Handler {
    fn interest(&self) -> EventInterest {
        EventInterest::of(&[
            EventKind::PairingQrCode,
            EventKind::PairSuccess,
            EventKind::PairError,
            EventKind::LoggedOut,
        ])
    }
    fn handle_event(&self, event: Arc<Event>) {
        if let Some(linker) = self.0.upgrade() {
            linker.event(&event);
        }
    }
}
impl WhatsappLinking {
    fn new(
        owner: Owner,
        source: Arc<Client>,
        authority: Arc<dyn AccountAuthority>,
        resumed: Option<String>,
    ) -> Result<Arc<Self>, SendError> {
        authority.check()?;
        let linker = Arc::new(Self {
            owner,
            source,
            authority,
            state: Mutex::new(State {
                presentation: None,
                verified: None,
                interrupted: false,
            }),
            resumed,
            started: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            task: Mutex::new(None),
            subscription: Mutex::new(None),
        });
        let subscription = linker
            .source
            .subscribe_handler(Arc::new(Handler(Arc::downgrade(&linker))));
        *linker
            .subscription
            .lock()
            .map_err(|_| SendError::Unavailable)? = Some(subscription);
        Ok(linker)
    }
    fn check(&self) -> Result<(), SendError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(SendError::Revoked);
        }
        self.authority.check()
    }
    fn event(&self, event: &Event) {
        if self.check().is_err() {
            self.cancel();
            return;
        }
        let Ok(mut state) = self.state.lock() else {
            self.cancel();
            return;
        };
        if state.interrupted {
            return;
        }
        if self.resumed.is_some()
            && matches!(
                event,
                Event::PairingQrCode(_)
                    | Event::PairSuccess(_)
                    | Event::PairError(_)
                    | Event::LoggedOut(_)
            )
        {
            state.interrupted = true;
            state.presentation = None;
            drop(state);
            self.cancel();
            return;
        }
        match event {
            Event::PairingQrCode(qr)
                if !qr.code.is_empty() && qr.code.len() <= 4096 && qr.timeout.as_secs() > 5 =>
            {
                let life = Duration::from_secs(qr.timeout.as_secs().min(120).saturating_sub(5));
                let Some(expires) = Instant::now().checked_add(life) else {
                    state.interrupted = true;
                    return;
                };
                state.presentation = Some(Presentation {
                    attempt: uuid::Uuid::new_v4().to_string(),
                    payload: Zeroizing::new(qr.code.as_bytes().to_vec()),
                    expires,
                });
            }
            Event::PairSuccess(pair) => {
                let snapshot = self.source.persistence_manager().get_device_snapshot();
                let valid = snapshot
                    .pn
                    .as_ref()
                    .is_some_and(|pn| pn.to_non_ad() == pair.id.to_non_ad())
                    && snapshot
                        .lid
                        .as_ref()
                        .is_some_and(|lid| lid.to_non_ad() == pair.lid.to_non_ad());
                let identity = pair.id.to_non_ad_string();
                if valid
                    && state
                        .presentation
                        .as_ref()
                        .is_some_and(|p| p.expires > Instant::now())
                    && identity.len() <= 128
                    && identity.ends_with("@s.whatsapp.net")
                    && identity.split_once('@').is_some_and(|(number, _)| {
                        !number.is_empty() && number.bytes().all(|c| c.is_ascii_digit())
                    })
                    && state
                        .verified
                        .as_ref()
                        .is_none_or(|verified| verified == &identity)
                    && self.check().is_ok()
                {
                    state.verified = Some(identity);
                } else {
                    state.interrupted = true;
                    state.presentation = None;
                }
            }
            Event::PairError(_) | Event::LoggedOut(_) | Event::PairingQrCode(_) => {
                state.interrupted = true;
                state.presentation = None;
            }
            _ => {}
        }
    }
    fn start(&self) -> Result<(), SendError> {
        self.check()?;
        let handle = tokio::runtime::Handle::try_current().map_err(|_| SendError::Unavailable)?;
        if self.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let source = self.source.clone();
        let authority = self.authority.clone();
        let task = handle.spawn(async move {
            let run = source.run();
            tokio::pin!(run);
            loop {
                tokio::select! {
                    () = &mut run => break,
                    () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                        if authority.check().is_err() { break; }
                    }
                }
            }
            source.disconnect().await;
        });
        *self.task.lock().map_err(|_| SendError::Unavailable)? = Some(task);
        Ok(())
    }
    /// Begin only the original new account's explicitly approved linking.
    /// Refresh returns the current SDK presentation and its current attempt.
    /// # Errors
    /// Refuses revocation, absent/expired presentation or an interrupted source.
    pub async fn begin(&self) -> Result<Pairing, SendError> {
        if self.resumed.is_some() {
            return Err(SendError::AlreadyReserved);
        }
        self.start()?;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(60))
            .ok_or(SendError::Unavailable)?;
        loop {
            self.check()?;
            {
                let state = self.state.lock().map_err(|_| SendError::Unavailable)?;
                if state.interrupted {
                    return Err(SendError::Revoked);
                }
                if let Some(presentation) = &state.presentation
                    && presentation.expires > Instant::now()
                {
                    return Ok(Pairing::new(
                        ServiceAccount::new("whatsapp", &self.owner.account),
                        presentation.attempt.clone(),
                        self.owner.linking_generation,
                        presentation.payload.clone(),
                    ));
                }
            }
            if Instant::now() >= deadline {
                self.cancel();
                return Err(SendError::Unavailable);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    /// Verify the exact SDK device snapshot for the current private attempt.
    /// This supplies self identity, never a grant or public account publication.
    /// # Errors
    /// Refuses rotated/expired codes, another self identity or revoked authority.
    pub fn finish(&self, attempt: &str) -> Result<String, SendError> {
        self.check()?;
        let state = self.state.lock().map_err(|_| SendError::Unavailable)?;
        if state.interrupted
            || state
                .presentation
                .as_ref()
                .is_none_or(|p| p.attempt != attempt)
        {
            return Err(SendError::Revoked);
        }
        if state
            .presentation
            .as_ref()
            .is_some_and(|p| p.expires <= Instant::now())
        {
            drop(state);
            self.cancel();
            return Err(SendError::Revoked);
        }
        let verified = state.verified.clone().ok_or(SendError::Unavailable)?;
        drop(state);
        let current = self.source.persistence_manager().get_device_snapshot();
        if !self.source.is_logged_in() || !self.source.is_connected() {
            return Err(SendError::Unavailable);
        }
        if current
            .pn
            .as_ref()
            .is_none_or(|pn| pn.to_non_ad_string() != verified)
        {
            return Err(SendError::Revoked);
        }
        self.check()?;
        Ok(verified)
    }
    /// Resume the pinned encrypted identity without creating a pairing presentation.
    /// # Errors
    /// Refuses changed SDK identity, logout, relinking or revoked original authority.
    pub fn resume(&self) -> Result<(), SendError> {
        self.verify_resume()?;
        self.start()
    }
    /// Check the exact pinned self identity without connecting or blocking on I/O.
    /// # Errors
    /// Refuses changed identity, interrupted linking or revoked original authority.
    pub fn verify_resume(&self) -> Result<(), SendError> {
        self.check()?;
        let expected = self.resumed.as_ref().ok_or(SendError::InvalidRequest)?;
        if self
            .state
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .interrupted
            || self
                .source
                .persistence_manager()
                .get_device_snapshot()
                .pn
                .as_ref()
                .is_none_or(|pn| pn.to_non_ad_string() != *expected)
        {
            self.cancel();
            return Err(SendError::Revoked);
        }
        self.check()
    }
    /// Observe original authority and pairing interruption without exposing secrets.
    /// # Errors
    /// Refuses cancellation, revocation, logout or malformed SDK pairing events.
    pub fn check_linking(&self) -> Result<(), SendError> {
        self.check()?;
        if self
            .state
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .interrupted
        {
            return Err(SendError::Revoked);
        }
        Ok(())
    }
    pub fn cancel(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Ok(mut task) = self.task.lock()
            && let Some(task) = task.take()
        {
            task.abort();
        }
        if let Ok(mut state) = self.state.lock() {
            state.presentation = None;
            state.verified = None;
            state.interrupted = true;
        }
    }
}
impl Drop for WhatsappLinking {
    fn drop(&mut self) {
        self.cancel();
    }
}
impl AccountSupervisor {
    /// Capture only this supervisor's exact installed SDK source and original
    /// account authority. This never searches for another account or grant.
    /// # Errors
    /// Refuses missing native authority, unavailable or already linked SDK state.
    pub fn whatsapp_linking(&self) -> Result<Arc<WhatsappLinking>, SendError> {
        let authority = self.authority.clone().ok_or(SendError::Revoked)?;
        authority.check()?;
        let source = self
            .runtime
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .whatsapp_source
            .clone()
            .ok_or(SendError::Unavailable)?;
        if source
            .persistence_manager()
            .get_device_snapshot()
            .pn
            .is_some()
        {
            return Err(SendError::AlreadyReserved);
        }
        WhatsappLinking::new(self.owner.clone(), source, authority, None)
    }
    /// Resume only the already reviewed identity from this exact encrypted SDK.
    /// # Errors
    /// Refuses absent credentials, changed self identity or an unlinked device.
    pub fn whatsapp_resume(&self, expected: &str) -> Result<Arc<WhatsappLinking>, SendError> {
        let authority = self.authority.clone().ok_or(SendError::Revoked)?;
        authority.check()?;
        if expected.len() > 128
            || !expected
                .strip_suffix("@s.whatsapp.net")
                .is_some_and(|number| {
                    !number.is_empty() && number.bytes().all(|c| c.is_ascii_digit())
                })
        {
            return Err(SendError::InvalidRequest);
        }
        let source = self
            .runtime
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .whatsapp_source
            .clone()
            .ok_or(SendError::Unavailable)?;
        if source
            .persistence_manager()
            .get_device_snapshot()
            .pn
            .as_ref()
            .is_none_or(|pn| pn.to_non_ad_string() != expected)
        {
            return Err(SendError::Revoked);
        }
        WhatsappLinking::new(
            self.owner.clone(),
            source,
            authority,
            Some(expected.to_owned()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whatsapp_rust::wacore::types::events::PairingQrCode;
    async fn fixture() -> (
        tempfile::TempDir,
        Arc<WhatsappLinking>,
        Arc<super::super::network::tests::Gate>,
    ) {
        let root = tempfile::tempdir().unwrap();
        let owner = Owner {
            principal: "11".repeat(32),
            device: "fixture-device".into(),
            service: crate::Service::Whatsapp,
            account: "fixture-account".into(),
            linking_generation: 1,
        };
        let (gate, _, network) = super::super::network::tests::fixture(false, false);
        let bootstrap = super::super::WhatsappBootstrap::open_profile(
            &root.path().join("sdk"),
            owner.clone(),
            secret_vault::VaultKey::from_bytes([23; 32]),
            network,
            gate.clone(),
        )
        .await
        .unwrap();
        let linker = WhatsappLinking::new(owner, bootstrap.client(), gate.clone(), None).unwrap();
        (root, linker, gate)
    }
    fn qr(code: &str) -> Event {
        Event::PairingQrCode(
            PairingQrCode::builder()
                .code(code.to_owned())
                .timeout(std::time::Duration::from_secs(60))
                .build(),
        )
    }
    #[tokio::test]
    async fn private_sdk_code_rotation_never_proves_authentication_and_revocation_clears_it() {
        let (_root, linker, gate) = fixture().await;
        linker.event(&qr("private-first-code"));
        let first = linker
            .state
            .lock()
            .unwrap()
            .presentation
            .as_ref()
            .unwrap()
            .attempt
            .clone();
        assert_eq!(linker.finish(&first), Err(SendError::Unavailable));
        linker.event(&qr("private-second-code"));
        let next = linker
            .state
            .lock()
            .unwrap()
            .presentation
            .as_ref()
            .unwrap()
            .attempt
            .clone();
        assert_ne!(first, next);
        assert_eq!(linker.finish(&first), Err(SendError::Revoked));
        assert_eq!(linker.finish(&next), Err(SendError::Unavailable));
        assert!(!linker.source.is_logged_in());
        assert!(!linker.source.is_connected());
        gate.0.store(false, Ordering::Release);
        linker.event(&qr("refused-code"));
        assert!(linker.state.lock().unwrap().presentation.is_none());
        assert_eq!(linker.finish(&next), Err(SendError::Revoked));
    }
    #[tokio::test]
    async fn expired_private_sdk_attempt_refuses_completion_before_identity_or_connection() {
        let (_root, linker, _gate) = fixture().await;
        linker.event(&qr("private-expiring-code"));
        let attempt = {
            let mut state = linker.state.lock().unwrap();
            let presentation = state.presentation.as_mut().unwrap();
            presentation.expires = Instant::now();
            let attempt = presentation.attempt.clone();
            drop(state);
            attempt
        };
        assert_eq!(linker.finish(&attempt), Err(SendError::Revoked));
        assert!(linker.state.lock().unwrap().presentation.is_none());
        assert_eq!(linker.check_linking(), Err(SendError::Revoked));
        assert!(!linker.source.is_logged_in());
        assert!(!linker.source.is_connected());
    }
    #[tokio::test]
    async fn restart_without_exact_encrypted_self_identity_cannot_connect_or_pair() {
        let (_root, linker, _gate) = fixture().await;
        let resumed = WhatsappLinking::new(
            linker.owner.clone(),
            linker.source.clone(),
            linker.authority.clone(),
            Some("12025550123@s.whatsapp.net".into()),
        )
        .unwrap();
        assert_eq!(resumed.resume(), Err(SendError::Revoked));
        assert!(resumed.task.lock().unwrap().is_none());
        assert!(!resumed.source.is_connected());
        resumed.event(&qr("refused-relink"));
        assert!(resumed.state.lock().unwrap().presentation.is_none());
    }
}
