//! Account presentation adapter; native consent, workers and custody live in the backend.
pub use imager_backend::account_transfer::*;

#[cfg(test)]
pub(crate) mod dav_readiness;

#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
mod destination_native;
