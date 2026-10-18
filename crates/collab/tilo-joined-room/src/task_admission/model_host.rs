//! Physical-host MODEL enrollment, separate from per-request requirements.
//! Enrollment is signed from a host's live broker/lease authority. A requirement
//! cannot enroll a connection, choose its profile, or reactivate a revoked run.
use super::model::{
    NativeModelAuthority, NativeModelContext, NativeModelPlanning, TypedNativeModelRequirement,
};
use super::{NativeGovernorAdmission, NativeInvocationBinding};
use crate::{NativeWorkspaceOwner, RoomId};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tilo_user_identity::DeviceCert;

const REGISTRATION_LIFETIME: Duration = Duration::from_secs(120);

fn timestamp(now: SystemTime) -> Result<u64, String> {
    now.duration_since(UNIX_EPOCH)
        .map_err(|_| "native MODEL clock predates epoch".to_string())?
        .as_nanos()
        .try_into()
        .map_err(|_| "native MODEL clock exceeds range".to_string())
}

fn certified_host(caller: &DeviceCert, now: SystemTime) -> Result<(), String> {
    tilo_user_identity::verify_device_cert_at(caller, timestamp(now)?)
        .map_err(|_| "native MODEL host certificate invalid".to_string())
}

/// Opaque host process connection and delivered lease identities. Generate new
/// UUIDs when either is replaced; never derive these from a request or an actor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeModelConnection {
    pub connection: uuid::Uuid,
    pub lease: uuid::Uuid,
}

/// Host-signed enrollment. Private fields prevent constructing it from a MODEL
/// requirement in the host API; the wire decoder alone is not authorization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeModelHostRegistration {
    pub(crate) id: uuid::Uuid,
    pub(crate) room: RoomId,
    pub(crate) host: [u8; 32],
    connection: NativeModelConnection,
    pub(crate) context: NativeModelContext,
    issued_at_ns: u64,
    expires_at_ns: u64,
    signature: [[u8; 32]; 2],
    // Governor-owned durable tombstone, not part of the host-signed statement.
    // Keep the original enrollment so Stop still includes this physical host.
    #[serde(default)]
    revoked: bool,
}

impl NativeModelHostRegistration {
    /// Capture actual broker/lease/window/profile custody, not app JSON. Hold the
    /// host's connection/window guards around this call and submission. The same
    /// authority must be refreshed at the final HTTP enqueue. Keep this nonce
    /// for the live connection/run, not one MODEL call. It expires after 120
    /// seconds; renew before `expires_at` with unchanged connection/lease/context.
    /// Closing it revokes only that connection's future admission, not the task.
    /// # Errors
    /// Missing live MODEL lease, wrong physical host key or incomplete custody.
    pub fn capture(
        owner: &NativeWorkspaceOwner,
        room: RoomId,
        connection: NativeModelConnection,
        authority: &impl NativeModelAuthority,
        key: &SigningKey,
    ) -> Result<Self, String> {
        owner.with_current(room, |view| {
            let now = SystemTime::now();
            super::host_evidence(view, view.device(), now)?;
            certified_host(view.device(), now)?;
            if view
                .peer()
                .permissions(view.device().device_public_key, now)?
                .delegate
                .is_some()
            {
                return Err("native MODEL host cannot be a delegated agent".into());
            }
            let issued_at_ns = timestamp(now)?;
            let expires_at_ns = issued_at_ns
                .checked_add(REGISTRATION_LIFETIME.as_nanos() as u64)
                .ok_or("native MODEL enrollment expiry overflow")?;
            if key.verifying_key().to_bytes() != view.device().device_public_key {
                return Err(
                    "native MODEL enrollment requires physical host signing custody".into(),
                );
            }
            let context = authority.current_model_context(view, now)?;
            let mut registration = Self {
                id: uuid::Uuid::new_v4(),
                room,
                host: view.device().device_public_key,
                connection,
                context,
                issued_at_ns,
                expires_at_ns,
                signature: [[0; 32]; 2],
                revoked: false,
            };
            registration.validate()?;
            let signature = super::sign("tilo-native-model-host-v1", &registration.payload(), key)?;
            registration.signature = signature;
            Ok(registration)
        })
    }

