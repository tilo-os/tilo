include!("fixture_build.rs");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    emit_hex_fixture(
        include_str!("tests/fixtures/fonts/TiloSansFeaturesFixture-Regular.ttf.hex"),
        "TiloSansFeaturesFixture-Regular.ttf",
    )?;
    emit_hex_fixture(
        include_str!("tests/fixtures/test_image.png.hex"),
        "test_image.png",
    )?;
    emit_hex_fixture(
        include_str!("../../browser/paint-stream/tests/fixtures/layers.top.png.hex"),
        "layers.top.png",
    )?;
    Ok(())
}
