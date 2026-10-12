//! Canonical co-signing of complete, publisher-verified package payloads.
use std::path::Path;

use anyhow::{Result, bail};
use ed25519_dalek::Signer;
use tilo_app_package::{
    BlobHash, PackageMeta, Verification,
    manifest::TrustAttestation,
    signed::SignedRoot,
    trust::{AttestorScope, attestation_preimage},
    verify,
};

pub(super) fn run(
    publisher_path: &Path,
    attestor_path: &Path,
    id: &str,
    scope: &str,
    input: &Path,
    output: &Path,
) -> Result<()> {
    if id.trim().is_empty() || id != id.trim() || id.chars().any(char::is_control) {
        bail!(
            "attestor ID must be nonempty and contain no control characters or surrounding whitespace"
        );
    }
    let publisher = super::load_identity(publisher_path)?;
    let attestor = super::load_signing_key(attestor_path)?;
    let (package, blobs) = super::read_tapp(input, true)?;
    if !package.is_complete() {
        bail!("attestation requires every verified payload blob; thin or incomplete input refused");
    }
    if !matches!(
        verify(&package.meta, &[publisher.public_key_bytes()]),
        Verification::Trusted { .. }
    ) {
        bail!("input signature does not match the original publisher identity");
    }
    let mut manifest = package.meta.manifest.clone();
    if !AttestorScope::parse(scope)?.covers(&manifest.package.id, &manifest.providers.mime) {
        bail!("attestor scope does not cover this package");
    }
    let signature = attestor
        .sign(&attestation_preimage(
            &package.meta.root.payload_root_hash,
            &publisher.public_key_bytes(),
        ))
        .to_bytes();
    manifest.trust.attested_by.retain(|row| row.attestor != id);
    manifest.trust.attested_by.push(TrustAttestation {
        attestor: id.into(),
        claim_hex: String::new(),
        signature_hex: hex::encode(signature),
    });
    let manifest_bytes = manifest.to_toml_string()?.into_bytes();
    let mut root = SignedRoot {
        manifest_hash: BlobHash::of(&manifest_bytes),
        signature: Some([0; 64]),
        ..package.meta.root
    };
    root.signature = Some(publisher.sign_signed_root(&root)?);
    let meta = PackageMeta::from_root(root, manifest_bytes, package.meta.collection_bytes)?;
    // A fresh create prevents accidental replacement of official/user packages.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    tilo_app_package::tapp::write(file, &meta, Some(&blobs))?;
    println!("{}", meta.id);
    Ok(())
}
