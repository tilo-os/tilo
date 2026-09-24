//! Shared room traversal. The legacy adapter carries no allocation qualification.

pub(crate) mod collections;

use automerge::{ObjId, ObjType, ReadDoc, ScalarValue, Value};

pub(crate) enum Selection {
    Winner,
    All,
}

pub(crate) enum RoomValue<'a, O> {
    Object { kind: ObjType, object: O },
    Bytes(&'a [u8]),
    Str(&'a str),
    Boolean(bool),
    Other,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReadError<E> {
    Malformed,
    Resource(automerge::resource_limits::NativeResourceError),
    Visitor(E),
}

impl<E> ReadError<ReadError<E>> {
    fn flatten(self) -> ReadError<E> {
        match self {
            Self::Malformed => ReadError::Malformed,
            Self::Resource(error) => ReadError::Resource(error),
            Self::Visitor(error) => error,
        }
    }
}

pub(crate) trait CopyValue {
    type Bytes: AsRef<[u8]>;
    type Text: AsRef<str>;
    fn bytes(
        &self,
        value: &[u8],
    ) -> Result<Self::Bytes, automerge::resource_limits::NativeResourceError>;
    fn text(
        &self,
        value: &str,
    ) -> Result<Self::Text, automerge::resource_limits::NativeResourceError>;
}

pub(crate) struct LegacyCopy;
impl CopyValue for LegacyCopy {
    type Bytes = Vec<u8>;
    type Text = String;
    fn bytes(
        &self,
        value: &[u8],
    ) -> Result<Vec<u8>, automerge::resource_limits::NativeResourceError> {
        Ok(value.to_vec())
    }
    fn text(&self, value: &str) -> Result<String, automerge::resource_limits::NativeResourceError> {
        Ok(value.to_owned())
    }
}

/// Backing drops before its current-turn reservation. No Clone or extraction.
pub(crate) struct Copied<T> {
    value: T,
    _reservation: automerge::resource_limits::NativeReservation,
}
impl AsRef<[u8]> for Copied<Vec<u8>> {
    fn as_ref(&self) -> &[u8] {
        &self.value
    }
}
impl AsRef<str> for Copied<String> {
    fn as_ref(&self) -> &str {
        &self.value
    }
}

pub(crate) struct TurnCopy<'a>(pub(crate) &'a automerge::resource_limits::NativeWorkTurn);
impl CopyValue for TurnCopy<'_> {
    type Bytes = Copied<Vec<u8>>;
    type Text = Copied<String>;
    fn bytes(
        &self,
        value: &[u8],
    ) -> Result<Self::Bytes, automerge::resource_limits::NativeResourceError> {
        use automerge::resource_limits::NativeResourceError;
        let reservation = self.0.reserve(value.len())?;
        let mut bytes = Vec::new();
        // The pinned standard Global allocator requests this exact byte layout.
        // Reserve before its first allocation, then write without growing it.
        bytes
            .try_reserve_exact(value.len())
            .map_err(|_| NativeResourceError::WorkBytes)?;
        if bytes.capacity() > reservation.bytes() {
            return Err(NativeResourceError::Unaccounted);
        }
        bytes.extend_from_slice(value);
        Ok(Copied {
            value: bytes,
            _reservation: reservation,
        })
    }
    fn text(
        &self,
        value: &str,
    ) -> Result<Self::Text, automerge::resource_limits::NativeResourceError> {
        use automerge::resource_limits::NativeResourceError;
        let reservation = self.0.reserve(value.len())?;
        let mut text = String::new();
        text.try_reserve_exact(value.len())
            .map_err(|_| NativeResourceError::WorkBytes)?;
        if text.capacity() > reservation.bytes() {
            return Err(NativeResourceError::Unaccounted);
        }
        text.push_str(value);
        Ok(Copied {
            value: text,
            _reservation: reservation,
        })
    }
}

