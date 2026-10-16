//! Where a terminal session runs.
//!
//! Every session is bound to a target (`docs/app-model/environments.md`):
//! the device's own login shell ([`SessionTarget::Host`]), a guest built from
//! an environment record on this device or a relay
//! ([`SessionTarget::Environment`]), or a legacy VM
//! ([`SessionTarget::Legacy`]). The target is part of the synced
//! `terminal.session` record, so reopening a session — here or on another
//! device — goes back to the same place where it can.

use serde::{Deserialize, Serialize};

/// Where an environment session runs: this device, or one of the person's
/// relays by endpoint id — the vocabulary the supervisor speaks.
pub use tilo_environment::Location;

/// Where a session runs.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SessionTarget {
    /// The device's own login shell, unconfined.
    Host,
    /// A guest built from an environment record. `None` is the default
    /// environment.
    Environment {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        record: Option<String>,
        #[serde(default, skip_serializing_if = "Location::is_this_device")]
        location: Location,
    },
    /// A legacy VM, by name.
    Legacy { vm: String },
}

impl Default for SessionTarget {
    fn default() -> Self {
        Self::default_environment()
    }
}

/// The most a stored target may weigh; anything larger is not a target.
const MAX_TARGET_BYTES: usize = 1024;

impl SessionTarget {
    /// The default environment on this device.
    pub fn default_environment() -> Self {
        Self::Environment {
            record: None,
            location: Location::ThisDevice,
        }
    }

    /// An environment on this device.
    pub fn environment(record: impl Into<String>) -> Self {
        let record = record.into();
        Self::Environment {
            record: (record != tilo_environment::DEFAULT_RECORD).then_some(record),
            location: Location::ThisDevice,
        }
    }

    pub fn is_host(&self) -> bool {
        matches!(self, Self::Host)
    }

    /// The environment record key this target names, if it is one.
    pub fn environment_record(&self) -> Option<&str> {
        match self {
            Self::Environment { record, .. } => Some(
                record
                    .as_deref()
                    .unwrap_or(tilo_environment::DEFAULT_RECORD),
            ),
            _ => None,
        }
    }

    /// Whether every field is one a request may carry: record keys and relay
    /// and VM names arrive from synced state, so they are checked before they
    /// are used for anything.
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Host => true,
            Self::Environment { record, location } => {
                record
                    .as_deref()
                    .is_none_or(tilo_environment::is_record_key)
                    && location.is_valid()
            }
            Self::Legacy { vm } => tilo_environment::is_record_key(vm),
        }
    }

    /// The target as the `ush` supervisor on this device understands it:
    /// it serves this device's environments and carries a relay's.
    /// `None` for a legacy VM.
    pub fn to_supervisor(&self) -> Option<tilo_environment::Target> {
        match self {
            Self::Host => Some(tilo_environment::Target::Host),
            Self::Environment { record, location } => Some(tilo_environment::Target::Environment {
                record: record.clone(),
                location: location.clone(),
            }),
            Self::Legacy { .. } => None,
        }
    }

    /// The same environment, run at `location`; any other target as it is.
    #[must_use]
    pub fn at(&self, location: Location) -> Self {
        match self {
            Self::Environment { record, .. } => Self::Environment {
                record: record.clone(),
                location,
            },
            other => other.clone(),
        }
    }

    /// Where an environment runs; `None` for any other target.
    pub fn location(&self) -> Option<&Location> {
        match self {
            Self::Environment { location, .. } => Some(location),
            _ => None,
        }
    }

    /// Encode for the record.
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Decode a stored target; anything malformed or invalid is no target.
    pub fn decode(raw: &str) -> Option<Self> {
        if raw.len() > MAX_TARGET_BYTES {
            return None;
        }
        serde_json::from_str::<Self>(raw)
            .ok()
            .filter(Self::is_valid)
    }
}

/// An environment as the target picker and the management sheet show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentSummary {
    /// The record key.
    pub key: String,
    pub name: String,
    /// Package names the environment's tools provide, in order.
    pub tools: Vec<String>,
    /// Granted directories, `(path, writable)`.
    pub grants: Vec<(String, bool)>,
}

/// One of the person's relays, as the picker offers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelaySummary {
    /// The relay's endpoint id (hex).
    pub relay: String,
    pub label: String,
}

/// Every target the picker offers, in order: each environment, then the
/// device's own shell, then legacy VMs (only when the legacy VM service
/// reports any).
pub fn picker_targets(
    environments: &[EnvironmentSummary],
    host_available: bool,
    legacy_vms: &[String],
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
        legacy_vms
            .iter()
            .filter(|vm| tilo_environment::is_record_key(vm))
            .map(|vm| SessionTarget::Legacy { vm: vm.clone() }),
    );
    out
}

/// The target a new tab opens on: the most recently used environment, else
/// the default environment — or the device's own shell where no environment
/// can run at all.
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

/// The most recently used environment, a device-local setting of the
/// app's: which environment you last opened here says nothing about the next
/// device, and nothing about it reaches a collaborator.
#[derive(Clone, Debug)]
pub struct RecentTarget {
    settings: Option<ui::storage::Settings<RecentTargetSetting>>,
}

/// What [`RecentTarget`] keeps: the target, encoded as a request carries it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecentTargetSetting {
    #[serde(default)]
    pub target: Option<String>,
}

