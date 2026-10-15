//! Runs the sibling side of disk unlock in the person's session.
//!
//! * Early boot of another device reaches [`serve_connection`] over
//!   [`tilo_unlock_protocol::UNLOCK_ALPN`] on the shell's peer endpoint.
//! * A request waits for the owner's `device.unlock` decision, offered as a
//!   consent in the device-fabric pane and the palette
//!   ([`pending_device_unlocks`], [`decide_device_unlock`]).
//! * `tilo-disk-encrypt enrol --sibling` reaches [`enrol`] through the
//!   session's enrolment socket ([`tilo_unlock_protocol::ENROL_SOCKET`]).
//! * The fabric tick ([`refresh`]) publishes this device's enrolments, merges
//!   everyone else's, drops a removed device's halves, and writes every
//!   request, approval, denial, refusal and removal to the owner's history.
//!
//! The release needs the person's root on this device: the half is sealed to
//! the root-derived secret-sync key, which only the identity derives.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Write as _};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tilo_seal::SecretKey;
use tilo_unlock_protocol::{
    BootKeyCert, CHALLENGE_LIFETIME_NS, ClientFrame, EnrolReply, EnrolRequest, EnrolResponse,
    PROTOCOL_VERSION, SiblingContact, SiblingFrame, UnlockHello, decode_frame, encode_frame,
    frame_len, seal_sibling_half,
};
use tilo_user_identity::{CertifiedFleet, DeviceStanding};
use tokio::sync::oneshot;

use crate::shell::device_unlock::{
    AutoApproval, PendingUnlock, UnlockDecision, UnlockEnrolment, UnlockEvent, UnlockOutcome,
    UnlockSibling, UnlockView, merged_unlock_state,
};
use crate::shell::network::ShellNetwork;
use crate::shell::personal_continuity::DeviceId;

/// How long a boot may take to answer its challenge.
const ANSWER_TIMEOUT: Duration = Duration::from_nanos(CHALLENGE_LIFETIME_NS);
/// How often the fleet and root custody are read again from disk.
const CUSTODY_REFRESH: Duration = Duration::from_secs(30);

/// A request waiting for the owner, as the consent prompt shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceUnlockConsentView {
    pub request_digest: [u8; 32],
    pub approval_code: String,
    pub device_label: String,
    pub device_public_key: [u8; 32],
}

/// The identity this session unlocks for, and what it holds.
#[derive(Clone)]
struct Custody {
    identity_id: String,
    fleet: CertifiedFleet,
    custodian: bool,
    read_at: SystemTime,
}

#[derive(Default)]
struct Service {
    local_device: Option<[u8; 32]>,
    custody: Option<Custody>,
    sibling: Option<UnlockSibling>,
    enrolments: BTreeMap<[u8; 32], UnlockEnrolment>,
    auto_approvals: BTreeMap<[u8; 32], AutoApproval>,
    labels: BTreeMap<[u8; 32], String>,
    /// Removed devices already written to the history.
    removed: BTreeSet<[u8; 32]>,
    /// Boots waiting on the owner, by request digest.
    waiters: BTreeMap<[u8; 32], oneshot::Sender<SiblingFrame>>,
    /// Enrolments made here, not yet in this device's snapshot.
    outbox: Vec<UnlockEnrolment>,
    /// Owner settings made here, not yet in this device's snapshot.
    settings_outbox: Vec<AutoApproval>,
}

impl Service {
    fn view<'a>(&'a self, secret_sync: Option<&'a SecretKey>) -> Option<UnlockView<'a>> {
        let custody = self.custody.as_ref()?;
        Some(UnlockView {
            fleet: &custody.fleet,
            enrolments: &self.enrolments,
            auto_approvals: &self.auto_approvals,
            custodian: custody.custodian,
            secret_sync,
        })
    }

    fn drain_history(&mut self) {
        let Some(sibling) = self.sibling.as_mut() else {
            return;
        };
        for event in sibling.drain_history() {
            record_history(&event, self.labels.get(&event.device_public_key));
        }
    }
}

