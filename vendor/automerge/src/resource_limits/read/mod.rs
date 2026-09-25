//! Borrowed reads of retained native storage. A read never qualifies its source.
//!
//! OWNER: lane L4. Public types are the 0.8 contract verbatim; method bodies
//! are to be ported onto 0.12 (0.8 source for reference:
//! alignment branch vendor/automerge/src/resource_limits/retained/read*).
mod scalar;
use super::custody::{NativeCloneCandidate, NativeOwned, NativeWorkTurn};
use super::{NativeReservation, NativeResourceError};
use crate::types::OpId;
use crate::{AutoCommit, ObjType};
use std::cell::Cell;
use std::ops::ControlFlow;
/// Which visible values of a map property to visit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSelection {
    /// The greatest visible operation ID, matching the native winner.
    Winner,
    /// Every visible conflicting operation in native order.
    All,
}

/// Resource refusal is separate from the embedding visitor's own error.
#[derive(Debug, PartialEq, Eq)]
pub enum NativeReadError<E> {
    /// The operation encountered a resource or input refusal.
    Resource(NativeResourceError),
    /// The visitor stopped traversal with its own error.
    Visitor(E),
}

impl<E> From<NativeResourceError> for NativeReadError<E> {
    fn from(error: NativeResourceError) -> Self {
        Self::Resource(error)
    }
}

/// An operation identity tied to one actual retained document, without an
/// owning actor ID or a public native identifier.
#[derive(Clone, Copy)]
pub struct NativeId<'doc> {
    document: &'doc AutoCommit,
    id: OpId,
}

impl PartialEq for NativeId<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.document, other.document) && self.id == other.id
    }
}
impl Eq for NativeId<'_> {}
impl std::fmt::Debug for NativeId<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeId").finish_non_exhaustive()
    }
}

/// An object handle borrowing its actual retained document. Handles from a
/// different document are rejected even when their operation IDs coincide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeObject<'doc> {
    id: NativeId<'doc>,
}
impl<'doc> NativeObject<'doc> {
    /// The object's nonowning identity, suitable for bounded canonical encoding.
    pub fn id(self) -> NativeId<'doc> {
        self.id
    }
}

/// Scalar data is lent only for the duration of a visitor call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NativeScalar<'value> {
    Bytes(&'value [u8]),
    Str(&'value str),
    Int(i64),
    Uint(u64),
    F64(f64),
    Counter(i64),
    Timestamp(i64),
    Boolean(bool),
    Unknown { type_code: u8, bytes: &'value [u8] },
    Null,
}

/// A scalar loan or an opaque object identity; no public native Value escapes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NativeValue<'value, 'doc> {
    Object {
        kind: ObjType,
        object: NativeObject<'doc>,
    },
    Scalar(NativeScalar<'value>),
}

/// An immutable session using the current owner-domain work turn. Each query
/// reserves its own workspace, so visitor callbacks may nest other queries.
///
/// Scalar loans cannot be retained outside their callback:
/// ```compile_fail
/// use automerge::{AutoCommit, resource_limits::*};
/// fn leak(owned: &NativeOwned<AutoCommit>, turn: &NativeWorkTurn) {
///     let read = owned.read(turn).unwrap();
///     let mut escaped = None;
///     read.visit_property(read.root(), "key", NativeSelection::Winner, |value, _| {
///         if let NativeValue::Scalar(NativeScalar::Str(value)) = value {
///             escaped = Some(value);
///         }
///         Ok::<_, ()>(())
///     }).unwrap();
///     println!("{:?}", escaped);
/// }
/// ```
/// Object identities cannot outlive their retained document:
/// ```compile_fail
/// use automerge::{AutoCommit, resource_limits::*};
/// fn leak(owned: NativeOwned<AutoCommit>, turn: &NativeWorkTurn) -> NativeObject<'static> {
///     owned.read(turn).unwrap().root()
/// }
/// ```
/// Read access does not expose a raw native document:
/// ```compile_fail
/// use automerge::{AutoCommit, resource_limits::*};
/// fn raw(owned: &NativeOwned<AutoCommit>) -> &AutoCommit { owned }
/// ```
pub struct NativeReadSession<'doc, 'turn> {
    document: &'doc AutoCommit,
    allowance: &'turn super::NativeAllowance,
}

impl std::fmt::Debug for NativeReadSession<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeReadSession").finish_non_exhaustive()
    }
}

/// One consumer's logical traversal ceiling. This is separate from the actual
/// current turn's memory ledger; nested queries must share this same counter.
/// No reset or Clone can refresh a partially spent traversal allowance.
#[derive(Debug)]
pub struct NativeReadSteps {
    remaining: Cell<usize>,
}
impl NativeReadSteps {
    pub fn new(maximum: usize) -> Result<Self, NativeResourceError> {
        if maximum == 0 || maximum > 1_000_000 {
            return Err(NativeResourceError::Operations);
        }
        Ok(Self {
            remaining: Cell::new(maximum),
        })
    }
    pub fn charge(&self, steps: usize) -> Result<(), NativeResourceError> {
        let remaining = self
            .remaining
            .get()
            .checked_sub(steps)
            .ok_or(NativeResourceError::Operations)?;
        self.remaining.set(remaining);
        Ok(())
    }
    pub fn remaining(&self) -> usize {
        self.remaining.get()
    }
}

