include!("../../tools/test-assets-build.rs");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    prepare_fixture_assets("../..", "sample-media/artifacts.json", None)?;
    let output =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").ok_or("missing build output")?);
    let media = output.join("sample-media/media");
    std::fs::create_dir_all(&media)?;
    for entry in std::fs::read_dir("sample-media/media")? {
        let entry = entry?;
        println!("cargo:rerun-if-changed={}", entry.path().display());
        std::fs::copy(entry.path(), media.join(entry.file_name()))?;
    }
    for (cached, original) in [
        ("ambient_pad.wav", "Ambient_Pad.wav"),
        ("vo_take3.wav", "VO_Take3.wav"),
    ] {
        std::fs::copy(output.join(cached), media.join(original))?;
    }
    Ok(())
}
