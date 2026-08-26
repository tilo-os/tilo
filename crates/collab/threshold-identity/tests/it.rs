// One test binary instead of one per file; #[path] keeps the files in place.

#[path = "dkg_three_devices.rs"]
mod dkg_three_devices;
#[path = "dkg_two_devices.rs"]
mod dkg_two_devices;
#[path = "interop_with_collab_types.rs"]
mod interop_with_collab_types;
#[path = "recovery_share.rs"]
mod recovery_share;
#[path = "single_device.rs"]
mod single_device;
