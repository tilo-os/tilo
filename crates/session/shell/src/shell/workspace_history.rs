//! Native compatibility path for the shared workspace history index.
#[cfg(test)]
use crate::shell::state::{RoomId, WorkspaceTree};
#[cfg(test)]
use automerge::AutoCommit;
pub use qos_desktop::activity::index::*;
#[cfg(test)]
use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use ui::record::{RecordCheckout, RecordKind};
#[cfg(test)]
use ui::widget::HistoryEntryId;
#[cfg(test)]
use ui::{RecordId, RecordStore};
include!("workspace_history_tests.rs");
