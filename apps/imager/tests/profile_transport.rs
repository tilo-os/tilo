//! Production Imager codec → live extractor → installer validator → backend binding.
#![cfg(target_os = "linux")]
#[path = "../../../crates/session/installer/src/layouts.rs"]
pub mod layouts;
#[path = "../../../crates/session/installer/src/media_profile.rs"]
pub mod media_profile;

use imager::{model, profile};
use sha2::{Digest, Sha256};
use std::{fs, process::Command};

#[test]
fn prepared_profile_survives_live_transport_and_rejects_mutations() {
    let root = tempfile::tempdir().unwrap();
    let media = root.path().join("media.img");
    fs::write(&media, vec![0xa5; 131_072]).unwrap();
    let sealed = b"TILOMIG1opaque ciphertext transport fixture never authenticated by this test";
    let mut draft = model::InstallProfile {
        purpose: "server".into(),
        device_label: "Travel computer".into(),
        prepared_setup: Some(model::PreparedSetupExpectation {
            schema: 1,
            sealed_bytes: sealed.len() as u64,
            ciphertext_sha256: hex::encode(Sha256::digest(sealed)),
        }),
        ..Default::default()
    };
    draft.user.username = "targetowner".into();
    draft.user.full_name = "Target Owner".into();
    draft.encryption_passphrase = "secret-must-stay-local".into();
    let (restored, legacy) = public_profiles(&draft).unwrap();
    assert_eq!(restored.purpose, draft.purpose);
    assert_eq!(restored.device_label, draft.device_label);
    assert!(
        restored.prepared_setup.is_none(),
        "no self-referential ciphertext binding inside its own package"
    );
    assert!(restored.encryption_passphrase.is_empty());
    assert_eq!(legacy.purpose, "computer");
    profile::write_profile_block(&media, 131_072, &draft).unwrap();
    let output = root.path().join("live");
    let os = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../os/usr/libexec");
    assert!(
        Command::new("bash")
            .arg(os.join("tilo-installer-profile-extract"))
            .env("TILO_INSTALLER_PROFILE_DEVICE", &media)
            .env("TILO_INSTALLER_PROFILE_ALLOW_FILE", "1")
            .env("TILO_INSTALLER_PROFILE_OUT_DIR", &output)
            .status()
            .unwrap()
            .success()
    );
    let path = output.join("profile.json");
    let received = media_profile::load_from(&path).unwrap().unwrap();
    assert!(received.repairs.is_empty());
    assert_eq!(received.purpose, "server");
    assert_eq!(received.device_label, draft.device_label);
    let expectation = received.prepared_setup.unwrap();
    assert_eq!(
        expectation.ciphertext_sha256,
        draft.prepared_setup.as_ref().unwrap().ciphertext_sha256
    );
    let bytes = fs::read(&path).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("secret-must-stay-local"));
    fs::write(output.join("prepared-setup.sealed"), sealed).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            output.join("prepared-setup.sealed"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let preflight = || {
        Command::new(tilo_test_env::package_binary!(
            "tilo-installer-prepared-setup-extract"
        ))
        .args(["preflight-profile", "--source-dir"])
        .arg(&output)
        .output()
        .unwrap()
    };
    assert!(preflight().status.success());
    let receipt: install_profile_model::PreparedSetupExpectation =
        serde_json::from_slice(&fs::read(output.join("receipt.json")).unwrap()).unwrap();
    assert_eq!(receipt, expectation);
    fs::write(
        output.join("prepared-setup.sealed"),
        vec![b'x'; sealed.len()],
    )
    .unwrap();
    assert!(!preflight().status.success());
    for mutation in [
        serde_json::json!(null),
        serde_json::json!({"schema": 2, "sealed_bytes": 48, "ciphertext_sha256": "a".repeat(64)}),
        serde_json::json!({"schema": 1, "sealed_bytes": 48, "ciphertext_sha256": "a".repeat(64), "password": "do-not-log"}),
    ] {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["prepared_setup"] = mutation;
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let error = media_profile::load_from(&path).unwrap_err();
        assert!(!error.contains("do-not-log"));
        assert!(!preflight().status.success());
    }
    let original = fs::read(&media).unwrap();
    draft.prepared_setup.as_mut().unwrap().sealed_bytes = 0;
    assert!(profile::write_profile_block(&media, 131_072, &draft).is_err());
    assert_eq!(fs::read(&media).unwrap(), original);
}

fn public_profiles(
    draft: &model::InstallProfile,
) -> Result<(model::InstallProfile, model::InstallProfile), Box<dyn std::error::Error>> {
    let authenticated = imager::setup::prepared_setup::PublicInstallProfile::from_draft(draft)?;
    let restored = authenticated.to_draft()?;
    let mut legacy = serde_json::to_value(&authenticated)?;
    let object = legacy.as_object_mut().ok_or("profile must be an object")?;
    object.remove("purpose");
    object.remove("device_label");
    let legacy: imager::setup::prepared_setup::PublicInstallProfile =
        serde_json::from_value(legacy)?;
    Ok((restored, legacy.to_draft()?))
}