impl Service {
    /// Merge the fabric's enrolments and settings under the current fleet.
    /// Returns the removed devices whose snapshots still carry halves.
    fn merge_fabric(
        &mut self,
        fabric: &crate::shell::device_fabric::DeviceFabric,
        now: u64,
    ) -> Vec<DeviceId> {
        let Some(custody) = self.custody.as_ref() else {
            return Vec::new();
        };
        let (enrolments, approvals) = merged_unlock_state(
            fabric.snapshots().values().map(|snapshot| &snapshot.unlock),
            &custody.fleet,
            now,
        );
        let mut removed = Vec::new();
        let mut newly_removed = Vec::new();
        for (owner, snapshot) in fabric.snapshots() {
            let Some(key) = key_of(owner) else {
                continue;
            };
            self.labels.insert(key, snapshot.presence.label.clone());
            if !snapshot.unlock.enrolments.is_empty()
                && matches!(
                    custody.fleet.standing(&key, now),
                    DeviceStanding::Revoked(_)
                )
            {
                removed.push(owner.clone());
                if !self.removed.contains(&key) {
                    newly_removed.push(key);
                }
            }
        }
        self.enrolments = enrolments;
        self.auto_approvals = approvals;
        for key in newly_removed {
            self.removed.insert(key);
            if let Some(sibling) = self.sibling.as_mut() {
                sibling.remove_device(key, now);
            }
        }
        // Boots of removed devices no longer have anything to wait for.
        let pending: BTreeSet<[u8; 32]> = self
            .sibling
            .as_ref()
            .map(|sibling| sibling.pending().map(|p| p.request_digest).collect())
            .unwrap_or_default();
        let gone: Vec<[u8; 32]> = self
            .waiters
            .keys()
            .filter(|digest| !pending.contains(*digest))
            .copied()
            .collect();
        for digest in gone {
            if let Some(waiter) = self.waiters.remove(&digest) {
                let _ = waiter.send(SiblingFrame::Refused(
                    tilo_unlock_protocol::RefusalReason::Revoked,
                ));
            }
        }
        removed
    }

    /// Deny every request nobody decided in time.
    fn expire(&mut self, now: u64) {
        let Some(sibling) = self.sibling.as_mut() else {
            return;
        };
        for digest in sibling.expire(now) {
            if let Some(waiter) = self.waiters.remove(&digest) {
                let _ = waiter.send(SiblingFrame::Denied);
            }
        }
    }
}

fn service() -> &'static Mutex<Service> {
    static SERVICE: OnceLock<Mutex<Service>> = OnceLock::new();
    SERVICE.get_or_init(Mutex::default)
}

fn with_service<R>(f: impl FnOnce(&mut Service) -> R) -> Option<R> {
    let mut guard = service().lock().ok()?;
    let result = f(&mut guard);
    drop(guard);
    Some(result)
}

fn key_of(device: &DeviceId) -> Option<[u8; 32]> {
    let mut key = [0_u8; 32];
    hex::decode_to_slice(device.as_str(), &mut key).ok()?;
    Some(key)
}

/// Write one event to the owner's activity history: the session journal
/// under a stable target, which the history views and the device's own
/// tooling read.
fn record_history(event: &UnlockEvent, label: Option<&String>) {
    let device = hex::encode(event.device_public_key);
    let label = label.map_or("", String::as_str);
    let slot = event.slot.map_or_else(String::new, |slot| {
        format!("{}:{}", hex::encode(slot.volume_uuid), slot.keyslot)
    });
    let boot_id = event
        .boot_id
        .map_or_else(String::new, |boot| hex::encode(boot.0));
    let (outcome, reason, by) = match &event.outcome {
        UnlockOutcome::Requested { approval_code } => ("requested", approval_code.clone(), None),
        UnlockOutcome::Approved { approver } => ("approved", String::new(), Some(*approver)),
        UnlockOutcome::AutoApproved { boot_chain } => {
            ("auto_approved", hex::encode(boot_chain), None)
        }
        UnlockOutcome::Denied { approver } => ("denied", String::new(), *approver),
        UnlockOutcome::Refused { reason } => ("refused", format!("{reason:?}"), None),
        UnlockOutcome::Removed => ("removed", String::new(), None),
    };
    let approver = by.map_or_else(String::new, hex::encode);
    tracing::info!(
        target: "tilo_shell::device_unlock",
        device = %device,
        label = %label,
        slot = %slot,
        boot_id = %boot_id,
        outcome = %outcome,
        reason = %reason,
        approver = %approver,
        at_ns = event.at_ns,
        "device unlock: outcome={outcome} reason={reason} device={device} slot={slot}"
    );
}

