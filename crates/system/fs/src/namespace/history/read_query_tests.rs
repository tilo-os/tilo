//! The bounded history read query, proven against the snapshot the filesystem serves.

use fs_protocol::files::{
    FileHistoryReadErrorV1 as Error, FileHistoryReadQueryV1 as Query,
    FileHistoryReadScopeV1 as Scope, FileHistoryReadValueV1 as Value, MAX_HISTORY_PAGE_VERSIONS,
};
use fs_protocol::history::RecordHistorySnapshot;
use fs_protocol::{
    ConflictStatus, ContentHash, Ed25519PublicKey, FileActor, FileDocumentId, FileProvenance,
    FileVersionAttributionV1, FileVersionId, FileVersionV1,
};
use uuid::Uuid;

/// The snapshot a record with `count` versions has: each version its
/// parent's child, all metadata-only.
fn metadata_snapshot(record: FileDocumentId, count: u64) -> RecordHistorySnapshot {
    let mut versions = Vec::new();
    let mut parent = None;
    for index in 0..count {
        let version = FileVersionV1 {
            id: FileVersionId::new(),
            document_id: record,
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
        versions.push(version);
    }
    versions.sort_by_key(|version| version.id);
    let head = versions.last().map(|version| version.id);
    RecordHistorySnapshot {
        document_id: record,
        versions,
        heads: head.into_iter().collect(),
        conflicts: Vec::new(),
    }
}

tilo_test_env::scenario! {
    fn history_reads_are_bounded_and_carry_their_agents;

    phase metadata_history_uses_exact_bounded_shared_query;
    {
        let record = FileDocumentId::new();
        let snapshot = metadata_snapshot(record, 25);
        let latest = snapshot.heads[0];
        let scope = Scope {
            workspace_id: Uuid::new_v4(),
            mount_lifetime: Uuid::new_v4(),
            document_id: record,
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
        let _ = ConflictStatus::Open;
    }

    phase reads_carry_the_agent_behind_each_version_they_return;
    // A page carries the agent behind each of its own versions an agent
    // caused, and nothing about versions on other pages; a single-version
    // read carries that version's.
    {
        let record = FileDocumentId::new();
        let snapshot = metadata_snapshot(record, 25);
        let latest = snapshot.heads[0];
        let oldest = snapshot.versions.first().unwrap().id;
        let scope = Scope {
            workspace_id: Uuid::new_v4(),
            mount_lifetime: Uuid::new_v4(),
            document_id: record,
            frontier: [7; 32],
        };
        let scribe = fs_protocol::FileAttributionV1 {
            writer: fs_protocol::AttributedWriter::agent("scribe"),
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
