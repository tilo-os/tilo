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
    if let Some(arg) = args.next() {
        if arg != "--read-limit" {
            return Err("unknown diagnostic argument".into());
        }
        read_limit = args
            .next()
            .ok_or("missing read limit")?
            .to_str()
            .ok_or("invalid read limit")?
            .parse()?;
    }
    if args.next().is_some() {
        return Err("unexpected diagnostic argument".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err("image test requires a regular file".into());
    }
    let report = limits::run(
        &file,
        file.metadata()?.len(),
        5,
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
