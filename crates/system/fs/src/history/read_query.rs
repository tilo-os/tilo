//! Shared bounded immutable-history query handling. This owns no visibility
//! admission; callers obtain their snapshot and scope under current authority.
use super::RecordHistorySnapshot;
use fs_protocol::{
    ContentRevisionV1, FileVersionAttributionV1,
    files::{
        FileHistoryPageV1, FileHistoryReadErrorV1 as Error, FileHistoryReadQueryV1 as Query,
        FileHistoryReadResponseV1, FileHistoryReadScopeV1 as Scope,
        FileHistoryReadValueV1 as Value, MAX_HISTORY_PAGE_VERSIONS, MAX_HISTORY_READ_BYTES,
    },
};
impl RecordHistorySnapshot {
    pub(crate) fn read_query(
        self,
        scope: Scope,
        current_revision: Option<ContentRevisionV1>,
        query: Query,
        attributions: &[FileVersionAttributionV1],
    ) -> Result<Value, Error> {
        // Who caused one version, when an agent did, as its own signing
        // device recorded it.
        let attribution_of = |version_id| {
            attributions
                .iter()
                .find(|attributed: &&FileVersionAttributionV1| attributed.version_id == version_id)
        };
        match query {
            Query::Page { cursor } => {
                let start = match cursor {
                    None => 0,
                    Some((expected, id)) => {
                        if expected != scope.frontier {
                            return Err(Error::Stale);
                        }
                        self.versions
                            .iter()
                            .position(|v| v.id == id)
                            .and_then(|i| i.checked_add(1))
                            .ok_or(Error::InvalidRequest)?
                    }
                };
                let remaining = self.versions.get(start..).ok_or(Error::InvalidRequest)?;
                let mut count = remaining.len().min(MAX_HISTORY_PAGE_VERSIONS);
                loop {
                    let versions = remaining
                        .get(..count)
                        .ok_or(Error::InvalidRequest)?
                        .to_vec();
                    let next_after = if remaining.len() > count {
                        versions.last().map(|v| v.id)
                    } else {
                        None
                    };
                    let attributions = versions
                        .iter()
                        .filter_map(|version| attribution_of(version.id))
                        .cloned()
                        .collect();
                    let value = Value::Page(FileHistoryPageV1 {
                        scope,
                        versions,
                        total_versions: self.versions.len() as u64,
                        heads: self.heads.clone(),
                        current_revision,
                        next_after,
                        attributions,
                    });
                    let size = serde_json::to_vec(&FileHistoryReadResponseV1 {
                        schema_version: 1,
                        result: Ok(value.clone()),
                    })
                    .map_err(|_| Error::Unavailable)?
                    .len();
                    if size <= MAX_HISTORY_READ_BYTES {
                        return Ok(value);
                    }
                    count = count
                        .checked_sub(1)
                        .filter(|count| *count > 0)
                        .ok_or(Error::TooLarge)?;
                }
            }
            Query::Version {
                frontier: expected,
                version_id,
            } => {
                if scope.frontier != expected {
                    return Err(Error::Stale);
                }
                let version = self
                    .versions
                    .into_iter()
                    .find(|v| v.id == version_id)
                    .ok_or(Error::MissingVersion)?;
                let attribution =
                    attribution_of(version_id).map(|attributed| attributed.attribution.clone());
                Ok(Value::Version {
                    scope,
                    version,
                    attribution,
                })
            }
            Query::Preview { .. } => Err(Error::InvalidRequest),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::FileHistoryStore;
    use fs_protocol::{
        ContentHash, Ed25519PublicKey, FileActor, FileProvenance, FileRecordId, FileVersionId,
        FileVersionV1,
    };
    use uuid::Uuid;

    fn append_metadata_versions(store: &FileHistoryStore, record: FileRecordId) -> FileVersionId {
        let mut parent = None;
        for index in 0..25 {
            let version = FileVersionV1 {
                id: FileVersionId::new(),
                record_id: record,
                parents: parent.into_iter().collect(),
                actor: FileActor {
                    person: Ed25519PublicKey([1; 32]),
                    device: Ed25519PublicKey([2; 32]),
                },
                timestamp_ms: index,
                content_hash: None,
                metadata_hash: ContentHash([3; 32]),
                provenance: if parent.is_none() {
                    FileProvenance::Create
                } else {
                    FileProvenance::Rename
                },
            };
            parent = Some(version.id);
            store.append(version).unwrap();
        }
        parent.unwrap()
    }

    #[test]
    fn durable_metadata_history_uses_exact_bounded_shared_query() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileHistoryStore::open(directory.path().join("history.json")).unwrap();
        let record = FileRecordId::new();
        let latest = append_metadata_versions(&store, record);
        let snapshot = store.snapshot(record).unwrap();
        let scope = Scope {
            workspace_id: Uuid::new_v4(),
            mount_lifetime: Uuid::new_v4(),
            record_id: record,
            frontier: [7; 32],
        };
        let Value::Page(first) = snapshot
            .clone()
            .read_query(scope, None, Query::Page { cursor: None }, &[])
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(first.versions.len(), MAX_HISTORY_PAGE_VERSIONS);
        assert_eq!(first.total_versions, 25);
        assert!(
            first
                .versions
                .iter()
                .all(|version| version.content_hash.is_none())
        );
        let mut found = first.versions;
        let mut cursor = first.next_after;
        while let Some(version) = cursor {
            let Value::Page(page) = snapshot
                .clone()
                .read_query(
                    scope,
                    None,
                    Query::Page {
                        cursor: Some((scope.frontier, version)),
                    },
                    &[],
                )
                .unwrap()
            else {
                panic!()
            };
            assert_eq!(page.scope, scope);
            found.extend(page.versions);
            cursor = page.next_after;
        }
        assert_eq!(found, snapshot.versions);
        assert_eq!(
            snapshot.clone().read_query(
                scope,
                None,
                Query::Page {
                    cursor: Some(([8; 32], latest))
                },
                &[],
            ),
            Err(Error::Stale)
        );
        let Value::Version { version, .. } = snapshot
            .clone()
            .read_query(
                scope,
                None,
                Query::Version {
                    frontier: scope.frontier,
                    version_id: latest,
                },
                &[],
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(version.content_hash, None);
        assert_eq!(
            snapshot.read_query(
                scope,
                None,
                Query::Preview {
                    frontier: scope.frontier,
                    version_id: latest
                },
                &[],
            ),
            Err(Error::InvalidRequest)
        );
    }

    /// A page carries the agent behind each of its own versions an agent
    /// caused, and nothing about versions on other pages; a single-version
    /// read carries that version's.
    #[test]
    fn reads_carry_the_agent_behind_each_version_they_return() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileHistoryStore::open(directory.path().join("history.json")).unwrap();
        let record = FileRecordId::new();
        let latest = append_metadata_versions(&store, record);
        let snapshot = store.snapshot(record).unwrap();
        let oldest = snapshot.versions.first().unwrap().id;
        let scope = Scope {
            workspace_id: Uuid::new_v4(),
            mount_lifetime: Uuid::new_v4(),
            record_id: record,
            frontier: [7; 32],
        };
        let scribe = fs_protocol::FileAttributionV1 {
            actor: "agent:scribe".into(),
            label: "Scribe".into(),
            color: [10, 20, 30],
        };
        let attributions = [latest, oldest].map(|version_id| FileVersionAttributionV1 {
            version_id,
            attribution: scribe.clone(),
        });
        let Value::Page(first) = snapshot
            .clone()
            .read_query(scope, None, Query::Page { cursor: None }, &attributions)
            .unwrap()
        else {
            panic!()
        };
        assert!(first.versions.iter().any(|version| version.id == oldest));
        assert!(first.versions.iter().all(|version| version.id != latest));
        assert_eq!(first.attributions, vec![attributions[1].clone()]);

        let Value::Version { attribution, .. } = snapshot
            .clone()
            .read_query(
                scope,
                None,
                Query::Version {
                    frontier: scope.frontier,
                    version_id: latest,
                },
                &attributions,
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(attribution, Some(scribe));
        let second = snapshot.versions[1].id;
        let Value::Version { attribution, .. } = snapshot
            .read_query(
                scope,
                None,
                Query::Version {
                    frontier: scope.frontier,
                    version_id: second,
                },
                &attributions,
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(attribution, None, "the person's own version");
    }
}
