//! Borrowed reads of retained native storage. A read never qualifies its source.
mod identity;
mod scalar;
mod sequence;
use super::{NativeCloneCandidate, NativeOwned, NativeWorkTurn};
use crate::op_set2::types::{Action, KeyRef, ScalarValue};
use crate::op_set2::{Op, OpIter};
use crate::resource_limits::{NativeAllowance, NativeReservation, NativeResourceError};
use crate::types::{ObjId, OpId};
use crate::{AutoCommit, ObjType};
pub use sequence::{NativeListEntry, NativeReadSteps};
use std::borrow::Cow;
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

// A column cursor carries substantial inline state, so each query keeps its
// cursor and possible winner in a separately reserved heap workspace. The
// fields drop before credit, including on a visitor's early return or unwind.
struct Query<'doc> {
    workspace: Box<Workspace<'doc>>,
    _reservation: NativeReservation,
}
struct Workspace<'doc> {
    iter: OpIter<'doc>,
    winner: Option<Op<'doc>>,
}

// OpIter::try_next uses at most one of the fixed InvalidOpId strings in its
// failure path (largest: "missing actor or counter", 24 bytes). All other
// ReadOpError variants are inline. Keep that error allocation covered too.
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
    /// Reads a measured but explicitly unqualified clone. This does not permit
    /// conversion to NativeOwned, admission, or native readiness.
    pub fn read_unqualified<'doc, 'turn>(
        &'doc self,
        turn: &'turn NativeWorkTurn,
    ) -> Result<NativeReadSession<'doc, 'turn>, NativeResourceError> {
        NativeReadSession::new(&self.value, self.check_turn(turn)?)
    }
}

impl<'doc, 'turn> NativeReadSession<'doc, 'turn> {
    /// Reserve caller-owned output from this session's same current turn.
    /// This is a budget operation, not an owner or disclosure qualification.
    pub fn reserve_output(&self, bytes: usize) -> Result<NativeReservation, NativeResourceError> {
        self.allowance.reserve(bytes)
    }

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
                query.workspace.winner = Some(op);
            } else {
                self.visit_op(&op, steps, false, &mut visitor)?;
                count += 1;
            }
        }
        if let Some(op) = &query.workspace.winner {
            self.visit_op(op, steps, false, &mut visitor)?;
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

    /// Required bytes for the native canonical textual object/operation ID.
    pub fn id_len(&self, id: NativeId<'doc>) -> Result<usize, NativeResourceError> {
        self.check_id(id)?;
        if id.id == ObjId::root().0 {
            return Ok(5);
        }
        let actor = self
            .document
            .doc
            .ops
            .get_actor_safe(id.id.actor())
            .ok_or(NativeResourceError::Invalid)?;
        let digits = decimal_digits(id.id.counter());
        actor
            .to_bytes()
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(digits + 1))
            .ok_or(NativeResourceError::WorkBytes)
    }

    /// Writes a canonical ID into already bounded caller storage. A short sink
    /// is refused before writing any bytes; this operation does not allocate.
    pub fn write_id(
        &self,
        id: NativeId<'doc>,
        sink: &mut [u8],
    ) -> Result<usize, NativeResourceError> {
        let length = self.id_len(id)?;
        if sink.len() < length {
            return Err(NativeResourceError::WorkBytes);
        }
        if id.id == ObjId::root().0 {
            sink[..length].copy_from_slice(b"_root");
            return Ok(length);
        }
        let actor = self
            .document
            .doc
            .ops
            .get_actor_safe(id.id.actor())
            .ok_or(NativeResourceError::Invalid)?;
        let digits = decimal_digits(id.id.counter());
        let mut value = id.id.counter();
        for position in (0..digits).rev() {
            sink[position] = b'0' + (value % 10) as u8;
            value /= 10;
        }
        sink[digits] = b'@';
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for (byte, output) in actor
            .to_bytes()
            .iter()
            .zip(sink[digits + 1..length].chunks_exact_mut(2))
        {
            output[0] = HEX[(byte >> 4) as usize];
            output[1] = HEX[(byte & 15) as usize];
        }
        Ok(length)
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
        let valid_kind = match (sequence, ops.object_type(&object)) {
            (false, Some(ObjType::Map | ObjType::Table)) => true,
            (true, Some(ObjType::List | ObjType::Text)) => true,
            _ => false,
        };
        if !valid_kind {
            return Err(NativeResourceError::Invalid);
        }
        // Query setup itself consumes a step before any workspace allocation.
        // Nested visitors use the same consumer ceiling without resetting it.
        if let Some(steps) = steps {
            steps.charge(1)?;
        }
        let bytes = size_of::<Workspace<'doc>>()
            .checked_add(READ_ERROR_BYTES)
            .and_then(|bytes| bytes.checked_add(ops.native_read_search_bytes()?))
            .ok_or(NativeResourceError::WorkBytes)?;
        let reservation = self.allowance.reserve(bytes)?;
        // Range planning, iterator construction and later successor seeks
        // allocate a temporary tree-search stack. Its largest concrete peak
        // stays charged separately from the live Box, including during nested
        // callbacks. RawReader and StrCursor lend slab slices; no public get/
        // keys, actor clone, result Vec, clock or marking iterator is used.
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
                    let mut value = *initial;
                    for (_, increment) in op.succ_inc() {
                        if let Some(increment) = increment {
                            value = value
                                .checked_add(increment)
                                .ok_or(NativeResourceError::Invalid)?;
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

fn decimal_digits(mut value: u64) -> usize {
    let mut digits = 1;
    while value >= 10 {
        digits += 1;
        value /= 10;
    }
    digits
}