/// Callbacks may inspect a borrowed scalar but cannot retain its native backing.
/// Owned application output needs its own allocation and custody boundary.
pub(crate) trait RoomRead {
    type Object;
    fn root(&self) -> Self::Object;
    fn visit_keys<E>(
        &self,
        object: &Self::Object,
        visitor: impl for<'a> FnMut(&'a str) -> Result<(), E>,
    ) -> Result<usize, ReadError<E>>;

    fn visit_property<E>(
        &self,
        object: &Self::Object,
        key: &str,
        selection: Selection,
        visitor: impl for<'a> FnMut(RoomValue<'a, Self::Object>) -> Result<(), E>,
    ) -> Result<usize, ReadError<E>>;
}

/// Borrowed logical governance values. Destination copies require caller custody.
pub(crate) enum GovernanceEntry<'a> {
    Owner(&'a str),
    Capability { key: &'a str, bytes: &'a [u8] },
    Peer { key: &'a str, name: &'a str },
    Identity { key: &'a str, bytes: &'a [u8] },
}

pub(crate) fn visit_governance_entries<R: RoomRead, E>(
    read: &R,
    mut visitor: impl for<'a> FnMut(GovernanceEntry<'a>) -> Result<(), E>,
) -> Result<(), ReadError<E>> {
    let root = read.root();
    let owner = read.visit_property(&root, "owner", Selection::Winner, |value| {
        if let RoomValue::Str(value) = value {
            visitor(GovernanceEntry::Owner(value))?;
        }
        Ok(())
    });
    // Match the existing room projection's ignored native lookup errors.
    // Resource and destination failures are never turned into absent policy.
    if let Err(error) = owner {
        if !matches!(error, ReadError::Malformed) {
            return Err(error);
        }
    }
    for (field, kind) in [("capabilities", 0), ("peers", 1), ("peer_identities_v1", 2)] {
        let values = read
            .visit_property(&root, field, Selection::Winner, |value| {
                // Legacy map-key lookup ignores malformed sequence containers.
                // Do not ask the native map query to traverse a List/Text and
                // turn that historical absence into a resource refusal.
                if let RoomValue::Object {
                    kind: ObjType::Map | ObjType::Table,
                    object,
                } = value
                {
                    read.visit_keys(&object, |key| {
                        let property =
                            read.visit_property(&object, key, Selection::Winner, |value| {
                                match (kind, value) {
                                    (0, RoomValue::Bytes(bytes)) => {
                                        visitor(GovernanceEntry::Capability { key, bytes })
                                    }
                                    (1, RoomValue::Str(name)) => {
                                        visitor(GovernanceEntry::Peer { key, name })
                                    }
                                    (2, RoomValue::Bytes(bytes)) => {
                                        visitor(GovernanceEntry::Identity { key, bytes })
                                    }
                                    _ => Ok(()),
                                }
                            });
                        match property {
                            Err(ReadError::Malformed) => Ok(()),
                            result => result.map(|_| ()),
                        }
                    })
                    .map_err(ReadError::flatten)?;
                }
                Ok::<(), ReadError<E>>(())
            })
            .map_err(ReadError::flatten);
        if let Err(error) = values {
            if !matches!(error, ReadError::Malformed) {
                return Err(error);
            }
        }
    }
    Ok(())
}

pub(crate) struct LegacyRead<'a, D: ReadDoc>(pub(crate) &'a D);

