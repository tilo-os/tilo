//! Shared workspace traversal over any Automerge document.

use automerge::{ObjId, ObjType, ReadDoc, ScalarValue, Value};

pub(crate) enum Selection {
    Winner,
    All,
}

pub(crate) enum WorkspaceValue<'a> {
    Object { kind: ObjType, object: ObjId },
    Bytes(&'a [u8]),
    Str(&'a str),
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadError<E> {
    Malformed,
    Visitor(E),
}

impl<E> ReadError<ReadError<E>> {
    fn flatten(self) -> ReadError<E> {
        match self {
            Self::Malformed => ReadError::Malformed,
            Self::Visitor(error) => error,
        }
    }
}

/// Callbacks borrow each scalar for the duration of the call.
/// What [`WorkspaceRead::visit_entries`] calls with each key of a map and
/// its values.
pub(crate) type EntryVisitor<'v, E> =
    dyn for<'a> FnMut(&'a str, &[WorkspaceValue<'a>]) -> Result<(), E> + 'v;

/// The reads a workspace's governance makes of an Automerge document.
pub(crate) trait WorkspaceRead {
    fn visit_keys<E>(
        &self,
        object: &ObjId,
        visitor: impl for<'a> FnMut(&'a str) -> Result<(), E>,
    ) -> Result<usize, ReadError<E>>;

    fn visit_property<E>(
        &self,
        object: &ObjId,
        key: &str,
        selection: Selection,
        visitor: impl for<'a> FnMut(WorkspaceValue<'a>) -> Result<(), E>,
    ) -> Result<usize, ReadError<E>>;

    /// Every key of the map `object` in [`Self::visit_keys`]'s order, each
    /// with the values [`Self::visit_property`] would visit for it under
    /// `selection`, in one pass over the map. A workspace's governance holds a
    /// row for every member: looking each one up by its key on its own made
    /// a whole reading cost a seek per member through a map that grows with
    /// them.
    ///
    /// The visitor is taken by reference, so the walk is built once for each
    /// kind of error rather than for every visitor.
    fn visit_entries<E>(
        &self,
        object: &ObjId,
        selection: Selection,
        visitor: &mut EntryVisitor<'_, E>,
    ) -> Result<usize, ReadError<E>>;
}

/// Borrowed logical governance values.
pub(crate) enum GovernanceEntry<'a> {
    Owner(&'a str),
    Sandbox { key: &'a str, bytes: &'a [u8] },
    Peer { key: &'a str, name: &'a str },
    Identity { key: &'a str, bytes: &'a [u8] },
}

pub(crate) fn visit_governance_entries<R: WorkspaceRead, E>(
    read: &R,
    mut visitor: impl for<'a> FnMut(GovernanceEntry<'a>) -> Result<(), E>,
) -> Result<(), ReadError<E>> {
    crate::work::admission_read();
    let root = automerge::ROOT;
    let owner = read.visit_property(&root, "owner", Selection::Winner, |value| {
        if let WorkspaceValue::Str(value) = value {
            visitor(GovernanceEntry::Owner(value))?;
        }
        Ok(())
    });
    // A malformed lookup reads as absent; a visitor's failure is never
    // turned into absent policy.
    if let Err(error) = owner
        && !matches!(error, ReadError::Malformed)
    {
        return Err(error);
    }
    for (field, kind) in [("peers", 1), ("peer_identities_v1", 2), ("sandbox", 4)] {
        let values = read
            .visit_property(&root, field, Selection::Winner, |value| {
                // Map-key lookup ignores malformed sequence containers: a
                // List or Text here reads as absent, as it always has.
                if let WorkspaceValue::Object {
                    kind: ObjType::Map | ObjType::Table,
                    object,
                } = value
                {
                    read.visit_entries(&object, Selection::Winner, &mut |key, values| {
                        for value in values {
                            match (kind, value) {
                                (4, WorkspaceValue::Bytes(bytes)) => {
                                    visitor(GovernanceEntry::Sandbox { key, bytes })?;
                                }
                                (1, WorkspaceValue::Str(name)) => {
                                    visitor(GovernanceEntry::Peer { key, name })?;
                                }
                                (2, WorkspaceValue::Bytes(bytes)) => {
                                    visitor(GovernanceEntry::Identity { key, bytes })?;
                                }
                                _ => {}
                            }
                        }
                        Ok(())
                    })?;
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

impl<D: ReadDoc> WorkspaceRead for D {
    fn visit_keys<E>(
        &self,
        object: &ObjId,
        mut visitor: impl for<'a> FnMut(&'a str) -> Result<(), E>,
    ) -> Result<usize, ReadError<E>> {
        let mut count = 0usize;
        for key in self.keys(object) {
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
        mut visitor: impl for<'a> FnMut(WorkspaceValue<'a>) -> Result<(), E>,
    ) -> Result<usize, ReadError<E>> {
        match selection {
            Selection::Winner => match self.get(object, key).map_err(|_| ReadError::Malformed)? {
                Some((value, object)) => {
                    visit_value(value, object, &mut visitor)?;
                    Ok(1)
                }
                None => Ok(0),
            },
            Selection::All => {
                let values = self
                    .get_all(object, key)
                    .map_err(|_| ReadError::Malformed)?;
                let count = values.len();
                for (value, object) in values {
                    visit_value(value, object, &mut visitor)?;
                }
                Ok(count)
            }
        }
    }

    fn visit_entries<E>(
        &self,
        object: &ObjId,
        selection: Selection,
        visitor: &mut EntryVisitor<'_, E>,
    ) -> Result<usize, ReadError<E>> {
        let mut count = 0usize;
        for item in self.map_range(object, ..) {
            count = count.saturating_add(1);
            if item.conflict && matches!(selection, Selection::All) {
                // The few keys written concurrently: every value, as a
                // lookup by key reads them.
                let all = self
                    .get_all(object, item.key.as_ref())
                    .map_err(|_| ReadError::Malformed)?;
                let values: Vec<WorkspaceValue<'_>> = all
                    .iter()
                    .map(|(value, object)| keyed_value(value, object.clone()))
                    .collect();
                visitor(&item.key, &values).map_err(ReadError::Visitor)?;
                continue;
            }
            let value = entry_value(&item.value, || item.id());
            visitor(&item.key, std::slice::from_ref(&value)).map_err(ReadError::Visitor)?;
        }
        Ok(count)
    }
}

/// `value`, an entry of a map read in one pass, as a [`WorkspaceValue`].
fn entry_value<'a>(
    value: &'a automerge::ValueRef<'_>,
    object: impl FnOnce() -> ObjId,
) -> WorkspaceValue<'a> {
    use automerge::ScalarValueRef;
    match value {
        automerge::ValueRef::Object(kind) => WorkspaceValue::Object {
            kind: *kind,
            object: object(),
        },
        automerge::ValueRef::Scalar(ScalarValueRef::Bytes(bytes)) => WorkspaceValue::Bytes(bytes),
        automerge::ValueRef::Scalar(ScalarValueRef::Str(value)) => WorkspaceValue::Str(value),
        automerge::ValueRef::Scalar(_) => WorkspaceValue::Other,
    }
}

/// `value`, read by key, as a [`WorkspaceValue`].
fn keyed_value<'a>(value: &'a Value<'_>, object: ObjId) -> WorkspaceValue<'a> {
    match value {
        Value::Object(kind) => WorkspaceValue::Object {
            kind: *kind,
            object,
        },
        Value::Scalar(value) => match value.as_ref() {
            ScalarValue::Bytes(bytes) => WorkspaceValue::Bytes(bytes),
            ScalarValue::Str(value) => WorkspaceValue::Str(value),
            _ => WorkspaceValue::Other,
        },
    }
}

pub(super) fn visit_value<E>(
    value: Value<'_>,
    object: ObjId,
    visitor: &mut impl for<'a> FnMut(WorkspaceValue<'a>) -> Result<(), E>,
) -> Result<(), ReadError<E>> {
    let value = match value {
        Value::Object(kind) => {
            return visitor(WorkspaceValue::Object { kind, object }).map_err(ReadError::Visitor);
        }
        Value::Scalar(value) => value,
    };
    let view = match value.as_ref() {
        ScalarValue::Bytes(bytes) => WorkspaceValue::Bytes(bytes),
        ScalarValue::Str(value) => WorkspaceValue::Str(value),
        _ => WorkspaceValue::Other,
    };
    visitor(view).map_err(ReadError::Visitor)
}

pub(crate) fn strict_map<R: WorkspaceRead>(
    read: &R,
    parent: &ObjId,
    key: &str,
) -> Result<ObjId, ReadError<super::basis::BasisError>> {
    use super::basis::BasisError;
    let mut result = None;
    read.visit_property(parent, key, Selection::All, |value| match value {
        WorkspaceValue::Object {
            kind: ObjType::Map,
            object,
        } if result.is_none() => {
            result = Some(object);
            Ok(())
        }
        _ => Err(BasisError::Conflict),
    })?;
    result.ok_or(ReadError::Visitor(BasisError::Conflict))
}

/// [`strict_string`] of an entry [`WorkspaceRead::visit_entries`] read.
pub(crate) fn strict_string_of(
    values: &[WorkspaceValue<'_>],
) -> Result<String, ReadError<super::basis::BasisError>> {
    use super::basis::BasisError;
    let mut result: Option<&str> = None;
    for value in values {
        let WorkspaceValue::Str(value) = value else {
            return Err(ReadError::Visitor(BasisError::Malformed));
        };
        match result {
            Some(prior) if prior != *value => {
                return Err(ReadError::Visitor(BasisError::Conflict));
            }
            Some(_) => {}
            None => result = Some(value),
        }
    }
    result
        .map(str::to_owned)
        .ok_or(ReadError::Visitor(BasisError::Missing))
}

/// [`strict_bytes`] of an entry [`WorkspaceRead::visit_entries`] read.
pub(crate) fn strict_bytes_of(
    values: &[WorkspaceValue<'_>],
) -> Result<Vec<u8>, ReadError<super::basis::BasisError>> {
    use super::basis::BasisError;
    let mut result: Option<&[u8]> = None;
    for value in values {
        let WorkspaceValue::Bytes(value) = value else {
            return Err(ReadError::Visitor(BasisError::Malformed));
        };
        match result {
            Some(prior) if prior != *value => {
                return Err(ReadError::Visitor(BasisError::Conflict));
            }
            Some(_) => {}
            None => result = Some(value),
        }
    }
    result
        .map(<[u8]>::to_vec)
        .ok_or(ReadError::Visitor(BasisError::Missing))
}

pub(crate) fn strict_string<R: WorkspaceRead>(
    read: &R,
    parent: &ObjId,
    key: &str,
) -> Result<String, ReadError<super::basis::BasisError>> {
    use super::basis::BasisError;
    let mut result: Option<String> = None;
    read.visit_property(parent, key, Selection::All, |value| {
        let WorkspaceValue::Str(value) = value else {
            return Err(ReadError::Visitor(BasisError::Malformed));
        };
        match &result {
            Some(prior) if prior != value => {
                return Err(ReadError::Visitor(BasisError::Conflict));
            }
            Some(_) => {}
            None => result = Some(value.to_owned()),
        }
        Ok(())
    })
    .map_err(ReadError::flatten)?;
    result.ok_or(ReadError::Visitor(BasisError::Missing))
}

#[cfg(test)]
pub(crate) fn strict_bytes<R: WorkspaceRead>(
    read: &R,
    parent: &ObjId,
    key: &str,
) -> Result<Vec<u8>, ReadError<super::basis::BasisError>> {
    use super::basis::BasisError;
    let mut result: Option<Vec<u8>> = None;
    read.visit_property(parent, key, Selection::All, |value| {
        let WorkspaceValue::Bytes(value) = value else {
            return Err(ReadError::Visitor(BasisError::Malformed));
        };
        match &result {
            Some(prior) if prior.as_slice() != value => {
                return Err(ReadError::Visitor(BasisError::Conflict));
            }
            Some(_) => {}
            None => result = Some(value.to_vec()),
        }
        Ok(())
    })
    .map_err(ReadError::flatten)?;
    result.ok_or(ReadError::Visitor(BasisError::Missing))
}

/// The policy error a read failure is reported as.
pub(crate) fn policy_error(error: ReadError<super::basis::BasisError>) -> super::basis::BasisError {
    use super::basis::BasisError;
    match error {
        ReadError::Malformed => BasisError::Malformed,
        ReadError::Visitor(error) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::basis::BasisError;
    use automerge::{AutoCommit, ROOT, transaction::Transactable};
    use tilo_automerge::ApplyIncoming;

    /// `strict_map` answers the one map a key holds and refuses every other
    /// shape of it, one row each.
    #[test]
    fn workspace_read_adapter_returns_the_single_map_and_refuses_every_other_shape() {
        let absent = AutoCommit::new();
        let mut scalar = AutoCommit::new();
        scalar.put(ROOT, "channels", "wrong kind").unwrap();
        scalar.commit();
        let mut list = AutoCommit::new();
        list.put_object(ROOT, "channels", ObjType::List).unwrap();
        list.commit();
        let mut single = AutoCommit::new();
        let object = single.put_object(ROOT, "channels", ObjType::Map).unwrap();
        single.commit();
        let mut concurrent = AutoCommit::new();
        let mut second = concurrent.fork();
        concurrent
            .put_object(ROOT, "channels", ObjType::Map)
            .unwrap();
        concurrent.commit();
        second.put_object(ROOT, "channels", ObjType::Map).unwrap();
        second.commit();
        concurrent.merge_incoming(&mut second).unwrap();
        assert_eq!(concurrent.get_all(ROOT, "channels").unwrap().len(), 2);
        for (case, document, expected) in [
            ("the actual single map", &single, Ok(object)),
            (
                "an absent key",
                &absent,
                Err(ReadError::Visitor(BasisError::Conflict)),
            ),
            (
                "a value that is not a map",
                &scalar,
                Err(ReadError::Visitor(BasisError::Conflict)),
            ),
            (
                "a list where the map belongs",
                &list,
                Err(ReadError::Visitor(BasisError::Conflict)),
            ),
            (
                "two concurrent map objects",
                &concurrent,
                Err(ReadError::Visitor(BasisError::Conflict)),
            ),
        ] {
            assert_eq!(strict_map(document, &ROOT, "channels"), expected, "{case}");
        }
    }

    /// Each key with every value a lookup by key reads for it.
    type Read = Vec<(String, Vec<String>)>;

    fn described(value: &WorkspaceValue<'_>) -> String {
        match value {
            WorkspaceValue::Object { kind, object } => format!("{kind:?} {object}"),
            WorkspaceValue::Bytes(bytes) => format!("bytes {bytes:?}"),
            WorkspaceValue::Str(value) => format!("str {value}"),
            WorkspaceValue::Other => "other".into(),
        }
    }

    fn by_key(read: &AutoCommit, object: &ObjId, winner: bool) -> Read {
        let mut keys = Vec::new();
        read.visit_keys::<()>(object, |key| {
            keys.push(key.to_owned());
            Ok(())
        })
        .unwrap();
        keys.into_iter()
            .map(|key| {
                let mut values = Vec::new();
                let selection = if winner {
                    Selection::Winner
                } else {
                    Selection::All
                };
                read.visit_property::<()>(object, &key, selection, |value| {
                    values.push(described(&value));
                    Ok(())
                })
                .unwrap();
                (key, values)
            })
            .collect()
    }

    fn in_one_pass(read: &AutoCommit, object: &ObjId, winner: bool) -> Read {
        let mut entries = Vec::new();
        let selection = if winner {
            Selection::Winner
        } else {
            Selection::All
        };
        read.visit_entries::<()>(object, selection, &mut |key, values| {
            entries.push((key.to_owned(), values.iter().map(described).collect()));
            Ok(())
        })
        .unwrap();
        entries
    }

    /// A map read in one pass reads what looking up each key reads: the
    /// same keys in the same order, each with the winning value or, for a
    /// key written concurrently, every value.
    #[test]
    fn a_map_read_in_one_pass_reads_what_each_key_reads() {
        let mut first = AutoCommit::new();
        let rows = first.put_object(ROOT, "rows", ObjType::Map).unwrap();
        for n in 0..40u8 {
            first.put(&rows, format!("row-{n:02}"), vec![n]).unwrap();
        }
        first.put(&rows, "name", "first").unwrap();
        first.put(&rows, "flag", true).unwrap();
        first.put_object(&rows, "nested", ObjType::Map).unwrap();
        first.commit();
        let mut second = first.fork();
        first.put(&rows, "row-07", vec![70]).unwrap();
        first.put(&rows, "name", "one").unwrap();
        first.commit();
        second.put(&rows, "row-07", vec![71]).unwrap();
        second.put(&rows, "name", "other").unwrap();
        second.delete(&rows, "row-09").unwrap();
        second.commit();
        first.merge_incoming(&mut second).unwrap();
        assert_eq!(first.get_all(&rows, "row-07").unwrap().len(), 2);
        let read = &first;
        for winner in [true, false] {
            let expected = by_key(read, &rows, winner);
            assert_eq!(
                in_one_pass(read, &rows, winner),
                expected,
                "winner: {winner}"
            );
            assert_eq!(expected.len(), 42);
        }
        assert_eq!(
            in_one_pass(read, &rows, false)
                .iter()
                .find(|(key, _)| key == "row-07")
                .unwrap()
                .1
                .len(),
            2
        );
    }

    fn compare_governance_entries(doc: &automerge::AutoCommit) -> usize {
        let expected = super::super::governance_of(Some(doc));
        let expected_count = [
            usize::from(expected.owner.is_some()),
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
        let read = &document;
        let mut count = 0usize;
        visit_governance_entries(read, |entry| {
            count = count.checked_add(1).unwrap();
            match entry {
                GovernanceEntry::Owner(value) => assert_eq!(Some(value), expected.owner.as_deref()),
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
            visit_governance_entries(read, |_| Err(42_u8)),
            Err(ReadError::Visitor(42)),
        );
        count
    }

    tilo_test_env::scenario! {
        fn workspace_read_adapter_admission_reads_valid_fields_and_conflict_winners;

        phase workspace_read_adapter_governance_ignores_list_and_text_but_reads_later_valid_fields;
        {
            use automerge::transaction::Transactable;
            for kind in [ObjType::List, ObjType::Text] {
                let mut doc = automerge::AutoCommit::new();
                doc.put(automerge::ROOT, "owner", "real owner").unwrap();
                for field in ["sandbox", "peers"] {
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
                let governance = super::super::governance_of(Some(&doc));
                assert!(governance.peers.is_empty());
                assert_eq!(
                    governance.identities.get("later valid row"),
                    Some(&vec![7, 8, 9])
                );
                assert_eq!(compare_governance_entries(&doc), 2);
            }
        }

        phase workspace_read_adapter_governance_preserves_scalar_and_conflicting_container_winners;
        {
            use automerge::transaction::Transactable;
            let mut scalar = automerge::AutoCommit::new();
            scalar.put(automerge::ROOT, "owner", "owner").unwrap();
            for field in ["sandbox", "peers", "peer_identities_v1"] {
                scalar.put(automerge::ROOT, field, 42_u64).unwrap();
            }
            scalar.commit();
            assert_eq!(compare_governance_entries(&scalar), 1);
            let mut valid = scalar.clone();
            let capabilities = valid
                .put_object(automerge::ROOT, "sandbox", ObjType::Map)
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
                    .put_object(automerge::ROOT, "sandbox", ObjType::Map)
                    .unwrap();
                map.put(&capabilities, "winning cap", ScalarValue::Bytes(vec![1, 2]))
                    .unwrap();
                map.commit();
                let malformed = list
                    .put_object(automerge::ROOT, "sandbox", ObjType::List)
                    .unwrap();
                list.insert(&malformed, 0, "not a capability").unwrap();
                list.commit();
                map.merge_incoming(&mut list).unwrap();
                assert_eq!(map.get_all(automerge::ROOT, "sandbox").unwrap().len(), 2);
                assert_eq!(
                    map.get(automerge::ROOT, "sandbox").unwrap().unwrap().0,
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
}
