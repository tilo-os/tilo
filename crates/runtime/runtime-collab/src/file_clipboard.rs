//! The local host's Files clipboard. Offers are opaque, never path lists.
use serde::{Deserialize, Serialize};

/// The most descriptors one command hands over (each item of a plain
/// [`Command::Capture`] travels as its own descriptor).
pub const MAX_ENTRIES: usize = 256;
pub const MAX_WIRE_BYTES: usize = 128 * 1024;
/// The most entries one [`Command::CaptureNamed`] names. It bounds one
/// message, which [`MAX_WIRE_BYTES`] bounds anyway; a selection of any size
/// is captured in as many commands as it needs, one folder descriptor each,
/// and published as one offer.
pub const MAX_NAMED_PER_COMMAND: usize = 8192;
/// The longest leaf name a [`Command::CaptureNamed`] carries (`NAME_MAX`).
pub const MAX_NAME_BYTES: usize = 255;

/// A precondition from the actual retained object shown by this window.
/// This is not an identity claim or a grant: the host compares it with both
/// its authorized lookup and the descriptor the attested peer handed over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectIdentity {
    pub device: u64,
    pub inode: u64,
}

/// The highest [`SelectedDescriptor::descriptor`] a command may carry.
pub const MAX_DESCRIPTOR: u32 = i32::MAX.unsigned_abs();

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedDescriptor {
    pub path: Vec<u8>,
    /// The app passes the descriptor open in its process; on the wire this is
    /// the number of the duplicate handed over on the connection's descriptor
    /// lane (`runtime_portal::descriptor_lane`), never a descriptor the host
    /// reaches into the app for.
    pub descriptor: u32,
    pub expected: ObjectIdentity,
}

impl SelectedDescriptor {
    fn valid(&self) -> bool {
        !self.path.is_empty()
            && self.path.len() <= 4096
            && !self.path.contains(&0)
            && self.descriptor <= MAX_DESCRIPTOR
    }
}

/// One leaf name in a folder: not empty, no `/` or NUL, not `.` or `..`.
fn valid_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && !name.contains(&b'/')
        && !name.contains(&0)
        && name != b"."
        && name != b".."
}

/// An entry of a shown folder, by its leaf name and the object the window
/// showed under it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedEntry {
    pub name: Vec<u8>,
    pub expected: ObjectIdentity,
}

/// What a paste does with an item whose name the destination already has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Conflict {
    /// Ask first: when names clash, the paste does not start and answers
    /// [`Reply::Conflicts`] so the window can ask the person.
    Ask,
    /// Give the pasted item a free name ("report 2.pdf").
    #[default]
    KeepBoth,
    /// Leave the existing item and don't paste this one.
    Skip,
    /// Move the existing item to Trash, then paste.
    Replace,
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
    /// Capture the entries of the shown folder `folder` as one ticket. One
    /// descriptor travels however many entries there are; each entry names
    /// the object the window showed, and the host pins it by that identity,
    /// not by a descriptor.
    CaptureNamed {
        folder: SelectedDescriptor,
        entries: Vec<NamedEntry>,
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
        /// What to do about names the destination already has. An older
        /// window that names none keeps both, as before.
        #[serde(default)]
        conflict: Conflict,
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
    /// Every selected descriptor this command carries.
    pub fn selected(&self) -> impl Iterator<Item = &SelectedDescriptor> {
        let (sources, single) = match self {
            Self::Capture {
                sources,
                destination,
            } => (sources.as_slice(), destination.as_ref()),
            Self::RestoreTrash { source, .. } => (<&[SelectedDescriptor]>::default(), Some(source)),
            Self::CaptureNamed { folder, .. } => (<&[SelectedDescriptor]>::default(), Some(folder)),
            _ => (<&[SelectedDescriptor]>::default(), None),
        };
        sources.iter().chain(single)
    }

    /// [`Self::selected`], to rename them.
    pub fn selected_mut(&mut self) -> impl Iterator<Item = &mut SelectedDescriptor> {
        let (sources, single) = match self {
            Self::Capture {
                sources,
                destination,
            } => (sources.as_mut_slice(), destination.as_mut()),
            Self::RestoreTrash { source, .. } => {
                (<&mut [SelectedDescriptor]>::default(), Some(source))
            }
            Self::CaptureNamed { folder, .. } => {
                (<&mut [SelectedDescriptor]>::default(), Some(folder))
            }
            _ => (<&mut [SelectedDescriptor]>::default(), None),
        };
        sources.iter_mut().chain(single)
    }

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
            Self::CaptureNamed { folder, entries } => {
                folder.valid()
                    && !entries.is_empty()
                    && entries.len() <= MAX_NAMED_PER_COMMAND
                    && entries.iter().all(|entry| valid_name(&entry.name))
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
                ..
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
    /// The entries of a [`Command::CaptureNamed`], held as one ticket.
    CapturedNamed {
        ticket: String,
        count: usize,
    },
    /// The paste asked first ([`Conflict::Ask`]) and the destination already
    /// has `count` of the names, the first of them in `names`. Nothing was
    /// pasted; paste again with the person's choice.
    Conflicts {
        names: Vec<String>,
        count: usize,
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
                conflict: Conflict::KeepBoth,
            }
            .valid()
        );
        assert!(
            serde_json::from_str::<Command>(r#"{"Publish":{"paths":[[47]],"cut":false}}"#).is_err()
        );
    }

    #[test]
    fn named_capture_carries_one_descriptor_and_old_pastes_keep_both() {
        let selected = || SelectedDescriptor {
            path: b"/workspace/folder".to_vec(),
            descriptor: 3,
            expected: ObjectIdentity {
                device: 1,
                inode: 2,
            },
        };
        let id = "39949036-9d70-4796-9a8d-388648dc95f6";
        let named = |names: Vec<Vec<u8>>| Command::CaptureNamed {
            folder: selected(),
            entries: names
                .into_iter()
                .map(|name| NamedEntry {
                    name,
                    expected: ObjectIdentity {
                        device: 1,
                        inode: 3,
                    },
                })
                .collect(),
        };
        assert!(
            named(vec![b"a.txt".to_vec(); MAX_NAMED_PER_COMMAND]).valid(),
            "past 256 entries"
        );
        assert!(!named(vec![b"a.txt".to_vec(); MAX_NAMED_PER_COMMAND + 1]).valid());
        for bad in [
            vec![],
            b".".to_vec(),
            b"..".to_vec(),
            b"a/b".to_vec(),
            vec![b'a', 0],
        ] {
            assert!(!named(vec![bad]).valid());
        }
        assert!(!named(vec![]).valid());
        assert_eq!(
            named(vec![b"a".to_vec()]).selected().count(),
            1,
            "one descriptor"
        );
        // A window that names no policy keeps both, as before.
        let old: Command = serde_json::from_str(&format!(
            r#"{{"Paste":{{"offer":"{id}","destination":"{id}","operation":"{id}"}}}}"#
        ))
        .unwrap();
        assert!(matches!(
            old,
            Command::Paste {
                conflict: Conflict::KeepBoth,
                ..
            }
        ));
    }
}
