//! Path-free APIs use opaque handles; this protected cleanup receipt is never a public action result.
use super::failure;
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
};

/// Protected alias identity; no caller-authored path grants mount authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "AliasWire")]
pub struct ProjectionReceipt {
    pub(super) path: PathBuf,
    pub(super) dev: u64,
    pub(super) ino: u64,
    pub(super) source: u64,
    pub(super) output: Option<u64>,
    #[serde(default)]
    pub(super) scratch: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AliasWire {
    path: PathBuf,
    dev: u64,
    ino: u64,
    source: u64,
    output: Option<u64>,
    #[serde(default)]
    scratch: Option<u64>,
}

impl TryFrom<AliasWire> for ProjectionReceipt {
    type Error = io::Error;
    fn try_from(wire: AliasWire) -> io::Result<Self> {
        let value = Self {
            path: wire.path,
            dev: wire.dev,
            ino: wire.ino,
            source: wire.source,
            output: wire.output,
            scratch: wire.scratch,
        };
        let name = value
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| failure("invalid capture alias name"))?;
        if value.path.parent() != Some(Path::new("/tmp"))
            || !name.starts_with("tilo-capture-projection-")
            || name.len() > 128
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || value.ino == 0
            || value.source == 0
            || value.output == Some(0)
            || value.scratch == Some(0)
        {
            return Err(failure("invalid capture alias shape"));
        }
        Ok(value)
    }
}

#[cfg(not(target_os = "linux"))]
impl ProjectionReceipt {
    /// # Errors
    /// Always returns the capture unsupported-platform error.
    pub fn cleanup(&self) -> io::Result<()> {
        Err(super::Error::Unsupported.into())
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn rejects_aliases_outside_protected_namespace() {
        let valid = serde_json::json!({
            "path": "/tmp/tilo-capture-projection-test",
            "dev": 1, "ino": 2, "source": 3, "output": null, "scratch": null
        });
        let alias: ProjectionReceipt = serde_json::from_value(valid.clone()).unwrap();
        assert_eq!(serde_json::to_value(alias).unwrap(), valid);
        for path in [
            "/etc/tilo-capture-projection-test",
            "/tmp/unrelated",
            "/tmp/tilo-capture-projection-../x",
        ] {
            let mut wire = valid.clone();
            wire["path"] = serde_json::json!(path);
            assert!(serde_json::from_value::<ProjectionReceipt>(wire).is_err());
        }
    }
}
