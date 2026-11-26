//! What the crash reports publish to programs: the reports this computer recorded (`reports` of
//! `crash-reports`), each what happened, when, and what the kernel met, and one report's text
//! (`crash-report:<id>`) with the stacks it kept. A report is read, never edited; the list offers
//! one change, removing a report from this computer.
//!
//! The file beside this crate ([`crash-reports-surface.json`]) is what `tilolang check` compiles
//! a view against and a host reads the package from. The test here writes it from the types the
//! service reads as and the actions it declares, so the two cannot drift.

#[cfg(test)]
mod tests {
    use tilo_resource::NoEffect;
    use tilo_resource::surface::reading_surface;

    use crate::reports::{CrashReportText, Listed};
    use crate::service::CrashReportsAction;

    /// The file a tilolang view compiles the reports against is what this crate serves.
    /// `UPDATE_SNAPSHOTS=1` writes it.
    #[test]
    fn the_crash_reports_surface_file_is_what_this_crate_publishes() {
        let surfaces = [
            reading_surface::<Listed, CrashReportsAction>("crash-reports", "Crash reports"),
            reading_surface::<CrashReportText, NoEffect>("crash-report-text", "Crash report"),
        ];
        let text = tilo_surface_compile::canonical(&serde_json::json!({ "resources": surfaces }));
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("crash-reports-surface.json");
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
            std::fs::write(&path, &text).expect("the surface file is written");
            return;
        }
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some(text.as_str()),
            "crates/services/crash-reports/crash-reports-surface.json is not what this crate \
             publishes; regenerate it with UPDATE_SNAPSHOTS=1"
        );
    }
}
