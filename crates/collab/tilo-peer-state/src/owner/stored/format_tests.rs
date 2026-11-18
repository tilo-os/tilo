//! A workspace's entry in the owner record: the layout this build writes is
//! pinned by the bytes it writes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]
use super::{FORMAT, stored_version};
use crate::owner::stored::{StoredWorkspace, WORKSPACE_VERSION};
use tilo_desktop_model::WorkspaceId;
use tilo_files::native::migration::assert_golden_bytes;

/// An entry as this build writes it.
const CURRENT: &str = include_str!("../../../golden/owner-workspace-document/v5.hex");
const FOUNDERS: &str = "40267648-f762-4c8d-89ad-c5ab27aa13b8";

fn bytes(hex: &str) -> Vec<u8> {
    hex.split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect()
}

fn founders() -> WorkspaceId {
    WorkspaceId::parse(FOUNDERS).unwrap()
}

tilo_test_env::scenario! {
    fn the_record_format_reads_only_the_layout_this_build_writes;

    phase the_format_declares_the_layout_this_build_writes;
    {
        assert!(FORMAT.is_whole());
        assert_eq!(FORMAT.version, WORKSPACE_VERSION);
    }

    phase this_builds_entry_reads_and_writes_as_it_is;
    // This build's layout: a field added, removed, renamed or reordered breaks
    // it, which is when the entry needs a new version.
    {
        let golden = bytes(CURRENT);
        assert_eq!(stored_version(&golden).unwrap(), WORKSPACE_VERSION);
        let read = || serde_json::from_slice::<StoredWorkspace>(&golden).unwrap();
        assert_golden_bytes(
            "owner workspace document",
            &golden,
            |_| {
                let stored = read();
                assert_eq!(stored.workspace, founders());
                assert!(stored.hosted_here());
            },
            || crate::owner::file_store::part_bytes(&read()).unwrap(),
        );
    }

    phase an_entry_naming_its_join_log_as_the_code_does_reads_with_the_log_whole;
    // For a while builds wrote the join log under the name the code gives it,
    // into entries of this same version. Such an entry reads with what its
    // log holds, and is written under the stored name again.
    {
        let mut entry: serde_json::Value = serde_json::from_slice(&bytes(CURRENT)).unwrap();
        let (id, digest) = ([7_u8; 32], [9_u8; 32]);
        let request = serde_json::json!([{
            "request_id": id,
            "request_digest": digest,
            "generation": 3,
        }]);
        let fields = entry.as_object_mut().unwrap();
        let mut log = fields
            .remove("join_ledger")
            .expect("the entry stores its join log under this name");
        log.as_object_mut()
            .unwrap()
            .insert("administration".into(), request);
        fields.insert("join_log".into(), log.clone());
        let between = serde_json::to_vec(&entry).unwrap();
        assert_eq!(stored_version(&between).unwrap(), WORKSPACE_VERSION);

        let read: StoredWorkspace = serde_json::from_slice(&between).unwrap();
        assert_eq!(read.workspace, founders());
        let held = &read.join_log.administration;
        assert_eq!(held.len(), 1, "the log an entry held is read, not left empty");
        let answered = held.first().unwrap();
        assert_eq!(
            (answered.request_id, answered.request_digest, answered.generation),
            (id, digest, 3)
        );

        let written = crate::owner::file_store::part_bytes(&read).unwrap();
        let written: serde_json::Value = serde_json::from_slice(&written).unwrap();
        assert_eq!(written.get("join_ledger"), Some(&log));
        assert_eq!(written.get("join_log"), None);
    }

    phase a_newer_entry_is_refused;
    // An entry a newer build wrote is refused, and one from before this layout
    // does not read as this one.
    {
        assert!(
            FORMAT
                .migrate(bytes(CURRENT), WORKSPACE_VERSION + 1)
                .is_err()
        );
    }
}
