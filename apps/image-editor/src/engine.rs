//! Deterministic evaluator shared by preview and export.
use crate::model::{Operation, Project};
use image::{DynamicImage, RgbaImage, imageops};
use std::collections::BTreeMap;
const MAX_IMAGE_DIMENSION: u32 = 32_768;
const MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;
#[derive(Debug)]
pub enum EvalError {
    InvalidGraph,
    MissingOperation(String),
    MissingInput(String),
    Open(String),
    Save(String),
}

/// A source image kept in the app's state instead of at a path: one opened
/// by contents, where files have no path (a browser page).
pub const OPENED_SOURCE: &str = "opened:";

/// The bundled sample photograph, a harbour at dusk. It needs no file, so a
/// first look at the editor and its screenshots open the same picture.
pub const SAMPLE_SOURCE: &str = "sample:harbour-light.png";

/// Decode the source at `path`, or the kept image an [`OPENED_SOURCE`] names.
pub fn open_source(path: &str) -> Result<RgbaImage, EvalError> {
    open_bounded(path)
}

/// The name a person knows the source by: its file name, without the
/// scheme of a kept or bundled image.
pub fn source_file_name(path: &str) -> &str {
    let path = path
        .strip_prefix(OPENED_SOURCE)
        .or_else(|| path.strip_prefix("sample:"))
        .unwrap_or(path);
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

/// The bundled sample: a harbour at dusk with a lighthouse on the
/// breakwater and the low sun on the water.
pub fn sample_image() -> RgbaImage {
    const WIDTH: u32 = 1200;
    const HEIGHT: u32 = 800;
    let mix = |a: [f32; 3], b: [f32; 3], t: f32| -> [f32; 3] {
        let t = t.clamp(0.0, 1.0);
        [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
        ]
    };
    let horizon = 0.6_f32;
    let sun = (0.66_f32, 0.47_f32);
    let mut image = RgbaImage::new(WIDTH, HEIGHT);
    for (y, row) in image.rows_mut().enumerate() {
        let fy = y as f32 / HEIGHT as f32;
        for (x, p) in row.enumerate() {
            let fx = x as f32 / WIDTH as f32;
            let dx = (fx - sun.0) * 1.5;
            let sun_distance = (dx * dx + (fy - sun.1) * (fy - sun.1)).sqrt();
            let glow = (1.0 - sun_distance / 0.35).max(0.0).powi(2);
            let mut colour = if fy < horizon {
                // Sky: deep blue overhead to a warm band at the horizon.
                let t = fy / horizon;
                let sky = mix([38.0, 52.0, 102.0], [214.0, 128.0, 112.0], t.powf(1.6));
                let sky = mix(sky, [252.0, 196.0, 140.0], (t - 0.75).max(0.0) * 3.0);
                mix(sky, [255.0, 214.0, 160.0], glow * 0.8)
            } else {
                // Water: darker with depth, the sun's path glittering on it.
                let t = (fy - horizon) / (1.0 - horizon);
                let water = mix([96.0, 84.0, 112.0], [22.0, 34.0, 58.0], t.powf(0.7));
                let ripple = ((fy * 900.0).sin() * (fx * 37.0 + fy * 11.0).cos()).max(0.0);
                let path = (1.0 - ((fx - sun.0).abs() / (0.05 + t * 0.12))).max(0.0);
                mix(
                    water,
                    [255.0, 206.0, 150.0],
                    path * (0.35 + 0.65 * ripple) * 0.9,
                )
            };
            if sun_distance < 0.045 && fy < horizon {
                colour = [255.0, 236.0, 196.0];
            }
            // Hills behind the harbour on the left.
            let hill = horizon - 0.09 * (1.0 - fx / 0.5).max(0.0) * (1.0 + (fx * 9.0).sin() * 0.15);
            if fy < horizon && fy > hill {
                colour = mix([62.0, 48.0, 74.0], colour, 0.25);
            }
            // The breakwater and its lighthouse: a tapering tower with a
            // gallery, a lit lantern and a dark cap.
            let (tower_x, tower_h) = (0.22_f32, 0.2_f32);
            let top = horizon - tower_h;
            let rise = ((horizon - fy) / tower_h).clamp(0.0, 1.0);
            let half = 0.016 - 0.005 * rise;
            let from_axis = (fx - tower_x).abs();
            let wall = fx < 0.42 && fy > horizon - 0.012 && fy < horizon + 0.03;
            let tower = from_axis < half && fy > top && fy < horizon;
            let gallery = from_axis < 0.019 && fy > top - 0.007 && fy <= top;
            let cap = from_axis < 0.012 && fy > top - 0.05 && fy < top - 0.04;
            let lantern = from_axis < 0.009 && fy >= top - 0.04 && fy <= top - 0.007;
            let across = (fx - tower_x) * 1.5;
            let below = fy - (top - 0.024);
            let lamp_glow = (1.0 - (across * across + below * below).sqrt() / 0.07)
                .max(0.0)
                .powi(2);
            if fy < horizon {
                colour = mix(colour, [255.0, 226.0, 160.0], lamp_glow * 0.55);
            }
            if wall || tower || gallery || cap {
                colour = [30.0, 26.0, 40.0];
            }
            if lantern {
                colour = [255.0, 232.0, 168.0];
            }
            *p = image::Rgba([
                colour[0].clamp(0.0, 255.0) as u8,
                colour[1].clamp(0.0, 255.0) as u8,
                colour[2].clamp(0.0, 255.0) as u8,
                255,
            ]);
        }
    }
    image
}

fn open_bounded(path: &str) -> Result<RgbaImage, EvalError> {
    if path == SAMPLE_SOURCE {
        return Ok(sample_image());
    }
    if let Some(name) = path.strip_prefix(OPENED_SOURCE) {
        let file = crate::web_file::recall()
            .filter(|file| file.name == name)
            .ok_or_else(|| EvalError::Open("the opened image is no longer kept".into()))?;
        let reader = image::ImageReader::new(std::io::Cursor::new(file.bytes))
            .with_guessed_format()
            .map_err(|error| EvalError::Open(error.to_string()))?;
        return decode_bounded(reader);
    }
    let reader = image::ImageReader::open(path)
        .and_then(image::ImageReader::with_guessed_format)
        .map_err(|error| EvalError::Open(error.to_string()))?;
    decode_bounded(reader)
}

fn decode_bounded<R: std::io::BufRead + std::io::Seek>(
    mut reader: image::ImageReader<R>,
) -> Result<RgbaImage, EvalError> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    reader
        .decode()
        .map(DynamicImage::into_rgba8)
        .map_err(|error| EvalError::Open(error.to_string()))
}
pub fn evaluate(project: &Project) -> Result<RgbaImage, EvalError> {
    let order = project
        .graph
        .evaluation_order()
        .map_err(|_| EvalError::InvalidGraph)?;
    let mut values: BTreeMap<String, RgbaImage> = BTreeMap::new();
    for id in order {
        let op = project
            .operations
            .get(&id)
            .ok_or_else(|| EvalError::MissingOperation(id.clone()))?;
        let input = || {
            project
                .graph
                .edges
                .iter()
                .find(|e| e.to_node == id)
                .and_then(|e| values.get(&e.from_node))
                .cloned()
                .ok_or_else(|| EvalError::MissingInput(id.clone()))
        };
        let output = match op {
            Operation::Source { path } => open_bounded(path)?,
            Operation::Output => input()?,
            Operation::Rotate { quarter_turns } => {
                let v = input()?;
                match quarter_turns % 4 {
                    1 => imageops::rotate90(&v),
                    2 => imageops::rotate180(&v),
                    3 => imageops::rotate270(&v),
                    _ => v,
                }
            }
            Operation::Crop {
                x,
                y,
                width,
                height,
            } => {
                let v = input()?;
                let w = v.width();
                let h = v.height();
                let px = (*x).clamp(0.0, 1.0) * (w as f32);
                let py = (*y).clamp(0.0, 1.0) * (h as f32);
                let pw = ((*width).clamp(0.001, 1.0) * (w as f32)).min((w as f32) - px);
                let ph = ((*height).clamp(0.001, 1.0) * (h as f32)).min((h as f32) - py);
                imageops::crop_imm(
                    &v,
                    px as u32,
                    py as u32,
                    pw.max(1.0) as u32,
                    ph.max(1.0) as u32,
                )
                .to_image()
            }
            Operation::Exposure { stops } => map(input()?, |c| {
                ((c as f32) * 2.0f32.powf(*stops)).clamp(0.0, 255.0) as u8
            }),
            Operation::Contrast { amount } => map(input()?, |c| {
                ((((c as f32) / 255.0 - 0.5) * (1.0 + *amount) + 0.5) * 255.0).clamp(0.0, 255.0)
                    as u8
            }),
            Operation::Saturation { amount } => {
                let mut v = input()?;
                for p in v.pixels_mut() {
                    let l = 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
                    for c in 0..3 {
                        p[c] = (l + (p[c] as f32 - l) * *amount).clamp(0.0, 255.0) as u8;
                    }
                }
                v
            }
            Operation::Grayscale => {
                let mut v = input()?;
                for p in v.pixels_mut() {
                    let l =
                        (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) as u8;
                    p[0] = l;
                    p[1] = l;
                    p[2] = l;
                }
                v
            }
        };
        values.insert(id, output);
    }
    values
        .remove(&project.output)
        .ok_or_else(|| EvalError::MissingInput(project.output.clone()))
}
fn map(mut image: RgbaImage, f: impl Fn(u8) -> u8) -> RgbaImage {
    for p in image.pixels_mut() {
        p[0] = f(p[0]);
        p[1] = f(p[1]);
        p[2] = f(p[2]);
    }
    image
}
/// The evaluated image as PNG bytes, for a save by contents.
pub fn export_png(project: &Project) -> Result<Vec<u8>, EvalError> {
    let mut out = std::io::Cursor::new(Vec::new());
    evaluate(project)?
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(|e| EvalError::Save(e.to_string()))?;
    Ok(out.into_inner())
}
pub fn export(project: &Project, path: &std::path::Path) -> Result<(), EvalError> {
    evaluate(project)?
        .save(path)
        .map_err(|e| EvalError::Save(e.to_string()))
}
/// Evaluate the image exactly after `node`, for adjustment-stack thumbnails
/// and before/after scrubbing. The graph stays intact; choosing a different
/// output merely asks the deterministic evaluator for an earlier value.
pub fn evaluate_at(project: &Project, node: &str) -> Result<RgbaImage, EvalError> {
    if !project.graph.nodes.contains_key(node) {
        return Err(EvalError::MissingOperation(node.into()));
    }
    let mut snapshot = project.clone();
    snapshot.output = node.into();
    evaluate(&snapshot)
}
#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;
    /// Graph evaluation: upstream changes keep later steps, intermediate previews are
    /// the named step, and oversized inputs are rejected before allocation.
    #[test]
    fn evaluation_follows_the_graph_and_bounds_inputs() {
        changing_upstream_preserves_later_steps();
        intermediate_preview_is_the_named_step_not_the_final_output();
        oversized_dimensions_are_rejected_before_editor_allocation();
    }

    fn changing_upstream_preserves_later_steps() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("in.png");
        RgbaImage::from_pixel(2, 1, Rgba([64, 32, 16, 255]))
            .save(&src)
            .unwrap();
        let mut p = Project::blank();
        if let Operation::Source { path } = p.operations.get_mut("source").unwrap() {
            *path = src.to_string_lossy().into();
        }
        p.insert_before_output("exposure", "Exposure", Operation::Exposure { stops: 1.0 });
        p.insert_before_output("gray", "Grayscale", Operation::Grayscale);
        let a = evaluate(&p).unwrap();
        p.operations
            .insert("exposure".into(), Operation::Exposure { stops: 2.0 });
        let b = evaluate(&p).unwrap();
        assert_ne!(a, b);
        assert_eq!(b.get_pixel(0, 0)[0], b.get_pixel(0, 0)[1]);
    }
    fn intermediate_preview_is_the_named_step_not_the_final_output() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("in.png");
        RgbaImage::from_pixel(1, 1, Rgba([80, 20, 10, 255]))
            .save(&src)
            .unwrap();
        let mut p = Project::blank();
        if let Operation::Source { path } = p.operations.get_mut("source").unwrap() {
            *path = src.to_string_lossy().into();
        }
        p.insert_before_output("bright", "Exposure", Operation::Exposure { stops: 1.0 });
        p.insert_before_output("gray", "Grayscale", Operation::Grayscale);
        let bright = evaluate_at(&p, "bright").unwrap();
        let final_image = evaluate(&p).unwrap();
        assert_ne!(bright, final_image);
        assert_ne!(bright.get_pixel(0, 0)[0], bright.get_pixel(0, 0)[1]);
        assert_eq!(
            final_image.get_pixel(0, 0)[0],
            final_image.get_pixel(0, 0)[1]
        );
    }

    fn oversized_dimensions_are_rejected_before_editor_allocation() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("too-wide.png");
        RgbaImage::from_pixel(MAX_IMAGE_DIMENSION + 1, 1, Rgba([1, 2, 3, 255]))
            .save(&source)
            .unwrap();
        let error = open_bounded(source.to_str().unwrap()).unwrap_err();
        assert!(matches!(error, EvalError::Open(_)));
    }
}