/// One visible sequence winner. Indices include native mark boundaries, as
/// legacy ListRange does. Plain-text consumers skip is_mark, never scalar kinds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NativeListEntry<'value, 'doc> {
    pub index: usize,
    pub value: NativeValue<'value, 'doc>,
    pub id: NativeId<'doc>,
    pub is_mark: bool,
    pub conflict: bool,
}

impl NativeOwned<AutoCommit> {
    /// Opens a read session after checking the current turn's owner domain.
    pub fn read<'doc, 'turn>(
        &'doc self,
        turn: &'turn NativeWorkTurn,
    ) -> Result<NativeReadSession<'doc, 'turn>, NativeResourceError> {
        NativeReadSession::new(&self.value, self.check_turn(turn)?)
    }
}
impl NativeCloneCandidate<AutoCommit> {
    /// Reads a measured but explicitly unqualified clone.
    pub fn read_unqualified<'doc, 'turn>(
        &'doc self,
        turn: &'turn NativeWorkTurn,
    ) -> Result<NativeReadSession<'doc, 'turn>, NativeResourceError> {
        NativeReadSession::new(&self.value, self.check_turn(turn)?)
    }
}

impl<'doc, 'turn> NativeReadSession<'doc, 'turn> {
    fn new(
        document: &'doc AutoCommit,
        allowance: &'turn super::NativeAllowance,
    ) -> Result<Self, NativeResourceError> {
        if document.native_read_is_isolated() {
            return Err(NativeResourceError::Unaccounted);
        }
        Ok(Self { document, allowance })
    }
    /// Reserve caller-owned output from this session's same current turn.
    pub fn reserve_output(&self, bytes: usize) -> Result<NativeReservation, NativeResourceError> {
        self.allowance.reserve(bytes)
    }
    /// The root of this exact document; constructing the handle allocates nothing.
    pub fn root(&self) -> NativeObject<'doc> {
        NativeObject {
            id: NativeId {
                document: self.document,
                id: crate::types::ObjId::root().0,
            },
        }
    }
    pub fn pending_ops(&self) -> usize {
        crate::transaction::Transactable::pending_ops(self.document)
    }
    pub fn operation_count(&self) -> usize {
        todo!("L4")
    }
    pub fn visit_property<E>(
        &self,
        object: NativeObject<'doc>,
        key: &str,
        selection: NativeSelection,
        visitor: impl for<'value> FnMut(NativeValue<'value, 'doc>, NativeId<'doc>) -> Result<(), E>,
    ) -> Result<usize, NativeReadError<E>> {
        let _ = (object, key, selection, visitor);
        todo!("L4")
    }
    pub fn visit_property_with_steps<E>(
        &self,
        object: NativeObject<'doc>,
        key: &str,
        selection: NativeSelection,
        steps: &NativeReadSteps,
        visitor: impl for<'value> FnMut(NativeValue<'value, 'doc>, NativeId<'doc>) -> Result<(), E>,
    ) -> Result<usize, NativeReadError<E>> {
        let _ = (object, key, selection, steps, visitor);
        todo!("L4")
    }
    pub fn visit_keys<E>(
        &self,
        object: NativeObject<'doc>,
        visitor: impl for<'key> FnMut(&'key str) -> Result<(), E>,
    ) -> Result<usize, NativeReadError<E>> {
        let _ = (object, visitor);
        todo!("L4")
    }
    pub fn visit_keys_with_steps<E>(
        &self,
        object: NativeObject<'doc>,
        steps: &NativeReadSteps,
        visitor: impl for<'key> FnMut(&'key str) -> Result<ControlFlow<()>, E>,
    ) -> Result<usize, NativeReadError<E>> {
        let _ = (object, steps, visitor);
        todo!("L4")
    }
    pub fn visit_list<E>(
        &self,
        object: NativeObject<'doc>,
        steps: &NativeReadSteps,
        visitor: impl for<'value> FnMut(NativeListEntry<'value, 'doc>) -> Result<ControlFlow<()>, E>,
    ) -> Result<usize, NativeReadError<E>> {
        let _ = (object, steps, visitor);
        todo!("L4")
    }
    /// Counts the complete current committed frontier before delivering any
    /// hashes. Refuses pending operations; never commits or emits a prefix.
    pub fn visit_heads(
        &self,
        maximum: usize,
        visitor: impl FnMut(crate::ChangeHash),
    ) -> Result<usize, NativeResourceError> {
        let _ = (maximum, visitor);
        todo!("L4")
    }
    pub fn id_len(&self, id: NativeId<'doc>) -> Result<usize, NativeResourceError> {
        let _ = id;
        todo!("L4")
    }
    pub fn write_id(&self, id: NativeId<'doc>, sink: &mut [u8]) -> Result<usize, NativeResourceError> {
        let _ = (id, sink);
        todo!("L4")
    }
    /// Required bytes for the existing canonical binary object ID encoding.
    pub fn id_bytes_len(&self, id: NativeId<'doc>) -> Result<usize, NativeResourceError> {
        let _ = id;
        todo!("L4")
    }
    /// Writes the existing binary encoding without allocating.
    pub fn write_id_bytes(&self, id: NativeId<'doc>, sink: &mut [u8]) -> Result<usize, NativeResourceError> {
        let _ = (id, sink);
        todo!("L4")
    }
    /// Lends semantic counter/actor identity. The root is None.
    pub fn visit_id_parts<R>(
        &self,
        id: NativeId<'doc>,
        visitor: impl for<'a> FnOnce(Option<(u64, &'a [u8])>) -> R,
    ) -> Result<R, NativeResourceError> {
        let _ = (id, visitor);
        todo!("L4")
    }
}
