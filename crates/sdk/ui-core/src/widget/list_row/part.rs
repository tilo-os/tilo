//! The parts of a row a press can land on, and the keys that name them across rebuilds.

/// One of a row's own pressable controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RowPart {
    Action(usize),
    Check,
    Change,
}

impl RowPart {
    /// What names the control across a rebuild: the row's id and which control of the row
    /// it is.
    pub(super) fn key(self, row: &str) -> String {
        if row.is_empty() {
            return String::new();
        }
        match self {
            Self::Action(index) => format!("{row}/action/{index}"),
            Self::Check => format!("{row}/check"),
            Self::Change => format!("{row}/change"),
        }
    }
}
