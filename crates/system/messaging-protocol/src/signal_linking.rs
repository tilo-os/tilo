//! Explicit private provisioning over a host-owned Signal multi-account stream.
//! No socket discovery, registration, trust change or credential grant occurs.
use crate::{
    journal::Owner,
    signal_runtime::{Deadline, DeadlineReader, DeadlineStream},
    supervisor::AccountAuthority,
};
use serde::{Deserialize, Deserializer};
use std::{
    io::Write,
    net::Shutdown,
    os::unix::net::UnixStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
};
use tilo_clock::{Duration, Instant};
use tilo_messaging::SendError;
use zeroize::Zeroizing;

const FRESH: u8 = 0;
const STARTING: u8 = 1;
const WAITING: u8 = 2;
const FINISHING: u8 = 3;
const FINISHED: u8 = 4;
const INTERRUPTED: u8 = 5;
const ACCEPTED: u8 = 6;
const LINK_LIFETIME: Duration = Duration::from_secs(120);

/// Private display material. It has no serialization or payload diagnostics.
pub struct SignalPairing {
    pub attempt: String,
    uri: Zeroizing<String>,
}
impl SignalPairing {
    pub fn qr_bytes(&self) -> &[u8] {
        self.uri.as_bytes()
    }
}
impl std::fmt::Debug for SignalPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SignalPairing(<private>)")
    }
}
struct Pending {
    attempt: String,
    uri: Zeroizing<String>,
    expires: Instant,
}
struct Runtime {
    reader: DeadlineReader,
    writer: DeadlineStream,
    deadline: Deadline,
}
/// One explicit link generation, original admission and exact intended ACI.
/// A successful link is account evidence, never evidence of a ready receiver.
pub struct SignalLinking {
    owner: Owner,
    target: Target,
    authority: Arc<dyn AccountAuthority>,
    phase: AtomicU8,
    runtime: Mutex<Runtime>,
    pending: Mutex<Option<Pending>>,
    stop: UnixStream,
}
enum Target {
    Existing(String),
    Enrollment,
}

/// First-account provisioning, before a canonical account or messaging grant
/// exists. The host supplies an isolated owned profile/stream and an authority
/// checking the original personal session and its provisioning permission.
/// This handle exports no messaging client, route, credential or ready state.
pub struct SignalEnrollment {
    linker: Arc<SignalLinking>,
}
impl SignalEnrollment {
    pub fn owner(&self) -> &Owner {
        self.linker.owner()
    }
    /// # Errors
    /// Refuses malformed provisional ownership or an unusable owned stream.
    pub fn new(
        owner: Owner,
        authority: Arc<dyn AccountAuthority>,
        stream: UnixStream,
    ) -> Result<Self, SendError> {
        Ok(Self {
            linker: SignalLinking::for_target(owner, Target::Enrollment, authority, stream)?,
        })
    }
    /// # Errors
    /// Refuses retired provisioning permission, replay or invalid private data.
    pub async fn begin(&self) -> Result<SignalPairing, SendError> {
        self.linker.begin().await
    }
    /// The result is private identity evidence awaiting explicit host review.
    /// It does not publish an account, grant access or activate a receiver.
    /// # Errors
    /// Refuses stale/expired attempts, retirement or a malformed provider ACI.
    pub async fn finish(&self, attempt: String) -> Result<VerifiedSignalEnrollment, SendError> {
        let account = self.linker.finish(attempt).await?;
        Ok(VerifiedSignalEnrollment {
            linker: self.linker.clone(),
            account,
        })
    }
    pub fn state(&self) -> tilo_messaging::AccountState {
        self.linker.state()
    }
    pub fn wake_after(&self) -> Option<Duration> {
        self.linker.wake_after()
    }
    pub fn cancel(&self) {
        self.linker.cancel();
    }
}

