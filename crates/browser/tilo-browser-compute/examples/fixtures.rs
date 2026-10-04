//! Deterministic real formats and native production pixel oracle for Chrome qualification.
use std::{fs, io::Cursor, path::Path};
fn write_fixture(
    root: &Path,
    name: &str,
    width: u32,
    height: u32,
    format: image::ImageFormat,
    cap: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |x, y| {
        let v = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(2_246_822_519);
        let bytes = v.to_le_bytes();
        image::Rgb([bytes[0], bytes[1], bytes[2]])
    }));
    let mut encoded = Cursor::new(Vec::new());
    image.write_to(&mut encoded, format)?;
    fs::write(root.join(name), encoded.get_ref())?;
    let result = tilo_browser_compute::decode(encoded.get_ref(), cap)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    fs::write(root.join(format!("{name}.rgba")), result.rgba)?;
    fs::write(
        root.join(format!("{name}.dimensions")),
        format!("{},{}", result.width, result.height),
    )?;
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args()
        .nth(1)
        .ok_or("fixture output directory required")?;
    let root = Path::new(&output);
    fs::create_dir_all(root)?;
    for (name, format) in [
        ("small.png", image::ImageFormat::Png),
        ("small.jpg", image::ImageFormat::Jpeg),
        ("small.webp", image::ImageFormat::WebP),
    ] {
        write_fixture(root, name, 11, 7, format, 5)?;
    }
    write_fixture(root, "work.png", 2048, 2048, image::ImageFormat::Png, 2048)?;
    let image = image::DynamicImage::ImageRgb8(image::RgbImage::new(4097, 1));
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png)?;
    fs::write(root.join("oversized.png"), bytes.into_inner())?;
    Ok(())
}
