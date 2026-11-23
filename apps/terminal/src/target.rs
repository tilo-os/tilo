//! Where a terminal session runs: every session is bound to a target
//! (docs/app-model/environments.md).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use settings_model::{Scope, SettingsSchema};
#[cfg(feature = "desktop")]
use ui::framework::SurfaceCx;
use ui::storage::Settings;
use ui::tilo_resource::{self, Model, Refusal, resource};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SessionTarget {
    Host,
    Environment {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        record: Option<String>,
    },
    LinuxVm {
        vm: String,
    },
}

impl Default for SessionTarget {
    fn default() -> Self {
        Self::default_environment()
    }
}

const MAX_TARGET_BYTES: usize = 1024;

impl SessionTarget {
    pub fn default_environment() -> Self {
        Self::Environment { record: None }
    }

    pub fn environment(record: impl Into<String>) -> Self {
        let record = record.into();
        Self::Environment {
            record: (record != tilo_environment::DEFAULT_RECORD).then_some(record),
        }
    }

    pub fn is_host(&self) -> bool {
        matches!(self, Self::Host)
    }

    pub fn environment_resource(&self) -> Option<&str> {
        match self {
            Self::Environment { record, .. } => Some(
                record
                    .as_deref()
                    .unwrap_or(tilo_environment::DEFAULT_RECORD),
            ),
            _ => None,
        }
    }

    pub fn is_valid(&self) -> bool {
        match self {
            Self::Host => true,
            Self::Environment { record } => record
                .as_deref()
                .is_none_or(tilo_environment::is_record_key),
            Self::LinuxVm { vm } => tilo_environment::is_record_key(vm),
        }
    }

    pub fn to_supervisor(&self) -> Option<tilo_environment::Target> {
        match self {
            Self::Host => Some(tilo_environment::Target::Host),
            Self::Environment { record } => Some(tilo_environment::Target::Environment {
                record: record.clone(),
            }),
            Self::LinuxVm { .. } => None,
        }
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn decode(raw: &str) -> Option<Self> {
        if raw.len() > MAX_TARGET_BYTES {
            return None;
        }
        serde_json::from_str::<Self>(raw)
            .ok()
            .filter(Self::is_valid)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentSummary {
    pub key: String,
    pub name: String,
    pub tools: Vec<String>,
    pub grants: Vec<(String, bool)>,
}

/// Every target the picker offers, in order: each environment, the device's own shell, then Linux
/// VMs (only when the Linux VM service reports any).
pub fn picker_targets(
    environments: &[EnvironmentSummary],
    host_available: bool,
    linux_vms: &[String],
) -> Vec<SessionTarget> {
    let mut out: Vec<SessionTarget> = environments
        .iter()
        .map(|env| SessionTarget::environment(env.key.clone()))
        .collect();
    if out.is_empty() {
        out.push(SessionTarget::default_environment());
    }
    if host_available {
        out.push(SessionTarget::Host);
    }
    out.extend(
        linux_vms
            .iter()
            .filter(|vm| tilo_environment::is_record_key(vm))
            .map(|vm| SessionTarget::LinuxVm { vm: vm.clone() }),
    );
    out
}

pub fn default_target(
    recent: Option<&SessionTarget>,
    environments_available: bool,
) -> SessionTarget {
    if !environments_available {
        return SessionTarget::Host;
    }
    match recent {
        Some(target @ SessionTarget::Environment { .. }) if target.is_valid() => target.clone(),
        _ => SessionTarget::default_environment(),
    }
}

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

    tilo_test_env::scenario! {
        fn a_session_target_is_carried_served_and_listed;

        phase a_target_round_trips_and_rejects_what_a_request_may_not_carry;
        {
            for target in [
                SessionTarget::Host,
                SessionTarget::default_environment(),
                SessionTarget::environment("work"),
                SessionTarget::LinuxVm { vm: "linux".into() },
            ] {
                assert_eq!(SessionTarget::decode(&target.encode()), Some(target));
            }
            assert_eq!(
                SessionTarget::environment(tilo_environment::DEFAULT_RECORD),
                SessionTarget::default_environment()
            );
            assert_eq!(
                SessionTarget::decode(r#"{"kind":"environment","shell":"linux"}"#),
                Some(SessionTarget::default_environment())
            );
            assert_eq!(
                SessionTarget::decode(r#"{"kind":"environment","record":"work","shell":"linux"}"#),
                Some(SessionTarget::environment("work"))
            );
            for bad in [
                r#"{"kind":"environment","record":"../etc"}"#,
                r#"{"kind":"linux-vm","vm":""}"#,
                r#"{"kind":"nonsense"}"#,
                "not json",
            ] {
                assert_eq!(SessionTarget::decode(bad), None, "{bad}");
            }
            assert_eq!(
                SessionTarget::decode(&format!(
                    r#"{{"kind":"linux-vm","vm":"{}"}}"#,
                    "a".repeat(2000)
                )),
                None
            );
        }

        phase the_supervisor_serves_and_carries_every_target_but_linux_vms;
        {
            assert_eq!(
                SessionTarget::Host.to_supervisor(),
                Some(tilo_environment::Target::Host)
            );
            assert_eq!(
                SessionTarget::environment("work").to_supervisor(),
                Some(tilo_environment::Target::Environment {
                    record: Some("work".into()),
                })
            );
            assert_eq!(
                SessionTarget::decode(r#"{"kind":"environment","location":{"kind":"relay"}}"#),
                Some(SessionTarget::default_environment()),
                "a location stored while environments could run elsewhere means this device"
            );
            assert_eq!(
                SessionTarget::LinuxVm { vm: "linux".into() }.to_supervisor(),
                None
            );
        }

        phase the_picker_lists_environments_then_the_host_then_linux_vms_when_reported;
        {
            let work = EnvironmentSummary {
                key: "work".into(),
                name: "Work".into(),
                tools: vec![],
                grants: vec![],
            };
            assert_eq!(
                picker_targets(&[], true, &[]),
                vec![SessionTarget::default_environment(), SessionTarget::Host]
            );
            assert_eq!(
                picker_targets(std::slice::from_ref(&work), false, &[]),
                vec![SessionTarget::environment("work")]
            );
            assert_eq!(
                picker_targets(&[work], true, &["linux".into(), "../x".into()]),
                vec![
                    SessionTarget::environment("work"),
                    SessionTarget::Host,
                    SessionTarget::LinuxVm { vm: "linux".into() }
                ]
            );
        }
    }

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
