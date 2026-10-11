//! Certified device display observations, published by the existing compositor.
//! These are observations, never input permissions or durable display intent.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDisplay {
    pub device: String,
    pub device_label: String,
    pub connector: String,
    /// Stable output identity from the same certified topology revision.
    /// Absent on legacy observations; never inferred from a port or label.
    #[serde(default)]
    pub output_id: Option<String>,
    pub label: String,
    pub online: bool,
    pub local: bool,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_milli: u16,
    /// Owner-authored topology revision; zero denotes a legacy observation.
    #[serde(default)]
    pub revision: u64,
}

/// A semantic drag over one observed screen. The peer authors the resulting
/// placement; this request grants no input access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceDisplayMove {
    pub screen: DeviceDisplay,
    pub x: i32,
    pub y: i32,
}

impl DeviceDisplay {
    /// Device-qualified semantic identity even when peers have identical ports.
    pub fn key(&self) -> String {
        format!("{}/{}", self.device, self.connector)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDisplaySnapshot {
    pub publisher_pid: u32,
    pub publisher_start_ticks: u64,
    pub displays: Vec<DeviceDisplay>,
}

impl DeviceDisplaySnapshot {
    pub fn load() -> Option<Self> {
        Self::load_at(&snapshot_path()?)
    }

    /// Read an observation only while its compositor owns the runtime lease.
    pub fn load_at(path: &std::path::Path) -> Option<Self> {
        let bytes = tilo_files::native::bounded::read(path, 2_097_152).ok()??;
        let snapshot: Self = serde_json::from_slice(&bytes).ok()?;
        crate::hardware::publisher_live_in(
            path.parent()?,
            snapshot.publisher_pid,
            snapshot.publisher_start_ticks,
        )
        .then_some(snapshot)
    }

    /// Publish under the existing compositor's runtime lease.
    /// # Errors
    /// Fails if the private runtime directory is unavailable or writing fails.
    pub fn save(&self) -> std::io::Result<()> {
        let path = snapshot_path().ok_or_else(|| std::io::Error::other("runtime unavailable"))?;
        // The clock's sample must not re-author unchanged runtime observations.
        // Loading still validates the live PID/start-time lease; missing,
        // corrupt, changed or stale-publisher state takes the atomic path.
        if tilo_files::native::is_regular_file(&path).unwrap_or(false)
            && Self::load_at(&path).as_ref() == Some(self)
        {
            return Ok(());
        }
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        tilo_files::native::atomic::write_creating_parent(&path, &bytes)
    }
}

fn snapshot_path() -> Option<std::path::PathBuf> {
    crate::hardware::display_snapshot_path()
        .map(|path| path.with_file_name("device-display-state.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observations_expire_with_the_compositor_lease() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-display-state.json");
        let snapshot = DeviceDisplaySnapshot {
            publisher_pid: 4_000_000,
            publisher_start_ticks: 7,
            displays: vec![DeviceDisplay {
                device: "peer".into(),
                device_label: "Peer".into(),
                connector: "eDP-1".into(),
                output_id: None,
                label: "Panel".into(),
                online: true,
                local: false,
                x: 1920,
                y: 0,
                width: 1280,
                height: 720,
                scale_milli: 1250,
                revision: 1,
            }],
        };
        std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
        assert!(DeviceDisplaySnapshot::load_at(&path).is_none());
        let lease_path = dir.path().join("publisher-4000000-7.lock");
        let lease = std::fs::File::create(lease_path).unwrap();
        assert!(DeviceDisplaySnapshot::load_at(&path).is_none());
        let lease = advisory_lock::FileLock::try_exclusive(lease).unwrap();
        assert_eq!(
            DeviceDisplaySnapshot::load_at(&path),
            Some(snapshot.clone())
        );
        let mut stale = snapshot;
        stale.publisher_start_ticks += 1;
        std::fs::write(&path, serde_json::to_vec(&stale).unwrap()).unwrap();
        assert!(
            DeviceDisplaySnapshot::load_at(&path).is_none(),
            "reused PID cannot vouch for a previous publisher"
        );
        stale.publisher_start_ticks -= 1;
        std::fs::write(&path, serde_json::to_vec(&stale).unwrap()).unwrap();
        drop(lease);
        assert!(
            DeviceDisplaySnapshot::load_at(&path).is_none(),
            "stale connector observations disappear after the compositor exits"
        );
    }
}

#[cfg(test)]
mod identity_compatibility_tests {
    use super::DeviceDisplay;
    #[test]
    fn legacy_observations_do_not_invent_stable_output_identity() {
        let legacy = serde_json::json!({
            "device":"peer", "device_label":"Peer", "connector":"DP-1", "label":"Panel",
            "online":true, "local":false, "x":0, "y":0, "width":1920, "height":1080,
            "scale_milli":1000, "revision":7
        });
        let screen: DeviceDisplay = serde_json::from_value(legacy).unwrap();
        assert_eq!(screen.output_id, None);
        assert_eq!(screen.key(), "peer/DP-1");
    }
}

#[cfg(all(test, any(unix, windows)))]
mod publication_regressions {
    use super::*;

    #[test]
    fn unchanged_live_device_observations_keep_the_published_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", dir.path());
        let (publisher_pid, publisher_start_ticks) = crate::hardware::publisher_identity().unwrap();
        let snapshot = DeviceDisplaySnapshot {
            publisher_pid,
            publisher_start_ticks,
            displays: Vec::new(),
        };
        snapshot.save().unwrap();
        let path = snapshot_path().unwrap();
        let retained = std::fs::File::open(&path).unwrap();
        let original = tilo_files::native::atomic::FileIdentity::of(&retained).unwrap();
        snapshot.save().unwrap();
        let current = std::fs::File::open(&path).unwrap();
        assert_eq!(
            original,
            tilo_files::native::atomic::FileIdentity::of(&current).unwrap(),
            "an unchanged live observation must not perform durable replacement"
        );
        assert_eq!(DeviceDisplaySnapshot::load(), Some(snapshot.clone()));
        let mut changed = snapshot.clone();
        changed.displays.push(DeviceDisplay {
            device: "own-device".into(),
            device_label: "Own device".into(),
            connector: "DP-1".into(),
            output_id: Some("stable-output".into()),
            label: "Calibrated output".into(),
            online: true,
            local: true,
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale_milli: 1000,
            revision: 1,
        });
        changed.save().unwrap();
        assert_eq!(DeviceDisplaySnapshot::load(), Some(changed));
        std::fs::write(&path, b"invalid observation").unwrap();
        snapshot.save().unwrap();
        assert_eq!(DeviceDisplaySnapshot::load(), Some(snapshot.clone()));
        std::fs::remove_file(&path).unwrap();
        snapshot.save().unwrap();
        assert_eq!(DeviceDisplaySnapshot::load(), Some(snapshot));
    }
}
