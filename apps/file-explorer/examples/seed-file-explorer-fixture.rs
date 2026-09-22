//! Usage: seed-file-explorer-fixture /absolute/path/to/new-directory
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let directory = args.next().ok_or("expected a new fixture directory")?;
    if args.next().is_some() {
        return Err("expected exactly one directory".into());
    }
    let manifest = file_explorer::native_fixture::seed(std::path::Path::new(&directory))?;
    println!("{}", serde_json::to_string(&manifest)?);
    Ok(())
}
