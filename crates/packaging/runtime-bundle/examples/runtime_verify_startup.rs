//! Launch-path cost of verifying a per-user runtime.
//!
//! Builds a signed bundle whose toolkit dylib is 256 MiB (the x86-64 runtime
//! is 260 MB, 240 MB of it `libruntime_dylib.so`) and times, warm, the two
//! ways a launch can establish its bytes: hashing all of them
//! (`verify_bundle_with`, what every launch did) and the fs-verity path
//! (`verify_bundle_verity_with`, what the launcher does now), before and
//! after sealing the bundle as the installer does. Budget: 2 ms sealed.
//!
//! ```sh
//! cargo run --release -p runtime-bundle --example runtime_verify_startup -- [DIR]
//! ```
//!
//! `DIR` (default: `TILO_FSVERITY_DIR`, else the system temporary directory)
//! must be on btrfs or ext4 with `verity` for the sealed numbers.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use runtime_bundle::{
    BuildOptions, Identity, PayloadCheck, VerifyOptions, build_bundle_with, enable_bundle_verity,
    verify_bundle_metadata_with, verify_bundle_verity_with, verify_bundle_with,
};

const DYLIB_SIZE: usize = 256 << 20;
const RUNS: usize = 7;
const BUDGET: Duration = Duration::from_millis(2);

type Error = Box<dyn std::error::Error>;

fn main() -> Result<(), Error> {
    let parent = std::env::args_os()
        .nth(1)
        .or_else(|| std::env::var_os("TILO_FSVERITY_DIR"))
        .map_or_else(std::env::temp_dir, PathBuf::from);
    let work = tempfile::tempdir_in(&parent)?;
    let host = work.path().join("host");
    let loader = host.join("lib64/ld-linux-x86-64.so.2");
    std::fs::create_dir_all(host.join("lib64"))?;
    std::fs::write(&loader, b"loader")?;
    let dylib = work.path().join("libruntime_dylib.so");
    let bytes: Vec<u8> = (0..=250u8).cycle().take(DYLIB_SIZE).collect();
    std::fs::write(&dylib, bytes)?;
    let identity = Identity::generate();
    let identity_path = work.path().join("identity.json");
    std::fs::write(&identity_path, identity.to_json_bytes())?;
    let bundle = work.path().join("bundle");
    build_bundle_with(
        &bundle,
        "2026.10",
        &BuildOptions {
            from: Some(&host),
            arch: Some("x86_64"),
            identity: Some(&identity_path),
            extra_libs: &[dylib],
        },
    )?;
    let policy = VerifyOptions {
        require_publisher: Some(identity.public_key_hex()),
        require_signature: true,
        require_arch: None,
    };
    println!(
        "bundle: {} ({} MiB toolkit)",
        bundle.display(),
        DYLIB_SIZE >> 20
    );

    let full = median(RUNS, || verify_bundle_with(&bundle, &policy).map(|_| ()))?;
    println!("full hash (launch before the change):  {full:?}");
    let (unsealed, check) = timed_verity(&bundle, &policy)?;
    println!("verity path, unsealed (fallback):   {unsealed:?}  {check:?}");
    // Manifest, signatures and one stat per file: the sealed path's work
    // without its one FS_IOC_MEASURE_VERITY per file, on any filesystem.
    let metadata = median(RUNS, || {
        verify_bundle_metadata_with(&bundle, &policy).map(|_| ())
    })?;
    println!("manifest + signatures + stats only: {metadata:?}");

    let manifest = verify_bundle_with(&bundle, &policy)?;
    match enable_bundle_verity(&bundle, &manifest) {
        Ok(sealed_files) => {
            let (sealed, check) = timed_verity(&bundle, &policy)?;
            println!("verity path, sealed {sealed_files} files:      {sealed:?}  {check:?}");
            if check.hashed_bytes > 0 || sealed > BUDGET {
                return Err(format!("over budget: {sealed:?} > {BUDGET:?} or payload read").into());
            }
        }
        Err(error) => println!("not sealed: {error} (use a btrfs or ext4+verity DIR)"),
    }
    Ok(())
}

fn timed_verity(
    bundle: &std::path::Path,
    policy: &VerifyOptions,
) -> Result<(Duration, PayloadCheck), Error> {
    let mut check = PayloadCheck::default();
    let elapsed = median(RUNS, || {
        verify_bundle_verity_with(bundle, policy).map(|(_, seen)| check = seen)
    })?;
    Ok((elapsed, check))
}

/// Median of `runs` warm runs of `f` (after one warm-up).
fn median<E: Into<Error>>(
    runs: usize,
    mut f: impl FnMut() -> Result<(), E>,
) -> Result<Duration, Error> {
    f().map_err(Into::into)?;
    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        let started = Instant::now();
        f().map_err(Into::into)?;
        times.push(started.elapsed());
    }
    times.sort();
    Ok(times.get(runs / 2).copied().unwrap_or_default())
}
