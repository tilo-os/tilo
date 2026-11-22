//! The person's root, held by this device, for enrollment.

use anyhow::{Context, bail};
use tilo_user_identity::StatementSigner;

/// The person's root held here. Every statement this crate issues is signed
/// through [`StatementSigner`], the one signing path a person's statements
/// share.
pub trait RootSigner: StatementSigner {}

impl RootSigner for ed25519_dalek::SigningKey {}

impl RootSigner for tilo_identity::store::SigningIdentity {}

/// Open the person's root from the active identity in their identity store,
/// the one place a root is kept.
///
/// # Errors
///
/// No identity directory is named, it holds no active identity, or that
/// identity cannot sign alone here.
pub fn load_root(identity_dir: Option<&std::path::Path>) -> anyhow::Result<Box<dyn RootSigner>> {
    Ok(Box::new(load_signing_identity(identity_dir)?))
}

pub(crate) fn load_signing_identity(
    identity_dir: Option<&std::path::Path>,
) -> anyhow::Result<tilo_identity::store::SigningIdentity> {
    const MISSING_ROOT: &str =
        "this device does not hold the identity root needed to approve enrollment";
    let identity_dir = identity_dir
        .with_context(|| format!("{MISSING_ROOT}: name the identity directory that holds it"))?;
    let file = tilo_identity::store::resolve_active_identity(identity_dir).with_context(|| {
        format!(
            "{MISSING_ROOT}: {} holds no active identity",
            identity_dir.display()
        )
    })?;
    let identity = tilo_identity::store::load_from_path(file.clone()).map_err(|error| {
        anyhow::anyhow!(
            "{MISSING_ROOT}: cannot open {}: {error} (start this as the account that owns the \
             identity, in a session that can reach its secret store)",
            file.display()
        )
    })?;
    if !identity.is_single_device() {
        bail!("{}", tilo_identity::store::CANNOT_SIGN_ALONE);
    }
    Ok(identity)
}

/// The person's root as a signer, on a device that holds it whole: what signs
/// a certificate for a new device of theirs, and the approver commands a host
/// takes from the person's root. A device that holds only a share of it signs
/// nothing, and says so.
pub struct PersonSigner(tilo_identity::store::SigningIdentity);

impl PersonSigner {
    /// `identity` signing as the person.
    #[must_use]
    pub const fn new(identity: tilo_identity::store::SigningIdentity) -> Self {
        Self(identity)
    }

    fn sign_whole(&self, message: &[u8]) -> Result<[u8; 64], String> {
        if !self.0.is_single_device() {
            return Err(tilo_identity::store::CANNOT_SIGN_ALONE.into());
        }
        self.0.sign(message).map_err(|error| error.to_string())
    }
}

impl collab_types::signer::DeviceSigner for PersonSigner {
    fn device_public_key(&self) -> [u8; 32] {
        self.0.group_public_key()
    }

    fn sign(&self, message: Vec<u8>) -> collab_types::signer::SignFuture<'_> {
        let signed = self
            .sign_whole(&message)
            .map_err(collab_types::signer::SignError);
        Box::pin(std::future::ready(signed))
    }
}

/// The statements a person's root makes (device certificates, revocations)
/// sign through the same key as its commands.
impl StatementSigner for PersonSigner {
    fn public_key(&self) -> [u8; 32] {
        self.0.group_public_key()
    }

    fn sign_digest(&self, digest: &[u8; 32]) -> Result<[u8; 64], tilo_user_identity::SignError> {
        self.sign_whole(digest)
            .map_err(tilo_user_identity::SignError)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert};

    const NOW: u64 = 1_800_000_000_000_000_000;

    fn record(
        group: [u8; 32],
        single: bool,
        secret: &[u8],
    ) -> tilo_identity::store::SigningIdentity {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "display_name": "Fixture", "avatar_color": [1, 2, 3],
            "created_at": "2026-10-22T00:00:00Z", "schema_version": 1,
            "threshold_identity": {
                "single_device": single, "group_public_key": group, "threshold": 1,
                "total_shares": 1, "share_index": u16::from(single), "secret_bytes": secret,
            },
        }))
        .unwrap();
        tilo_identity::store::load_from_bytes(&bytes).unwrap()
    }

    /// What a signer that answers at once signs: its future is ready on the first poll.
    fn at_once<F: std::future::Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        match future
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        {
            std::task::Poll::Ready(output) => output,
            std::task::Poll::Pending => panic!("the person's key answers at once"),
        }
    }

    tilo_test_env::scenario! {
        fn the_person_signer;

        phase a_certificate_the_person_signer_signs_verifies;
        // A certificate the person's whole key signs through [`PersonSigner`] is one every
        // device verifies as the person's: the signer signs exactly the bytes the certificate's
        // check reads.
        {
            let key = ed25519_dalek::SigningKey::from_bytes(&[21; 32]);
            let person = key.verifying_key().to_bytes();
            let signer = PersonSigner::new(record(person, true, &[21; 32]));
            assert_eq!(
                collab_types::signer::DeviceSigner::device_public_key(&signer),
                person
            );
            let mut cert = DeviceCert {
                user_public_key: person,
                device_public_key: ed25519_dalek::SigningKey::from_bytes(&[22; 32])
                    .verifying_key()
                    .to_bytes(),
                device_label: "Phone".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: NOW,
                expires_at_ns: Some(NOW.checked_add(1_000_000_000).unwrap()),
                signature: [0; 64],
                issuer: None,
            };
            at_once(tilo_user_identity::sign_device_cert_async(&mut cert, &signer)).unwrap();
            tilo_user_identity::verify_device_cert_at(&cert, NOW).unwrap();
            // Another message signed with it is the person's signature too, as a host checks an
            // approver command.
            let message = b"any message the person signs".to_vec();
            let signature = at_once(collab_types::signer::DeviceSigner::sign(
                &signer,
                message.clone(),
            ))
            .unwrap();
            let person = ed25519_dalek::VerifyingKey::from_bytes(&person).unwrap();
            person
                .verify_strict(&message, &ed25519_dalek::Signature::from_bytes(&signature))
                .unwrap();
        }

        phase a_device_without_the_whole_key_signs_nothing;
        // A device that holds none of the whole key signs nothing alone, and says so.
        {
            let person = ed25519_dalek::SigningKey::from_bytes(&[23; 32])
                .verifying_key()
                .to_bytes();
            let signer = PersonSigner::new(record(person, false, &[]));
            let refused = at_once(collab_types::signer::DeviceSigner::sign(&signer, vec![1, 2, 3]));
            assert!(refused.is_err());
        }
    }
}
