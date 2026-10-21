//! Both halves of the 2-of-2 DKG that pairs a new device with an existing
//! identity, run through a [`PairingHandshake`] broker.
//!
//! The existing device (share 1) publishes a [`PairingPayload`] carrying its
//! round-1 packet and the pinned broker; the new device (share 2) answers with
//! its round-1 and round-2 packets, the host replies with its round-2 packet,
//! and both adopt the same group key. Every packet is authenticated against
//! the full pairing transcript.

use collab_types::Ed25519PublicKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use threshold_identity::{DkgSession, EnrolmentRound1, EnrolmentRound2, ThresholdIdentity};

use crate::{IdentityFlowError, PairingHandshake, PairingPayload, pairing_transcript_mac};

/// The existing device's half of a pairing.
pub struct HostPairing {
    nonce: [u8; 16],
    payload: PairingPayload,
    session: Option<DkgSession>,
    /// The completed identity, held until the broker acknowledges the reply
    /// the new device needs to finish.
    pending: Option<(ThresholdIdentity, EnrolmentRound2, [u8; 64])>,
}

impl HostPairing {
    /// Start hosting a pairing with a fresh channel nonce. `session` and
    /// `round1` come from the host identity's `begin_enrolment(1, 2, 2, ..)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the pairing payload cannot be encoded.
    pub fn begin(
        round1: &EnrolmentRound1,
        session: DkgSession,
        initiator_pk: &Ed25519PublicKey,
    ) -> Result<Self, IdentityFlowError> {
        let mut nonce = [0u8; 16];
        UnwrapErr(SysRng).fill_bytes(&mut nonce);
        Ok(Self {
            nonce,
            payload: PairingPayload::new(nonce, initiator_pk, round1)?,
            session: Some(session),
            pending: None,
        })
    }

    pub fn nonce(&self) -> [u8; 16] {
        self.nonce
    }

    /// What the new device needs to pair: shown as a QR code or copied over.
    pub fn payload(&self) -> &PairingPayload {
        &self.payload
    }

    /// Bind the broker the exchange runs through. This must happen before
    /// the payload is shown: the endpoint and exact certificate are part of
    /// every packet's authenticated transcript.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid endpoint or certificate.
    pub fn attach_broker(
        &mut self,
        endpoint: impl Into<String>,
        certificate_der: &[u8],
    ) -> Result<(), IdentityFlowError> {
        self.payload.set_broker(endpoint, certificate_der)
    }

    /// Advance the exchange without blocking. Returns the host's new
    /// threshold identity once the new device's reply has been verified and
    /// the host's own reply acknowledged; the caller must persist it before
    /// reporting success.
    ///
    /// # Errors
    ///
    /// Returns an error if the new device's packet fails authentication or
    /// the DKG rejects it.
    pub fn poll(
        &mut self,
        broker: &PairingHandshake,
    ) -> Result<Option<ThresholdIdentity>, IdentityFlowError> {
        if self.pending.is_none()
            && let Some((pairer_round1, reply, mac)) = broker.take_pairer_exchange(&self.nonce)
        {
            self.pending = Some(self.complete_dkg(&pairer_round1, reply, mac)?);
        }
        let Some((_, round2, tag)) = &self.pending else {
            return Ok(None);
        };
        if !broker.publish_host_round2(&self.nonce, round2.clone(), *tag) {
            return Ok(None);
        }
        Ok(self.pending.take().map(|(identity, _, _)| identity))
    }

    fn complete_dkg(
        &mut self,
        pairer_round1: &EnrolmentRound1,
        reply: EnrolmentRound2,
        mac: [u8; 64],
    ) -> Result<(ThresholdIdentity, EnrolmentRound2, [u8; 64]), IdentityFlowError> {
        let expected =
            pairing_transcript_mac(&self.payload, "pairer-to-host", pairer_round1, &reply)?;
        if expected != mac {
            return Err(IdentityFlowError::PairingMacInvalid);
        }
        let mut session = self
            .session
            .take()
            .ok_or_else(|| IdentityFlowError::DecodeFailed("pairing already completed".into()))?;
        // The host is share 1 and the pairer share 2, so one round-2 receive
        // completes the 2-of-2 DKG.
        let outgoing = session.receive_round1(std::slice::from_ref(pairer_round1))?;
        let round2 = outgoing.get(&2).cloned().ok_or_else(|| {
            IdentityFlowError::DecodeFailed("DKG produced no round-2 packet for share 2".into())
        })?;
        session.receive_round2(&[reply])?;
        let mut identity = ThresholdIdentity::bootstrap_single_device(&mut UnwrapErr(SysRng))?;
        identity.adopt_dkg_outcome(session)?;
        let tag = pairing_transcript_mac(&self.payload, "host-to-pairer", pairer_round1, &round2)?;
        Ok((identity, round2, tag))
    }
}

