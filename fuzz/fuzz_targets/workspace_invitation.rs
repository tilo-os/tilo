#![no_main]

use libfuzzer_sys::fuzz_target;
use tilo_user_identity::RevocationList;
use tilo_workspace_join::{decode_and_verify_invitation, decode_and_verify_invitation_text};

// Signed workspace invitations arrive from whoever sends a link or code,
// before the recipient has decided to trust the sender.
fuzz_target!(|data: &[u8]| {
    let revocations = RevocationList::default();
    let now = 2_000_000_000_000_000_000;
    let _ = decode_and_verify_invitation(data, None, &revocations, now);
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = decode_and_verify_invitation_text(text, None, &revocations, now);
    }
});
