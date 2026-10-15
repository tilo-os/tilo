//! The thumbnails package as a compute worker receives it.
//!
//! The real `tilo-app.toml` and the built `thumbnail-batch`, staged and
//! packed the way `dist/apps/package-apps.sh` packs every app, give a signed
//! `.tapp` that reads back and verifies; its manifest offers exactly the
//! catalogue entrypoint a compute worker requires before it launches
//! anything; and the packaged binary, run with the command line the worker
//! gives it over a request staged the way the worker stages one, writes the
//! very thumbnails the requesting device would have made itself.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use thumbnail::batch::{
    BatchError, ENTRYPOINT, EXIT_UNDECODABLE, PACKAGE, output_name, thumbnail_batch,
};
use tilo_app_package::blobs::BlobHash;
use tilo_app_package::view::read_manifest;
use tilo_app_package::{Manifest, SigningIdentity, Verification, pack, tapp, unpack, verify};

/// The manifest Release / apps packs, before it is staged.
const MANIFEST: &str = include_str!("../tilo-app.toml");
/// The publisher placeholder the release pipeline replaces with its key.
const PLACEHOLDER: &str =
    "\"ed25519:0000000000000000000000000000000000000000000000000000000000000000\"";

/// The package, unpacked from its verified `.tapp` into `dir`.
struct Unpacked {
    _dir: tempfile::TempDir,
    payload: PathBuf,
    manifest: PathBuf,
}

/// Stage, pack, sign, write, read back, verify and unpack the package.
fn packaged() -> Unpacked {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("staging");
    std::fs::create_dir_all(staging.join("bin")).unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_thumbnail-batch"),
        staging.join("bin").join(ENTRYPOINT),
    )
    .unwrap();

    let identity = SigningIdentity::from_seed(&[7; 32]);
    let publisher = identity.public_key_bytes();
    assert_eq!(
        MANIFEST.matches(PLACEHOLDER).count(),
        1,
        "the manifest names the publisher placeholder the release pipeline stages"
    );
    let staged = MANIFEST.replace(
        PLACEHOLDER,
        &format!("\"ed25519:{}\"", hex::encode(publisher)),
    );
    let manifest = Manifest::from_toml_str(&staged).expect("the manifest parses");
    let packed = pack(&staging, &manifest, &identity).expect("the package packs");

    let tapp_path = dir.path().join("thumbnails.tapp");
    tapp::write(
        std::fs::File::create(&tapp_path).unwrap(),
        &packed.meta,
        Some(&packed),
    )
    .unwrap();
    let mut blobs = BTreeMap::<BlobHash, Vec<u8>>::new();
    let read = tapp::read(std::fs::File::open(&tapp_path).unwrap(), &mut blobs)
        .expect("the written tapp reads back");
    assert!(
        read.missing().is_empty(),
        "the tapp carries its whole payload"
    );
    assert!(
        matches!(
            verify(&read.meta, &[publisher]),
            Verification::Trusted { .. }
        ),
        "the tapp verifies against its publisher"
    );

    let payload = dir.path().join("payload");
    unpack(&read.meta, &blobs, &payload).expect("the verified tapp unpacks");
    let manifest = dir.path().join("manifest.toml");
    std::fs::write(&manifest, &read.meta.manifest_bytes).unwrap();
    Unpacked {
        _dir: dir,
        payload,
        manifest,
    }
}

/// A small picture with distinct pixels, encoded as `format`.
fn picture(width: u32, height: u32, seed: u8, format: image::ImageFormat) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([
            u8::try_from(x % 256).unwrap() ^ seed,
            u8::try_from(y % 256).unwrap(),
            seed,
        ])
    });
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut std::io::Cursor::new(&mut bytes), format)
        .unwrap();
    bytes
}

