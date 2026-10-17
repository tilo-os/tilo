//! A completion signature is issued only after canonical archive verification.
use super::{Activation, Grants, failure, verify_activation};
use crate::setup::{portable_transfer::ImportReview, prepared_setup::BrowserHandoff};
use browser_continuity::completion::{self, Completion};
use std::{io, sync::atomic::AtomicBool};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};

fn certified_signer(person: [u8; 32]) -> io::Result<(ed25519_dalek::SigningKey, Vec<u8>)> {
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    let signer = tilo_user_identity::persist::load_device_key(&paths).map_err(io::Error::other)?;
    let certificate =
        tilo_user_identity::persist::read_device_cert(&paths.device_cert_for_user(&person))
            .map_err(io::Error::other)?;
    tilo_user_identity::verify_device_cert(&certificate).map_err(io::Error::other)?;
    if certificate.user_public_key != person
        || certificate.device_public_key != signer.verifying_key().to_bytes()
    {
        return Err(failure());
    }
    // The active person and enrolled fleet, rather than a key supplied through
    // arguments or an arbitrary certificate file, authorize this host receipt.
    tilo_identity_host::certificates::authorize_join("", &certificate).map_err(io::Error::other)?;
    let certificate =
        tilo_user_identity::canonical::canonical_bytes(&certificate).map_err(io::Error::other)?;
    Ok((signer, certificate))
}
pub(super) fn publish(
    grants: &Grants,
    activation: &Activation,
    handoff: &BrowserHandoff,
    browser: &mut ImportReview,
    setup: &mut ImportReview,
    cancel: &AtomicBool,
) -> io::Result<()> {
    setup.verify_held_ciphertext(cancel)?;
    let unsigned = completion::capture(
        handoff.plan.clone(),
        setup.ciphertext_digest(),
        browser.ciphertext_digest(),
        handoff.payload.operation.clone(),
        activation.projection.clone(),
    )?;
    // Capturing arbitrary directory bytes cannot establish completion. The
    // canonical decrypted archive and exact selection must independently agree.
    verify_activation(grants, activation, handoff, browser, cancel)?;
    setup.verify_held_ciphertext(cancel)?;
    unsigned.verify_unchanged()?;
    let (signer, certificate) = certified_signer(handoff.plan.identity_public_key)?;
    unsigned.verify_unchanged()?;
    let signed = unsigned.sign(&signer, certificate)?;
    signed.verify_binding(
        &handoff.plan,
        setup.ciphertext_digest(),
        browser.ciphertext_digest(),
        &handoff.payload.operation,
    )?;
    signed.verify_projection(signer.verifying_key().to_bytes())?;
    let directory =
        StorageDirectory::from_file(super::durable::readable_directory(&grants.workspace.file)?)?;
    directory
        .publish(
            completion::FILE_NAME.as_ref(),
            &signed.encode()?,
            PublishMode::Replace,
            Privacy::OwnerOnly,
        )
        .map_err(io::Error::other)?;
    grants.validate()?;
    let saved = Completion::read(&grants.workspace_path.join(completion::FILE_NAME))?;
    if saved != signed {
        return Err(failure());
    }
    saved.verify_projection(signer.verifying_key().to_bytes())?;
    verify_activation(grants, activation, handoff, browser, cancel)?;
    setup.verify_held_ciphertext(cancel)
}
