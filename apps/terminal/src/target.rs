//! Where a terminal session runs (the engine's [`SessionTarget`]) and the environment each
//! device used last (this app's record).

pub use tilo_terminal::target::{
    EnvironmentSummary, SessionTarget, default_target, parse_target_argument, picker_targets,
};

use serde::{Deserialize, Serialize};
use settings_model::{Scope, SettingsSchema};
use std::collections::BTreeMap;
#[cfg(feature = "desktop")]
use ui::framework::SurfaceCx;
use ui::storage::Settings;
use ui::tilo_resource::{self, Model, Refusal, resource};

/// The environment each device used last, by device: what one device opened says nothing about the
/// next, so each reads and writes only its own entry.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct RecentEnvironments {
    /// The environment, encoded as a request carries it, by device.
    pub devices: std::collections::BTreeMap<String, String>,
}

impl RecentEnvironments {
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "terminal.recent-environments",
    read_content = RecentEnvironments::read_document
)]
impl RecentEnvironments {
    #[action(housekeeping)]
    pub fn used(&mut self, device: String, target: String) -> Result<(), Refusal> {
        if !matches!(
            SessionTarget::decode(&target),
            Some(SessionTarget::Environment { .. })
        ) {
            return Err(Refusal::new("not an environment"));
        }
        self.devices.insert(device, target);
        Ok(())
    }
}

impl RecentEnvironments {
    #[must_use]
    pub fn on(&self, device: &str) -> Option<SessionTarget> {
        SessionTarget::decode(self.devices.get(device)?.trim())
    }
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct DeviceEnvironments {
    #[serde(default)]
    by_workspace: BTreeMap<String, SessionTarget>,
}

impl SettingsSchema for DeviceEnvironments {
    const SCOPE: Scope = Scope::DeviceLocal;
}

#[derive(Clone, Default)]
pub struct RecentTarget {
    record: Option<(Settings<DeviceEnvironments>, String)>,
}

impl std::fmt::Debug for RecentTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecentTarget")
            .field(
                "workspace",
                &self.record.as_ref().map(|(_, workspace)| workspace),
            )
            .finish_non_exhaustive()
    }
}

impl RecentTarget {
    #[cfg(feature = "desktop")]
    pub(crate) fn open(cx: &SurfaceCx<crate::Terminal>) -> Self {
        let workspace = cx.workspace().unwrap_or_default();
        let settings = cx.settings::<DeviceEnvironments>();
        Self::in_settings(settings.settings().clone(), workspace)
    }

    #[cfg(any(test, feature = "desktop"))]
    pub(crate) fn in_settings(
        settings: Settings<DeviceEnvironments>,
        workspace: impl Into<String>,
    ) -> Self {
        Self {
            record: Some((settings, workspace.into())),
        }
    }

    pub fn none() -> Self {
        Self::default()
    }

    pub fn load(&self) -> Option<SessionTarget> {
        let (settings, workspace) = self.record.as_ref()?;
        settings.get().by_workspace.get(workspace).cloned()
    }

    /// Remember `target` if it is an environment; the device's own shell and
    /// Linux VMs are never the default for a new tab.
    pub fn remember(&self, target: &SessionTarget) {
        if !matches!(target, SessionTarget::Environment { .. }) {
            return;
        }
        let Some((settings, workspace)) = self.record.as_ref() else {
            return;
        };
        if self.load().as_ref() == Some(target) {
            return;
        }
        if let Err(error) = settings.update(|state| {
            state.by_workspace.insert(workspace.clone(), target.clone());
        }) {
            tracing::warn!(%error, "terminal: the recent environment could not be kept");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_tab_opens_the_most_recent_environment_and_never_the_host_by_habit() {
        let app = ui::framework::testing::TestApp::<crate::TerminalShell>::new().unwrap();
        let recent = RecentTarget::in_settings(
            app.settings::<DeviceEnvironments>(),
            ui::framework::testing::TEST_WORKSPACE,
        );
        assert_eq!(recent.load(), None);
        assert_eq!(
            default_target(recent.load().as_ref(), true),
            SessionTarget::default_environment()
        );

        recent.remember(&SessionTarget::environment("work"));
        assert_eq!(
            default_target(recent.load().as_ref(), true),
            SessionTarget::environment("work")
        );

        recent.remember(&SessionTarget::Host);
        recent.remember(&SessionTarget::LinuxVm { vm: "linux".into() });
        assert_eq!(
            recent.load(),
            Some(SessionTarget::environment("work")),
            "the host shell and Linux VMs are chosen each time, never remembered"
        );

        assert_eq!(
            default_target(recent.load().as_ref(), false),
            SessionTarget::Host,
            "where no environment can run, the device's own shell is the only target"
        );
    }
}
