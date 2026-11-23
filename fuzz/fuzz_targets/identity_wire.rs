#![no_main]

use libfuzzer_sys::fuzz_target;
use tilo_user_identity::{
    DeviceCert, RevocationEntry, verify_device_cert, verify_revocation_entry,
};

fuzz_target!(|data: &[u8]| {
    if let Ok(cert) = postcard::from_bytes::<DeviceCert>(data) {
        let _ = verify_device_cert(&cert);
    }
    if let Ok(revocation) = postcard::from_bytes::<RevocationEntry>(data) {
        let _ = verify_revocation_entry(&revocation);
    }
});
