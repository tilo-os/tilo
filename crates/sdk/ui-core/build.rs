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

#[path = "component_scan.rs"]
#[allow(
    dead_code,
    reason = "the coverage check reads the parts of the scan the registry does not"
)]
mod component_scan;

/// The registry of described widgets, generated from where each is declared
/// (`component_scan.rs`): a widget is in it because its source says `impl Described`, never
/// because a list names it.
fn emit_component_registry() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rerun-if-changed=component_scan.rs");
    let scan = component_scan::scan(std::path::Path::new("src"))?;
    // Each source the scan read, not the directory: an edit to a test does not rebuild the
    // toolkit. A new file is declared by `mod` in one the scan read, which reruns it.
    for file in &scan.files {
        println!("cargo::rerun-if-changed={}", file.display());
    }
    let output =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").ok_or("missing build output")?);
    std::fs::write(
        output.join("component_registry.rs"),
        component_scan::registry_source(&scan),
    )?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    emit_component_registry()?;
    // Without an explicit input list Cargo watches the whole package, so a
    // documentation or test edit needlessly rebuilds the toolkit and relinks
    // every app using it. These fixtures and the sources the component registry is
    // generated from are the script's only inputs.
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
