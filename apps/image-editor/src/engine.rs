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

/// Decode the source at `path`, or the kept image an [`OPENED_SOURCE`] names.
pub fn open_source(path: &str) -> Result<RgbaImage, EvalError> {
    open_bounded(path)
}

fn open_bounded(path: &str) -> Result<RgbaImage, EvalError> {
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