/// The person's identity this session holds, by the fabric's user key.
fn identity_for(user_public_key: [u8; 32]) -> Option<String> {
    let store = crate::identity_registry::IdentityRegistryStore::from_env().ok()?;
    let registry = store.load_reconciled_cached().ok()?;
    registry
        .identities
        .values()
        .find(|profile| profile.user_public_key == user_public_key)
        .map(|profile| profile.id.clone())
}

fn read_custody(user_public_key: [u8; 32]) -> Option<Custody> {
    let identity_id = identity_for(user_public_key)?;
    let store = crate::device_fleet::DeviceFleetStore::from_env().ok()?;
    let fleet = store.certified_fleet(&identity_id, user_public_key).ok()?;
    let custodian = store.secret_sync_key(&identity_id).is_ok();
    Some(Custody {
        identity_id,
        fleet,
        custodian,
        read_at: SystemTime::now(),
    })
}

fn secret_sync_key() -> Option<SecretKey> {
    let identity_id = with_service(|service| {
        service
            .custody
            .as_ref()
            .map(|custody| custody.identity_id.clone())
    })??;
    crate::device_fleet::DeviceFleetStore::from_env()
        .ok()?
        .secret_sync_key(&identity_id)
        .ok()
}

/// Whether a custody read is running on its worker.
static CUSTODY_READING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Read custody with `read` on a thread of its own and publish it to the
/// service when it returns. The read lists the identity directory, parses the
/// fleet certificate and probes the secret store: disk reads that, on a busy
/// disk, held the compositor loop that ticks the fabric for hundreds of
/// milliseconds. One read runs at a time; a tick that finds one running
/// leaves it be and uses the custody already published.
fn refresh_custody_in_background(read: impl FnOnce() -> Option<Custody> + Send + 'static) {
    use std::sync::atomic::Ordering;
    if CUSTODY_READING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("unlock-custody".to_owned())
        .spawn(move || {
            if let Some(custody) = read() {
                let _ = with_service(|service| service.custody = Some(custody));
            }
            CUSTODY_READING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        CUSTODY_READING.store(false, Ordering::Release);
    }
}

/// One fabric tick: refresh custody, merge the fabric's enrolments, publish
/// this device's own, drop removed devices' halves, expire undecided
/// requests and write the history.
pub fn refresh(network: &ShellNetwork) {
    let Some((device, _)) = network.local_device_fabric_identity() else {
        return;
    };
    let Some(local) = key_of(&device) else {
        return;
    };
    let Some(user) = network.personal_user_public_key() else {
        return;
    };
    let now = tilo_clock::unix_now_ns();
    let stale = with_service(|service| {
        service.custody.as_ref().is_none_or(|custody| {
            custody
                .read_at
                .elapsed()
                .map_or(true, |age| age >= CUSTODY_REFRESH)
        })
    })
    .unwrap_or(true);
    if stale {
        refresh_custody_in_background(move || read_custody(user));
    }

    let fabric = network.device_fabric();
    let Some((removed, outbox)) = with_service(|service| {
        if service.local_device != Some(local) {
            service.local_device = Some(local);
            service.sibling = Some(UnlockSibling::new(local));
        }
        let removed = service.merge_fabric(&fabric, now);
        service.expire(now);
        service.drain_history();
        let outbox = (
            std::mem::take(&mut service.outbox),
            std::mem::take(&mut service.settings_outbox),
        );
        (removed, outbox)
    }) else {
        return;
    };
    // A removed device's halves are deleted from this device's fabric, so
    // they neither stay on disk nor replicate further.
    for owner in &removed {
        network.forget_device_fabric(owner);
    }
    let (enrolments, settings) = outbox;
    if !(enrolments.is_empty() && settings.is_empty())
        && let Err(error) = network.update_local_unlock_state(|state, stamp| {
            for mut enrolment in enrolments {
                enrolment.stamp = stamp.clone();
                state
                    .enrolments
                    .retain(|current| current.cert.slot != enrolment.cert.slot);
                state.enrolments.push(enrolment);
            }
            for setting in settings {
                state
                    .auto_approvals
                    .retain(|current| current.device_public_key != setting.device_public_key);
                state.auto_approvals.push(setting);
            }
        })
    {
        tracing::warn!(%error, "could not publish disk-unlock enrolments");
    }
}

/// Requests waiting for the owner's `device.unlock` decision on this device.
#[must_use]
pub fn pending_device_unlocks() -> Vec<DeviceUnlockConsentView> {
    with_service(|service| {
        let Some(sibling) = service.sibling.as_ref() else {
            return Vec::new();
        };
        sibling
            .pending()
            .filter(|pending| service.waiters.contains_key(&pending.request_digest))
            .map(|pending: &PendingUnlock| DeviceUnlockConsentView {
                request_digest: pending.request_digest,
                approval_code: pending.approval_code.clone(),
                device_label: service
                    .labels
                    .get(&pending.device_public_key)
                    .cloned()
                    .unwrap_or_default(),
                device_public_key: pending.device_public_key,
            })
            .collect()
    })
    .unwrap_or_default()
}

/// Apply the owner's `device.unlock` decision on this device.
///
/// # Errors
///
/// Returns an error when no such request is waiting.
pub fn decide_device_unlock(request_digest: [u8; 32], approved: bool) -> Result<(), String> {
    let secret_sync = if approved { secret_sync_key() } else { None };
    let decided = with_service(|service| {
        let local = service.local_device?;
        let decision = if approved {
            UnlockDecision::Approve { approver: local }
        } else {
            UnlockDecision::Deny { approver: local }
        };
        let now = tilo_clock::unix_now_ns();
        let mut sibling = service.sibling.take()?;
        let frame = service
            .view(secret_sync.as_ref())
            .and_then(|view| sibling.decide(&request_digest, decision, &view, now));
        service.sibling = Some(sibling);
        service.drain_history();
        let frame = frame?;
        if let Some(waiter) = service.waiters.remove(&request_digest) {
            let _ = waiter.send(frame);
        }
        Some(())
    })
    .flatten();
    decided.ok_or_else(|| crate::shell::i18n::t("device-unlock-error-not-pending"))
}

/// Remove `device` from the person's fleet: the root signs its revocation,
/// which also revokes its boot keys; the next tick deletes its halves.
///
/// # Errors
///
/// Returns an error when this device cannot sign for the identity or the
/// device is not in the fleet.
pub fn remove_device(device: [u8; 32]) -> Result<(), String> {
    let identity_id = with_service(|service| {
        if service.local_device == Some(device) {
            return None;
        }
        service
            .custody
            .as_ref()
            .map(|custody| custody.identity_id.clone())
    })
    .flatten()
    .ok_or_else(|| crate::shell::i18n::t("device-unlock-error-cannot-remove"))?;
    crate::device_fleet::DeviceFleetStore::from_env()
        .and_then(|store| {
            store.revoke(
                &identity_id,
                device,
                tilo_user_identity::RevocationReason::Retired,
                tilo_clock::unix_now_ns(),
            )
        })
        .map_err(|error| error.to_string())?;
    // Read the fleet again at the next tick.
    with_service(|service| {
        if let Some(custody) = service.custody.as_mut() {
            custody.read_at = UNIX_EPOCH;
        }
    });
    Ok(())
}

/// Certify the boot key of `request` for this device, seal its sibling half
/// to the person's secret-sync key, and queue the enrolment for this
/// device's fabric snapshot.
#[must_use]
pub fn enrol(request: &EnrolRequest) -> EnrolReply {
    match try_enrol(request) {
        Ok(response) => EnrolReply::Enrolled(Box::new(response)),
        Err(reason) => EnrolReply::Refused(reason),
    }
}

fn try_enrol(request: &EnrolRequest) -> Result<EnrolResponse, String> {
    request
        .verify()
        .map_err(|error| format!("enrolment request: {error}"))?;
    let (local, custody, labels) = with_service(|service| {
        Some((
            service.local_device?,
            service.custody.clone()?,
            service.labels.clone(),
        ))
    })
    .flatten()
    .ok_or_else(|| "this session holds no identity for the person's devices".to_string())?;
    let store = crate::device_fleet::DeviceFleetStore::from_env().map_err(|e| e.to_string())?;
    let mut cert = BootKeyCert {
        version: PROTOCOL_VERSION,
        user_public_key: *custody.fleet.user_public_key(),
        device_public_key: local,
        boot_public_key: request.boot_public_key,
        slot: request.slot,
        attestation_key: request.attestation_key.clone(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        signature: [0; 64],
    };
    store
        .certify_boot_key(&custody.identity_id, &mut cert)
        .map_err(|error| error.to_string())?;
    let secret_sync = store
        .secret_sync_key(&custody.identity_id)
        .and_then(|key| key.public_key().map_err(std::io::Error::other))
        .map_err(|error| error.to_string())?;
    let sealed_half = seal_sibling_half(&secret_sync, &cert, &request.sibling_half)
        .map_err(|error| error.to_string())?;
    let now = tilo_clock::unix_now_ns();
    let siblings = labels
        .iter()
        .filter(|(key, _)| {
            **key != local && custody.fleet.standing(key, now) == DeviceStanding::Certified
        })
        .map(|(key, label)| SiblingContact {
            device_public_key: *key,
            label: label.clone(),
            endpoint: String::new(),
        })
        .collect();
    let stamp = crate::shell::device_fabric::FabricStamp {
        counter: 0,
        device: DeviceId::new(hex::encode(local)).map_err(|error| error.to_string())?,
    };
    with_service(|service| {
        service.outbox.push(UnlockEnrolment {
            cert: cert.clone(),
            sealed_half,
            stamp,
        });
    });
    tracing::info!(
        target: "tilo_shell::device_unlock",
        device = %hex::encode(local),
        slot = %format!("{}:{}", hex::encode(cert.slot.volume_uuid), cert.slot.keyslot),
        outcome = "enrolled",
        "device unlock: outcome=enrolled device={}",
        hex::encode(local)
    );
    Ok(EnrolResponse { cert, siblings })
}

/// Serve the enrolment socket for this session, one request per connection.
pub fn spawn_enrol_socket() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if cfg!(test) || STARTED.set(()).is_err() {
        return;
    }
    let Some(runtime) = tilo_paths::xdg::runtime_dir() else {
        return;
    };
    let path = runtime.join(tilo_unlock_protocol::ENROL_SOCKET);
    let listener = match crate::shell::test_control_socket::bind_private(&path) {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, "disk-unlock enrolment socket unavailable");
            return;
        }
    };
    let spawned = std::thread::Builder::new()
        .name("device-unlock-enrol".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                answer_enrolment(stream);
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not start the disk-unlock enrolment socket");
    }
}

