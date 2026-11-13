//! Which terminal tabs may control the desktop right now, between the compositor and Settings.
//!
//! A "for this tab" grant lives in the compositor's memory until the tab closes; it publishes the
//! live ones as a [`DesktopControlSnapshot`] in the private runtime directory, and Settings asks to
//! stop one with a [`StopRequest`], which the compositor answers at its settings reload point.
//! "Always" grants are workspace capabilities and are listed from the workspace tree.

use serde::{Deserialize, Serialize};
use tilo_clock::unix_now_ms;

/// How old a stop request may be when the compositor first sees it and
/// still answer it: a restarted compositor must not replay an old one.
pub const STOP_FRESH_MS: u64 = 10_000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabGrantEntry {
    pub id: String,
    pub app: String,
    pub app_label: String,
    pub tab: String,
    pub workspace: String,
}

/// Read-only state written atomically by the compositor in the private user
/// runtime directory: the "this tab" grants alive right now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopControlSnapshot {
    pub publisher_pid: u32,
    pub publisher_start_ticks: u64,
    pub tabs: Vec<TabGrantEntry>,
}

impl DesktopControlSnapshot {
    pub fn load() -> Option<Self> {
        Self::load_at(&snapshot_path()?)
    }

    pub fn load_at(path: &std::path::Path) -> Option<Self> {
        crate::published::note_file_read();
        let bytes = tilo_files::native::bounded::read(path, MAX_SNAPSHOT_BYTES).ok()??;
        let snapshot: Self = serde_json::from_slice(&bytes).ok()?;
        crate::hardware::publisher_live_in(
            path.parent()?,
            snapshot.publisher_pid,
            snapshot.publisher_start_ticks,
        )
        .then_some(snapshot)
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = snapshot_path()
            .ok_or_else(|| std::io::Error::other("XDG_RUNTIME_DIR is unavailable"))?;
        self.save_at(&path)
    }

    pub fn save_at(&self, path: &std::path::Path) -> std::io::Result<()> {
        let mut body = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        body.push(b'\n');
        tilo_files::native::atomic::write_creating_parent(path, &body)
    }
}

const MAX_SNAPSHOT_BYTES: u64 = 1024 * 1024;

pub fn snapshot_path() -> Option<std::path::PathBuf> {
    tilo_paths::xdg::runtime_dir().map(|path| path.join("tilo/desktop-control-state.json"))
}

/// Settings asks the compositor to stop one "this tab" grant. The id is the
/// request time, so a compositor that restarts never replays it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StopRequest {
    pub requested_unix_ms: u64,
    pub grant: String,
}

impl StopRequest {
    pub const SECTION: &'static str = "desktop_control_stop";

    pub fn request(grant: &str) -> std::io::Result<Self> {
        let request = Self {
            requested_unix_ms: unix_now_ms(),
            grant: grant.to_owned(),
        };
        let value = serde_json::to_value(&request).map_err(std::io::Error::other)?;
        super::update_section(
            Self::SECTION,
            value.as_object().cloned().unwrap_or_default(),
        )?;
        Ok(request)
    }

    pub fn load() -> Option<Self> {
        let section = super::load_section(Self::SECTION);
        if !section.contains_key("requested_unix_ms") {
            return None;
        }
        serde_json::from_value(serde_json::Value::Object(section)).ok()
    }

    pub fn is_fresh(&self, now_unix_ms: u64) -> bool {
        now_unix_ms >= self.requested_unix_ms
            && now_unix_ms.saturating_sub(self.requested_unix_ms) <= STOP_FRESH_MS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_counts_only_while_its_publisher_lives() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("desktop-control-state.json");
        let (pid, start) = crate::hardware::publisher_identity_in(dir.path()).expect("lease");
        let snapshot = DesktopControlSnapshot {
            publisher_pid: pid,
            publisher_start_ticks: start,
            tabs: vec![TabGrantEntry {
                id: "41:9".into(),
                app: "so.tilo.terminal".into(),
                app_label: "Terminal".into(),
                tab: "claude".into(),
                workspace: "Home".into(),
            }],
        };
        snapshot.save_at(&path).expect("save");
        assert_eq!(
            DesktopControlSnapshot::load_at(&path),
            Some(snapshot.clone())
        );
        let stale = DesktopControlSnapshot {
            publisher_start_ticks: start.wrapping_add(1),
            ..snapshot
        };
        stale.save_at(&path).expect("save");
        assert_eq!(DesktopControlSnapshot::load_at(&path), None);
    }

    #[test]
    fn a_stop_request_is_answered_only_while_fresh() {
        let request = StopRequest {
            requested_unix_ms: 1_000,
            grant: "41:9".into(),
        };
        assert!(request.is_fresh(1_000));
        assert!(request.is_fresh(1_000 + STOP_FRESH_MS));
        assert!(!request.is_fresh(1_001 + STOP_FRESH_MS));
        assert!(!request.is_fresh(999), "from the future");
    }
}