    #[must_use]
    pub fn id(&self) -> uuid::Uuid {
        self.id
    }

    /// Renew with a fresh signed nonce while this enrollment is still live.
    /// The governor retires the previous nonce for the same connection/lease.
    #[must_use]
    pub fn expires_at(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_nanos(self.expires_at_ns)
    }

    #[must_use]
    pub fn context(&self) -> &NativeModelContext {
        &self.context
    }

    fn payload(&self) -> impl Serialize + '_ {
        (
            self.id,
            self.room,
            self.host,
            &self.connection,
            &self.context,
            self.issued_at_ns,
            self.expires_at_ns,
        )
    }

    fn validate(&self) -> Result<(), String> {
        if self.id.is_nil()
            || self.connection.connection.is_nil()
            || self.connection.lease.is_nil()
            || self.context.task.is_empty()
            || self.context.actor.is_empty()
            || self.context.epoch == 0
            || self.context.private_run_digest == [0; 32]
            || self.context.planning != NativeModelPlanning::Model
            || self.expires_at_ns.checked_sub(self.issued_at_ns)
                != Some(REGISTRATION_LIFETIME.as_nanos() as u64)
        {
            return Err("native MODEL enrollment incomplete".into());
        }
        Ok(())
    }

    fn check_live(&self, now: SystemTime) -> Result<(), String> {
        let now = timestamp(now)?;
        if self.revoked || now < self.issued_at_ns || now >= self.expires_at_ns {
            return Err("native MODEL enrollment revoked, expired or not yet valid".into());
        }
        Ok(())
    }

    pub(super) fn verify(&self) -> Result<(), String> {
        self.validate()?;
        super::verify(
            "tilo-native-model-host-v1",
            &self.payload(),
            self.host,
            self.signature,
        )
    }
}

impl NativeGovernorAdmission<'_> {
    /// Enroll only an authenticated physical host's separate signed statement.
    /// Caller must independently validate the private run digest and persist this
    /// ledger under the same room writer lock before returning success.
    /// # Errors
    /// Forged host, stopped run, stale frontier, replacement or capacity overflow.
    pub fn register_model_host(
        &mut self,
        registration: &NativeModelHostRegistration,
        caller: &DeviceCert,
        now: SystemTime,
    ) -> Result<(), String> {
        self.custody()?;
        self.host(caller, now)?;
        registration.verify()?;
        registration.check_live(now)?;
        registration.check_live(SystemTime::now())?;
        certified_host(caller, now)?;
        if self
            .peer
            .permissions(caller.device_public_key, now)?
            .delegate
            .is_some()
        {
            return Err("native MODEL host cannot be a delegated agent".into());
        }
        let context = &registration.context;
        if registration.host != caller.device_public_key
            || registration.room != self.room
            || self.ledger.is_fenced(&context.task, context.epoch)
        {
            return Err("native MODEL enrollment host, room or live run mismatch".into());
        }
        let state = tilo_peer_state::workspace_task::snapshot(
            self.peer
                .admitted_room()
                .ok_or("native MODEL room absent")?,
        )?;
        let control = state
            .controls
            .get(&context.task)
            .ok_or("native MODEL control absent")?;
        let run = control
            .active_run
            .as_ref()
            .ok_or("native MODEL run absent")?;
        if run.agent_id != context.actor
            || run.epoch != context.epoch
            || control.run_epoch != context.epoch
            || control
                .directions
                .last()
                .map_or(0, |direction| direction.revision)
                != context.revision
        {
            return Err("native MODEL enrollment frontier changed".into());
        }
        self.peer.workspace().check_task_execution_actor(
            &context.actor,
            run.approval.as_ref(),
            now,
        )?;
        if let Some(prior) = self.ledger.model_hosts.get(&registration.id) {
            prior.check_live(now)?;
            return if prior == registration {
                Ok(())
            } else {
                Err("native MODEL enrollment changed".into())
            };
        }
        // A freshly signed capture may renew an unchanged, non-revoked live
        // host connection even after its previous deadline. The new capture
        // attests current protected custody; replay of an old nonce cannot do
        // this. Keep tombstones across governor/process restarts.
        let matching: Vec<_> = self
            .ledger
            .model_hosts
            .values()
            .filter(|prior| {
                prior.host == registration.host && prior.connection == registration.connection
            })
            .collect();
        let predecessor = if matching.is_empty() {
            None
        } else {
            let prior = matching
                .iter()
                .find(|prior| !prior.revoked)
                .ok_or("native MODEL connection revoked")?;
            if registration.issued_at_ns <= prior.issued_at_ns {
                return Err("native MODEL renewal capture is not newer".into());
            }
            if prior.context != registration.context {
                return Err("native MODEL renewal context changed".into());
            }
            Some(prior.id)
        };
        if self.ledger.model_hosts.len() >= super::MAX_RESERVATIONS {
            return Err("native MODEL enrollment capacity exceeded".into());
        }
        if let Some(id) = predecessor {
            self.ledger
                .model_hosts
                .get_mut(&id)
                .ok_or("native MODEL renewal predecessor absent")?
                .revoked = true;
        }
        self.ledger
            .model_hosts
            .insert(registration.id, registration.clone());
        Ok(())
    }

    /// Close only this MODEL connection's admission. This is not task Stop and
    /// does not claim to cancel an HTTP transfer already dispatched. Retain the
    /// enrollment tombstone durably: replay cannot reopen it after a restart,
    /// and task Stop must still fence hosts with in-flight effects.
    /// # Errors
    /// Foreign host or unknown enrollment.
    pub fn revoke_model_host(
        &mut self,
        registration: uuid::Uuid,
        caller: &DeviceCert,
        now: SystemTime,
    ) -> Result<(), String> {
        self.custody()?;
        self.host(caller, now)?;
        certified_host(caller, now)?;
        if self
            .peer
            .permissions(caller.device_public_key, now)?
            .delegate
            .is_some()
        {
            return Err("native MODEL host cannot be a delegated agent".into());
        }
        let registered = self
            .ledger
            .model_hosts
            .get_mut(&registration)
            .ok_or("native MODEL enrollment absent")?;
        if registered.host != caller.device_public_key {
            return Err("native MODEL revocation host mismatch".into());
        }
        registered.revoked = true;
        Ok(())
    }
}

