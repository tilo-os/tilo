//! Per-process cost of establishing the font catalog's bytes.
//!
//! Times, warm, the two ways a process can trust the catalog at `ROOT`:
//! hashing every file ([`Trust::Verify`], what every process did) and the
//! immutable path ([`Trust::of_root`] plus each file's type, size and
//! [`file_is_immutable`] probe, what a process on the Tilo image does now).
//! The immutable path makes the same system calls whether or not the probe
//! accepts, so its time is meaningful on any host; the trust line says which
//! path this host's root actually takes.
//!
//! ```sh
//! cargo run --release -p tilo-font-catalog --example catalog_verify_startup -- [ROOT]
//! ```
//!
//! `ROOT` defaults to the installed catalog, else the checkout's.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tilo_font_catalog::{Trust, check_manifest, file_is_immutable, locate, manifest};

const RUNS: usize = 9;
const MEDIAN: usize = RUNS / 2;

type Error = Box<dyn std::error::Error>;

fn main() -> Result<(), Error> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(locate)
        .ok_or("no font catalog installed or in the checkout")?
        .canonicalize()?;
    check_manifest(&root)?;
    let assets: Vec<_> = manifest()?
        .font
        .iter()
        .filter(|asset| asset.scope.os() && asset.path_in(&root).is_ok_and(|p| p.is_file()))
        .collect();
    let bytes: u64 = assets.iter().map(|asset| asset.bytes).sum();

    let hashed = median(|| {
        for asset in &assets {
            asset.verify_with(&root, Trust::Verify)?;
        }
        Ok(())
    })?;
    let mut accepted = 0;
    let immutable = median(|| {
        accepted = usize::from(Trust::of_root(&root) == Trust::Immutable);
        for asset in &assets {
            let path = asset.path_in(&root)?;
            let metadata = std::fs::metadata(&path)?;
            if !metadata.is_file() || metadata.len() != asset.bytes {
                return Err(format!("{} differs in size", path.display()).into());
            }
            accepted = accepted.saturating_add(usize::from(file_is_immutable(&path)));
        }
        Ok(())
    })?;

    println!("root: {}", root.display());
    println!(
        "files: {}, bytes: {bytes}, trust here: {:?}",
        assets.len(),
        Trust::of_root(&root)
    );
    println!("hash every file (Verify):          {:>9.3} ms", ms(hashed));
    println!(
        "probe and size only (Immutable):   {:>9.3} ms",
        ms(immutable)
    );
    println!(
        "paths the probe accepted: {accepted} of {} (root and files)",
        assets.len().saturating_add(1)
    );
    Ok(())
}

fn median(mut run: impl FnMut() -> Result<(), Error>) -> Result<Duration, Error> {
    run()?; // warm the page cache and the manifest
    let mut samples = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let start = Instant::now();
        run()?;
        samples.push(start.elapsed());
    }
    samples.sort();
    Ok(samples.get(MEDIAN).copied().ok_or("no samples")?)
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