/// Run the new device's half of a pairing through the exact pinned broker
/// `payload` names, blocking until the host's reply arrives.
///
/// # Errors
///
/// Returns an error when the broker endpoint or pin is invalid, or any
/// authenticated DKG exchange fails or times out.
#[cfg(not(target_arch = "wasm32"))]
pub fn pair_with_host(payload: &PairingPayload) -> Result<ThresholdIdentity, IdentityFlowError> {
    pair_through(payload, &payload.remote_broker()?)
}

#[cfg(not(target_arch = "wasm32"))]
fn pair_through(
    payload: &PairingPayload,
    broker: &PairingHandshake,
) -> Result<ThresholdIdentity, IdentityFlowError> {
    let nonce = payload.nonce()?;
    let host_round1 = payload.initiator_round1()?;
    let mut rng = UnwrapErr(SysRng);
    let mut identity = ThresholdIdentity::bootstrap_single_device(&mut rng)?;
    let (mut session, round1) = identity.begin_enrolment(2, 2, 2, &mut rng)?;
    let reply = session
        .receive_round1(std::slice::from_ref(&host_round1))?
        .remove(&1)
        .ok_or_else(|| {
            IdentityFlowError::DecodeFailed("DKG produced no round-2 reply to share 1".into())
        })?;
    let mac = pairing_transcript_mac(payload, "pairer-to-host", &round1, &reply)?;
    // Publication is one acknowledged, idempotent exchange, so a transient
    // TLS timeout is retried rather than stranding the host.
    poll_until(
        "pairing broker did not acknowledge the DKG exchange",
        || {
            Ok(broker
                .publish_pairer_exchange(&nonce, round1.clone(), reply.clone(), mac)
                .then_some(()))
        },
    )?;
    let host_round2 = poll_until("host never published round-2 to pairer", || {
        let Some((round2, host_mac)) = broker.take_host_round2(&nonce) else {
            return Ok(None);
        };
        if pairing_transcript_mac(payload, "host-to-pairer", &round1, &round2)? != host_mac {
            return Err(IdentityFlowError::PairingMacInvalid);
        }
        Ok(Some(round2))
    })?;
    session.receive_round2(&[host_round2])?;
    identity.adopt_dkg_outcome(session)?;
    Ok(identity)
}

/// Retry `attempt` every 20 ms for up to 15 s.
#[cfg(not(target_arch = "wasm32"))]
fn poll_until<T>(
    timeout_message: &str,
    mut attempt: impl FnMut() -> Result<Option<T>, IdentityFlowError>,
) -> Result<T, IdentityFlowError> {
    let deadline = tilo_clock::now()
        .checked_add(tilo_clock::Duration::from_secs(15))
        .ok_or_else(|| IdentityFlowError::DecodeFailed("pairing deadline overflow".into()))?;
    loop {
        if let Some(value) = attempt()? {
            return Ok(value);
        }
        if tilo_clock::now() >= deadline {
            return Err(IdentityFlowError::DecodeFailed(timeout_message.into()));
        }
        std::thread::sleep(tilo_clock::Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> HostPairing {
        let mut rng = UnwrapErr(SysRng);
        let mut bootstrap = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let (session, round1) = bootstrap.begin_enrolment(1, 2, 2, &mut rng).unwrap();
        HostPairing::begin(&round1, session, bootstrap.group_public_key()).unwrap()
    }

    #[test]
    fn both_halves_agree_on_the_group_key_and_reject_forged_replies() {
        let mut host = host();
        let broker = PairingHandshake::new();
        let payload = host.payload().clone();
        // The pairer's own deadline is held, so only the host's progress,
        // bounded by the hang bound below, decides when it is answered.
        let pairer = {
            let broker = broker.clone();
            std::thread::spawn(move || {
                tilo_clock::testing::with_monotonic_time(tilo_clock::now(), || {
                    pair_through(&payload, &broker)
                })
            })
        };
        let deadline = tilo_test_env::hang_deadline();
        let host_identity = loop {
            if let Some(identity) = host.poll(&broker).unwrap() {
                break identity;
            }
            assert!(std::time::Instant::now() < deadline, "pairing hung");
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        let pairer_identity = pairer.join().unwrap().unwrap();
        assert_eq!(
            host_identity.group_public_key(),
            pairer_identity.group_public_key()
        );

        // A reply authenticated under another payload's transcript is refused.
        let mut forged = self::host();
        let mut rng = UnwrapErr(SysRng);
        let mut pairer = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let (mut session, round1) = pairer.begin_enrolment(2, 2, 2, &mut rng).unwrap();
        let reply = session
            .receive_round1(&[host_round1(&forged)])
            .unwrap()
            .remove(&1)
            .unwrap();
        let wrong_mac =
            pairing_transcript_mac(host.payload(), "pairer-to-host", &round1, &reply).unwrap();
        assert!(broker.publish_pairer_exchange(&forged.nonce(), round1, reply, wrong_mac));
        assert!(matches!(
            forged.poll(&broker),
            Err(IdentityFlowError::PairingMacInvalid)
        ));
    }

    fn host_round1(host: &HostPairing) -> EnrolmentRound1 {
        host.payload().initiator_round1().unwrap()
    }
}
