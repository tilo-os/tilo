//! One device's identity material, as this machine keeps it.
//!
//! `tilo-user-identity` is the model and its wire ABI: keys, certificates,
//! revocations, canonical encoding and every signature over them. This crate
//! is where those values live on a machine — the device key in exactly one
//! keystore, the certificates issued to it, the revocations it trusts, the
//! journal a disk copied from another machine re-keys through, and the two
//! boot tools that mint and check them.
//!
//! The layout under `/var/lib/tilo/` is described in [`persist`]. A device key
//! is either a group-private file (the OS layout, where the root filesystem
//! daemon and the session share it) or, where only the user's own runtime
//! reads it, an item in the login Keychain. [`persist::load_or_create_device_key`]
//! is the only entry point that mints one.

#![deny(rust_2018_idioms)]

#[cfg(not(target_arch = "wasm32"))]
pub mod branch;
#[cfg(not(target_arch = "wasm32"))]
pub mod clock;
#[cfg(target_os = "macos")]
mod keychain;
pub mod login;
#[cfg(target_os = "linux")]
pub mod nofollow;
#[cfg(not(target_arch = "wasm32"))]
pub mod persist;
#[cfg(unix)]
pub mod update_lock;

#[cfg(not(target_arch = "wasm32"))]
pub use clock::{WallClockWait, wait_for_wall_clock};
#[cfg(not(target_arch = "wasm32"))]
pub use persist::{
    DeviceKeyOutcome, IdentityPaths, PersistError, RevocationsError, adopt_device_key,
    append_revocation, install_device_cert, joined_device_is_enrolled, joined_device_is_ready,
    load_or_create_device_key, provision_joined_device, read_revocations, revocations_path,
};
