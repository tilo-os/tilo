// The Cap'n Proto bindings are checked in under src/generated/, so building
// this crate never needs the `capnp` compiler. Both fingerprints are written by
// `tools/regenerate-capnp.sh crates/doc/format-host . format-router.capnp`;
// a schema edited without regenerating, or bindings edited by hand, fail here.

const EXPECTED_SCHEMA_FNV1A: u64 = 0xc860_5c5f_aafe_98ae;
const EXPECTED_BINDINGS_FNV1A: u64 = 0x5100_ca22_0cad_974b;
const SCHEMAS: [&str; 1] = ["format-router.capnp"];
const GENERATED: [&str; 1] = ["src/generated/format_router_capnp.rs"];

fn fingerprint(paths: &[&str]) -> std::io::Result<u64> {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for path in paths {
        println!("cargo:rerun-if-changed={path}");
        for byte in std::fs::read(path)? {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
        }
    }
    Ok(hash)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let stale = if fingerprint(&SCHEMAS)? != EXPECTED_SCHEMA_FNV1A {
        "the Cap'n Proto schema changed without regenerating its bindings"
    } else if fingerprint(&GENERATED)? != EXPECTED_BINDINGS_FNV1A {
        "the checked-in Cap'n Proto bindings were edited by hand"
    } else {
        return Ok(());
    };
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!(
            "{stale}; run `tools/regenerate-capnp.sh crates/doc/format-host . format-router.capnp`"
        ),
    )
    .into())
}
