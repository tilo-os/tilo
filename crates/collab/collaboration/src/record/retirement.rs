//! Whether a record left its room: moving a record to another room stamps
//! its last change here retired (the toolkit's `ui::record::transfer`), and
//! a retired record is never opened as a live document again.

use automerge::{AutoCommit, ObjType};

use super::fields::{ObjectAt, StringAt, meta_ro, object_at, string_at};

/// Key, in a record's meta map, marking a record that left its room.
pub const RETIRED: &str = "retired";

/// Where a moved record went and who moved it, as its retirement records.
pub use collab_types::handoff::Successor;

/// Why a record is retired, as stamped into its last change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Retirement {
    /// Moved to another room; the successor there carries its history.
    /// `None` only for a stamp that no longer says where.
    Moved(Option<Successor>),
    /// Imported for a transfer that did not complete; nothing ever opened it.
    Abandoned,
}

impl Retirement {
    /// The reason the stamp records.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Moved(_) => "moved",
            Self::Abandoned => "abandoned",
        }
    }
}

/// The retirement stamped into `doc`, when it has left its room.
#[must_use]
pub fn retirement(doc: &AutoCommit) -> Option<Retirement> {
    let meta = meta_ro(doc)?;
    let stamp = match object_at(doc, &meta, RETIRED, ObjType::Map) {
        ObjectAt::Object(stamp) => stamp,
        ObjectAt::Missing => {
            // Present in some other shape: retired all the same.
            return (!matches!(string_at(doc, &meta, RETIRED), StringAt::Missing))
                .then_some(Retirement::Moved(None));
        }
        ObjectAt::Invalid => return Some(Retirement::Moved(None)),
    };
    let field = |key: &str| match string_at(doc, &stamp, key) {
        StringAt::String(value) => Some(value),
        StringAt::Missing | StringAt::Invalid => None,
    };
    if field("reason").as_deref() == Some("abandoned") {
        return Some(Retirement::Abandoned);
    }
    let successor = match (field("room"), field("record"), field("by")) {
        (Some(room), Some(record), Some(by)) => Some(Successor { room, record, by }),
        _ => None,
    };
    Some(Retirement::Moved(successor))
}

/// Whether `doc` is a record that left its room.
#[must_use]
pub fn is_retired(doc: &AutoCommit) -> bool {
    retirement(doc).is_some()
}
