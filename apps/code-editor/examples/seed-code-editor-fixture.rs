//! Usage: seed-code-editor-fixture /absolute/path/to/new-fixture.rs
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .ok_or("expected a new fixture source-file path")?;
    if args.next().is_some() {
        return Err("expected exactly one source-file path".into());
    }
    let manifest = code_editor::native_fixture::seed(std::path::Path::new(&path))?;
    println!("{}", serde_json::to_string(&manifest)?);
    Ok(())
}