impl<'doc> RoomRead for automerge::resource_limits::NativeReadSession<'doc, '_> {
    type Object = automerge::resource_limits::NativeObject<'doc>;
    fn root(&self) -> Self::Object {
        self.root()
    }

    fn visit_keys<E>(
        &self,
        object: &Self::Object,
        visitor: impl for<'a> FnMut(&'a str) -> Result<(), E>,
    ) -> Result<usize, ReadError<E>> {
        self.visit_keys(*object, visitor)
            .map_err(|error| match error {
                automerge::resource_limits::NativeReadError::Resource(error) => {
                    ReadError::Resource(error)
                }
                automerge::resource_limits::NativeReadError::Visitor(error) => {
                    ReadError::Visitor(error)
                }
            })
    }

    fn visit_property<E>(
        &self,
        object: &Self::Object,
        key: &str,
        selection: Selection,
        mut visitor: impl for<'a> FnMut(RoomValue<'a, Self::Object>) -> Result<(), E>,
    ) -> Result<usize, ReadError<E>> {
        use automerge::resource_limits::{
            NativeReadError, NativeScalar, NativeSelection, NativeValue,
        };
        let selection = match selection {
            Selection::Winner => NativeSelection::Winner,
            Selection::All => NativeSelection::All,
        };
        self.visit_property(*object, key, selection, |value, _| {
            visitor(match value {
                NativeValue::Object { kind, object } => RoomValue::Object { kind, object },
                NativeValue::Scalar(NativeScalar::Bytes(bytes)) => RoomValue::Bytes(bytes),
                NativeValue::Scalar(NativeScalar::Str(value)) => RoomValue::Str(value),
                NativeValue::Scalar(NativeScalar::Boolean(value)) => RoomValue::Boolean(value),
                NativeValue::Scalar(_) => RoomValue::Other,
            })
        })
        .map_err(|error| match error {
            NativeReadError::Resource(error) => ReadError::Resource(error),
            NativeReadError::Visitor(error) => ReadError::Visitor(error),
        })
    }
}

impl<D: ReadDoc> RoomRead for LegacyRead<'_, D> {
    type Object = ObjId;
    fn root(&self) -> Self::Object {
        automerge::ROOT
    }

    fn visit_keys<E>(
        &self,
        object: &Self::Object,
        mut visitor: impl for<'a> FnMut(&'a str) -> Result<(), E>,
    ) -> Result<usize, ReadError<E>> {
        let mut count = 0;
        for key in self.0.keys(object) {
            visitor(&key).map_err(ReadError::Visitor)?;
            count += 1;
        }
        Ok(count)
    }

    fn visit_property<E>(
        &self,
        object: &ObjId,
        key: &str,
        selection: Selection,
        mut visitor: impl for<'a> FnMut(RoomValue<'a, ObjId>) -> Result<(), E>,
    ) -> Result<usize, ReadError<E>> {
        match selection {
            Selection::Winner => match self.0.get(object, key).map_err(|_| ReadError::Malformed)? {
                Some((value, object)) => {
                    visit_legacy_value(value, object, &mut visitor)?;
                    Ok(1)
                }
                None => Ok(0),
            },
            Selection::All => {
                let values = self
                    .0
                    .get_all(object, key)
                    .map_err(|_| ReadError::Malformed)?;
                let count = values.len();
                for (value, object) in values {
                    visit_legacy_value(value, object, &mut visitor)?;
                }
                Ok(count)
            }
        }
    }
}

pub(super) fn visit_legacy_value<E>(
    value: Value<'_>,
    object: ObjId,
    visitor: &mut impl for<'a> FnMut(RoomValue<'a, ObjId>) -> Result<(), E>,
) -> Result<(), ReadError<E>> {
    let value = match value {
        Value::Object(kind) => {
            return visitor(RoomValue::Object { kind, object }).map_err(ReadError::Visitor);
        }
        Value::Scalar(value) => value,
    };
    let view = match value.as_ref() {
        ScalarValue::Bytes(bytes) => RoomValue::Bytes(bytes),
        ScalarValue::Str(value) => RoomValue::Str(value),
        ScalarValue::Boolean(value) => RoomValue::Boolean(*value),
        _ => RoomValue::Other,
    };
    visitor(view).map_err(ReadError::Visitor)
}

