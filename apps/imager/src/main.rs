//! Disk Imager — thin binary entry point. All logic lives in the library.
//!
//! The privileged and staging helpers this binary also is run first; then it
//! is the framework app [`imager::ImagerShell`]: the write wizard, the guided
//! setup with `--setup`, the folder imports with `--import-folders`, and the
//! standard `--shots`, `--shots-dir` and `--headless` modes over its
//! scenarios.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// # Errors
///
/// Returns an error when a helper or a packaging probe fails.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(target_os = "linux")]
    if let Some(code) = imager::setup::migration_native::maybe_run_command(&args)? {
        std::process::exit(code);
    }
    if let Some(code) = imager::setup::desktop_worker::maybe_run_desktop_helper(&args)? {
        std::process::exit(code);
    }
    if let Some(code) = imager::write::maybe_run_write_helper(&args)? {
        std::process::exit(code);
    }
    if let Some(code) = imager::setup::staging::helper::maybe_run_staging_helper(&args)? {
        std::process::exit(code);
    }
    if let Some(code) = imager::setup::staging::final_bootnext::maybe_run(&args)? {
        std::process::exit(code);
    }
    if let Some(code) = imager::setup::staging::cli::maybe_run_staging_request(&args)? {
        std::process::exit(code);
    }
    // Packaging smoke probe: reaching this point proves the executable and
    // all runtime-linked libraries load without starting graphics, portals,
    // device discovery, or a privileged path.
    if args.iter().any(|arg| arg == "--tilo-package-smoke") {
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--tilo-flatpak-probe-smoke") {
        if !cfg!(feature = "flatpak-media-writer")
            || std::env::var("TILO_IMAGER_FLATPAK").as_deref() != Ok("1")
        {
            return Err("the Flatpak probe smoke test is outside its sandbox".into());
        }
        let drives = imager::probe::probe()
            .ok_or("the Flatpak could not reconcile lsblk with host UDisks2 inventory")?;
        if drives.is_empty() {
            return Err("the Flatpak returned an empty drive inventory".into());
        }
        return Ok(());
    }
    ui::launch::run_or_exit(
        &ui::framework::launch_spec::<imager::ImagerShell>()
            .runtime_attachment(|_| !cfg!(target_os = "windows")),
    );
    Ok(())
}
