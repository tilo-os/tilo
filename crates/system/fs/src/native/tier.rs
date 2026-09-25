//! Choosing the data plane at startup.
//!
//! The tier follows from probes run against the store volume, never from a
//! kernel version string: the dedicated-volume check first (a filesystem
//! mark on a shared volume would put every file on the machine behind the
//! daemon), then the feature probes of `fs_native::probe`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Which data plane the operator asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum DataPlane {
    /// Tier 1 when the store volume and kernel support it, else FUSE.
    #[default]
    Auto,
    /// Always the FUSE data plane (Tier 2/3).
    Fuse,
    /// Tier 1 or fail: an operator who asks for the native plane is never
    /// silently given FUSE.
    Native,
}

/// The outcome of tier selection, for the startup log and the decision.
#[derive(Clone, Debug)]
pub struct TierSelection {
    /// Serve Tier 1 from the store volume.
    pub native: bool,
    pub store: PathBuf,
    /// One line per reason, in the order they were decided.
    pub reasons: Vec<String>,
    /// The full probe report, when probes ran.
    pub report: Option<String>,
    pub probe_time: Duration,
}

impl TierSelection {
    /// One line for the service log.
    pub fn summary(&self) -> String {
        if self.native {
            format!(
                "Data plane: Tier 1 (native tree on store volume {}, probed in {} ms)",
                self.store.display(),
                self.probe_time.as_millis()
            )
        } else {
            format!("Data plane: FUSE (Tier 2/3): {}", self.reasons.join("; "))
        }
    }
}

fn fuse(store: &Path, reasons: Vec<String>, report: Option<String>, t: Instant) -> TierSelection {
    TierSelection {
        native: false,
        store: store.to_path_buf(),
        reasons,
        report,
        probe_time: t.elapsed(),
    }
}

/// Decides the data plane for a workspace whose store volume would be
/// `store`.
pub fn select(choice: DataPlane, store: &Path) -> TierSelection {
    let t = Instant::now();
    if choice == DataPlane::Fuse {
        return fuse(store, vec!["FUSE requested".into()], None, t);
    }
    if !store.is_dir() {
        return fuse(
            store,
            vec![format!("no store volume at {}", store.display())],
            None,
            t,
        );
    }
    if let Err(error) = fs_native::store::check_dedicated(store) {
        return fuse(
            store,
            vec![format!("{} is {error}", store.display())],
            None,
            t,
        );
    }
    let report = match fs_native::probe::probe(store, None) {
        Ok(report) => report,
        Err(error) => {
            return fuse(store, vec![format!("probes failed: {error}")], None, t);
        }
    };
    let decision = report.decide();
    let rendered = Some(report.render());
    if decision.tier == fs_native::probe::Tier::Native {
        TierSelection {
            native: true,
            store: store.to_path_buf(),
            reasons: decision.reasons,
            report: rendered,
            probe_time: t.elapsed(),
        }
    } else {
        fuse(store, decision.reasons, rendered, t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_shared_store_selects_fuse_with_a_reason() {
        let missing = select(DataPlane::Auto, Path::new("/nonexistent/tilo-store"));
        assert!(!missing.native);
        assert!(missing.summary().contains("no store volume"), "{missing:?}");

        let temp = tempfile::tempdir().unwrap();
        let shared = select(DataPlane::Auto, temp.path());
        assert!(!shared.native);
        assert!(
            shared.report.is_none(),
            "probes never run on a shared volume"
        );

        let forced = select(DataPlane::Fuse, temp.path());
        assert_eq!(forced.reasons, ["FUSE requested"]);
    }
}
