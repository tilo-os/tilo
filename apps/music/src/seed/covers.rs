//! Original abstract illustrations for the fictional screenshot catalogue.
//! Generated from source, not shipped binary assets or replacement live covers.
use base64::Engine as _;
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use std::sync::OnceLock;

type Palette = (&'static str, [u8; 3], [u8; 3], [u8; 3]);
const PALETTES: &[Palette] = &[
    (
        "al_glasslight",
        [16, 43, 60],
        [137, 209, 200],
        [237, 188, 181],
    ),
    (
        "al_paperboats",
        [36, 54, 75],
        [131, 172, 184],
        [226, 215, 182],
    ),
    (
        "al_afterhours",
        [32, 31, 70],
        [160, 115, 178],
        [238, 173, 140],
    ),
    ("al_vhs", [57, 38, 64], [203, 125, 145], [231, 183, 124]),
    (
        "al_riverstone",
        [37, 61, 58],
        [125, 157, 130],
        [222, 209, 171],
    ),
    ("al_pulse", [21, 46, 71], [86, 138, 175], [182, 217, 220]),
    ("al_strata", [59, 51, 59], [177, 154, 136], [231, 206, 176]),
    (
        "al_nocturnes",
        [36, 43, 71],
        [114, 123, 175],
        [217, 197, 207],
    ),
    (
        "al_harborlights",
        [24, 62, 75],
        [83, 157, 163],
        [243, 187, 142],
    ),
    ("pl_focus", [29, 46, 68], [99, 150, 165], [195, 209, 196]),
    ("pl_drive", [43, 35, 72], [177, 126, 174], [234, 177, 148]),
    ("pl_morning", [76, 75, 67], [173, 169, 141], [242, 216, 178]),
];

fn mix(a: [u8; 3], b: [u8; 3], amount: f64) -> [u8; 3] {
    let [ar, ag, ab] = a;
    let [br, bg, bb] = b;
    [[ar, br], [ag, bg], [ab, bb]]
        .map(|[a, b]| (f64::from(a) + (f64::from(b) - f64::from(a)) * amount) as u8)
}

fn illustration(index: usize, dark: [u8; 3], mid: [u8; 3], light: [u8; 3]) -> RgbImage {
    let phase = index as f64 * 0.73;
    // Precompute the sculptural ridge outlines; each cover has its own rhythm.
    let ridges: Vec<[f64; 5]> = (0..512)
        .map(|x| {
            std::array::from_fn(|layer| {
                285.0
                    + layer as f64 * 42.0
                    + 42.0 * (f64::from(x) / 130.0 + phase + layer as f64 * 1.7).sin()
                    + 0.08 * f64::from(x)
            })
        })
        .collect();
    RgbImage::from_fn(512, 512, |x, y| {
        let px = f64::from(x);
        let py = f64::from(y);
        let glow = (-(((px - 310.0) / 290.0).powi(2) + ((py - 170.0) / 220.0).powi(2))).exp();
        let mut color = mix(dark, mid, glow * 0.7);
        if (px - 318.0).hypot(py - 170.0) < 80.0 {
            color = mix(color, light, 0.92);
        }
        let orbit = ((px - 260.0) / 185.0).powi(2) + ((py - 235.0) / 185.0).powi(2);
        if (orbit - 1.0).abs() < 0.006 {
            color = mix(color, light, 0.25);
        }
        if let Some(ridges) = ridges.get(x as usize) {
            for (layer, ridge) in ridges.iter().enumerate() {
                if py >= *ridge {
                    color = mix(dark, mid, 0.65 - layer as f64 * 0.11);
                }
                if (py - ridge).abs() < 0.65 {
                    color = mix(color, light, 0.3);
                }
            }
        }
        if (36..74).contains(&x) && (39..41).contains(&y) {
            color = mix(color, light, 0.8);
        }
        Rgb(color)
    })
}

pub(super) fn urls() -> Vec<(String, String)> {
    static COVERS: OnceLock<Vec<(String, String)>> = OnceLock::new();
    COVERS
        .get_or_init(|| {
            PALETTES
                .iter()
                .enumerate()
                .filter_map(|(index, (id, dark, mid, light))| {
                    let image = illustration(index, *dark, *mid, *light);
                    let mut png = std::io::Cursor::new(Vec::new());
                    DynamicImage::ImageRgb8(image)
                        .write_to(&mut png, ImageFormat::Png)
                        .ok()?;
                    Some((
                        (*id).to_owned(),
                        format!(
                            "data:image/png;base64,{}",
                            base64::engine::general_purpose::STANDARD.encode(png.into_inner())
                        ),
                    ))
                })
                .collect()
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;

    #[test]
    fn sample_covers_are_complete_deterministic_and_decodable() {
        let covers = super::urls();
        assert_eq!(covers, super::urls());
        assert_eq!(covers.len(), 12);
        let mut unique = std::collections::HashSet::new();
        for (_, url) in covers {
            assert!(
                crate::actions::portable_cover_value(&url),
                "the production loader must accept the image"
            );
            let encoded = url
                .strip_prefix("data:image/png;base64,")
                .expect("portable cover");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .expect("encoded image");
            let image = image::load_from_memory(&bytes).expect("real image data");
            assert_eq!((image.width(), image.height()), (512, 512));
            assert!(unique.insert(bytes), "each cover is distinct");
        }
    }
}
