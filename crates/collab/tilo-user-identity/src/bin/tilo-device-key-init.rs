//! Root-only early-boot provisioning for the machine device identity.

use tilo_user_identity::persist::{DeviceKeyOutcome, IdentityPaths, load_or_create_device_key};

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
    let (_, outcome) = load_or_create_device_key(&paths).map_err(|error| error.to_string())?;
    match outcome {
        DeviceKeyOutcome::Created => eprintln!("created machine device identity"),
        DeviceKeyOutcome::Loaded => {}
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::Path;

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
                .any(|line| line == "ConditionPathExists=!/var/lib/tilo/device-private/keypair.bin")
        );
    }
}
