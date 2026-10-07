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
        if self.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let source = self.source.clone();
        let task = tokio::runtime::Handle::try_current()
            .map_err(|_| SendError::Unavailable)?
            .spawn(async move {
                source.run().await;
            });
        *self.task.lock().map_err(|_| SendError::Unavailable)? = Some(task);
        Ok(())
    }
    /// Begin only the original new account's explicitly approved linking.
    /// Refresh returns the current SDK presentation and its current attempt.
    /// # Errors
    /// Refuses revocation, absent/expired presentation or an interrupted source.
    pub async fn begin(&self) -> Result<Pairing, SendError> {
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
            || !state
                .presentation
                .as_ref()
                .is_some_and(|p| p.attempt == attempt)
        {
            return Err(SendError::Revoked);
        }
        let verified = state.verified.clone().ok_or(SendError::Unavailable)?;
        let current = self.source.persistence_manager().get_device_snapshot();
        if !self.source.is_logged_in() || !self.source.is_connected() {
            return Err(SendError::Unavailable);
        }
        if !current
            .pn
            .as_ref()
            .is_some_and(|pn| pn.to_non_ad_string() == verified)
        {
            return Err(SendError::Revoked);
        }
        self.check()?;
        Ok(verified)
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
        WhatsappLinking::new(self.owner.clone(), source, authority)
    }
}
