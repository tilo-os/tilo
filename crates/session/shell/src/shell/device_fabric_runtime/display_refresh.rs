//! Wake sampling for changed display facts without reading files on each frame.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use settings_store::hardware::DisplayHardwareSnapshot;
use settings_store::live::{LiveSettings, SectionFollower};

use super::LocalFabricCapabilities;

pub(super) struct Changes {
    pub resample: bool,
    pub policy: bool,
}

pub(super) struct DisplayRefresh {
    settings: LiveSettings,
    sections: SectionFollower,
    hardware_changed: Arc<AtomicBool>,
    last_local: Option<LocalFabricCapabilities>,
    _hardware_watch: Option<settings_store::watch::SettingsWatch>,
}

impl DisplayRefresh {
    pub fn new(wake: crate::session_runtime::Wake) -> Self {
        let settings_wake = Arc::clone(&wake);
        let settings = LiveSettings::start(move || settings_wake());
        let sections = SectionFollower::new(&settings);
        let hardware_changed = Arc::new(AtomicBool::new(false));
        let previous = Mutex::new(hardware_observation());
        let changed = Arc::clone(&hardware_changed);
        let observe = Arc::new(move || {
            let current = hardware_observation();
            let Ok(mut previous) = previous.lock() else {
                return;
            };
            if *previous != current {
                *previous = current;
                if !changed.swap(true, Ordering::AcqRel) {
                    wake();
                }
            }
        });
        let hardware_watch = settings_store::hardware::display_snapshot_path().and_then(|path| {
            let notification = Arc::clone(&observe);
            match settings_store::watch::watch_settings_at(&path, move || notification()) {
                Ok(watch) => Some(watch),
                Err(error) => {
                    tracing::warn!(%error, "display observation watch unavailable; using periodic sampling");
                    None
                }
            }
        });
        // Close the read/watch-registration race. This is only a dirty signal;
        // the pump still loads and validates fresh hardware before publication.
        observe();
        Self {
            settings,
            sections,
            hardware_changed,
            last_local: None,
            _hardware_watch: hardware_watch,
        }
    }

    pub fn take_changes(&mut self, local: LocalFabricCapabilities) -> Changes {
        let sections = self.sections.changed_sections(&self.settings);
        let local_changed = self.last_local.replace(local) != Some(local);
        let hardware_changed = self.hardware_changed.swap(false, Ordering::AcqRel);
        Changes {
            resample: local_changed || hardware_changed || !sections.is_empty(),
            policy: sections.contains("continuity"),
        }
    }
}

fn hardware_observation() -> Option<DisplayHardwareSnapshot> {
    let mut snapshot = DisplayHardwareSnapshot::load()?;
    // A refreshed lease with identical facts must not re-author display state.
    snapshot.generated_unix_ms = 0;
    Some(snapshot)
}
