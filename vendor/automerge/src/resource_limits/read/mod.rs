//! Borrowed reads of retained native storage. A read never qualifies its source.
//!
//! The 0.12 port walks the op set's own column iterators (`OpIter`) instead of
//! the public read API: the public API clones actor IDs into `ExId`s, collects
//! result vectors and builds clocks, none of which is reserved. hexane 1.0's
//! positioned iterators and slab searches are allocation-free, so the only heap
//! allocation a query makes is its boxed workspace (plus, on a malformed column
//! only, one fixed error string), and both are reserved before they happen.
mod identity;
mod scalar;
mod sequence;
use super::custody::{NativeCloneCandidate, NativeOwned, NativeWorkTurn};
use super::{NativeAllowance, NativeReservation, NativeResourceError};
use crate::op_set2::op_set::OpIter;
use crate::op_set2::types::{Action, KeyRef, ScalarValue};
use crate::op_set2::Op;
use crate::types::{ObjId, OpId};
use crate::{AutoCommit, ObjType};
use std::borrow::Cow;
use std::cell::Cell;
use std::mem::size_of;
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
    allowance: &'turn NativeAllowance,
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

// A column cursor carries substantial inline state, so each query keeps its
// cursor and possible winner in a separately reserved heap workspace. The
// fields drop before credit, including on a visitor's early return or unwind,
// and nested visitor queries never grow the caller's stack by a whole cursor.
struct Query<'doc> {
    workspace: Box<Workspace<'doc>>,
    _reservation: NativeReservation,
}
struct Workspace<'doc> {
    iter: OpIter<'doc>,
    winner: Option<Op<'doc>>,
}

