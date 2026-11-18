//! The `monitor.processes` resource: the process table's layout, shared with
//! everyone who sees it, as the app that draws it and an agent that sets it both
//! see it.

use tilo_resource::{self, Model, resource};

/// The column the process table sorts by.
#[derive(
    Model, Default, Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    Pid,
    Name,
    User,
    #[default]
    Cpu,
    Mem,
    Threads,
}

impl SortKey {
    /// Numeric columns default to descending, text columns to ascending.
    #[must_use]
    pub fn default_desc(self) -> bool {
        !matches!(self, SortKey::Name | SortKey::User)
    }
}

/// The process table's layout, shared with everyone who sees it. The tab, the
/// filter and the selection are each window's own.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Monitor {
    /// Sort column
    pub sort: SortKey,
    /// Ascending
    pub ascending: bool,
    /// Flat list
    pub flat: bool,
}

impl Monitor {
    /// What the resource holds: the shared table layout.
    ///
    /// # Errors
    /// When a field cannot be read from the document.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "monitor.processes",
    read_content = Monitor::read_document
)]
impl Monitor {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_layout_sorts_by_cpu_and_numeric_columns_descend() {
        assert_eq!(Monitor::default().sort, SortKey::Cpu);
        assert!(SortKey::Cpu.default_desc());
        assert!(SortKey::Mem.default_desc());
        assert!(!SortKey::Name.default_desc());
        assert!(!SortKey::User.default_desc());
    }
}
