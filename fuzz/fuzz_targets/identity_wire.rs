#![no_main]

use libfuzzer_sys::fuzz_target;
use qos_user_identity::{
    DeviceCert, EnrollmentToken, RevocationEntry, verify_device_cert,
    verify_enrollment_token, verify_revocation_entry,
};

fuzz_target!(|data: &[u8]| {
    if let Ok(cert) = postcard::from_bytes::<DeviceCert>(data) {
        let _ = verify_device_cert(&cert);
    }
    if let Ok(revocation) = postcard::from_bytes::<RevocationEntry>(data) {
        let _ = verify_revocation_entry(&revocation);
    }
    if let Ok(enrollment) = postcard::from_bytes::<EnrollmentToken>(data) {
        let _ = verify_enrollment_token(&enrollment, 2_000_000_000_000_000_000);
    }
});
