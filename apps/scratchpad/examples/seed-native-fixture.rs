//! Usage: seed-native-fixture /absolute/path/to/new-directory
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let directory = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("expected a new fixture directory"))?;
    anyhow::ensure!(
        args.next().is_none(),
        "expected exactly one fixture directory"
    );
    let manifest = scratchpad::seed_native_fixture(std::path::Path::new(&directory))?;
    println!("{}", serde_json::to_string(&manifest)?);
    Ok(())
}
