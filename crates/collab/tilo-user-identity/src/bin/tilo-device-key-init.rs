//! Early-boot provisioning of a device identity's key: the machine's
//! (root-only, no arguments), or with `--root DIR --public-env-out FILE` a
//! node session's own tree, created by the session's account before the
//! node's install certifies it. The public half goes to `FILE` as
//! `TILO_NODE_SESSION_DEVICE=<hex>`, a systemd `EnvironmentFile` the node's
//! relay names the session with (`--session-device`).

use tilo_user_identity::persist::{DeviceKeyOutcome, IdentityPaths, load_or_create_device_key};

fn main() {
    if let Err(error) = provision(&std::env::args().skip(1).collect::<Vec<_>>()) {
        eprintln!("tilo-device-key-init: {error}");
        std::process::exit(1);
    }
}

fn provision(args: &[String]) -> Result<(), String> {
    match args {
        [] => {
            if rustix::process::geteuid().as_raw() != 0 {
                return Err("must run as root".into());
            }
            create(&IdentityPaths::default_system(), "machine").map(drop)
        }
        [root_flag, root, out_flag, out]
            if root_flag == "--root" && out_flag == "--public-env-out" =>
        {
            let device = create(&IdentityPaths::under(root), "node session")?;
            let line = format!(
                "TILO_NODE_SESSION_DEVICE={}\n",
                hex::encode(device.verifying_key().to_bytes())
            );
            std::fs::write(out, line).map_err(|error| format!("write {out}: {error}"))
        }
        _ => Err("usage: tilo-device-key-init [--root DIR --public-env-out FILE]".into()),
    }
}

fn create(paths: &IdentityPaths, what: &str) -> Result<ed25519_dalek::SigningKey, String> {
    let (device, outcome) = load_or_create_device_key(paths).map_err(|error| error.to_string())?;
    if outcome == DeviceKeyOutcome::Created {
        eprintln!("created {what} device identity");
    }
    Ok(device)
}

#[cfg(test)]
#[cfg(unix)]
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
