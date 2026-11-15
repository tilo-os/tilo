//! A workspace's entry in the owner record: the layout this build writes is
//! pinned by the bytes it writes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]
use super::{FORMAT, stored_version};
use crate::owner::record::{StoredWorkspace, WORKSPACE_VERSION};
use tilo_desktop_model::WorkspaceId;
use tilo_files::native::migration::assert_golden_bytes;

/// An entry as this build writes it.
const CURRENT: &str = include_str!("../../../golden/owner-workspace-record/v5.hex");
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
            "owner workspace record",
            &golden,
            |_| {
                let stored = read();
                assert_eq!(stored.workspace, founders());
                assert!(stored.hosted_here());
            },
            || crate::owner::file_store::part_bytes(&read()).unwrap(),
        );
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