/// Nonserializable, noncloneable evidence from one completed private attempt.
/// The original authority and generation remain live until acceptance. Merely
/// displaying the self identity cannot enable sends or create a credential.
pub struct VerifiedSignalEnrollment {
    linker: Arc<SignalLinking>,
    account: crate::signal_setup::SignalAccount,
}
impl VerifiedSignalEnrollment {
    pub fn owner(&self) -> &Owner {
        &self.linker.owner
    }
    pub fn account(&self) -> &crate::signal_setup::SignalAccount {
        &self.account
    }
    /// Consume once after the native host's confirmed identity review. The
    /// host still owns canonical account publication, protected credential
    /// custody, a fresh scoped grant and independently verified activation.
    /// # Errors
    /// Refuses cancelled/retired evidence or an already accepted generation.
    pub fn accept(self) -> Result<(Owner, crate::signal_setup::SignalAccount), SendError> {
        self.linker.reserve(FINISHED, ACCEPTED)?;
        self.linker.authority.check()?;
        if self.linker.phase.load(Ordering::Acquire) != ACCEPTED {
            return Err(SendError::Revoked);
        }
        Ok((self.linker.owner.clone(), self.account))
    }
}
impl SignalLinking {
    /// The native supervisor supplies a separate authenticated setup stream.
    /// Merely constructing this handle performs no protocol operation.
    /// # Errors
    /// Refuses malformed ownership, noncanonical ACI or unusable owned handles.
    pub fn new(
        owner: Owner,
        expected_aci: String,
        authority: Arc<dyn AccountAuthority>,
        stream: UnixStream,
    ) -> Result<Arc<Self>, SendError> {
        if uuid::Uuid::parse_str(&expected_aci)
            .map_err(|_| SendError::InvalidRequest)?
            .to_string()
            != expected_aci
        {
            return Err(SendError::InvalidRequest);
        }
        Self::for_target(owner, Target::Existing(expected_aci), authority, stream)
    }
    fn for_target(
        owner: Owner,
        target: Target,
        authority: Arc<dyn AccountAuthority>,
        stream: UnixStream,
    ) -> Result<Arc<Self>, SendError> {
        if owner.service != crate::Service::Signal
            || !owner.valid_principal()
            || !crate::identifier(&owner.device)
            || !crate::identifier(&owner.account)
            || owner.linking_generation == 0
        {
            return Err(SendError::InvalidRequest);
        }
        let stop = stream.try_clone().map_err(|_| SendError::Unavailable)?;
        let (reader, writer, deadline) = crate::signal_runtime::private_connection(stream)
            .map_err(|_| SendError::Unavailable)?;
        Ok(Arc::new(Self {
            owner,
            target,
            authority,
            phase: AtomicU8::new(FRESH),
            runtime: Mutex::new(Runtime {
                reader,
                writer,
                deadline,
            }),
            pending: Mutex::new(None),
            stop,
        }))
    }
    pub fn client(self: &Arc<Self>) -> tilo_messaging::SetupClient {
        tilo_messaging::SetupClient::new(
            self.owner.principal.clone(),
            tilo_messaging::ServiceAccount::new("signal", &self.owner.account),
            self.owner.linking_generation,
            Arc::new(Setup(self.clone())),
        )
    }
    pub fn owner(&self) -> &Owner {
        &self.owner
    }
    pub fn state(&self) -> tilo_messaging::AccountState {
        if self.authority.check().is_err() {
            self.cancel();
            return tilo_messaging::AccountState::Locked;
        }
        if self.phase.load(Ordering::Acquire) == WAITING
            && self.pending.lock().map_or(true, |pending| {
                pending
                    .as_ref()
                    .is_none_or(|pending| tilo_clock::now() >= pending.expires)
            })
        {
            self.cancel();
        }
        match self.phase.load(Ordering::Acquire) {
            STARTING | WAITING | FINISHING => tilo_messaging::AccountState::Linking,
            FINISHED | ACCEPTED => tilo_messaging::AccountState::Offline,
            INTERRUPTED => tilo_messaging::AccountState::NeedsRelink,
            _ => tilo_messaging::AccountState::Unavailable,
        }
    }
    fn wake_after(&self) -> Option<Duration> {
        if self.phase.load(Ordering::Acquire) != WAITING {
            return None;
        }
        Some(self.pending.lock().map_or(Duration::ZERO, |pending| {
            pending.as_ref().map_or(Duration::ZERO, |pending| {
                pending.expires.saturating_duration_since(tilo_clock::now())
            })
        }))
    }
    fn reserve(&self, from: u8, to: u8) -> Result<(), SendError> {
        self.authority.check()?;
        self.phase
            .compare_exchange(from, to, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| SendError::AlreadyReserved)
    }
    /// Begin only after the native setup door confirms the user operation.
    /// Cancellation interrupts only this owned stream, without replay.
    /// # Errors
    /// Refuses revoked authority, repeated setup or invalid private provider data.
    pub async fn begin(self: &Arc<Self>) -> Result<SignalPairing, SendError> {
        self.reserve(FRESH, STARTING)?;
        let mut guard = Interrupted::new(self.clone());
        let this = self.clone();
        let pairing = tokio::task::spawn_blocking(move || this.start())
            .await
            .map_err(|_| SendError::Unavailable)??;
        guard.commit(WAITING)?;
        Ok(pairing)
    }
    fn start(&self) -> Result<SignalPairing, SendError> {
        self.authority.check()?;
        let response: Reply<Started> = {
            let mut runtime = self.runtime.lock().map_err(|_| SendError::Unavailable)?;
            runtime.exchange("startLink", &serde_json::json!({}))?
        };
        self.authority.check()?;
        if self.phase.load(Ordering::Acquire) != STARTING {
            return Err(SendError::Revoked);
        }
        let uri = response
            .result
            .ok_or(SendError::Unavailable)?
            .device_link_uri;
        validate_uri(&uri)?;
        let expires = tilo_clock::now()
            .checked_add(LINK_LIFETIME)
            .ok_or(SendError::Unavailable)?;
        let attempt = uuid::Uuid::new_v4().to_string();
        let presentation = SignalPairing {
            attempt: attempt.clone(),
            uri: uri.clone(),
        };
        let mut pending = self.pending.lock().map_err(|_| SendError::Unavailable)?;
        if self.phase.load(Ordering::Acquire) != STARTING {
            return Err(SendError::Revoked);
        }
        *pending = Some(Pending {
            attempt,
            uri,
            expires,
        });
        drop(pending);
        Ok(presentation)
    }
    /// Finish exactly the displayed attempt and the native host's frozen ACI.
    /// A timeout or wrong account retires this generation; neither is replayed.
    /// # Errors
    /// Refuses stale attempts, expiry, revoked authority or another linked ACI.
    pub async fn finish(
        self: &Arc<Self>,
        attempt: String,
    ) -> Result<crate::signal_setup::SignalAccount, SendError> {
        self.authority.check()?;
        let matching = {
            let pending = self.pending.lock().map_err(|_| SendError::Unavailable)?;
            pending
                .as_ref()
                .is_some_and(|pending| pending.attempt == attempt)
        };
        if !matching {
            return Err(SendError::InvalidRequest);
        }
        self.reserve(WAITING, FINISHING)?;
        let mut guard = Interrupted::new(self.clone());
        let this = self.clone();
        let account = tokio::task::spawn_blocking(move || this.complete(&attempt))
            .await
            .map_err(|_| SendError::Unavailable)??;
        guard.commit(FINISHED)?;
        Ok(account)
    }
    fn complete(&self, attempt: &str) -> Result<crate::signal_setup::SignalAccount, SendError> {
        self.authority.check()?;
        let pending = self
            .pending
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .take()
            .ok_or(SendError::InvalidRequest)?;
        if pending.attempt != attempt || tilo_clock::now() >= pending.expires {
            return Err(SendError::InvalidRequest);
        }
        let params = FinishParams {
            device_link_uri: &pending.uri,
            device_name: "Tilo",
        };
        let mut runtime = self.runtime.lock().map_err(|_| SendError::Unavailable)?;
        // Pinned signal-cli finishLink returns only the linked number. The ACI
        // comes from the selected account's local own-identity query, not an
        // invented listAccounts ACI field or a public registration lookup.
        let reply: Reply<Completed> = runtime.exchange("finishLink", &params)?;
        self.authority.check()?;
        if self.phase.load(Ordering::Acquire) != FINISHING {
            return Err(SendError::Revoked);
        }
        let number = reply.result.ok_or(SendError::Unavailable)?.number;
        if !valid_number(&number) {
            return Err(SendError::InvalidRequest);
        }
        let accounts: Reply<serde_json::Value> =
            runtime.exchange("listAccounts", &serde_json::json!({}))?;
        drop(pending);
        self.authority.check()?;
        if self.phase.load(Ordering::Acquire) != FINISHING {
            return Err(SendError::Revoked);
        }
        let rows = accounts.result.ok_or(SendError::Unavailable)?;
        if matches!(self.target, Target::Enrollment)
            && rows.as_array().is_none_or(|rows| rows.len() != 1)
        {
            return Err(SendError::InvalidRequest);
        }
        if rows.as_array().is_none_or(|rows| {
            rows.iter()
                .filter(|row| {
                    row.get("number").and_then(serde_json::Value::as_str) == Some(&number)
                })
                .count()
                != 1
        }) {
            return Err(SendError::InvalidRequest);
        }
        self.authority.check()?;
        let identities: Reply<serde_json::Value> = runtime.exchange(
            "listIdentities",
            &serde_json::json!({"account":number,"number":number}),
        )?;
        drop(runtime);
        self.authority.check()?;
        if self.phase.load(Ordering::Acquire) != FINISHING {
            return Err(SendError::Revoked);
        }
        let account = crate::signal_setup::account(
            &rows,
            &identities.result.ok_or(SendError::Unavailable)?,
            &number,
        )
        .map_err(|_| SendError::InvalidRequest)?;
        if matches!(&self.target, Target::Existing(expected) if &account.aci != expected) {
            return Err(SendError::Revoked);
        }
        Ok(account)
    }

