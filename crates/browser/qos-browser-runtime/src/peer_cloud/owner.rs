//! Browser Cloud owner custody: enrollment checkpoints, owner refresh and admission.
//! Persisted material is custody, not evidence of current workspace ownership:
//! ownership is derived only from admitted signed governance.
use super::identity;
use super::protocol::{EnrolledScope, HostedAttempt, HostedAttemptError, HostedStep, Selection};
use crate::peer_delivery::incoming::IncomingBudget;
use qos_peer_state::join::{self, JoinTransportRequest};

// Same bounded bootstrap envelope as invited admission.
const MAX_GOVERNANCE_ITEMS: usize = 4096;
const MAX_GOVERNANCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_SELECTION: usize = 256;

fn refused(error: HostedAttemptError) -> String {
    format!("Cloud owner enrollment refused ({error:?})")
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn selection(account_id: &str, machine_id: &str) -> Result<Selection, String> {
    if account_id.is_empty()
        || machine_id.is_empty()
        || account_id.len() > MAX_SELECTION
        || machine_id.len() > MAX_SELECTION
    {
        return Err("Invalid Cloud owner account or machine".into());
    }
    Ok(Selection {
        account_id: account_id.into(),
        machine_id: machine_id.into(),
    })
}

/// Custody locator. It has no authority semantics and survives expiry.
pub(crate) fn scope(account_id: &str, machine_id: &str) -> Result<String, String> {
    selection(account_id, machine_id)?;
    identity::canonical::signing_digest(
        b"qos-browser-cloud-custody-locator-v4\0",
        &(account_id, machine_id),
    )
    .map(|digest| hex(&digest))
    .map_err(|error| error.to_string())
}

/// New first-claim owner identity (root + device keys) for one Cloud machine.
pub(crate) fn create(
    account_id: &str,
    machine_id: &str,
    label: &str,
    budget: &IncomingBudget,
    now: u64,
) -> Result<Vec<u8>, String> {
    let selected = selection(account_id, machine_id)?;
    HostedAttempt::create_material(&selected, label, now, budget.allowance()).map_err(refused)
}

pub(crate) enum CloudStep {
    Inspect,
    Apply {
        operation_key: String,
        payload: String,
    },
    Enrolled,
}

/// Scope of an accepted enrollment: the pinned host and this owner device.
pub(crate) struct EnrolledOwner(EnrolledScope);
impl EnrolledOwner {
    pub(crate) fn room(&self) -> &str {
        &self.0.room
    }
    pub(crate) fn workspace(&self) -> u64 {
        self.0.workspace
    }
    pub(crate) fn host(&self) -> &identity::DeviceCert {
        &self.0.host
    }
    pub(crate) fn certificate(&self) -> &identity::DeviceCert {
        &self.0.certificate
    }
}

pub(crate) struct CloudOwner {
    attempt: HostedAttempt,
    scope: String,
    budget: IncomingBudget,
}
impl CloudOwner {
    pub(crate) fn restore(
        material: &[u8],
        account_id: &str,
        machine_id: &str,
        budget: &IncomingBudget,
        now: u64,
    ) -> Result<Self, String> {
        let selected = selection(account_id, machine_id)?;
        let attempt =
            HostedAttempt::restore(material, selected, budget.allowance(), now).map_err(refused)?;
        Ok(Self {
            attempt,
            scope: scope(account_id, machine_id)?,
            budget: budget.clone(),
        })
    }

    /// Public, secret-free custody summary. The binding fixes the person,
    /// device certificate and attempt across every later checkpoint.
    pub(crate) fn public(&self) -> Result<String, String> {
        let (attempt, created) = self.attempt.attempt_binding();
        let certificate = self.attempt.certificate();
        let binding = identity::canonical::signing_digest(
            b"qos-browser-cloud-custody-binding-v4\0",
            &(&self.scope, certificate, attempt, created),
        )
        .map_err(|error| error.to_string())?;
        let step = match self.attempt.step().map_err(refused)? {
            HostedStep::Inspect => "inspect",
            HostedStep::Apply => "apply",
            HostedStep::Enrolled => "enrolled",
        };
        serde_json::to_string(&serde_json::json!({
            "scope": self.scope,
            "device_public_key": hex(&certificate.device_public_key),
            "binding": hex(&binding),
            "step": step,
        }))
        .map_err(|error| error.to_string())
    }

    pub(crate) fn material(&self) -> &[u8] {
        self.attempt.current_material()
    }
    pub(crate) fn certificate(&self) -> &identity::DeviceCert {
        self.attempt.certificate()
    }

    pub(crate) fn step(&self) -> Result<CloudStep, String> {
        Ok(match self.attempt.step().map_err(refused)? {
            HostedStep::Inspect => CloudStep::Inspect,
            HostedStep::Apply => {
                let request = self.attempt.apply_request().map_err(refused)?;
                CloudStep::Apply {
                    operation_key: request.operation_key.to_string(),
                    payload: std::str::from_utf8(request.payload)
                        .map_err(|_| "Invalid native enrollment request")?
                        .into(),
                }
            }
            HostedStep::Enrolled => CloudStep::Enrolled,
        })
    }

    /// Signs the enrollment request for this exact challenge. The returned
    /// checkpoint must be durably saved and acknowledged before `apply`.
    pub(crate) fn enroll(&mut self, challenge: &str, now: u64) -> Result<Vec<u8>, String> {
        let checkpoint = self
            .attempt
            .prepare_enrollment(challenge.as_bytes(), now)
            .map_err(refused)?;
        Ok(checkpoint.material.to_vec())
    }

    /// Verifies the Store.apply reply against the retained signed request.
    pub(crate) fn accept(&mut self, reply: &str, now: u64) -> Result<Vec<u8>, String> {
        let checkpoint = self
            .attempt
            .accept_enrollment(reply.as_bytes(), now)
            .map_err(refused)?;
        Ok(checkpoint.material.to_vec())
    }

    pub(crate) fn acknowledge(&mut self, saved: &[u8], now: u64) -> Result<(), String> {
        self.attempt.acknowledge(saved, now).map_err(refused)
    }

    pub(crate) fn enrolled(&self) -> Result<EnrolledOwner, String> {
        self.attempt.enrolled().map(EnrolledOwner).map_err(refused)
    }
    pub(crate) fn device_key(&self) -> Result<ed25519_dalek::SigningKey, String> {
        self.attempt.device_key().map_err(refused)
    }
    pub(crate) fn refresh_challenge(
        &self,
        request_id: [u8; 32],
    ) -> Result<JoinTransportRequest, String> {
        self.attempt.refresh_challenge(request_id).map_err(refused)
    }
    pub(crate) fn refresh_proof(
        &self,
        challenge: &join::MembershipChallenge,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<JoinTransportRequest, String> {
        self.attempt
            .refresh_proof(challenge, request_id, now)
            .map_err(refused)
    }

    /// Admits the owner refresh reply's signed governance through the same
    /// bounded native cut as invitations, then requires this device to be the
    /// current structural Owner and the pinned host to be appointed.
    pub(crate) fn admit(
        &self,
        scope: &EnrolledOwner,
        bootstrap: &join::OwnerBootstrapV1,
        now: u64,
    ) -> Result<qos_peer_state::StatePeer, String> {
        self.attempt
            .check_owner_bootstrap(bootstrap, now)
            .map_err(refused)?;
        if bootstrap.authority != *scope.host()
            || bootstrap.device_cert != *scope.certificate()
            || bootstrap.device_cert != *self.certificate()
            || bootstrap.room != scope.room()
            || bootstrap.workspace_id != scope.workspace()
            || bootstrap.governance_messages.is_empty()
            || bootstrap.governance_messages.len() > MAX_GOVERNANCE_ITEMS
            || bootstrap
                .governance_messages
                .iter()
                .try_fold(0usize, |sum, bytes| sum.checked_add(bytes.len()))
                .is_none_or(|sum| sum > MAX_GOVERNANCE_BYTES)
        {
            return Err("Cloud owner bootstrap does not match enrolled custody".into());
        }
        let time = web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now);
        let initial = qos_peer_state::StatePeer::new(
            &bootstrap.room,
            bootstrap.authority.clone(),
            vec![bootstrap.authority.clone()],
            identity::RevocationList::new(),
            time,
        )?;
        let mut cut = crate::peer_delivery::admission::CutAdmission::begin(
            &initial,
            self.budget.clone(),
            time,
        )?;
        for bytes in &bootstrap.governance_messages {
            cut.push_retained(bytes, time)
                .map_err(|error| format!("Invalid owner governance message: {error}"))?;
        }
        let state = cut.finish(time)?;
        state
            .verify_current_owner(scope.certificate(), time)
            .map_err(|error| format!("Cloud owner device is not the structural owner: {error}"))?;
        if !state.certificates().contains(scope.host())
            || state
                .permissions(scope.host().device_public_key, time)
                .is_err()
        {
            return Err("Cloud host is not currently appointed".into());
        }
        Ok(state)
    }

    pub(crate) fn cancel(&mut self) {
        self.attempt.cancel();
    }
}

#[cfg(test)]
#[path = "owner_tests.rs"]
mod tests;
