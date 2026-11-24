//! Where a terminal session runs: every session is bound to a target
//! (docs/app-model/environments.md).

use serde::{Deserialize, Serialize};

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

/// [`SessionTarget`] as a person or a script writes it: `host`, `environment`,
/// `environment:<record>`, `linux-vm:<vm>`.
#[must_use]
pub fn parse_target_argument(raw: &str) -> Option<SessionTarget> {
    let raw = raw.trim();
    let target = match raw.split_once(':') {
        None if raw == "host" => SessionTarget::Host,
        None if raw == "environment" => SessionTarget::default_environment(),
        Some(("environment", record)) => SessionTarget::environment(record),
        Some(("linux-vm", vm)) => SessionTarget::LinuxVm { vm: vm.to_owned() },
        _ => return None,
    };
    target.is_valid().then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_target_is_carried_served_and_listed() {
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

    #[test]
    fn the_supervisor_serves_and_carries_every_target_but_linux_vms() {
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

    #[test]
    fn the_picker_lists_environments_then_the_host_then_linux_vms_when_reported() {
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

    #[test]
    fn a_target_argument_names_where_a_session_opens() {
        assert_eq!(
            parse_target_argument("environment:work"),
            Some(SessionTarget::environment("work"))
        );
        assert_eq!(
            parse_target_argument("environment"),
            Some(SessionTarget::default_environment())
        );
        assert_eq!(
            parse_target_argument("linux-vm:linux"),
            Some(SessionTarget::LinuxVm { vm: "linux".into() })
        );
        assert_eq!(parse_target_argument("host"), Some(SessionTarget::Host));
        assert_eq!(parse_target_argument("host@ab"), None);
        for bad in ["", "nonsense", "environment:../etc", "linux-vm:"] {
            assert_eq!(parse_target_argument(bad), None, "{bad}");
        }
    }
}
