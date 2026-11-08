//! What the `tilo-install` end-to-end tests share: the binary they drive, the
//! keys they sign with, and the `.tapp` they build.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_app_package::{Manifest, SigningIdentity, pack};

/// Path to the compiled `tilo-install` binary.
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

/// Build a `.tapp` at `tapp_path` with the layout the package CLI writes:
/// `manifest`, signed by `publisher_sk`, over the files in `payload_dir`.
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
