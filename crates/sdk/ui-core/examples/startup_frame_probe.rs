//! Synthetic native SDK application for the sandboxed startup timeline.
//! This is a test payload, not an installed system app or an enrollment path.

use tilo_ui::{App, State, wake::WakeScope, widget::Text};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    verify_native_confinement()?;
    tilo_startup_metrics::init();
    tilo_startup_metrics::mark("probe-main-start");
    let wake_scope = WakeScope::new();
    let _entry = wake_scope.enter();
    let app = App::from_scoped_parts(
        wake_scope,
        State::new(),
        Box::new(Text::new("Startup frame probe")),
    )
    .with_window_app_id("io.tilo.startup-frame-probe")
    .without_native_document_chrome();
    tilo_startup_metrics::mark("probe-app-ready");
    app.run();
    tilo_startup_metrics::dump();
    Ok(())
}

fn verify_native_confinement() -> std::io::Result<()> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let no_new_privs = status.lines().any(|line| line == "NoNewPrivs:\t1");
    if !no_new_privs {
        return Err(std::io::Error::other(
            "native startup probe requires no_new_privs",
        ));
    }
    eprintln!("startup-frame-probe: no_new_privs={no_new_privs}");
    if let Some(root) = std::env::var_os("TILO_PIPELINE_SEED_DIR") {
        let manifest = std::path::PathBuf::from(root).join("mesa/seed.json");
        if manifest.is_file() {
            let error = std::fs::OpenOptions::new()
                .write(true)
                .open(manifest)
                .err()
                .ok_or_else(|| {
                    std::io::Error::other("ordinary public seed allowed app write access")
                })?;
            if !matches!(error.raw_os_error(), Some(libc::EACCES | libc::EROFS)) {
                return Err(error);
            }
            eprintln!("startup-frame-probe: public seed write-open denied");
        }
    }
    Ok(())
}
