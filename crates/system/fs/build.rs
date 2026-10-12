include!("../../../tools/test-assets-build.rs");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    prepare_fixture_assets(
        "../../..",
        "../../doc/fixtures/artifacts.json",
        Some("sample.docx"),
    )?;
    Ok(())
}
