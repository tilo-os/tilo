//! What the `tilo-install` end-to-end tests share: the binary, the keys and the `.tapp` builder.

use std::path::{Path, PathBuf};
use std::process::Command;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_app_package::{Manifest, SigningIdentity, pack};
use tilo_test_env::SpawnUnderEnvLock as _;

pub fn install_bin() -> PathBuf {
    tilo_test_env::package_binary!("tilo-install")
}

/// A new signing key and the hex of its public key.
pub fn fresh_key() -> (SigningKey, String) {
    let mut rng = UnwrapErr(SysRng);
    let sk = SigningKey::generate(&mut rng);
    let pub_hex = hex::encode(sk.verifying_key().to_bytes());
    (sk, pub_hex)
}

/// A `.tapp` with the layout the package CLI writes: `manifest`, signed by `publisher_sk`, over the
/// files in `payload_dir`.
pub fn build_tapp(
    tapp_path: &Path,
    manifest: &Manifest,
    publisher_sk: &SigningKey,
    payload_dir: &Path,
) -> tilo_app_package::PackageId {
    let identity = SigningIdentity::from_signing_key(publisher_sk.clone());
    let packed = pack(payload_dir, manifest, &identity).expect("pack");
    let file = std::fs::File::create(tapp_path).unwrap();
    tilo_app_package::tapp::write(file, &packed.meta, Some(&packed)).unwrap();
    packed.id()
}

/// `tilo-install` pointed at the data and config dirs under `home`: nothing of the ambient
/// environment, the XDG dirs or `HOME` gets in.
pub fn cmd(home: &Path) -> Command {
    let mut cmd = Command::new(install_bin());
    offline(&mut cmd, &home.join("data"));
    cmd.env_clear();
    cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd.env("XDG_CONFIG_HOME", home.join("config"));
    cmd.env("XDG_DATA_HOME", home.join("data"));
    cmd.env("HOME", home);
    cmd
}

/// Changes `cmd` makes go to the store under `data_home` in its own process, the way an image build
/// makes them: the package service runs the same change, and its own tests drive it there.
pub fn offline(cmd: &mut Command, data_home: &Path) {
    cmd.arg("--offline-store").arg(data_home.join("tilo-apps"));
}

pub fn run(mut cmd: Command) -> (bool, String, String) {
    let out = cmd.output_locked().expect("spawn tilo-install");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}
