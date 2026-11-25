//! Compile an authored `surface.toml` into the canonical compiled surface, or re-encode an
//! existing compiled `surface.json` canonically:
//! `cargo run -p tilo-surface-compile --example compile -- <source> [<destination>]`.

use std::path::PathBuf;

fn main() -> Result<(), String> {
    let mut arguments = std::env::args_os().skip(1);
    let Some(source) = arguments.next().map(PathBuf::from) else {
        return Err("name the surface file to compile".to_owned());
    };
    let compiled = if source
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        let text = std::fs::read_to_string(&source)
            .map_err(|error| format!("{}: {error}", source.display()))?;
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| format!("{}: {error}", source.display()))?;
        tilo_surface_compile::canonical(&value)
    } else {
        tilo_surface_compile::compile_file(&source)?
    };
    match arguments.next().map(PathBuf::from) {
        Some(destination) => std::fs::write(&destination, compiled)
            .map_err(|error| format!("{}: {error}", destination.display())),
        None => {
            println!("{compiled}");
            Ok(())
        }
    }
}
