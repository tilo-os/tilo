//! Root-only early-boot provisioning for the machine device identity.

use std::fs;
use std::io;
use std::path::Path;

use tilo_user_identity::persist::{
    DeviceKeyOutcome, IdentityPaths, adopt_device_key, adopt_legacy_system_identity,
    load_or_create_device_key, read_device_cert, read_device_key, read_legacy_device_key,
};

fn main() {
    if let Err(error) = provision() {
        eprintln!("tilo-device-key-init: {error}");
        std::process::exit(1);
    }
}

fn provision() -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("must run as root".into());
    }
    let paths = IdentityPaths::default_system();
    adopt_legacy_system_identity(&paths, Path::new("/var/lib/qos"))
        .map_err(|error| error.to_string())?;
    migrate_legacy_key(&paths, Path::new("/var/lib/tilo/device/keypair.bin"))
        .map_err(|error| error.to_string())?;
    let (_, outcome) = load_or_create_device_key(&paths).map_err(|error| error.to_string())?;
    match outcome {
        DeviceKeyOutcome::Created => eprintln!("created machine device identity"),
        DeviceKeyOutcome::Loaded => {}
    }
    Ok(())
}

fn migrate_legacy_key(
    paths: &IdentityPaths,
    legacy: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if !legacy.try_exists()? {
        return Ok(());
    }
    let old = read_legacy_device_key(legacy)?;

    if paths.device_cert.try_exists()? {
        let certificate = read_device_cert(&paths.device_cert)?;
        tilo_user_identity::verify_device_cert(&certificate)?;
        if certificate.device_public_key != old.verifying_key().to_bytes() {
            return Err("legacy device key does not match its root-signed certificate".into());
        }
    }

    if paths.device_privkey.try_exists()? {
        let current = read_device_key(&paths.device_privkey)?;
        if current.to_bytes() != old.to_bytes() {
            return Err(
                "protected and legacy device keys disagree; refusing identity rotation".into(),
            );
        }
    } else {
        adopt_device_key(paths, &old.to_bytes())?;
    }

    fs::remove_file(legacy)?;
    sync_parent(legacy)?;
    Ok(())
}

fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::File::open(parent)?.sync_all()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[test]
    fn canonical_paths_match_consumers_and_boot_unit() {
        let paths = IdentityPaths::default_system();
        assert_eq!(
            paths.device_privkey,
            Path::new("/var/lib/tilo/device-private/keypair.bin")
        );
        assert_eq!(
            paths.device_cert,
            Path::new("/var/lib/tilo/device/cert.bin")
        );
        let unit = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../os/usr/lib/systemd/system/tilo-device-key-init.service"
        ));
        assert!(
            unit.lines()
                .any(|line| line == "ConditionPathExists=|/var/lib/qos/device/keypair.bin")
        );
        assert!(
            unit.lines()
                .any(|line| line == "ReadWritePaths=/var/lib/tilo/user-identity")
        );
        assert!(
            !unit
                .lines()
                .any(|line| line.starts_with("ReadWritePaths=") && line.contains("/var/lib/qos"))
        );
    }

    #[test]
    fn migration_preserves_identity_and_removes_the_writable_legacy_name() {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let paths = IdentityPaths::under(root.path());
        let legacy = root.path().join("device/keypair.bin");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        let key = SigningKey::from_bytes(&[42_u8; 32]);
        tilo_user_identity::persist::write_device_key(&legacy, &key).unwrap();

        migrate_legacy_key(&paths, &legacy).unwrap();

        assert!(!legacy.exists());
        assert_eq!(
            read_device_key(&paths.device_privkey).unwrap().to_bytes(),
            key.to_bytes()
        );
        let metadata = fs::metadata(&paths.device_privkey).unwrap();
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
    }

    #[test]
    fn migration_refuses_to_choose_between_different_device_identities() {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let paths = IdentityPaths::under(root.path());
        let legacy = root.path().join("device/keypair.bin");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        tilo_user_identity::persist::write_device_key(
            &legacy,
            &SigningKey::from_bytes(&[1_u8; 32]),
        )
        .unwrap();
        adopt_device_key(&paths, &[2_u8; 32]).unwrap();

        let error = migrate_legacy_key(&paths, &legacy).unwrap_err();
        assert!(error.to_string().contains("disagree"));
        assert!(legacy.exists());
        assert_eq!(
            read_device_key(&paths.device_privkey).unwrap().to_bytes(),
            [2_u8; 32]
        );
    }

    #[test]
    fn migration_requires_a_legacy_key_to_match_its_signed_certificate() {
        use tilo_user_identity::cert::{DeviceCapabilities, sign_device_cert};
        use tilo_user_identity::{DeviceCert, DeviceIdentity, UserIdentity};

        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let paths = IdentityPaths::under(root.path());
        let legacy = root.path().join("device/keypair.bin");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        tilo_user_identity::persist::write_device_key(
            &legacy,
            &SigningKey::from_bytes(&[3_u8; 32]),
        )
        .unwrap();

        let (root_key, user) = UserIdentity::generate("owner", 1);
        let (_, different_device) = DeviceIdentity::generate("other device", 2);
        let mut certificate = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: different_device.public_key,
            device_label: different_device.label,
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 3,
            expires_at_ns: None,
            signature: [0_u8; 64],
        };
        sign_device_cert(&mut certificate, &root_key).unwrap();
        tilo_user_identity::persist::write_device_cert(&paths.device_cert, &certificate).unwrap();

        let error = migrate_legacy_key(&paths, &legacy).unwrap_err();
        assert!(error.to_string().contains("does not match"));
        assert!(legacy.exists());
        assert!(!paths.device_privkey.exists());
    }
}
