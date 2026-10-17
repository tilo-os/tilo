//! Durable capture custody composes native supervision with app-owned cleanup.
pub(super) use crate::capture::process::Incarnation;
use crate::capture::process::{Custody as NativeCustody, Namespace};
use serde::{Deserialize, Serialize};
use std::io;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Custody {
    pub(super) host: Incarnation,
    pub(super) namespace: Option<Namespace>,
    #[serde(default)]
    pub(super) projection: Option<crate::capture::ProjectionReceipt>,
    #[serde(default)]
    pub(super) launch_acknowledged: bool,
    #[serde(default)]
    pub(super) inventory_scratch: Option<super::store::ScratchReceipt>,
}
impl Custody {
    fn native(&self) -> NativeCustody {
        NativeCustody {
            host: self.host,
            namespace: self.namespace,
            launch_acknowledged: self.launch_acknowledged,
        }
    }
    pub(super) fn active_namespace(&self) -> io::Result<bool> {
        self.native().active_namespace()
    }
    pub(super) fn admit_namespace(&mut self, namespace: Namespace) -> io::Result<()> {
        let mut native = self.native();
        native.admit_namespace(namespace)?;
        self.namespace = native.namespace;
        Ok(())
    }
    pub(super) fn require_stopped(&self) -> io::Result<()> {
        self.native().require_stopped()?;
        if let Some(alias) = &self.projection {
            alias.cleanup()?;
        }
        if let Some(scratch) = &self.inventory_scratch {
            scratch.cleanup()?;
        }
        Ok(())
    }
    pub(super) fn stop_and_observe(&self) -> io::Result<()> {
        self.native().stop_and_observe()?;
        self.require_stopped()
    }
}