// OpIter::try_next allocates at most one of the fixed ReadOpError::InvalidOpId
// strings on its failure path (largest: "missing actor or counter", 24
// bytes). Every other ReadOpError variant is inline. Keep that covered too.
const READ_ERROR_BYTES: usize = 24;

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
        allowance: &'turn NativeAllowance,
    ) -> Result<Self, NativeResourceError> {
        if document.native_read_is_isolated() {
            return Err(NativeResourceError::Unaccounted);
        }
        Ok(Self {
            document,
            allowance,
        })
    }

    /// Reserve caller-owned output from this session's same current turn.
    /// This is a budget operation, not an owner or disclosure qualification.
    pub fn reserve_output(&self, bytes: usize) -> Result<NativeReservation, NativeResourceError> {
        self.allowance.reserve(bytes)
    }

    /// The root of this exact document; constructing the handle allocates nothing.
    pub fn root(&self) -> NativeObject<'doc> {
        NativeObject {
            id: NativeId {
                document: self.document,
                id: ObjId::root().0,
            },
        }
    }

    /// Visits visible values without lending native owners or allocating output.
    pub fn visit_property<E>(
        &self,
        object: NativeObject<'doc>,
        key: &str,
        selection: NativeSelection,
        visitor: impl for<'value> FnMut(NativeValue<'value, 'doc>, NativeId<'doc>) -> Result<(), E>,
    ) -> Result<usize, NativeReadError<E>> {
        self.property(object, key, selection, None, visitor)
    }

    pub fn visit_property_with_steps<E>(
        &self,
        object: NativeObject<'doc>,
        key: &str,
        selection: NativeSelection,
        steps: &NativeReadSteps,
        visitor: impl for<'value> FnMut(NativeValue<'value, 'doc>, NativeId<'doc>) -> Result<(), E>,
    ) -> Result<usize, NativeReadError<E>> {
        self.property(object, key, selection, Some(steps), visitor)
    }

    fn property<E>(
        &self,
        object: NativeObject<'doc>,
        key: &str,
        selection: NativeSelection,
        steps: Option<&NativeReadSteps>,
        mut visitor: impl for<'value> FnMut(NativeValue<'value, 'doc>, NativeId<'doc>) -> Result<(), E>,
    ) -> Result<usize, NativeReadError<E>> {
        let mut query = self.query(object, Some(key), false, steps)?;
        let mut count = 0;
        while let Some(op) = self.next_op(&mut query, steps)? {
            if !self.visible(&op, steps)? || matches!(op.action, Action::Delete | Action::Increment)
            {
                continue;
            }
            if selection == NativeSelection::Winner {
                // Ops of one key are stored in ascending id order, so the last
                // visible op is the greatest visible id: the native winner.
                query.workspace.winner = Some(op);
            } else {
                self.visit_op(&op, steps, false, &mut visitor)?;
                count += 1;
            }
        }
        if let Some(op) = query.workspace.winner.take() {
            self.visit_op(&op, steps, false, &mut visitor)?;
            count = 1;
        }
        Ok(count)
    }

    /// Visits unique visible map keys, lending each key only within its callback.
    pub fn visit_keys<E>(
        &self,
        object: NativeObject<'doc>,
        mut visitor: impl for<'key> FnMut(&'key str) -> Result<(), E>,
    ) -> Result<usize, NativeReadError<E>> {
        self.keys(object, None, |key| visitor(key).map(ControlFlow::Continue))
    }

    pub fn visit_keys_with_steps<E>(
        &self,
        object: NativeObject<'doc>,
        steps: &NativeReadSteps,
        visitor: impl for<'key> FnMut(&'key str) -> Result<ControlFlow<()>, E>,
    ) -> Result<usize, NativeReadError<E>> {
        self.keys(object, Some(steps), visitor)
    }

    fn keys<E>(
        &self,
        object: NativeObject<'doc>,
        steps: Option<&NativeReadSteps>,
        mut visitor: impl for<'key> FnMut(&'key str) -> Result<ControlFlow<()>, E>,
    ) -> Result<usize, NativeReadError<E>> {
        let mut query = self.query(object, None, false, steps)?;
        let mut previous = None;
        let mut count = 0;
        while let Some(op) = self.next_op(&mut query, steps)? {
            if !self.visible(&op, steps)? || matches!(op.action, Action::Delete | Action::Increment)
            {
                continue;
            }
            let KeyRef::Map(Cow::Borrowed(key)) = op.key else {
                // Hexane's string cursor lends immutable slab bytes. Refuse a
                // future owning cursor representation until its work is planned.
                return Err(NativeResourceError::Unaccounted.into());
            };
            if previous != Some(key) {
                count += 1;
                if visitor(key).map_err(NativeReadError::Visitor)?.is_break() {
                    return Ok(count);
                }
                previous = Some(key);
            }
        }
        Ok(count)
    }

    fn next_op(
        &self,
        query: &mut Query<'doc>,
        steps: Option<&NativeReadSteps>,
    ) -> Result<Option<Op<'doc>>, NativeResourceError> {
        if query.workspace.iter.len() == 0 {
            return Ok(None);
        }
        if let Some(steps) = steps {
            steps.charge(1)?;
        }
        query
            .workspace
            .iter
            .try_next()
            .map_err(|_| NativeResourceError::Invalid)
    }

    fn visible(
        &self,
        op: &Op<'doc>,
        steps: Option<&NativeReadSteps>,
    ) -> Result<bool, NativeResourceError> {
        if let Some(steps) = steps {
            if op.is_counter() {
                // Counter visibility walks successors; charge its complete
                // conservative span before the first successor inspection.
                steps.charge(op.succ_inc().len())?;
            }
        }
        Ok(op.visible())
    }

    fn check_id(&self, id: NativeId<'doc>) -> Result<(), NativeResourceError> {
        if !std::ptr::eq(self.document, id.document) {
            return Err(NativeResourceError::Invalid);
        }
        Ok(())
    }

    fn query(
        &self,
        object: NativeObject<'doc>,
        key: Option<&str>,
        sequence: bool,
        steps: Option<&NativeReadSteps>,
    ) -> Result<Query<'doc>, NativeResourceError> {
        self.check_id(object.id)?;
        let object = ObjId(object.id.id);
        let ops = &self.document.doc.ops;
        let valid_kind = matches!(
            (sequence, ops.object_type(&object)),
            (false, Some(ObjType::Map | ObjType::Table))
                | (true, Some(ObjType::List | ObjType::Text))
        );
        if !valid_kind {
            return Err(NativeResourceError::Invalid);
        }
        // Query setup itself consumes a step before any workspace allocation.
        // Nested visitors use the same consumer ceiling without resetting it.
        if let Some(steps) = steps {
            steps.charge(1)?;
        }
        let reservation = self
            .allowance
            .reserve(size_of::<Workspace<'doc>>() + READ_ERROR_BYTES)?;
        // Range planning (hexane 1.0 slab-index descent and sorted scans),
        // iterator construction and successor cursors are allocation-free.
        // Values and keys borrow slab slices; no public get/keys, actor clone,
        // result Vec, clock or marking iterator is used.
        let range = match key {
            Some(key) => ops.prop_range(&object, key),
            None => ops.scope_to_obj(&object),
        };
        let workspace = Box::new(Workspace {
            iter: ops.iter_range(&range),
            winner: None,
        });
        Ok(Query {
            workspace,
            _reservation: reservation,
        })
    }

    fn visit_op<E>(
        &self,
        op: &Op<'doc>,
        steps: Option<&NativeReadSteps>,
        allow_mark: bool,
        visitor: &mut impl for<'value> FnMut(NativeValue<'value, 'doc>, NativeId<'doc>) -> Result<(), E>,
    ) -> Result<(), NativeReadError<E>> {
        let id = NativeId {
            document: self.document,
            id: op.id,
        };
        let kind = match op.action {
            Action::MakeMap => Some(ObjType::Map),
            Action::MakeList => Some(ObjType::List),
            Action::MakeText => Some(ObjType::Text),
            Action::MakeTable => Some(ObjType::Table),
            Action::Set => None,
            Action::Mark if allow_mark => None,
            _ => return Err(NativeResourceError::Invalid.into()),
        };
        let value = if let Some(kind) = kind {
            NativeValue::Object {
                kind,
                object: NativeObject { id },
            }
        } else {
            let scalar = match &op.value {
                ScalarValue::Bytes(bytes) => NativeScalar::Bytes(bytes),
                ScalarValue::Str(text) => NativeScalar::Str(text),
                ScalarValue::Int(value) => NativeScalar::Int(*value),
                ScalarValue::Uint(value) => NativeScalar::Uint(*value),
                ScalarValue::F64(value) => NativeScalar::F64(*value),
                ScalarValue::Counter(initial) => {
                    if let Some(steps) = steps {
                        steps.charge(op.succ_inc().len())?;
                    }
                    // Wrapping, exactly like the native get/list_range sums.
                    let mut value = *initial;
                    for (_, increment) in op.succ_inc() {
                        if let Some(increment) = increment {
                            value = value.wrapping_add(increment);
                        }
                    }
                    NativeScalar::Counter(value)
                }
                ScalarValue::Timestamp(value) => NativeScalar::Timestamp(*value),
                ScalarValue::Boolean(value) => NativeScalar::Boolean(*value),
                ScalarValue::Unknown { type_code, bytes } => NativeScalar::Unknown {
                    type_code: *type_code,
                    bytes,
                },
                ScalarValue::Null => NativeScalar::Null,
            };
            NativeValue::Scalar(scalar)
        };
        visitor(value, id).map_err(NativeReadError::Visitor)
    }
}
