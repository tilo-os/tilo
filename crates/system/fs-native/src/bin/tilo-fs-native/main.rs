//! Capability report and local benchmarks for the native data plane.
//!
//! ```text
//! tilo-fs-native probe <store> [--k5 FILES,DIRTY_MIB,SAMPLES]
//! tilo-fs-native userns-probe [<btrfs-dir>]
//! tilo-fs-native bench <io|meta|capture|hash|ttfb|all> <store> [--quick]
//! ```
//!
//! `probe` and most benchmarks need `CAP_SYS_ADMIN` and a dedicated Btrfs
//! store volume; `userns-probe` shows what an unprivileged user namespace
//! can and cannot do.

#[cfg(target_os = "linux")]
use std::path::Path;
use std::process::ExitCode;

#[cfg(target_os = "linux")]
use fs_native::probe::{self, K5Params};

#[cfg(target_os = "linux")]
mod bench;
#[cfg(target_os = "linux")]
mod userns;

#[cfg(target_os = "linux")]
fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  tilo-fs-native probe <store> [--k5 FILES,DIRTY_MIB,SAMPLES]\n  \
         tilo-fs-native userns-probe [<btrfs-dir>]\n  \
         tilo-fs-native bench <io|meta|capture|hash|ttfb|all> <store> [--quick]"
    );
    ExitCode::from(2)
}

#[cfg(target_os = "linux")]
fn parse_k5(spec: &str) -> Option<K5Params> {
    let mut it = spec.split(',');
    let files = it.next()?.parse().ok()?;
    let mib: u64 = it.next()?.parse().ok()?;
    let samples = it.next()?.parse().ok()?;
    Some(K5Params {
        files,
        dirty_bytes: mib.saturating_mul(1024 * 1024),
        samples,
    })
}

#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("probe") => {
            let Some(store) = args.get(1) else {
                return usage();
            };
            let k5 = match args.get(2).map(String::as_str) {
                Some("--k5") => match args.get(3).and_then(|s| parse_k5(s)) {
                    Some(p) => Some(p),
                    None => return usage(),
                },
                Some(_) => return usage(),
                None => Some(K5Params::default()),
            };
            match probe::probe(Path::new(store), k5) {
                Ok(report) => {
                    print!("{}", report.render());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("probe failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("userns-probe") => userns::run(args.get(1).map(Path::new)),
        Some("bench") => {
            let (Some(what), Some(store)) = (args.get(1), args.get(2)) else {
                return usage();
            };
            let quick = args.iter().any(|a| a == "--quick");
            bench::run(what, Path::new(store), quick)
        }
        _ => usage(),
    }
}

#[cfg(not(target_os = "linux"))]
fn main() -> ExitCode {
    eprintln!("tilo-fs-native: the native data plane uses Linux kernel interfaces");
    ExitCode::FAILURE
}
