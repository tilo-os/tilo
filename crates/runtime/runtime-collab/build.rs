const EXPECTED_SCHEMA_FNV1A: u64 = 0x5d5e_2f38_184b_3b6d;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    const SCHEMA: &str = "schema/collab.capnp";
    const GENERATED: &str = "src/generated/collab_capnp.rs";
    println!("cargo:rerun-if-changed={SCHEMA}");
    println!("cargo:rerun-if-changed={GENERATED}");

    let schema = std::fs::read(SCHEMA)?;
    let actual = schema.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    });
    if actual != EXPECTED_SCHEMA_FNV1A {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{SCHEMA} changed without regenerating {GENERATED}; run regenerate.sh"),
        )
        .into());
    }
    Ok(())
}