impl ui::storage::SettingsSchema for RecentTargetSetting {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::DeviceLocal;
}

/// The app id the setting is kept under.
const TERMINAL_APP_ID: &str = "so.tilo.terminal";

impl RecentTarget {
    /// The app's settings in the document its host keeps.
    pub fn discover() -> Self {
        Self {
            settings: Some(ui::storage::Settings::for_app(TERMINAL_APP_ID)),
        }
    }

    /// The setting in the settings document at `path`; tests use a scratch
    /// profile's.
    pub fn at(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            settings: Some(ui::storage::Settings::for_app(TERMINAL_APP_ID).in_file(path)),
        }
    }

    /// No store: nothing is remembered.
    pub fn none() -> Self {
        Self { settings: None }
    }

    pub fn load(&self) -> Option<SessionTarget> {
        let stored = self.settings.as_ref()?.get().target?;
        SessionTarget::decode(stored.trim())
    }

    /// Remember `target` if it is an environment; the device's own shell and
    /// legacy VMs are never the default for a new tab.
    pub fn remember(&self, target: &SessionTarget) {
        if !matches!(target, SessionTarget::Environment { .. }) {
            return;
        }
        let Some(settings) = self.settings.as_ref() else {
            return;
        };
        if let Err(error) = settings.set(&RecentTargetSetting {
            target: Some(target.encode()),
        }) {
            tracing::warn!(%error, "terminal: the recent environment could not be kept");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recent_environment_is_a_device_local_setting() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        let recent = RecentTarget::at(&file);
        recent.remember(&SessionTarget::Host);
        assert_eq!(
            recent.load(),
            None,
            "the device's own shell is never remembered"
        );
        recent.remember(&SessionTarget::environment("work"));
        assert_eq!(recent.load(), Some(SessionTarget::environment("work")));
        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(
            stored["app_device_local"][TERMINAL_APP_ID]["target"].is_string(),
            "{stored}"
        );
        assert!(
            stored.get("app_personal").is_none(),
            "it never roams to the person's other devices"
        );
    }

    #[test]
    fn a_target_round_trips_and_rejects_what_a_request_may_not_carry() {
        for target in [
            SessionTarget::Host,
            SessionTarget::default_environment(),
            SessionTarget::environment("work"),
            SessionTarget::Environment {
                record: Some("work".into()),
                location: Location::Relay {
                    relay: "ab".repeat(32),
                },
            },
            SessionTarget::Legacy { vm: "linux".into() },
        ] {
            assert_eq!(SessionTarget::decode(&target.encode()), Some(target));
        }
        assert_eq!(
            SessionTarget::environment(tilo_environment::DEFAULT_RECORD),
            SessionTarget::default_environment()
        );
        for bad in [
            r#"{"kind":"environment","record":"../etc"}"#,
            r#"{"kind":"legacy","vm":""}"#,
            r#"{"kind":"nonsense"}"#,
            "not json",
        ] {
            assert_eq!(SessionTarget::decode(bad), None, "{bad}");
        }
        assert_eq!(
            SessionTarget::decode(&format!(
                r#"{{"kind":"legacy","vm":"{}"}}"#,
                "a".repeat(2000)
            )),
            None
        );
    }

    #[test]
    fn the_supervisor_serves_and_carries_every_target_but_legacy_vms() {
        assert_eq!(
            SessionTarget::Host.to_supervisor(),
            Some(tilo_environment::Target::Host)
        );
        assert_eq!(
            SessionTarget::environment("work").to_supervisor(),
            Some(tilo_environment::Target::Environment {
                record: Some("work".into()),
                location: Location::ThisDevice,
            })
        );
        assert_eq!(
            SessionTarget::Environment {
                record: None,
                location: Location::Relay {
                    relay: "ab".repeat(32)
                }
            }
            .to_supervisor(),
            Some(tilo_environment::Target::Environment {
                record: None,
                location: Location::Relay {
                    relay: "ab".repeat(32)
                }
            }),
            "the supervisor carries a relay's environments"
        );
        assert!(
            SessionTarget::decode(
                r#"{"kind":"environment","location":{"kind":"relay","relay":"home"}}"#
            )
            .is_none(),
            "a relay is named by its endpoint id"
        );
        assert_eq!(
            SessionTarget::Legacy { vm: "linux".into() }.to_supervisor(),
            None
        );
    }

    #[test]
    fn a_new_tab_opens_the_most_recent_environment_and_never_the_host_by_habit() {
        let dir = tempfile::tempdir().unwrap();
        let recent = RecentTarget::at(dir.path().join("recent.json"));
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
        recent.remember(&SessionTarget::Legacy { vm: "linux".into() });
        assert_eq!(
            recent.load(),
            Some(SessionTarget::environment("work")),
            "the host shell and legacy VMs are chosen each time, never remembered"
        );

        assert_eq!(
            default_target(recent.load().as_ref(), false),
            SessionTarget::Host,
            "where no environment can run, the device's own shell is the only target"
        );
    }

    #[test]
    fn the_picker_lists_environments_then_the_host_then_legacy_vms_when_reported() {
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
                SessionTarget::Legacy { vm: "linux".into() }
            ]
        );
    }
}
