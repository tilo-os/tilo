//! TILO(model-device): the document of a model module, kept by its host.
//!
//! An app's model runs as a WebAssembly module on every device that holds a
//! workspace's keys. The module carries the app's verbs and nothing of
//! Automerge's storage: built with `--cfg tilo_model_guest`, its
//! [`AutoCommit`](crate::AutoCommit) is a handle on a document the host keeps,
//! and every read and write is one call to the host, in the form [`wire`]
//! writes. The host applies it to its own document — the one it signs, keeps
//! the history of and syncs — so a verb's change is made by the same Automerge
//! on every device, and the module stays the size of the app's own code.
//!
//! [`Documents`] is the device's side: it holds the documents a module has open (the
//! one the request is about is handle 0) and answers each call.

pub mod wire;

/// The calls a module makes on a document, by the byte that names each.
///
/// Every call but [`NEW`](op::NEW) and [`LOAD`](op::LOAD) is followed by the
/// handle of the document it is on.
pub mod op {
    pub const NEW: u8 = 0;
    pub const LOAD: u8 = 1;
    pub const CLONE: u8 = 2;
    pub const FORK: u8 = 3;
    pub const FORK_AT: u8 = 4;
    pub const COMMITTED_FORK_AT: u8 = 5;
    pub const RELEASE: u8 = 6;
    pub const MERGE: u8 = 7;
    pub const SET_ACTOR: u8 = 8;
    pub const GET_HEADS: u8 = 9;
    pub const COMMITTED_HEADS: u8 = 10;
    pub const COMMIT_WITH: u8 = 11;
    pub const ROLLBACK: u8 = 12;
    pub const EMPTY_CHANGE: u8 = 13;
    pub const PENDING_OPS: u8 = 14;
    pub const BASE_HEADS: u8 = 15;
    pub const ISOLATE: u8 = 16;
    pub const INTEGRATE: u8 = 17;
    pub const SAVE: u8 = 18;
    pub const SAVE_NOCOMPRESS: u8 = 19;
    pub const SAVE_AFTER: u8 = 20;
    pub const SAVE_INCREMENTAL: u8 = 21;
    pub const SAVE_COMMITTED: u8 = 22;
    pub const LOAD_INCREMENTAL: u8 = 23;
    pub const MISSING_DEPS: u8 = 24;
    pub const IMPORT: u8 = 25;
    pub const HASH_FOR_OPID: u8 = 26;
    pub const IS_EMPTY: u8 = 27;
    pub const KEYS: u8 = 28;
    pub const MAP_RANGE: u8 = 29;
    pub const LIST_RANGE: u8 = 30;
    pub const VALUES: u8 = 31;
    pub const LENGTH: u8 = 32;
    pub const OBJECT_TYPE: u8 = 33;
    pub const MARKS: u8 = 34;
    pub const GET_MARKS: u8 = 35;
    pub const TEXT: u8 = 36;
    pub const SPANS: u8 = 37;
    pub const GET: u8 = 38;
    pub const GET_ALL: u8 = 39;
    pub const PARENTS: u8 = 40;
    pub const GET_CURSOR: u8 = 41;
    pub const GET_CURSOR_POSITION: u8 = 42;
    pub const HYDRATE: u8 = 43;
    pub const PUT: u8 = 44;
    pub const PUT_OBJECT: u8 = 45;
    pub const INSERT: u8 = 46;
    pub const INSERT_OBJECT: u8 = 47;
    pub const INCREMENT: u8 = 48;
    pub const DELETE: u8 = 49;
    pub const SPLICE: u8 = 50;
    pub const SPLICE_TEXT: u8 = 51;
    pub const MARK: u8 = 52;
    pub const UNMARK: u8 = 53;
    pub const SPLIT_BLOCK: u8 = 54;
    pub const JOIN_BLOCK: u8 = 55;
    pub const REPLACE_BLOCK: u8 = 56;
    pub const UPDATE_TEXT: u8 = 57;
    pub const UPDATE_SPANS: u8 = 58;
    pub const UPDATE_OBJECT: u8 = 59;
    pub const BATCH_CREATE_OBJECT: u8 = 60;
    pub const INIT_ROOT_FROM_HYDRATE: u8 = 61;
    pub const DESCRIBE: u8 = 62;
    pub const REPLACE_VIEW: u8 = 63;
    pub const CHANGE_META: u8 = 64;
    pub const HASH_OF_ACTOR_SEQ: u8 = 65;
    pub const APPLIED_SEQ: u8 = 66;
    pub const RESET_DIFF_CURSOR: u8 = 67;
    pub const UPDATE_DIFF_CURSOR: u8 = 68;
    pub const DIFF_CURSOR: u8 = 69;
}

/// An answer that went well starts with this byte, followed by what was
/// asked; one that did not starts with [`REFUSED`] and the reason as text.
pub const ANSWERED: u8 = 0;
/// See [`ANSWERED`].
pub const REFUSED: u8 = 1;

#[cfg(not(tilo_model_guest))]
mod documents;
#[cfg(not(tilo_model_guest))]
pub use documents::Documents;

/// What a document's reads yield in a module: the host's answer, read once.
#[cfg(tilo_model_guest)]
#[derive(Clone, Debug)]
pub(crate) struct Hosted<'a, T> {
    items: std::vec::IntoIter<T>,
    doc: std::marker::PhantomData<&'a ()>,
}

#[cfg(tilo_model_guest)]
impl<T> Default for Hosted<'_, T> {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

#[cfg(tilo_model_guest)]
impl<T> Hosted<'_, T> {
    pub(crate) fn new(items: Vec<T>) -> Self {
        Self {
            items: items.into_iter(),
            doc: std::marker::PhantomData,
        }
    }
}

#[cfg(tilo_model_guest)]
impl<T> Iterator for Hosted<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.items.next()
    }
}

/// What a module cannot do with a document: the host keeps it, and only the
/// device does `what` (sync it, diff it, take its changes apart). The module
/// traps, and its host refuses the call it was making.
#[cfg(tilo_model_guest)]
#[cold]
#[inline(never)]
pub(crate) fn outside_device(what: &str) -> ! {
    panic!("a model module cannot do this with its host's document: {what}")
}
