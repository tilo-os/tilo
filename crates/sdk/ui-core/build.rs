fn emit_hex_fixture(text: &str, filename: &str) -> Result<(), Box<dyn std::error::Error>> {
    let digits: String = text
        .chars()
        .filter(|value| !value.is_ascii_whitespace())
        .collect();
    let pairs = digits.as_bytes().chunks_exact(2);
    if !pairs.remainder().is_empty() {
        return Err("invalid fixture hex encoding".into());
    }
    let mut bytes = Vec::new();
    for pair in pairs {
        bytes.push(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?);
    }
    let output =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").ok_or("missing build output")?);
    std::fs::write(output.join(filename), bytes)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Without an explicit input list Cargo watches the whole package, so a
    // documentation or test edit needlessly rebuilds the toolkit and relinks
    // every app using it. These fixtures are the script's only inputs; Rust
    // sources remain tracked by rustc's normal dependency information.
    for source in [
        "tests/fixtures/fonts/TiloSansFeaturesFixture-Regular.ttf.hex",
        "tests/fixtures/test_image.png.hex",
        "../../browser/paint-stream/tests/fixtures/layers.top.png.hex",
    ] {
        println!("cargo::rerun-if-changed={source}");
    }
    emit_hex_fixture(
        include_str!("tests/fixtures/fonts/TiloSansFeaturesFixture-Regular.ttf.hex"),
        "TiloSansFeaturesFixture-Regular.ttf",
    )?;
    emit_hex_fixture(
        include_str!("tests/fixtures/test_image.png.hex"),
        "test_image.png",
    )?;
    emit_hex_fixture(
        include_str!("../../browser/paint-stream/tests/fixtures/layers.top.png.hex"),
        "layers.top.png",
    )?;
    Ok(())
}
