//! Thumbnailing a batch of images as a content transform: the same bytes in
//! give the same PNGs out, whichever of a person's devices runs it.
//!
//! That property is what makes the batch offloadable. A device that would
//! rather not decode a few hundred camera originals itself hands the batch to
//! another of its person's devices, which runs [`ENTRYPOINT`] of the signed
//! [`PACKAGE`] under its launcher sandbox ([`run_compute_request`], the
//! package side of a compute job), and gets back exactly what
//! [`thumbnail_batch`] would have produced locally.
//!
//! A batch is per input: an input that is not a decodable image makes no
//! thumbnail of its own ([`Undecodable`]) while every other input still does,
//! locally and on a worker alike. The failure names the picture by its
//! content digest, never by a position in a deduplicated vector, so a caller
//! can tell the person which picture to look at.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use crate::generate::{encode_png, image_thumb};

/// The longest edge, in pixels, of every thumbnail in a batch.
pub const BATCH_EDGE: u32 = 256;
/// The signed package that carries [`ENTRYPOINT`].
pub const PACKAGE: &str = "so.tilo.thumbnails";
/// The package's catalogue command that thumbnails a batch.
pub const ENTRYPOINT: &str = "thumbnail-batch";
/// The exit status of [`ENTRYPOINT`] for a malformed or unreadable request.
pub const EXIT_REQUEST: u8 = 2;

/// Why a batch could not run at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchError {
    /// The compute request could not be read or the outputs not written.
    Request(String),
}

impl fmt::Display for BatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(detail) => write!(formatter, "compute request: {detail}"),
        }
    }
}

impl std::error::Error for BatchError {}

impl BatchError {
    /// The exit status [`ENTRYPOINT`] reports this failure with.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        EXIT_REQUEST
    }
}

/// One input that made no thumbnail: the content's own digest, which is how
/// the rest of Tilo names the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Undecodable {
    /// The BLAKE3 digest of the input bytes.
    pub digest: [u8; 32],
}

impl fmt::Display for Undecodable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "the content {} is not an image",
            hex::encode(self.digest)
        )
    }
}

impl std::error::Error for Undecodable {}

/// A PNG thumbnail, at most [`BATCH_EDGE`] on its longest edge, of each of
/// `inputs`, in order, or why that input made none. One undecodable input
/// does not sink the rest.
///
/// The work is spread over the device's cores; the output order, and every
/// byte of it, is independent of how.
pub fn thumbnail_batch(inputs: &[Vec<u8>]) -> Vec<Result<Vec<u8>, Undecodable>> {
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(inputs.len().max(1));
    let per_worker = inputs.len().div_ceil(workers).max(1);
    let mut thumbnails: Vec<Option<Result<Vec<u8>, Undecodable>>> = vec![None; inputs.len()];
    std::thread::scope(|scope| {
        for (inputs, slots) in inputs
            .chunks(per_worker)
            .zip(thumbnails.chunks_mut(per_worker))
        {
            scope.spawn(move || {
                for (input, slot) in inputs.iter().zip(slots) {
                    *slot = Some(
                        image_thumb(input, BATCH_EDGE)
                            .and_then(|thumb| encode_png(&thumb))
                            .map_or_else(
                                || {
                                    Err(Undecodable {
                                        digest: *blake3::hash(input).as_bytes(),
                                    })
                                },
                                Ok,
                            ),
                    );
                }
            });
        }
    });
    // Every slot is filled by its worker; one left empty reads as its input
    // having made no thumbnail rather than taking the batch down.
    thumbnails
        .into_iter()
        .zip(inputs)
        .map(|(thumbnail, input)| {
            thumbnail.unwrap_or_else(|| {
                Err(Undecodable {
                    digest: *blake3::hash(input).as_bytes(),
                })
            })
        })
        .collect()
}

/// The part of a staged compute request the entrypoint reads: its inputs,
/// by hex digest, in the order the requester listed them.
#[derive(serde::Deserialize)]
struct StagedRequest {
    inputs: Vec<String>,
}

/// The name of output `index`: zero-padded, so a worker that collects its
/// outputs by name returns them in batch order.
#[must_use]
pub fn output_name(index: usize) -> String {
    format!("{index:05}.png")
}

/// Run one compute job's batch: read the staged request at `request` (its
/// inputs are staged beside it, under `inputs/`, named by hex digest), and
/// write the thumbnail of each distinct input, in the order the request
/// lists them, into `outputs` as [`output_name`]. An input listed twice is
/// one input, as the worker stages it once.
///
/// Returns the inputs that made no thumbnail, named by digest; the rest are
/// written. The job succeeds with those outputs, as the local batch does.
///
/// # Errors
///
/// [`BatchError::Request`] when the request or an input cannot be read or an
/// output cannot be written.
pub fn run_compute_request(request: &Path, outputs: &Path) -> Result<Vec<Undecodable>, BatchError> {
    let request_error = |error: io::Error| BatchError::Request(error.to_string());
    let staged: StagedRequest = serde_json::from_slice(&fs::read(request).map_err(request_error)?)
        .map_err(|error| BatchError::Request(error.to_string()))?;
    let input_dir = request
        .parent()
        .ok_or_else(|| BatchError::Request("request has no directory".into()))?
        .join("inputs");
    let mut seen = std::collections::BTreeSet::new();
    let mut inputs = Vec::with_capacity(staged.inputs.len());
    for digest in staged.inputs {
        if !tilo_content::valid_hash(&digest) {
            return Err(BatchError::Request("input is not a digest".into()));
        }
        if seen.insert(digest.clone()) {
            inputs.push(fs::read(input_dir.join(&digest)).map_err(request_error)?);
        }
    }
    let mut failed = Vec::new();
    for (index, thumbnail) in thumbnail_batch(&inputs).into_iter().enumerate() {
        match thumbnail {
            Ok(thumbnail) => {
                fs::write(outputs.join(output_name(index)), thumbnail).map_err(request_error)?;
            }
            Err(error) => failed.push(error),
        }
    }
    Ok(failed)
}

