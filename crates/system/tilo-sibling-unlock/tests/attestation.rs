//! Early boot proves its boot chain to a sibling, against an emulated TPM
//! and a real LUKS2 header, without root or a VM.
//!
//! Requires `swtpm`, `tpm2-tools` and `cryptsetup`; a host without them skips
//! these tests rather than failing them.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::process::Command;

use tilo_test_env::SpawnUnderEnvLock as _;

use ed25519_dalek::SigningKey;
use tilo_sibling_unlock::material::{Attestation, Material};
use tilo_sibling_unlock::seal;
use tilo_sibling_unlock::token::{Sealing, Sibling, TOKEN_TYPE, TOKEN_VERSION, Token};
use tilo_sibling_unlock::tpm::{self, Tpm};
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_unlock_protocol::{
    BootId, BootKeyCert, Challenge, PROTOCOL_VERSION, SlotId, UnlockRequest, attest,
};

/// An emulated TPM and the handle keeping it alive for the test's duration.
fn emulated() -> Option<(tilo_test_env::swtpm::Swtpm, Tpm)> {
    match tilo_test_env::swtpm::Swtpm::start() {
        Ok(swtpm) => {
            let tpm = Tpm::at_tcti(swtpm.tcti());
            Some((swtpm, tpm))
        }
        Err(why) => {
            eprintln!("skipping: {why}");
            None
        }
    }
}

fn cert(attestation_key: Vec<u8>) -> BootKeyCert {
    let root = SigningKey::from_bytes(&[3; 32]);
    let boot = SigningKey::from_bytes(&[4; 32]);
    let mut cert = BootKeyCert {
        version: PROTOCOL_VERSION,
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: [2; 32],
        boot_public_key: boot.verifying_key().to_bytes(),
        slot: SlotId {
            volume_uuid: [7; 16],
            keyslot: 0,
        },
        attestation_key: Some(attestation_key),
        issued_at_ns: 1,
        signature: [0; 64],
    };
    cert.sign(&root).unwrap();
    cert
}

fn challenge() -> Challenge {
    Challenge {
        version: PROTOCOL_VERSION,
        sibling_public_key: [5; 32],
        nonce: [6; 32],
        expires_at_ns: u64::MAX,
    }
}

#[test]
fn a_quote_from_the_tpm_proves_the_chain_a_sibling_accepts() {
    let Some((_swtpm, tpm)) = emulated() else {
        return;
    };
    let (key, point) = tpm::create_attestation_key(&tpm).expect("an attestation key");
    let cert = cert(point.clone());
    let boot = SigningKey::from_bytes(&[4; 32]);
    let challenge = challenge();
    let boot_id = BootId([9; 16]);
    let ephemeral = tilo_seal::SecretKey::generate().unwrap();
    let ephemeral_public = ephemeral.public_key().unwrap();

    // The TPM quotes exactly this request.
    let nonce =
        attest::qualifying_data(&cert, &challenge, &boot_id, &ephemeral_public.to_bytes()).unwrap();
    let quote = tpm::quote(&tpm, &key, &nonce).expect("a quote");
    let chain = attest::verify_quote(&cert.attestation_key.clone().unwrap(), &quote, &nonce)
        .expect("the sibling accepts the quote");
    assert_eq!(chain, attest::pcr_composite(&quote.pcrs));
    assert_eq!(
        quote
            .pcrs
            .iter()
            .map(|(index, _)| *index)
            .collect::<Vec<_>>(),
        attest::QUOTED_PCRS.to_vec()
    );

    // The same quote answers no other request.
    assert_eq!(
        attest::verify_quote(&point, &quote, &[8; 32]),
        Err(attest::AttestError::WrongNonce)
    );

    // The chain the machine measures now is the chain it quoted.
    assert_eq!(tpm::current_chain(&tpm).unwrap(), chain);

    // And a request carrying the quote verifies as the sibling reads it.
    let request = UnlockRequest::sign(
        &boot,
        &cert,
        &challenge,
        boot_id,
        &ephemeral_public,
        Some(quote),
    )
    .unwrap();
    assert!(request.verify(&cert, &challenge, 2).is_ok());
}

#[test]
fn the_attestation_key_travels_in_the_luks_header() {
    if Command::new("cryptsetup")
        .arg("--version")
        .output_locked()
        .is_err()
    {
        eprintln!("skipping: cryptsetup is not installed");
        return;
    }
    let Some((_swtpm, tpm)) = emulated() else {
        return;
    };
    let (attestation, point) = tpm::create_attestation_key(&tpm).expect("an attestation key");
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.img");
    std::fs::File::create(&image)
        .unwrap()
        .set_len(48 << 20)
        .unwrap();
    let recovery = dir.path().join("recovery.key");
    std::fs::write(&recovery, b"a recovery passphrase").unwrap();
    let formatted = Command::new("cryptsetup")
        .args([
            "luksFormat",
            "--type",
            "luks2",
            "--batch-mode",
            "--pbkdf",
            "pbkdf2",
            "--pbkdf-force-iterations",
            "1000",
            "--key-file",
        ])
        .arg(&recovery)
        .arg(&image)
        .status_locked()
        .expect("cryptsetup luksFormat");
    assert!(formatted.success(), "luksFormat failed");

    let boot_key = SigningKey::from_bytes(&[4; 32]);
    let cert = cert(point);
    let material = Material::new(
        &tilo_unlock_protocol::Half::from_bytes([1; 32]),
        &boot_key,
        Vec::new(),
        Some(Attestation {
            public: attestation.public.clone(),
            private: attestation.private.clone(),
        }),
    );
    let token = Token {
        kind: TOKEN_TYPE.into(),
        keyslots: vec!["0".into()],
        tilo_version: TOKEN_VERSION,
        cert: Token::encode_cert(&cert).unwrap(),
        siblings: vec![Sibling {
            device: "aa".repeat(32),
            label: "Tower".into(),
            endpoint: String::new(),
        }],
        sealing: Sealing::Cleartext,
        material: seal::seal(Sealing::Cleartext, &material.to_json().unwrap()).unwrap(),
    };
    tilo_sibling_unlock::luks::import_token(&image, &token)
        .expect("the token goes into the header");

    // Reading it back from the header gives the same material, attestation
    // key included: what the next boot quotes with.
    let read = tilo_sibling_unlock::luks::token(&image)
        .unwrap()
        .expect("the token is in the header");
    assert_eq!(read, token);
    let opened = Material::from_json(
        &seal::unseal(read.sealing, &read.material).expect("the material unseals"),
    )
    .expect("the material is well formed");
    assert_eq!(
        opened.attestation.clone().unwrap().blobs().unwrap(),
        attestation.blobs().unwrap()
    );
}
