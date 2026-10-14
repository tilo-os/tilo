//! The local host's Files clipboard. Offers are opaque, never path lists.
use serde::{Deserialize, Serialize};

pub const MAX_ENTRIES: usize = 256;
pub const MAX_WIRE_BYTES: usize = 128 * 1024;

/// A precondition from the actual retained object shown by this window.
/// This is not an identity claim or a grant: the host compares it with both
/// its authorized lookup and a descriptor duplicated from the attested peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedDescriptor {
    pub path: Vec<u8>,
    pub descriptor: u32,
    pub expected: ObjectIdentity,
}

impl SelectedDescriptor {
    fn valid(&self) -> bool {
        !self.path.is_empty()
            && self.path.len() <= 4096
            && !self.path.contains(&0)
            && i32::try_from(self.descriptor).is_ok()
    }
}

fn valid_id(value: &str) -> bool {
    crate::valid_uuid(value) && value != "00000000-0000-0000-0000-000000000000"
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Command {
    Snapshot,
    Recover {
        operation: String,
    },
    Capture {
        sources: Vec<SelectedDescriptor>,
        destination: Option<SelectedDescriptor>,
    },
    Release {
        tickets: Vec<String>,
    },
    Publish {
        sources: Vec<String>,
        cut: bool,
    },
    Paste {
        offer: String,
        destination: String,
        operation: String,
    },
    RestoreTrash {
        source: SelectedDescriptor,
        operation: String,
    },
    Observe {
        operation: String,
    },
    Cancel {
        operation: String,
    },
    Undo {
        operation: String,
    },
    Redo {
        operation: String,
    },
}

impl Command {
    pub fn valid(&self) -> bool {
        match self {
            Self::Snapshot => true,
            Self::Capture {
                sources,
                destination,
            } => {
                (!sources.is_empty() || destination.is_some())
                    && sources.len() <= MAX_ENTRIES
                    && sources.iter().all(SelectedDescriptor::valid)
                    && destination.as_ref().is_none_or(SelectedDescriptor::valid)
            }
            Self::Release { tickets } => {
                !tickets.is_empty()
                    && tickets.len() <= MAX_ENTRIES + 1
                    && tickets.iter().all(|id| valid_id(id))
            }
            Self::Publish { sources, .. } => {
                !sources.is_empty()
                    && sources.len() <= MAX_ENTRIES
                    && sources.iter().all(|id| valid_id(id))
            }
            Self::Paste {
                offer,
                destination,
                operation,
            } => valid_id(offer) && valid_id(destination) && valid_id(operation),
            Self::RestoreTrash { source, operation } => source.valid() && valid_id(operation),
            Self::Recover { operation }
            | Self::Observe { operation }
            | Self::Cancel { operation }
            | Self::Undo { operation }
            | Self::Redo { operation } => valid_id(operation),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub id: String,
    pub count: usize,
    pub cut: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reply {
    Captured {
        sources: Vec<String>,
        destination: Option<String>,
    },
    Released,
    Recovered {
        operations: Vec<String>,
        completed: usize,
        error: Option<String>,
    },
    Snapshot {
        offer: Option<Offer>,
    },
    Published {
        offer: Offer,
    },
    Running {
        operation: String,
    },
    Settled {
        operation: String,
        completed: usize,
        error: Option<String>,
        undone: bool,
    },
    Rejected {
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clipboard_wire_commands_bound_paths_and_require_opaque_receipts() {
        let selected = || SelectedDescriptor {
            path: b"/workspace/report".to_vec(),
            descriptor: 3,
            expected: ObjectIdentity {
                device: 1,
                inode: 2,
            },
        };
        let capture = |source| Command::Capture {
            sources: vec![source],
            destination: None,
        };
        assert!(capture(selected()).valid());
        let mut long = selected();
        long.path = vec![b'x'; 4097];
        assert!(!capture(long).valid());
        let mut nul = selected();
        nul.path.push(0);
        assert!(!capture(nul).valid());
        let mut fd = selected();
        fd.descriptor = u32::MAX;
        assert!(!capture(fd).valid());
        assert!(
            !Command::Capture {
                sources: vec![selected(); MAX_ENTRIES + 1],
                destination: None
            }
            .valid()
        );
        assert!(
            !Command::Publish {
                sources: vec!["/workspace/report".into()],
                cut: false
            }
            .valid()
        );
        let id = "39949036-9d70-4796-9a8d-388648dc95f6";
        assert!(
            Command::Publish {
                sources: vec![id.into()],
                cut: false
            }
            .valid()
        );
        assert!(
            !Command::Publish {
                sources: vec![],
                cut: false
            }
            .valid()
        );
        assert!(
            !Command::Observe {
                operation: "/workspace/file".into()
            }
            .valid()
        );
        assert!(
            Command::Observe {
                operation: id.into()
            }
            .valid()
        );
        assert!(!Command::Release { tickets: vec![] }.valid());
        assert!(
            !Command::Release {
                tickets: vec![id.into(); MAX_ENTRIES + 2]
            }
            .valid()
        );
        assert!(
            !Command::Paste {
                offer: id.into(),
                destination: "/workspace/folder".into(),
                operation: id.into(),
            }
            .valid()
        );
        assert!(
            serde_json::from_str::<Command>(r#"{"Publish":{"paths":[[47]],"cut":false}}"#).is_err()
        );
    }
}
