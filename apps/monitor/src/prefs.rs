//! Persisted preferences, stored in the shared Automerge document so they
//! survive restarts and replicate to collaborators. Surviving a restart is
//! the store's doing, not this module's: `main` opens the app's document
//! through `ui::persist` and hands the writer to `App::with_autosave`, so
//! everything written here is on disk within the debounce window. View-level
//! state
//! (current sort, filter text, selected row, active tab) deliberately
//! lives in widget fields instead — see [`crate::app`].
//!
//! Layout: scalar keys written straight onto the app's root object.
//!
//! ```text
//! root
//! ├── "interval_ms"          (Int)  refresh cadence
//! ├── "auto_refresh"         (Bool) periodic sampling enabled
//! ├── "show_kernel_threads"  (Bool)
//! └── "confirm_kill"         (Bool)
//! ```

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue, Value};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Prefs {
    /// Sampler cadence in milliseconds. Cycled through a small set in the
    /// header control.
    pub interval_ms: u64,
    /// Whether periodic sampling is running. Manual Refresh remains available
    /// while paused, making pause a low-power inspection mode rather than a
    /// dead-end state.
    pub auto_refresh: bool,
    /// Whether the process table lists Linux kernel worker threads.
    pub show_kernel_threads: bool,
    /// Whether the kill action pops a confirmation dialog first.
    pub confirm_kill: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            interval_ms: 1500,
            auto_refresh: true,
            show_kernel_threads: false,
            confirm_kill: true,
        }
    }
}

fn get_int(doc: &AutoCommit, path: &ObjId, key: &str) -> Option<i64> {
    match doc.get(path, key) {
        Ok(Some((Value::Scalar(v), _))) => v.to_i64(),
        _ => None,
    }
}

fn get_bool(doc: &AutoCommit, path: &ObjId, key: &str) -> Option<bool> {
    match doc.get(path, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Boolean(b) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

impl Prefs {
    /// Read prefs out of the document, falling back to defaults for any
    /// key that hasn't been written yet.
    pub fn load(doc: &AutoCommit, path: &ObjId) -> Self {
        let d = Prefs::default();
        Prefs {
            interval_ms: get_int(doc, path, "interval_ms")
                .map_or(d.interval_ms, |v| v.clamp(250, 60_000) as u64),
            auto_refresh: get_bool(doc, path, "auto_refresh").unwrap_or(d.auto_refresh),
            show_kernel_threads: get_bool(doc, path, "show_kernel_threads")
                .unwrap_or(d.show_kernel_threads),
            confirm_kill: get_bool(doc, path, "confirm_kill").unwrap_or(d.confirm_kill),
        }
    }

    /// Write the full prefs set back into the document.
    pub fn store(&self, doc: &mut AutoCommit, path: &ObjId) {
        let _ = doc.put(path, "interval_ms", self.interval_ms as i64);
        let _ = doc.put(path, "auto_refresh", self.auto_refresh);
        let _ = doc.put(path, "show_kernel_threads", self.show_kernel_threads);
        let _ = doc.put(path, "confirm_kill", self.confirm_kill);
    }

    /// Advance to the next refresh cadence (1s → 1.5s → 2s → 3s → 5s → 1s).
    /// Any off-grid value (e.g. loaded from an older document) snaps back
    /// to the start of the cycle.
    pub fn cycle_interval(&mut self) {
        self.interval_ms = match self.interval_ms {
            1000 => 1500,
            1500 => 2000,
            2000 => 3000,
            3000 => 5000,
            // 5000 falls back here too, alongside any off-grid value.
            _ => 1000,
        };
    }

    pub fn interval_label(&self) -> String {
        if self.interval_ms.is_multiple_of(1000) {
            format!("{}s", self.interval_ms / 1000)
        } else {
            format!("{:.1}s", self.interval_ms as f32 / 1000.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;

    #[test]
    fn round_trip_through_doc() {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let p = Prefs {
            interval_ms: 3000,
            auto_refresh: false,
            show_kernel_threads: true,
            confirm_kill: false,
        };
        p.store(&mut doc, &root);

        let loaded = Prefs::load(&doc, &root);
        assert_eq!(loaded, p);
    }

    #[test]
    fn defaults_when_empty() {
        let doc = AutoCommit::new();
        let loaded = Prefs::load(&doc, &automerge::ROOT);
        assert_eq!(loaded, Prefs::default());
    }

    #[test]
    fn interval_cycles() {
        let mut p = Prefs {
            interval_ms: 1000,
            ..Prefs::default()
        };
        p.cycle_interval();
        assert_eq!(p.interval_ms, 1500);
        p.cycle_interval();
        assert_eq!(p.interval_ms, 2000);
        p.cycle_interval();
        assert_eq!(p.interval_ms, 3000);
        p.cycle_interval();
        assert_eq!(p.interval_ms, 5000);
        p.cycle_interval();
        assert_eq!(p.interval_ms, 1000);
    }
}
