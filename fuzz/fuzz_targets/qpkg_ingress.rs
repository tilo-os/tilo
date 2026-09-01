#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fixed-size signed-root decoding is the first operation performed on a
    // downloaded qpkg. Exercise arbitrary lengths as well as structurally
    // plausible headers; neither path may panic or allocate from attacker
    // controlled length fields.
    let _ = pkg::SignedRoot::decode(data);

    if let Ok(text) = std::str::from_utf8(data) {
        // TOML is the second untrusted surface. Parsing and the semantic
        // validator are deliberately driven together so fuzzing reaches path,
        // capability and package-identity invariants rather than stopping at
        // syntax.
        if let Ok(manifest) = pkg::Manifest::from_toml_str(text) {
            let _ = pkg::validate_manifest(&manifest);
        }
    }
});
