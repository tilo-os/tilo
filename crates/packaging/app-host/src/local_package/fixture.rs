use ed25519_dalek::{Signer, SigningKey};
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_app_package::Manifest;
use tilo_app_package::manifest::TrustAttestation;
use tilo_app_package::trust::TrustedAttestors;
use tilo_app_package::trust::{AttestorScope, TrustedAttestor, attestation_preimage};
use tilo_app_package::{SigningIdentity, pack};

pub(crate) fn fixture() -> (Vec<u8>, Vec<u8>, TrustedAttestors, String) {
    let publisher = SigningKey::generate(&mut UnwrapErr(SysRng));
    let attestor = SigningKey::generate(&mut UnwrapErr(SysRng));
    let payload = tempfile::tempdir().unwrap();
    std::fs::create_dir(payload.path().join("bin")).unwrap();
    std::fs::write(payload.path().join("bin/local"), b"local signed payload").unwrap();
    let key = hex::encode(publisher.verifying_key().to_bytes());
    let mut manifest = Manifest::from_toml_str(&format!(
        r#"
[package]
id = "so.tilo.unlisted-local"
name = "Trusted local fixture"
version = "1.0.0"
kind = "app"
[publisher]
identity = "ed25519:{key}"
signature = ""
[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[entrypoint]
bin = "bin/local"
splash-segments = []
splash-budget = 0
"#
    ))
    .unwrap();
    let identity = SigningIdentity::from_signing_key(publisher.clone());
    let initial = pack(payload.path(), &manifest, &identity).unwrap();
    let preimage = attestation_preimage(
        &initial.meta.root.payload_root_hash,
        &publisher.verifying_key().to_bytes(),
    );
    manifest.trust.attested_by.push(TrustAttestation {
        attestor: "existing-local-fixture".into(),
        claim_hex: String::new(),
        signature_hex: hex::encode(attestor.sign(&preimage).to_bytes()),
    });
    let packed = pack(payload.path(), &manifest, &identity).unwrap();
    let mut full = Vec::new();
    let mut thin = Vec::new();
    tilo_app_package::tapp::write(&mut full, &packed.meta, Some(&packed)).unwrap();
    tilo_app_package::tapp::write(&mut thin, &packed.meta, None).unwrap();
    let trust = TrustedAttestors {
        attestor: vec![TrustedAttestor {
            id: "existing-local-fixture".into(),
            pubkey_hex: hex::encode(attestor.verifying_key().to_bytes()),
            scope: AttestorScope::Prefix(vec!["so.tilo.unlisted-".into()]),
        }],
    };
    (full, thin, trust, key)
}