fn answer_enrolment(mut stream: std::os::unix::net::UnixStream) {
    let mut prefix = [0_u8; 4];
    let reply = if stream.read_exact(&mut prefix).is_err() {
        return;
    } else if let Ok(len) = frame_len(prefix) {
        let mut body = zeroize::Zeroizing::new(vec![0_u8; len]);
        if stream.read_exact(&mut body).is_err() {
            return;
        }
        decode_frame::<EnrolRequest>(&body).map_or_else(
            |_| EnrolReply::Refused("malformed enrolment request".into()),
            |request| enrol(&request),
        )
    } else {
        EnrolReply::Refused("enrolment request too large".into())
    };
    if let Ok(frame) = encode_frame(&reply) {
        let _ = stream.write_all(&frame);
    }
}

async fn read_frame(recv: &mut iroh::endpoint::RecvStream) -> Option<ClientFrame> {
    let mut prefix = [0_u8; 4];
    recv.read_exact(&mut prefix).await.ok()?;
    let mut body = vec![0_u8; frame_len(prefix).ok()?];
    recv.read_exact(&mut body).await.ok()?;
    decode_frame(&body).ok()
}

async fn write_frame(send: &mut iroh::endpoint::SendStream, frame: &SiblingFrame) -> bool {
    match encode_frame(frame) {
        Ok(bytes) => send.write_all(&bytes).await.is_ok(),
        Err(_) => false,
    }
}

