use super::PublicInstallProfile;
fn profile() -> PublicInstallProfile {
    PublicInstallProfile {
        schema: 1,
        purpose: "computer".into(),
        username: "alice".into(),
        full_name: "Alice".into(),
        device_label: String::new(),
        hostname: "alice-pc".into(),
        locale: "en_US.UTF-8".into(),
        keyboard: "us".into(),
        timezone: "Europe/Berlin".into(),
        filesystem: "btrfs".into(),
    }
}
#[test]
fn profile_validation_preserves_canonical_wire_and_rejects_secrets() {
    let profile = profile();
    profile.validate().unwrap();
    let bytes = serde_json::to_vec(&profile).unwrap();
    assert_eq!(
        serde_json::from_slice::<PublicInstallProfile>(&bytes).unwrap(),
        profile
    );
    let mutations: [fn(&mut PublicInstallProfile); 10] = [
        |p: &mut PublicInstallProfile| p.schema = 2,
        |p: &mut PublicInstallProfile| p.username = "root".into(),
        |p: &mut PublicInstallProfile| p.hostname.push(' '),
        |p: &mut PublicInstallProfile| p.full_name.insert(0, ' '),
        |p: &mut PublicInstallProfile| p.locale.push(' '),
        |p: &mut PublicInstallProfile| p.keyboard = "unknown".into(),
        |p: &mut PublicInstallProfile| p.timezone = "../etc".into(),
        |p: &mut PublicInstallProfile| p.filesystem = "unknown".into(),
        |p: &mut PublicInstallProfile| p.purpose = "unknown".into(),
        |p: &mut PublicInstallProfile| p.device_label = " ".into(),
    ];
    for mutate in mutations {
        let mut invalid = profile.clone();
        mutate(&mut invalid);
        assert!(invalid.validate().is_err());
    }
    let mut wire = serde_json::to_value(&profile).unwrap();
    wire["password"] = serde_json::json!("never transported");
    assert!(serde_json::from_value::<PublicInstallProfile>(wire).is_err());
}
#[cfg(target_os = "linux")]
#[test]
fn completed_capacity_refuses_out_of_bounds_and_overflow() {
    use super::capacity::*;
    assert_eq!(
        completed_setup_ciphertext_reservation(48, 100).unwrap(),
        48 + 32 * 1024
    );
    assert!(completed_setup_ciphertext_reservation(47, 100).is_err());
    assert!(completed_setup_ciphertext_reservation(101, 100).is_err());
    assert!(completed_setup_ciphertext_reservation(u64::MAX, u64::MAX).is_err());
    assert!(completed_setup_write_reservation(u64::MAX).is_err());
}
#[cfg(target_os = "linux")]
#[test]
fn installed_lease_refuses_noncanonical_operation_and_relative_namespace() {
    use super::installed_namespace::InstalledRecordLease;
    use std::path::Path;
    assert!(InstalledRecordLease::open(Path::new("/"), "../record").is_err());
    assert!(
        InstalledRecordLease::open(
            Path::new("relative"),
            "00000000-0000-4000-8000-000000000001"
        )
        .is_err()
    );
}
