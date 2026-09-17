#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use qos_ntfs_helper::limits;
    use std::{
        fs::OpenOptions,
        os::unix::fs::OpenOptionsExt,
        time::{Duration, Instant},
    };
    let mut args = std::env::args_os().skip(1);
    let path = args.next().ok_or("expected regular image path")?;
    let mut read_limit = limits::READ_BYTES;
    let mut operation = limits::NativeOperation::Probe { root: 5 };
    while let Some(arg) = args.next() {
        if arg == "--manifest" {
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
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}
#[cfg(not(unix))]
fn main() -> Result<(), &'static str> {
    Err("NTFS diagnostic reader requires Unix; no capability on this target")
}
