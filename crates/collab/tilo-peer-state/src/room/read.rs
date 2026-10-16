//! Shared room traversal over any Automerge document reader.

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

/// A read's explicit limit was reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Capacity;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadError<E> {
    Malformed,
    Capacity,
    Visitor(E),
}

impl<E> From<Capacity> for ReadError<E> {
    fn from(_: Capacity) -> Self {
        Self::Capacity
    }
}

impl<E> ReadError<ReadError<E>> {
    fn flatten(self) -> ReadError<E> {
        match self {
            Self::Malformed => ReadError::Malformed,
            Self::Capacity => ReadError::Capacity,
            Self::Visitor(error) => error,
        }
    }
}

/// Callbacks borrow each scalar for the duration of the call.
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

/// Borrowed logical governance values.
pub(crate) enum GovernanceEntry<'a> {
    Owner(&'a str),
    Grant { key: &'a str, bytes: &'a [u8] },
    Member { key: &'a str, bytes: &'a [u8] },
    Sandbox { key: &'a str, bytes: &'a [u8] },
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
    // Match the existing room projection's ignored lookup errors. Capacity
    // and visitor failures are never turned into absent policy.
    if let Err(error) = owner
        && !matches!(error, ReadError::Malformed)
    {
        return Err(error);
    }
    for (field, kind) in [
        ("grants", 0),
        ("peers", 1),
        ("peer_identities_v1", 2),
        ("members", 3),
        ("sandbox", 4),
    ] {
        let values = read
            .visit_property(&root, field, Selection::Winner, |value| {
                // Map-key lookup ignores malformed sequence containers: a
                // List or Text here reads as absent, as it always has.
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
                                        visitor(GovernanceEntry::Grant { key, bytes })
                                    }
                                    (3, RoomValue::Bytes(bytes)) => {
                                        visitor(GovernanceEntry::Member { key, bytes })
                                    }
                                    (4, RoomValue::Bytes(bytes)) => {
                                        visitor(GovernanceEntry::Sandbox { key, bytes })
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
        if let Err(error) = values
            && !matches!(error, ReadError::Malformed)
        {
            return Err(error);
        }
    }
    Ok(())
}

pub(crate) struct LegacyRead<'a, D: ReadDoc>(pub(crate) &'a D);

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
        let mut count = 0usize;
        for key in self.0.keys(object) {
            visitor(&key).map_err(ReadError::Visitor)?;
            count = count.saturating_add(1);
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

pub(crate) fn strict_string<R: RoomRead>(
    read: &R,
    parent: &R::Object,
    key: &str,
) -> Result<String, ReadError<super::media::MediaPolicyError>> {
    use super::media::MediaPolicyError;
    let mut result: Option<String> = None;
    read.visit_property(parent, key, Selection::All, |value| {
        let RoomValue::Str(value) = value else {
            return Err(ReadError::Visitor(MediaPolicyError::Malformed));
        };
        match &result {
            Some(prior) if prior != value => {
                return Err(ReadError::Visitor(MediaPolicyError::Conflict));
            }
            Some(_) => {}
            None => result = Some(value.to_owned()),
        }
        Ok(())
    })
    .map_err(ReadError::flatten)?;
    result.ok_or(ReadError::Visitor(MediaPolicyError::Binding))
}

pub(crate) fn strict_bytes<R: RoomRead>(
    read: &R,
    parent: &R::Object,
    key: &str,
) -> Result<Vec<u8>, ReadError<super::media::MediaPolicyError>> {
    use super::media::MediaPolicyError;
    let mut result: Option<Vec<u8>> = None;
    read.visit_property(parent, key, Selection::All, |value| {
        let RoomValue::Bytes(value) = value else {
            return Err(ReadError::Visitor(MediaPolicyError::Malformed));
        };
        match &result {
            Some(prior) if prior.as_slice() != value => {
                return Err(ReadError::Visitor(MediaPolicyError::Conflict));
            }
            Some(_) => {}
            None => result = Some(value.to_vec()),
        }
        Ok(())
    })
    .map_err(ReadError::flatten)?;
    result.ok_or(ReadError::Visitor(MediaPolicyError::Binding))
}

/// The policy error a read failure is reported as.
pub(crate) fn legacy_policy_error(
    error: ReadError<super::media::MediaPolicyError>,
) -> super::media::MediaPolicyError {
    use super::media::MediaPolicyError;
    match error {
        ReadError::Malformed => MediaPolicyError::Malformed,
        ReadError::Capacity => MediaPolicyError::Capacity,
        ReadError::Visitor(error) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room::media::MediaPolicyError;
    use automerge::{AutoCommit, ROOT, transaction::Transactable};
    use tilo_automerge::ApplyIncoming;

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
        first.merge_incoming(&mut second).unwrap();
        assert_eq!(first.get_all(ROOT, "channels").unwrap().len(), 2);
        assert_eq!(
            strict_map(&LegacyRead(&first), &ROOT, "channels"),
            Err(ReadError::Visitor(MediaPolicyError::Conflict))
        );
    }

    fn compare_governance_entries(doc: &automerge::AutoCommit) -> usize {
        let expected = super::super::governance_of(Some(doc));
        let expected_count = [
            usize::from(expected.owner.is_some()),
            expected.grants.len(),
            expected.members.len(),
            expected.sandbox.len(),
            expected.peers.len(),
            expected.identities.len(),
        ]
        .into_iter()
        .try_fold(0usize, usize::checked_add)
        .unwrap();
        let bytes = doc.clone().save_with_options(automerge::SaveOptions {
            deflate: false,
            retain_orphans: true,
        });
        let document = tilo_automerge_isolate::load(&bytes).unwrap();
        let read = LegacyRead(&document);
        let mut count = 0usize;
        visit_governance_entries(&read, |entry| {
            count = count.checked_add(1).unwrap();
            match entry {
                GovernanceEntry::Owner(value) => assert_eq!(Some(value), expected.owner.as_deref()),
                GovernanceEntry::Grant { key, bytes } => {
                    assert_eq!(expected.grants.get(key).map(Vec::as_slice), Some(bytes));
                }
                GovernanceEntry::Member { key, bytes } => {
                    assert_eq!(expected.members.get(key).map(Vec::as_slice), Some(bytes));
                }
                GovernanceEntry::Sandbox { key, bytes } => {
                    assert_eq!(expected.sandbox.get(key).map(Vec::as_slice), Some(bytes));
                }
                GovernanceEntry::Peer { key, name } => {
                    assert_eq!(expected.peers.get(key).map(String::as_str), Some(name));
                }
                GovernanceEntry::Identity { key, bytes } => {
                    assert_eq!(expected.identities.get(key).map(Vec::as_slice), Some(bytes));
                }
            }
            Ok::<_, ()>(())
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
            for field in ["grants", "peers"] {
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
            assert!(legacy.grants.is_empty());
            assert!(legacy.peers.is_empty());
            assert_eq!(
                legacy.identities.get("later valid row"),
                Some(&vec![7, 8, 9])
            );
            assert_eq!(compare_governance_entries(&doc), 2);
        }
    }

    #[test]
    fn room_read_adapter_governance_preserves_scalar_and_conflicting_container_winners() {
        use automerge::transaction::Transactable;
        let mut scalar = automerge::AutoCommit::new();
        scalar.put(automerge::ROOT, "owner", "owner").unwrap();
        for field in ["grants", "peers", "peer_identities_v1"] {
            scalar.put(automerge::ROOT, field, 42_u64).unwrap();
        }
        scalar.commit();
        assert_eq!(compare_governance_entries(&scalar), 1);
        let mut valid = scalar.clone();
        let capabilities = valid
            .put_object(automerge::ROOT, "grants", ObjType::Map)
            .unwrap();
        valid
            .put(&capabilities, "valid cap", ScalarValue::Bytes(vec![3, 4]))
            .unwrap();
        valid.commit();
        assert_eq!(compare_governance_entries(&valid), 2);
        for map_wins in [false, true] {
            let mut map = scalar
                .clone()
                .with_actor(automerge::ActorId::from(vec![if map_wins { 3 } else { 1 }]));
            let mut list = scalar.clone().with_actor(automerge::ActorId::from(vec![2]));
            let capabilities = map
                .put_object(automerge::ROOT, "grants", ObjType::Map)
                .unwrap();
            map.put(&capabilities, "winning cap", ScalarValue::Bytes(vec![1, 2]))
                .unwrap();
            map.commit();
            let malformed = list
                .put_object(automerge::ROOT, "grants", ObjType::List)
                .unwrap();
            list.insert(&malformed, 0, "not a capability").unwrap();
            list.commit();
            map.merge_incoming(&mut list).unwrap();
            assert_eq!(map.get_all(automerge::ROOT, "grants").unwrap().len(), 2);
            assert_eq!(
                map.get(automerge::ROOT, "grants").unwrap().unwrap().0,
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
