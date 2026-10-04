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

/// Where an environment session runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Location {
    /// On the device the terminal runs on.
    #[default]
    ThisDevice,
    /// On a relay the person is a member of.
    Relay { id: String },
}

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
        #[serde(default)]
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

/// The record key under which `terminal.session` stores its target.
pub const TARGET_KEY: &str = "terminal_target";

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
                    && match location {
                        Location::ThisDevice => true,
                        Location::Relay { id } => tilo_environment::is_record_key(id),
                    }
            }
            Self::Legacy { vm } => tilo_environment::is_record_key(vm),
        }
    }

    /// The target as the `ush` supervisor on this device understands it.
    /// `None` for a target this device's supervisor does not serve (a relay
    /// or a legacy VM).
    pub fn to_supervisor(&self) -> Option<tilo_environment::Target> {
        match self {
            Self::Host => Some(tilo_environment::Target::Host),
            Self::Environment {
                record,
                location: Location::ThisDevice,
            } => Some(tilo_environment::Target::Environment {
                record: record.clone(),
            }),
            Self::Environment {
                location: Location::Relay { .. },
                ..
            }
            | Self::Legacy { .. } => None,
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

/// The most recently used environment, kept per device in the app's data
/// directory. Device-local on purpose: which environment you last opened here
/// says nothing about the next device.
#[derive(Clone, Debug)]
pub struct RecentTarget {
    path: Option<std::path::PathBuf>,
}

impl RecentTarget {
    /// The store under the per-user data directory.
    pub fn discover() -> Self {
        Self {
            path: xdg_paths::data_dir().map(|dir| dir.join("tilo/terminal/recent-target.json")),
        }
    }

    /// A store at `path`; tests use a scratch directory.
    pub fn at(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
        }
    }

    /// No store: nothing is remembered.
    pub fn none() -> Self {
        Self { path: None }
    }

    pub fn load(&self) -> Option<SessionTarget> {
        let raw = std::fs::read_to_string(self.path.as_ref()?).ok()?;
        SessionTarget::decode(raw.trim())
    }

    /// Remember `target` if it is an environment; the device's own shell and
    /// legacy VMs are never the default for a new tab.
    pub fn remember(&self, target: &SessionTarget) {
        if !matches!(target, SessionTarget::Environment { .. }) {
            return;
        }
        let Some(path) = self.path.as_ref() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, target.encode());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_round_trips_and_rejects_what_a_request_may_not_carry() {
        for target in [
            SessionTarget::Host,
            SessionTarget::default_environment(),
            SessionTarget::environment("work"),
            SessionTarget::Environment {
                record: Some("work".into()),
                location: Location::Relay { id: "home".into() },
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
    fn only_this_devices_targets_reach_the_local_supervisor() {
        assert_eq!(
            SessionTarget::Host.to_supervisor(),
            Some(tilo_environment::Target::Host)
        );
        assert_eq!(
            SessionTarget::environment("work").to_supervisor(),
            Some(tilo_environment::Target::Environment {
                record: Some("work".into())
            })
        );
        assert_eq!(
            SessionTarget::Environment {
                record: None,
                location: Location::Relay { id: "r".into() }
            }
            .to_supervisor(),
            None
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
