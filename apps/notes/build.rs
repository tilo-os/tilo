include!("../../tools/test-assets-build.rs");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    prepare_fixture_assets("../..", "../settings/shots/artifacts.json", None)
}
