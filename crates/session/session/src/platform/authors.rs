//! Who wrote what a host shows: the name and colour presence shows a principal by, and the actors
//! the host's own mounts write records as.
//!
//! A change of a desk or a record names its author by the principal it writes as, never something
//! to show. A host that knows who that is — its presence roster, the mounts of its own page — names
//! them; one that does not leaves the change to the history's own naming.

use std::rc::Rc;
#[cfg(target_os = "linux")]
use std::sync::Arc;

/// The name and colour presence shows `principal` by, when this host knows them.
pub type People = Rc<dyn Fn(&str) -> Option<(String, (u8, u8, u8))>>;

/// Told the writer a record's document carries as it is read.
#[cfg(target_os = "linux")]
pub type Noted = Arc<dyn Fn(&ui::automerge::ActorId) + Send + Sync>;
#[cfg(not(target_os = "linux"))]
pub type Noted = Rc<dyn Fn(&ui::automerge::ActorId)>;

/// Name every actor this host's mounts write a record as: only the host that mounted them knows
/// which is whose.
#[cfg(target_os = "linux")]
pub type Register = Arc<dyn Fn(&mut ui::AuthorDirectory) + Send + Sync>;
#[cfg(not(target_os = "linux"))]
pub type Register = Rc<dyn Fn(&mut ui::AuthorDirectory)>;

/// How a host names the writers of the records the history reads.
#[derive(Clone)]
pub struct Writers {
    /// Told each record's writer as the history reads it.
    pub noted: Noted,
    /// Names every actor noted so far in the history's directory.
    pub register: Register,
}

impl Writers {
    /// A host whose records' writers it does not track.
    pub fn untracked() -> Self {
        Self {
            noted: new_noted(|_| ()),
            register: new_register(|_| ()),
        }
    }
}

#[cfg(target_os = "linux")]
pub fn new_noted(f: impl Fn(&ui::automerge::ActorId) + Send + Sync + 'static) -> Noted {
    Arc::new(f)
}

#[cfg(not(target_os = "linux"))]
pub fn new_noted(f: impl Fn(&ui::automerge::ActorId) + 'static) -> Noted {
    Rc::new(f)
}

#[cfg(target_os = "linux")]
pub fn new_register(f: impl Fn(&mut ui::AuthorDirectory) + Send + Sync + 'static) -> Register {
    Arc::new(f)
}

#[cfg(not(target_os = "linux"))]
pub fn new_register(f: impl Fn(&mut ui::AuthorDirectory) + 'static) -> Register {
    Rc::new(f)
}

/// A host that names nobody: every change keeps the history's own naming.
pub fn unnamed() -> People {
    Rc::new(|_| None)
}