    /// Cancel only this account's separate provisioning stream. No logout,
    /// credential deletion or daemon-wide shutdown is performed.
    pub fn cancel(&self) {
        self.phase.store(INTERRUPTED, Ordering::Release);
        let _ = self.stop.shutdown(Shutdown::Both);
        if let Ok(mut pending) = self.pending.lock() {
            *pending = None;
        }
    }
}
impl Drop for SignalLinking {
    fn drop(&mut self) {
        self.cancel();
    }
}
struct Setup(Arc<SignalLinking>);
impl tilo_messaging::SetupHost for Setup {
    fn state(&self) -> tilo_messaging::AccountState {
        self.0.state()
    }
    fn wake_after(&self) -> Option<Duration> {
        self.0.wake_after()
    }
    fn begin(&self) -> tilo_messaging::PairingFuture {
        let source = self.0.clone();
        Box::pin(async move {
            let private = source.begin().await?;
            Ok(tilo_messaging::Pairing::new(
                tilo_messaging::ServiceAccount::new("signal", &source.owner.account),
                private.attempt.clone(),
                source.owner.linking_generation,
                Zeroizing::new(private.qr_bytes().to_vec()),
            ))
        })
    }
    fn finish(&self, attempt: String) -> tilo_messaging::LinkingFuture {
        let source = self.0.clone();
        Box::pin(async move {
            let evidence = source.finish(attempt).await?;
            Ok(tilo_messaging::LinkedAccount {
                account: tilo_messaging::ServiceAccount::new("signal", &source.owner.account),
                generation: source.owner.linking_generation,
                scope: evidence.scope(),
            })
        })
    }
    fn cancel(&self) {
        self.0.cancel();
    }
}
struct Interrupted {
    linker: Arc<SignalLinking>,
    committed: bool,
}
impl Interrupted {
    fn new(linker: Arc<SignalLinking>) -> Self {
        Self {
            linker,
            committed: false,
        }
    }
    fn commit(&mut self, phase: u8) -> Result<(), SendError> {
        self.linker.authority.check()?;
        let expected = if phase == WAITING {
            STARTING
        } else {
            FINISHING
        };
        self.linker
            .phase
            .compare_exchange(expected, phase, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SendError::Revoked)?;
        self.committed = true;
        Ok(())
    }
}
impl Drop for Interrupted {
    fn drop(&mut self) {
        if !self.committed {
            self.linker.cancel();
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FinishParams<'a> {
    device_link_uri: &'a str,
    device_name: &'a str,
}
#[derive(Deserialize)]
struct Reply<T> {
    jsonrpc: String,
    id: String,
    result: Option<T>,
    error: Option<serde::de::IgnoredAny>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Started {
    #[serde(deserialize_with = "private_string")]
    device_link_uri: Zeroizing<String>,
}
fn private_string<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Zeroizing<String>, D::Error> {
    String::deserialize(deserializer).map(Zeroizing::new)
}
#[derive(Deserialize)]
struct Completed {
    number: String,
}
#[derive(serde::Serialize)]
struct Request<'a, P> {
    jsonrpc: &'a str,
    id: &'a str,
    method: &'a str,
    params: &'a P,
}

impl Runtime {
    fn exchange<T: serde::de::DeserializeOwned>(
        &mut self,
        method: &str,
        params: &impl serde::Serialize,
    ) -> Result<Reply<T>, SendError> {
        self.deadline
            .arm(crate::signal_runtime::MAX_OPERATION_TIMEOUT)
            .map_err(|_| SendError::Unavailable)?;
        let id = format!("tilo-link-{}", uuid::Uuid::new_v4());
        let request = Request {
            jsonrpc: "2.0",
            id: &id,
            method,
            params,
        };
        let mut bytes =
            Zeroizing::new(serde_json::to_vec(&request).map_err(|_| SendError::InvalidRequest)?);
        bytes.push(b'\n');
        self.writer
            .write_all(&bytes)
            .and_then(|()| self.writer.flush())
            .map_err(|_| SendError::Unavailable)?;
        let frame = crate::signal_session::read_frame(&mut self.reader)
            .map_err(|_| SendError::Unavailable)?;
        let response: Reply<T> =
            serde_json::from_slice(&frame).map_err(|_| SendError::Unavailable)?;
        if response.jsonrpc != "2.0"
            || response.id != id
            || response.result.is_none()
            || response.error.is_some()
        {
            return Err(SendError::Unavailable);
        }
        Ok(response)
    }
}
fn valid_number(number: &str) -> bool {
    number.strip_prefix('+').is_some_and(|digits| {
        !digits.is_empty()
            && digits.len() <= 15
            && !digits.starts_with('0')
            && digits.bytes().all(|b| b.is_ascii_digit())
    })
}
fn validate_uri(uri: &str) -> Result<(), SendError> {
    let query = uri
        .strip_prefix("sgnl://linkdevice?")
        .filter(|_| {
            uri.len() <= 4096
                && !uri.chars().any(|c| c.is_whitespace() || c.is_control())
                && !uri.contains('#')
        })
        .ok_or(SendError::InvalidRequest)?;
    let mut identity = None;
    let mut key = None;
    for field in query.split('&') {
        match field.split_once('=') {
            Some(("uuid", value)) if identity.is_none() => {
                identity =
                    Some(uuid::Uuid::parse_str(value).map_err(|_| SendError::InvalidRequest)?);
            }
            Some(("pub_key", value))
                if key.is_none() && !value.is_empty() && value.len() <= 256 =>
            {
                key = Some(value);
            }
            _ => return Err(SendError::InvalidRequest),
        }
    }
    if identity.is_none() || key.is_none() {
        return Err(SendError::InvalidRequest);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
