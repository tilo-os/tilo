//! Durable exact process incarnations. Reused numeric PIDs never authorize kill.
use super::failure;
use serde::{Deserialize, Serialize};
use std::io;

/// Retains the kernel-authenticated original caller, not a caller-supplied PID.
/// Credential snapshots are revalidated against the live proof on every read.
#[derive(Clone)]
pub struct OriginalCaller {
    #[cfg(target_os = "linux")]
    peer: std::sync::Arc<runtime_portal::process_binding::ExecutingPeer>,
    #[cfg(not(target_os = "linux"))]
    never: std::convert::Infallible,
}

impl OriginalCaller {
    #[cfg(target_os = "linux")]
    pub(in crate::capture) fn authenticated(
        peer: std::sync::Arc<runtime_portal::process_binding::ExecutingPeer>,
    ) -> Self {
        Self { peer }
    }

    /// Return DTO facts only after checking the exact live caller and its
    /// kernel credential snapshot. Serialized facts cannot construct this owner.
    pub fn original_caller_facts(&self) -> io::Result<super::original_caller::OriginalCallerFacts> {
        #[cfg(target_os = "linux")]
        {
            super::original_caller::OriginalCallerFacts::capture(&self.peer)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(super::Error::Unsupported.into())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Namespace {
    pub init_pid: u32,
    pub start_ticks: u64,
    pub namespace_dev: u64,
    pub namespace_ino: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Incarnation {
    pub pid: u32,
    start: u64,
    uid: u32,
    boot: [u8; 16],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IncarnationWire {
    pid: u32,
    start: u64,
    uid: u32,
    boot: [u8; 16],
}

impl<'de> Deserialize<'de> for Incarnation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(IncarnationWire::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

impl TryFrom<IncarnationWire> for Incarnation {
    type Error = io::Error;
    fn try_from(wire: IncarnationWire) -> io::Result<Self> {
        let value = Self {
            pid: wire.pid,
            start: wire.start,
            uid: wire.uid,
            boot: wire.boot,
        };
        if value.pid <= 1
            || value.pid > i32::MAX as u32
            || value.start == 0
            || value.boot == [0; 16]
        {
            return Err(failure("invalid process incarnation"));
        }
        Ok(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Custody {
    pub host: Incarnation,
    pub namespace: Option<Namespace>,
    #[serde(default)]
    pub launch_acknowledged: bool,
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn rejects_invalid_process_incarnations() {
        let valid = serde_json::json!({"pid": 2, "start": 1, "uid": 1000, "boot": vec![1; 16]});
        let parsed: Incarnation = serde_json::from_value(valid.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), valid);
        for (field, invalid) in [
            ("pid", serde_json::json!(1)),
            ("pid", serde_json::json!(u32::MAX)),
            ("start", serde_json::json!(0)),
            ("boot", serde_json::json!(vec![0; 16])),
        ] {
            let mut wire = valid.clone();
            wire[field] = invalid;
            assert!(serde_json::from_value::<Incarnation>(wire).is_err());
        }
    }
}

#[cfg(target_os = "linux")]
mod native;

#[cfg(not(target_os = "linux"))]
mod unsupported;
