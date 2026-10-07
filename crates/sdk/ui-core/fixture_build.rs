fn emit_hex_fixture(source: &str, filename: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed={source}");
    let text = std::fs::read_to_string(source)?;
    let digits: String = text
        .chars()
        .filter(|value| !value.is_ascii_whitespace())
        .collect();
    if digits.len() % 2 != 0 || !digits.is_ascii() {
        return Err("invalid fixture hex encoding".into());
    }
    let bytes = (0..digits.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&digits[index..index + 2], 16))
        .collect::<Result<Vec<_>, _>>()?;
    let output =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").ok_or("missing build output")?);
    std::fs::write(output.join(filename), bytes)?;
    Ok(())
}
