//! Hosted clients must not silently weaken the local broker's access controls.
#![cfg(not(unix))]
#![allow(clippy::expect_used, reason = "test fixture setup must succeed")]
#![allow(
    clippy::disallowed_methods,
    reason = "integration tests may read the real clock; the rule guards the library"
)]

use std::time::SystemTime;
use tilo_identity_ceremony::{IdentityFlowError, InviteRedemptionAuthority, PairingHandshake};

fn assert_unsupported<T>(result: Result<T, IdentityFlowError>) {
    assert!(matches!(
        result,
        Err(IdentityFlowError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
    ));
}

#[test]
fn local_authority_rejects_unsupported_storage_without_creating_files() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let authority = directory.path().join("authority");
    let pairing = directory.path().join("pairing");
    assert_unsupported(InviteRedemptionAuthority::at(&authority));
    assert_unsupported(PairingHandshake::durable(&pairing, SystemTime::now()));
    assert!(!authority.exists());
    assert!(!pairing.exists());
}