/// [`ENTRYPOINT`]'s command line: `--tilo-compute-request <request.json>
/// --tilo-compute-output <dir>`, as the compute worker launches it.
///
/// # Errors
///
/// [`BatchError::Request`] for any other command line, or what
/// [`run_compute_request`] reports.
pub fn run_from_args(
    args: impl IntoIterator<Item = String>,
) -> Result<Vec<Undecodable>, BatchError> {
    let mut request = None;
    let mut outputs = None;
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let slot = match flag.as_str() {
            "--tilo-compute-request" => &mut request,
            "--tilo-compute-output" => &mut outputs,
            _ => return Err(BatchError::Request(format!("unexpected argument {flag:?}"))),
        };
        *slot = args.next();
    }
    match (request, outputs) {
        (Some(request), Some(outputs)) => {
            run_compute_request(Path::new(&request), Path::new(&outputs))
        }
        _ => Err(BatchError::Request(
            "usage: --tilo-compute-request <file> --tilo-compute-output <dir>".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(width: u32, height: u32, seed: u8) -> Vec<u8> {
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([
                u8::try_from(x % 256).unwrap_or(0) ^ seed,
                u8::try_from(y % 256).unwrap_or(0),
                seed,
            ])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn a_batch_is_ordered_deterministic_and_fails_per_input() {
        let inputs = vec![jpeg(1200, 800, 1), jpeg(640, 960, 2), jpeg(300, 300, 3)];
        let first = thumbnail_batch(&inputs);
        assert_eq!(first, thumbnail_batch(&inputs));
        assert_eq!(first.len(), 3);
        for (thumbnail, (width, height)) in first.iter().zip([(256, 171), (171, 256), (256, 256)]) {
            let decoded = crate::decode_png(thumbnail.as_ref().unwrap()).unwrap();
            assert_eq!((decoded.width, decoded.height), (width, height));
        }
        let mut broken = inputs;
        let bad = b"not an image".to_vec();
        broken.insert(1, bad.clone());
        let made = thumbnail_batch(&broken);
        assert_eq!(made.len(), 4);
        assert!(made[0].is_ok() && made[2].is_ok() && made[3].is_ok());
        assert_eq!(
            made[1],
            Err(Undecodable {
                digest: *blake3::hash(&bad).as_bytes()
            })
        );
        assert!(
            made[1]
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains(&hex::encode(blake3::hash(&bad).as_bytes()))
        );
    }

    #[test]
    fn the_entrypoint_writes_what_the_batch_returns() {
        let root = tempfile::tempdir().unwrap();
        let io_root = root.path().join(".tilo-compute");
        fs::create_dir_all(io_root.join("inputs")).unwrap();
        fs::create_dir_all(io_root.join("outputs")).unwrap();
        let inputs = vec![jpeg(900, 600, 7), jpeg(400, 800, 8)];
        let digests: Vec<String> = inputs
            .iter()
            .map(|bytes| blake3::hash(bytes).to_hex().to_string())
            .collect();
        for (digest, bytes) in digests.iter().zip(&inputs) {
            fs::write(io_root.join("inputs").join(digest), bytes).unwrap();
        }
        // Listed twice, staged once: one output.
        let listed = [digests[0].clone(), digests[1].clone(), digests[0].clone()];
        fs::write(
            io_root.join("request.json"),
            serde_json::json!({"version": 1, "inputs": listed}).to_string(),
        )
        .unwrap();
        run_from_args([
            "--tilo-compute-request".to_string(),
            io_root.join("request.json").display().to_string(),
            "--tilo-compute-output".to_string(),
            io_root.join("outputs").display().to_string(),
        ])
        .unwrap();
        let expected: Vec<_> = thumbnail_batch(&inputs)
            .into_iter()
            .map(Result::unwrap)
            .collect();
        let mut names: Vec<_> = fs::read_dir(io_root.join("outputs"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, vec![output_name(0), output_name(1)]);
        for (name, thumbnail) in names.iter().zip(&expected) {
            assert_eq!(
                &fs::read(io_root.join("outputs").join(name)).unwrap(),
                thumbnail
            );
        }
        assert!(matches!(
            run_from_args(["--shell".to_string(), "sh".to_string()]),
            Err(BatchError::Request(_))
        ));
    }
}