/// Run the packaged entrypoint over `inputs` as the compute worker does:
/// the inputs staged under `inputs/` by digest beside `request.json`, the
/// command line naming the request and an empty output directory. Returns
/// the process output and the files written, by name.
fn run_job(package: &Unpacked, inputs: &[Vec<u8>]) -> (Output, BTreeMap<String, Vec<u8>>) {
    let job = tempfile::tempdir().unwrap();
    let io_root = job.path().join(".tilo-compute");
    let input_dir = io_root.join("inputs");
    let output_dir = io_root.join("outputs");
    std::fs::create_dir_all(&input_dir).unwrap();
    std::fs::create_dir_all(&output_dir).unwrap();
    let digests: Vec<String> = inputs
        .iter()
        .map(|bytes| blake3::hash(bytes).to_hex().to_string())
        .collect();
    for (digest, bytes) in digests.iter().zip(inputs) {
        std::fs::write(input_dir.join(digest), bytes).unwrap();
    }
    let request = serde_json::json!({
        "version": 1,
        "job_id": "00".repeat(16),
        "workspace_id": 1,
        "inputs": digests,
        "output_directory": "/run/tilo/app/.tilo-compute/outputs",
    });
    std::fs::write(
        io_root.join("request.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();

    let output = Command::new(entrypoint(package))
        .arg("--tilo-compute-request")
        .arg(io_root.join("request.json"))
        .arg("--tilo-compute-output")
        .arg(&output_dir)
        .env_clear()
        .output()
        .expect("the packaged entrypoint starts");
    let written = std::fs::read_dir(&output_dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect();
    (output, written)
}

/// The packaged file the worker's launch resolves for the entrypoint.
fn entrypoint(package: &Unpacked) -> PathBuf {
    let manifest = read_manifest(&package.manifest).unwrap();
    let bin = manifest.entrypoint.cli_bin().expect("a cli entrypoint");
    package.payload.join(Path::new(bin))
}

#[test]
fn the_signed_package_offers_the_entrypoint_a_compute_worker_runs() {
    let package = packaged();
    // What `ComputeWorker` checks of the installed manifest before it
    // launches the job, on the manifest the signed root commits to.
    let manifest = read_manifest(&package.manifest).expect("the signed manifest reads");
    assert_eq!(manifest.package_id, PACKAGE);
    assert_eq!(manifest.cli_commands, vec![ENTRYPOINT.to_string()]);
    assert_eq!(manifest.entrypoint.cli_bin(), Some("bin/thumbnail-batch"));
    // A windowless package with no egress and no services of its own.
    assert_eq!(manifest.kind, "service");
    assert_eq!(manifest.sandbox.net.mode, "off");
    assert!(manifest.sandbox.services.is_empty());
    assert!(entrypoint(&package).is_file());
}

#[test]
fn the_packaged_entrypoint_writes_the_thumbnails_the_requester_would_make() {
    let package = packaged();
    let inputs = vec![
        picture(640, 480, 3, image::ImageFormat::Jpeg),
        picture(300, 900, 41, image::ImageFormat::Png),
        picture(64, 64, 200, image::ImageFormat::Png),
    ];
    let (output, written) = run_job(&package, &inputs);
    assert!(
        output.status.success(),
        "thumbnail-batch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: BTreeMap<String, Vec<u8>> = thumbnail_batch(&inputs)
        .unwrap()
        .into_iter()
        .enumerate()
        .map(|(index, thumbnail)| (output_name(index), thumbnail))
        .collect();
    assert_eq!(written.len(), inputs.len());
    assert_eq!(
        written, expected,
        "the worker's thumbnails are byte-identical to the local batch"
    );
}

#[test]
fn the_packaged_entrypoint_fails_a_batch_with_a_non_image_whole() {
    let package = packaged();
    let inputs = vec![
        picture(32, 32, 9, image::ImageFormat::Png),
        b"not a picture".to_vec(),
    ];
    assert_eq!(
        thumbnail_batch(&inputs),
        Err(BatchError::Undecodable { index: 1 })
    );
    let (output, written) = run_job(&package, &inputs);
    assert_eq!(output.status.code(), Some(i32::from(EXIT_UNDECODABLE)));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("input 1 is not an image"),
        "the worker returns the reason on stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(written.is_empty(), "a failed batch writes no thumbnail");
}
