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

/// The registry of described widgets, generated from where each is declared
/// (`tilo-component-scan`): a widget is in it because its source says `impl Described`, never
/// because a list names it.
fn emit_component_registry() -> Result<(), Box<dyn std::error::Error>> {
    let scan = tilo_component_scan::scan(std::path::Path::new("src"))?;
    // Each source the scan read, not the directory: an edit to a test does not rebuild the
    // toolkit. A new file is declared by `mod` in one the scan read, which reruns it.
    for file in &scan.files {
        println!("cargo::rerun-if-changed={}", file.display());
    }
    let output =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").ok_or("missing build output")?);
    std::fs::write(
        output.join("component_registry.rs"),
        tilo_component_scan::registry_source(
            &scan,
            "::tilo_ui_base",
            tilo_component_scan::Exports::TopModules,
        ),
    )?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    emit_component_registry()?;
    // Without an explicit input list Cargo watches the whole package, so a
    // documentation or test edit needlessly rebuilds the toolkit and relinks
    // every app using it. These fixtures and the sources the component registry is
    // generated from are the script's only inputs.
    println!("cargo::rerun-if-changed=tests/fixtures/test_image.png.hex");
    emit_hex_fixture(
        include_str!("tests/fixtures/test_image.png.hex"),
        "test_image.png",
    )?;
    Ok(())
}
