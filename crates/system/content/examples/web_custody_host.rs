//! Run the actual browser fixture through rcargo's admitted `--on-builder run`.
//! Build and bind the WASM probe first; this host never invokes Cargo itself.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--chrome")) {
        return Err("pass --chrome with a qualified remote browser executable".into());
    }
    let chrome = args.next().ok_or("--chrome needs a remote executable")?;
    if !std::fs::metadata(&chrome)?.is_file() {
        return Err("CHROME must name a regular browser executable".into());
    }
    let status = std::process::Command::new("node")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/web/run.mjs"))
        .env("CHROME", chrome)
        .args(args)
        .status()?;
    if !status.success() {
        return Err(format!("browser custody fixture failed: {status}").into());
    }
    Ok(())
}