pub(crate) fn strict_map<R: RoomRead>(
    read: &R,
    parent: &R::Object,
    key: &str,
) -> Result<R::Object, ReadError<super::media::MediaPolicyError>> {
    use super::media::MediaPolicyError;
    let mut result = None;
    read.visit_property(parent, key, Selection::All, |value| match value {
        RoomValue::Object {
            kind: ObjType::Map,
            object,
        } if result.is_none() => {
            result = Some(object);
            Ok(())
        }
        _ => Err(MediaPolicyError::Conflict),
    })?;
    result.ok_or(ReadError::Visitor(MediaPolicyError::Conflict))
}

pub(crate) fn strict_string<R: RoomRead, C: CopyValue>(
    read: &R,
    copies: &C,
    parent: &R::Object,
    key: &str,
) -> Result<C::Text, ReadError<super::media::MediaPolicyError>> {
    use super::media::MediaPolicyError;
    let mut result: Option<C::Text> = None;
    read.visit_property(parent, key, Selection::All, |value| {
        let RoomValue::Str(value) = value else {
            return Err(ReadError::Visitor(MediaPolicyError::Malformed));
        };
        match &result {
            Some(prior) if prior.as_ref() != value => {
                return Err(ReadError::Visitor(MediaPolicyError::Conflict));
            }
            Some(_) => {}
            None => result = Some(copies.text(value).map_err(ReadError::Resource)?),
        }
        Ok(())
    })
    .map_err(ReadError::flatten)?;
    result.ok_or(ReadError::Visitor(MediaPolicyError::Binding))
}

pub(crate) fn strict_bytes<R: RoomRead, C: CopyValue>(
    read: &R,
    copies: &C,
    parent: &R::Object,
    key: &str,
) -> Result<C::Bytes, ReadError<super::media::MediaPolicyError>> {
    use super::media::MediaPolicyError;
    let mut result: Option<C::Bytes> = None;
    read.visit_property(parent, key, Selection::All, |value| {
        let RoomValue::Bytes(value) = value else {
            return Err(ReadError::Visitor(MediaPolicyError::Malformed));
        };
        match &result {
            Some(prior) if prior.as_ref() != value => {
                return Err(ReadError::Visitor(MediaPolicyError::Conflict));
            }
            Some(_) => {}
            None => result = Some(copies.bytes(value).map_err(ReadError::Resource)?),
        }
        Ok(())
    })
    .map_err(ReadError::flatten)?;
    result.ok_or(ReadError::Visitor(MediaPolicyError::Binding))
}