impl super::TaskAdmissionLedger {
    pub(super) fn validate_model_hosts(&self, room: RoomId) -> Result<(), String> {
        if self.model_hosts.len() > super::MAX_RESERVATIONS {
            return Err("native MODEL enrollment capacity exceeded".into());
        }
        for (id, registration) in &self.model_hosts {
            registration.verify()?;
            if *id != registration.id || registration.room != room {
                return Err("retained native MODEL enrollment binding invalid".into());
            }
        }
        Ok(())
    }

    /// Resolve host evidence from the independent durable enrollment, never from
    /// the per-request DTO. Use before both reservation and claim.
    /// # Errors
    /// Missing enrollment, foreign host, stopped lease or changed frozen context.
    pub fn check_model_host(
        &self,
        registration: uuid::Uuid,
        requirement: &TypedNativeModelRequirement,
        caller: &DeviceCert,
    ) -> Result<NativeModelContext, String> {
        let registered = self
            .model_hosts
            .get(&registration)
            .ok_or("native MODEL host not enrolled")?;
        registered.verify()?;
        // Use the real clock on every reserve and claim, not the run's captured
        // clock or a timestamp supplied in the request.
        registered.check_live(SystemTime::now())?;
        certified_host(caller, SystemTime::now())?;
        let context = &registered.context;
        let exact: NativeInvocationBinding = requirement.binding()?;
        if registered.room != exact.room
            || registered.host != caller.device_public_key
            || exact.host != registered.host
            || context.actor != exact.agent
            || context.task != exact.task
            || context.epoch != exact.epoch
            || context.revision != exact.revision
            || context.private_run_digest != exact.private_run_digest
            || context.contract != requirement.modelselection
            || self.is_fenced(&context.task, context.epoch)
        {
            return Err("native MODEL enrolled connection, lease or contract changed".into());
        }
        Ok(context.clone())
    }
}