/// Answer one early-boot connection on `UNLOCK_ALPN`.
pub async fn serve_connection(connection: iroh::endpoint::Connection) {
    let Ok((mut send, mut recv)) = connection.accept_bi().await else {
        return;
    };
    let Ok(Some(ClientFrame::Hello(hello))) =
        tokio::time::timeout(ANSWER_TIMEOUT, read_frame(&mut recv)).await
    else {
        return;
    };
    let mut nonce = [0_u8; 32];
    if tilo_seal::fill_random(&mut nonce).is_err() {
        return;
    }
    let Some(first) = answer_hello(&hello, nonce) else {
        return;
    };
    let challenged = matches!(first, SiblingFrame::Challenge(_));
    if !write_frame(&mut send, &first).await || !challenged {
        let _ = send.finish();
        return;
    }
    let Ok(Some(ClientFrame::Request(request))) =
        tokio::time::timeout(ANSWER_TIMEOUT, read_frame(&mut recv)).await
    else {
        return;
    };
    let secret_sync = tokio::task::spawn_blocking(secret_sync_key)
        .await
        .ok()
        .flatten();
    let Some((frame, waiter)) = with_service(|service| {
        let mut sibling = service.sibling.take()?;
        let now = tilo_clock::unix_now_ns();
        let frame = service
            .view(secret_sync.as_ref())
            .map(|view| sibling.request(&request, &hello, &nonce, &view, now));
        service.sibling = Some(sibling);
        service.drain_history();
        let frame = frame?;
        let waiter = if let SiblingFrame::Pending { .. } = frame {
            let digest = request
                .digest(&hello.boot_cert, &service.local_device?)
                .ok()?;
            let (tx, rx) = oneshot::channel();
            service.waiters.insert(digest, tx);
            Some((digest, rx))
        } else {
            None
        };
        Some((frame, waiter))
    })
    .flatten() else {
        return;
    };
    drop(secret_sync);
    if !write_frame(&mut send, &frame).await {
        return;
    }
    if let Some((digest, rx)) = waiter {
        tokio::select! {
            decided = rx => {
                if let Ok(frame) = decided {
                    let _ = write_frame(&mut send, &frame).await;
                }
            }
            _ = connection.closed() => {}
        }
        // Whether decided, expired or abandoned, nothing waits any more.
        with_service(|service| {
            service.waiters.remove(&digest);
            if let Some(sibling) = service.sibling.as_mut() {
                sibling.withdraw(&digest);
            }
        });
    }
    let _ = send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
}

fn answer_hello(hello: &UnlockHello, nonce: [u8; 32]) -> Option<SiblingFrame> {
    with_service(|service| {
        let mut sibling = service.sibling.take()?;
        let frame = service
            .view(None)
            .map(|view| sibling.hello(hello, &view, nonce, tilo_clock::unix_now_ns()));
        service.sibling = Some(sibling);
        service.drain_history();
        frame
    })
    .flatten()
}

#[cfg(test)]
mod custody_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};

    #[test]
    fn a_fabric_tick_never_waits_for_the_custody_read() {
        // The reader stands for a disk that does not answer.
        let (release, gate) = mpsc::channel::<()>();
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&reads);
        refresh_custody_in_background(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = gate.recv();
            None
        });
        // The tick came back with the read still blocked, and the ticks after
        // it start no second one.
        let again = Arc::clone(&reads);
        refresh_custody_in_background(move || {
            again.fetch_add(100, Ordering::SeqCst);
            None
        });
        assert!(CUSTODY_READING.load(Ordering::Acquire));
        drop(release);
        tilo_test_env::wait_until("the custody read to finish", || {
            !CUSTODY_READING.load(Ordering::Acquire)
        });
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }
}
