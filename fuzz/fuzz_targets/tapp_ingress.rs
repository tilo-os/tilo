#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Signed-root decoding must tolerate hostile lengths and headers.
    let _ = tilo_app_package::SignedRoot::decode(data);

    if let Ok(text) = std::str::from_utf8(data) {
        // Exercise semantic validation after parsing untrusted TOML.
        if let Ok(manifest) = tilo_app_package::Manifest::from_toml_str(text) {
            let _ = tilo_app_package::validate_manifest(&manifest);
        }
    }
});
