//! Usage: seed-native-fixture /absolute/path/to/new-directory

// The fixture drives the app through the native test host.
#[path = "../tests/support/native_fixture.rs"]
mod native_fixture;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let directory = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("expected a new fixture directory"))?;
    anyhow::ensure!(
        args.next().is_none(),
        "expected exactly one fixture directory"
    );
    let manifest = native_fixture::seed_native_fixture(std::path::Path::new(&directory))?;
    println!("{}", serde_json::to_string(&manifest)?);
    Ok(())
}
