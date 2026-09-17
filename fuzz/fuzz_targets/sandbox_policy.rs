#![no_main]

use launcher::landlock_shim::{ShimArgs, access_plan};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // NUL is a mutation-friendly separator that cannot occur inside argv.
    let args: Vec<String> = data
        .split(|byte| *byte == 0)
        .map(|token| String::from_utf8_lossy(token).into_owned())
        .collect();

    if let Ok(parsed) = ShimArgs::parse(&args) {
        // Keep policy compilation total across supported Landlock ABIs.
        for abi in [
            landlock::ABI::V1,
            landlock::ABI::V2,
            landlock::ABI::V3,
            landlock::ABI::V4,
            landlock::ABI::V5,
        ] {
            let _ = access_plan(&parsed.ro, &parsed.rw, abi);
        }
    }
});
