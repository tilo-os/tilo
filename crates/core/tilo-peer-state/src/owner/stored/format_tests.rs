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
use tilo_files::native::migration::{assert_golden_bytes, assert_step_reads_previous};

/// An entry as this build writes it.
const CURRENT: &str = include_str!("../../../golden/owner-workspace-document/v9.hex");
/// An entry as the build before this one wrote it.
const PREVIOUS: &str = include_str!("../../../golden/owner-workspace-document/v8.hex");
/// An entry as the oldest build this one brings up wrote it, with the device's join log.
const OLDEST: &str = include_str!("../../../golden/owner-workspace-document/v6.hex");
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
            },
            || crate::owner::file_store::part_bytes(&read()).unwrap(),
        );
    }

    phase an_entry_the_build_before_wrote_comes_up;
    // A device that updates keeps the workspaces it held: the entry its last
    // build wrote reads as this build's.
    {
        assert_step_reads_previous(&FORMAT, &bytes(PREVIOUS), |migrated| {
            assert_eq!(stored_version(migrated).unwrap(), WORKSPACE_VERSION);
            let stored = serde_json::from_slice::<StoredWorkspace>(migrated).unwrap();
            assert_eq!(stored.workspace, founders());
        });
    }

    phase the_oldest_entry_comes_up_through_every_step;
    {
        let migrated = FORMAT.migrate(bytes(OLDEST), 6).unwrap();
        assert_eq!(stored_version(&migrated).unwrap(), WORKSPACE_VERSION);
        let stored = serde_json::from_slice::<StoredWorkspace>(&migrated).unwrap();
        assert_eq!(stored.workspace, founders());
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
