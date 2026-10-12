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
