#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        fs::OpenOptions,
        os::unix::fs::OpenOptionsExt,
        time::{Duration, Instant},
    };
    use tilo_ntfs_helper::limits;
    let mut args = std::env::args_os().skip(1);
    let path = args.next().ok_or("expected regular image path")?;
    let mut copy_manifest = false;
    let mut read_limit = limits::READ_BYTES;
    let mut operation = limits::NativeOperation::Probe { root: 5 };
    while let Some(arg) = args.next() {
        if arg == "--copy-manifest" {
            copy_manifest = true;
            operation = limits::NativeOperation::Manifest { root: 5 };
        } else if arg == "--manifest" {
            operation = limits::NativeOperation::Manifest { root: 5 };
        } else if arg == "--read-chunk" {
            operation = limits::NativeOperation::ReadChunk {
                request: serde_json::from_str(
                    args.next()
                        .ok_or("missing request")?
                        .to_str()
                        .ok_or("request encoding")?,
                )?,
            };
        } else if arg == "--read-limit" {
            read_limit = args
                .next()
                .ok_or("missing read limit")?
                .to_str()
                .ok_or("invalid read limit")?
                .parse()?;
        } else {
            return Err("unknown diagnostic argument".into());
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err("image test requires a regular file".into());
    }
    let report = limits::run_operation(
        &file,
        file.metadata()?.len(),
        operation,
        read_limit,
        None,
        Instant::now()
            .checked_add(Duration::from_secs(limits::WALL_SECONDS))
            .ok_or("wall deadline overflow")?,
    )?;
    if copy_manifest {
        let manifest = tilo_ntfs_helper::manifest::verify(&report, 5)?;
        let identity = tilo_ntfs_helper::protocol::identity(&report, 5)?;
        println!(
            "{}",
            serde_json::json!({
                "diagnostic_only": true, "stats": report.stats,
                "identity": {"volume": identity.volume, "root": identity.root, "upcase": identity.upcase_sha256},
                "native_identity": identity, "entries": manifest.entries,
                "issues": manifest.issues, "upcase": manifest.upcase,
                "facts_digest": tilo_ntfs_helper::manifest::metadata_digest(&report.facts, 5)?
            })
        );
    } else {
        println!("{}", serde_json::to_string(&report)?);
    }
    Ok(())
}
#[cfg(not(unix))]
fn main() -> Result<(), &'static str> {
    Err("NTFS diagnostic reader requires Unix; no capability on this target")
}