/// Only the legacy facade erases allocation detail into its frozen error enum.
/// Bounded callers retain ReadError::Resource, including Unaccounted.
pub(crate) fn legacy_policy_error(
    error: ReadError<super::media::MediaPolicyError>,
) -> super::media::MediaPolicyError {
    use super::media::MediaPolicyError;
    match error {
        ReadError::Malformed => MediaPolicyError::Malformed,
        ReadError::Resource(_) => MediaPolicyError::Capacity,
        ReadError::Visitor(error) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room::media::MediaPolicyError;
    use automerge::{AutoCommit, ROOT, transaction::Transactable};

    #[test]
    fn room_read_adapter_copied_scalars_keep_current_credit_after_native_drop() {
        use automerge::resource_limits::{NativeAllocationSet, NativeAllowance};
        let mut document = AutoCommit::new();
        document.put(ROOT, "title", "a retained title").unwrap();
        document
            .put(ROOT, "blob", ScalarValue::Bytes(vec![3; 256]))
            .unwrap();
        document.commit();
        let current = NativeAllowance::new(1024 * 1024).unwrap();
        let initial = current.available_bytes();
        let mut allocations =
            NativeAllocationSet::new(NativeAllowance::new(1024 * 1024).unwrap()).unwrap();
        let turn = allocations.work_turn(current.clone()).unwrap();
        let candidate = document.clone_bounded(&mut allocations, &turn).unwrap();
        let read = candidate.read_unqualified(&turn).unwrap();
        let before = current.available_bytes();
        let title = strict_string(&read, &TurnCopy(&turn), &read.root(), "title").unwrap();
        let blob = strict_bytes(&read, &TurnCopy(&turn), &read.root(), "blob").unwrap();
        let retained = title.as_ref().len() + blob.as_ref().len();
        assert_eq!(current.available_bytes(), before - retained);
        drop(read);
        drop(candidate);
        drop(allocations);
        drop(turn);
        assert_eq!(title.as_ref(), "a retained title");
        assert_eq!(blob.as_ref(), &[3; 256]);
        assert_eq!(current.available_bytes(), initial - retained);
        drop(title);
        drop(blob);
        assert_eq!(current.available_bytes(), initial);
    }

    #[test]
    fn room_read_adapter_copy_refuses_before_work_and_retains_exact_boundary() {
        use automerge::resource_limits::{
            NativeAllocationSet, NativeAllowance, NativeResourceError,
        };
        let current = NativeAllowance::new(1024).unwrap();
        let allocations = NativeAllocationSet::new(NativeAllowance::new(1024).unwrap()).unwrap();
        let turn = allocations.work_turn(current.clone()).unwrap();
        let remaining = turn.reserve(current.available_bytes() - 4).unwrap();
        assert!(matches!(
            TurnCopy(&turn).bytes(&[1; 5]),
            Err(NativeResourceError::WorkBytes)
        ));
        assert_eq!(current.available_bytes(), 4);
        let exact = TurnCopy(&turn).text("four").unwrap();
        assert_eq!(exact.as_ref(), "four");
        assert_eq!(current.available_bytes(), 0);
        drop(exact);
        assert_eq!(current.available_bytes(), 4);
        drop(remaining);
    }

    #[test]
    fn room_read_adapter_scalar_conflicts_share_semantics_and_release_discarded_credit() {
        use automerge::resource_limits::{NativeAllocationSet, NativeAllowance};
        for other in ["same", "different"] {
            let mut document = AutoCommit::new();
            let mut remote = document.fork();
            document.put(ROOT, "value", "same").unwrap();
            document.commit();
            remote.put(ROOT, "value", other).unwrap();
            remote.commit();
            document.merge(&mut remote).unwrap();
            assert_eq!(document.get_all(ROOT, "value").unwrap().len(), 2);
            let current = NativeAllowance::new(1024 * 1024).unwrap();
            let mut allocations =
                NativeAllocationSet::new(NativeAllowance::new(1024 * 1024).unwrap()).unwrap();
            let turn = allocations.work_turn(current.clone()).unwrap();
            let candidate = document.clone_bounded(&mut allocations, &turn).unwrap();
            let read = candidate.read_unqualified(&turn).unwrap();
            let before = current.available_bytes();
            let result = strict_string(&read, &TurnCopy(&turn), &read.root(), "value");
            if other == "same" {
                let value = result.unwrap();
                assert_eq!(value.as_ref(), "same");
                assert_eq!(
                    current.available_bytes(),
                    before - 4,
                    "one copy despite two identical native values"
                );
                drop(value);
            } else {
                assert!(matches!(
                    result,
                    Err(ReadError::Visitor(MediaPolicyError::Conflict))
                ));
            }
            assert_eq!(current.available_bytes(), before);
        }
    }

    #[test]
    fn room_read_adapter_preserves_native_work_refusal_without_policy_reclassification() {
        use automerge::resource_limits::{
            NativeAllocationSet, NativeAllowance, NativeResourceError,
        };
        let mut document = AutoCommit::new();
        document.put_object(ROOT, "channels", ObjType::Map).unwrap();
        document.commit();
        let current = NativeAllowance::new(1024 * 1024).unwrap();
        let mut allocations =
            NativeAllocationSet::new(NativeAllowance::new(1024 * 1024).unwrap()).unwrap();
        let turn = allocations.work_turn(current.clone()).unwrap();
        let candidate = document.clone_bounded(&mut allocations, &turn).unwrap();
        let read = candidate.read_unqualified(&turn).unwrap();
        let exhausted = turn.reserve(current.available_bytes()).unwrap();
        assert_eq!(
            strict_map(&read, &read.root(), "channels"),
            Err(ReadError::Resource(NativeResourceError::WorkBytes))
        );
        assert_eq!(candidate.pending_ops(), 0);
        drop(exhausted);
        assert!(strict_map(&read, &read.root(), "channels").is_ok());
    }

    fn native_candidate(
        document: &AutoCommit,
    ) -> (
        automerge::resource_limits::NativeCloneCandidate<AutoCommit>,
        automerge::resource_limits::NativeWorkTurn,
    ) {
        use automerge::resource_limits::{NativeAllocationSet, NativeAllowance};
        let mut allocations =
            NativeAllocationSet::new(NativeAllowance::new(1024 * 1024).unwrap()).unwrap();
        let turn = allocations
            .work_turn(NativeAllowance::new(1024 * 1024).unwrap())
            .unwrap();
        let candidate = document.clone_bounded(&mut allocations, &turn).unwrap();
        (candidate, turn)
    }

    #[test]
    fn room_read_adapter_native_preserves_actual_map_identity() {
        let mut document = AutoCommit::new();
        let object = document.put_object(ROOT, "channels", ObjType::Map).unwrap();
        document.commit();
        let (candidate, turn) = native_candidate(&document);
        let read = candidate.read_unqualified(&turn).unwrap();
        let result = strict_map(&read, &read.root(), "channels").unwrap();
        let mut encoded = [0; 128];
        let len = read.write_id(result.id(), &mut encoded).unwrap();
        assert_eq!(&encoded[..len], object.to_string().as_bytes());
        assert_eq!(candidate.pending_ops(), 0);
    }

    #[test]
    fn room_read_adapter_native_preserves_actual_conflict_denial() {
        let mut document = AutoCommit::new();
        let mut remote = document.fork();
        document.put_object(ROOT, "channels", ObjType::Map).unwrap();
        document.commit();
        remote.put_object(ROOT, "channels", ObjType::Map).unwrap();
        remote.commit();
        document.merge(&mut remote).unwrap();
        assert_eq!(document.get_all(ROOT, "channels").unwrap().len(), 2);
        let (candidate, turn) = native_candidate(&document);
        let read = candidate.read_unqualified(&turn).unwrap();
        assert_eq!(
            strict_map(&read, &read.root(), "channels"),
            Err(ReadError::Visitor(MediaPolicyError::Conflict))
        );
    }

    #[test]
    fn room_read_adapter_returns_the_actual_single_map_identity() {
        let mut document = AutoCommit::new();
        let object = document.put_object(ROOT, "channels", ObjType::Map).unwrap();
        document.commit();
        assert_eq!(
            strict_map(&LegacyRead(&document), &ROOT, "channels").unwrap(),
            object
        );
    }

    #[test]
    fn room_read_adapter_refuses_absent_or_non_map_values() {
        let mut document = AutoCommit::new();
        assert_eq!(
            strict_map(&LegacyRead(&document), &ROOT, "channels"),
            Err(ReadError::Visitor(MediaPolicyError::Conflict))
        );
        document.put(ROOT, "channels", "wrong kind").unwrap();
        document.commit();
        assert_eq!(
            strict_map(&LegacyRead(&document), &ROOT, "channels"),
            Err(ReadError::Visitor(MediaPolicyError::Conflict))
        );
    }

    #[test]
    fn room_read_adapter_refuses_actual_concurrent_map_objects() {
        let mut first = AutoCommit::new();
        let mut second = first.fork();
        first.put_object(ROOT, "channels", ObjType::Map).unwrap();
        first.commit();
        second.put_object(ROOT, "channels", ObjType::Map).unwrap();
        second.commit();
        first.merge(&mut second).unwrap();
        assert_eq!(first.get_all(ROOT, "channels").unwrap().len(), 2);
        assert_eq!(
            strict_map(&LegacyRead(&first), &ROOT, "channels"),
            Err(ReadError::Visitor(MediaPolicyError::Conflict))
        );
    }

    #[test]
    fn room_read_adapter_governance_visits_actual_bounded_history_and_preserves_quota_failure() {
        use crate::resources::native::NativeBatch;
        use automerge::resource_limits::{
            NativeAllocationSet, NativeAllowance, NativeResourceError,
        };
        let (peer, _, _, _, _) = crate::admission_tests::seeded();
        let room = peer.admitted_room().unwrap();
        let expected = super::super::governance_of(Some(room));
        let expected_count = usize::from(expected.0.is_some())
            + expected.1.len()
            + expected.2.len()
            + expected.3.len();
        let mut legacy_count = 0;
        visit_governance_entries(&LegacyRead(room), |_| {
            legacy_count += 1;
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(legacy_count, expected_count);
        let bytes = room.clone().save_with_options(automerge::SaveOptions {
            deflate: false,
            retain_orphans: true,
        });
        let allowance = NativeAllowance::new(8 * 1024 * 1024).unwrap();
        let mut allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
        let turn = allocations.work_turn(allowance.clone()).unwrap();
        let document = NativeBatch::new(&mut allocations, &turn, bytes.len())
            .unwrap()
            .load(&bytes)
            .unwrap();
        let read = document.read(&turn).unwrap();
        let mut count = 0;
        crate::native_footprint_vendor_tests::assert_precharged(&allowance, || {
            visit_governance_entries(&read, |entry| {
                count += 1;
                match entry {
                    GovernanceEntry::Owner(value) => assert_eq!(Some(value), expected.0.as_deref()),
                    GovernanceEntry::Capability { key, bytes } => {
                        assert_eq!(expected.1.get(key).map(Vec::as_slice), Some(bytes))
                    }
                    GovernanceEntry::Peer { key, name } => {
                        assert_eq!(expected.2.get(key).map(String::as_str), Some(name))
                    }
                    GovernanceEntry::Identity { key, bytes } => {
                        assert_eq!(expected.3.get(key).map(Vec::as_slice), Some(bytes))
                    }
                }
                Ok::<_, ()>(())
            })
        })
        .unwrap();
        assert_eq!(count, expected_count);
        let exhausted = turn.reserve(allowance.available_bytes()).unwrap();
        assert_eq!(
            visit_governance_entries(&read, |_| Ok::<_, ()>(())),
            Err(ReadError::Resource(NativeResourceError::WorkBytes))
        );
        drop(exhausted);
    }

    fn compare_governance_entries(doc: &automerge::AutoCommit) -> usize {
        use crate::resources::native::NativeBatch;
        use automerge::resource_limits::{NativeAllocationSet, NativeAllowance};
        let expected = super::super::governance_of(Some(doc));
        let expected_count = usize::from(expected.0.is_some())
            + expected.1.len()
            + expected.2.len()
            + expected.3.len();
        let bytes = doc.clone().save_with_options(automerge::SaveOptions {
            deflate: false,
            retain_orphans: true,
        });
        let allowance = NativeAllowance::new(8 * 1024 * 1024).unwrap();
        let mut allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
        let turn = allocations.work_turn(allowance.clone()).unwrap();
        let document = NativeBatch::new(&mut allocations, &turn, bytes.len())
            .unwrap()
            .load(&bytes)
            .unwrap();
        let read = document.read(&turn).unwrap();
        let mut count = 0;
        crate::native_footprint_vendor_tests::assert_precharged(&allowance, || {
            visit_governance_entries(&read, |entry| {
                count += 1;
                match entry {
                    GovernanceEntry::Owner(value) => assert_eq!(Some(value), expected.0.as_deref()),
                    GovernanceEntry::Capability { key, bytes } => {
                        assert_eq!(expected.1.get(key).map(Vec::as_slice), Some(bytes))
                    }
                    GovernanceEntry::Peer { key, name } => {
                        assert_eq!(expected.2.get(key).map(String::as_str), Some(name))
                    }
                    GovernanceEntry::Identity { key, bytes } => {
                        assert_eq!(expected.3.get(key).map(Vec::as_slice), Some(bytes))
                    }
                }
                Ok::<_, ()>(())
            })
        })
        .unwrap();
        assert_eq!(count, expected_count);
        assert_eq!(
            visit_governance_entries(&read, |_| Err(42_u8)),
            Err(ReadError::Visitor(42)),
        );
        count
    }

    #[test]
    fn room_read_adapter_governance_ignores_list_and_text_but_reads_later_valid_fields() {
        use automerge::transaction::Transactable;
        for kind in [ObjType::List, ObjType::Text] {
            let mut doc = automerge::AutoCommit::new();
            doc.put(automerge::ROOT, "owner", "real owner").unwrap();
            for field in ["capabilities", "peers"] {
                let object = doc.put_object(automerge::ROOT, field, kind).unwrap();
                if kind == ObjType::List {
                    doc.insert(&object, 0, "not a map row").unwrap();
                } else {
                    doc.splice_text(&object, 0, 0, "not a map row").unwrap();
                }
            }
            let identities = doc
                .put_object(automerge::ROOT, "peer_identities_v1", ObjType::Map)
                .unwrap();
            doc.put(
                &identities,
                "later valid row",
                ScalarValue::Bytes(vec![7, 8, 9]),
            )
            .unwrap();
            doc.commit();
            let legacy = super::super::governance_of(Some(&doc));
            assert!(legacy.1.is_empty());
            assert!(legacy.2.is_empty());
            assert_eq!(legacy.3.get("later valid row"), Some(&vec![7, 8, 9]));
            assert_eq!(compare_governance_entries(&doc), 2);
        }
    }

    #[test]
    fn room_read_adapter_governance_preserves_scalar_and_conflicting_container_winners() {
        use automerge::transaction::Transactable;
        let mut scalar = automerge::AutoCommit::new();
        scalar.put(automerge::ROOT, "owner", "owner").unwrap();
        for field in ["capabilities", "peers", "peer_identities_v1"] {
            scalar.put(automerge::ROOT, field, 42_u64).unwrap();
        }
        scalar.commit();
        assert_eq!(compare_governance_entries(&scalar), 1);
        for kind in [ObjType::Map, ObjType::Table] {
            let mut valid = scalar.clone();
            let capabilities = valid
                .put_object(automerge::ROOT, "capabilities", kind)
                .unwrap();
            valid
                .put(&capabilities, "valid cap", ScalarValue::Bytes(vec![3, 4]))
                .unwrap();
            valid.commit();
            assert_eq!(compare_governance_entries(&valid), 2);
        }
        for map_wins in [false, true] {
            let mut map = scalar
                .clone()
                .with_actor(automerge::ActorId::from(vec![if map_wins { 3 } else { 1 }]));
            let mut list = scalar.clone().with_actor(automerge::ActorId::from(vec![2]));
            let capabilities = map
                .put_object(automerge::ROOT, "capabilities", ObjType::Map)
                .unwrap();
            map.put(&capabilities, "winning cap", ScalarValue::Bytes(vec![1, 2]))
                .unwrap();
            map.commit();
            let malformed = list
                .put_object(automerge::ROOT, "capabilities", ObjType::List)
                .unwrap();
            list.insert(&malformed, 0, "not a capability").unwrap();
            list.commit();
            map.merge(&mut list).unwrap();
            assert_eq!(
                map.get_all(automerge::ROOT, "capabilities").unwrap().len(),
                2
            );
            assert_eq!(
                map.get(automerge::ROOT, "capabilities").unwrap().unwrap().0,
                Value::Object(if map_wins {
                    ObjType::Map
                } else {
                    ObjType::List
                })
            );
            assert_eq!(compare_governance_entries(&map), 1 + usize::from(map_wins));
        }
    }
}
