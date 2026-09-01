#![no_main]

use std::time::{Duration, UNIX_EPOCH};

use collab_types::invite::{InviteToken, verify_chain};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // JSON is used here only as a mutation-friendly carrier. The important
    // target is verify_chain: signatures, parent links, monotone attenuation,
    // expiry and authority membership must remain total for hostile input.
    let Ok(chain) = serde_json::from_slice::<Vec<InviteToken>>(data) else {
        return;
    };
    let roots = chain.first().map(|token| vec![token.issuer]).unwrap_or_default();
    let now = UNIX_EPOCH + Duration::from_secs(2_000_000_000);
    let _ = verify_chain(&chain, now, &roots);
});
