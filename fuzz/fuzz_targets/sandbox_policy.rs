#![no_main]

use libfuzzer_sys::fuzz_target;
use launcher::landlock_shim::{ShimArgs, access_plan};

fuzz_target!(|data: &[u8]| {
    // NUL is an impossible byte in an actual argv element, so it makes a
    // mutation-friendly token separator while still allowing hostile paths,
    // flags, descriptors, ports, and arbitrarily placed `--` separators.
    let args: Vec<String> = data
        .split(|byte| *byte == 0)
        .map(|token| String::from_utf8_lossy(token).into_owned())
        .collect();

    if let Ok(parsed) = ShimArgs::parse(&args) {
        // Exercise the policy compiler independently of the running kernel.
        // Production currently requires V5; older ABIs remain relevant to
        // fail-closed compatibility handling and should stay total too.
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
