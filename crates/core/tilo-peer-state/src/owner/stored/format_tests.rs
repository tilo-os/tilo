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
use tilo_files::native::migration::{Format, assert_golden_bytes, assert_step_reads_previous};

/// An entry as this build writes it.
const CURRENT: &str = include_str!("../../../golden/owner-workspace-document/v11.hex");
/// An entry as the build before this one wrote it, with the ownership a build before it kept.
const PREVIOUS: &str = include_str!("../../../golden/owner-workspace-document/v10.hex");
/// The entries earlier builds wrote, by the version they name, oldest first: version 7 is what
/// the released build writes, and every one since is brought up.
const EARLIER: [(u32, &str); 3] = [
    (
        7,
        include_str!("../../../golden/owner-workspace-document/v7.hex"),
    ),
    (
        8,
        include_str!("../../../golden/owner-workspace-document/v8.hex"),
    ),
    (
        9,
        include_str!("../../../golden/owner-workspace-document/v9.hex"),
    ),
];
const FOUNDERS: &str = "40267648-f762-4c8d-89ad-c5ab27aa13b8";

fn bytes(hex: &str) -> Vec<u8> {
    hex.split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect()
}

fn founders() -> WorkspaceId {
    WorkspaceId::parse(FOUNDERS).unwrap()
}

/// [`FORMAT`] as it stood when `version` was the one its build wrote: its steps up to there.
fn format_at(version: u32) -> Format {
    let steps = usize::try_from(version.checked_sub(FORMAT.oldest).unwrap()).unwrap();
    Format {
        version,
        steps: &FORMAT.steps[..steps],
        ..FORMAT
    }
}

/// `migrated` names `version` and holds the founders' workspace.
fn reads_as(migrated: &[u8], version: u32) {
    assert_eq!(stored_version(migrated).unwrap(), version);
    let stored = serde_json::from_slice::<StoredWorkspace>(migrated).unwrap();
    assert_eq!(stored.workspace, founders());
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

    phase an_entry_the_build_before_wrote_comes_up_without_its_ownership;
    // A device that updates keeps the workspaces it held: the entry its last build wrote reads
    // as this build's, and the ownership a build before that kept is gone from it.
    {
        assert_step_reads_previous(&FORMAT, &bytes(PREVIOUS), |migrated| {
            assert_eq!(stored_version(migrated).unwrap(), WORKSPACE_VERSION);
            let stored = serde_json::from_slice::<StoredWorkspace>(migrated).unwrap();
            assert_eq!(stored.workspace, founders());
            let fields: serde_json::Value = serde_json::from_slice(migrated).unwrap();
            assert!(fields.get("publication").is_none(), "{fields}");
        });
    }

    phase each_step_brings_up_the_entry_its_version_wrote;
    // Every step from the released build's version on reads what the build before it wrote, so
    // a device that skipped builds keeps its workspaces through each of them.
    {
        for (version, golden) in EARLIER {
            let next = version.checked_add(1).unwrap();
            assert_step_reads_previous(&format_at(next), &bytes(golden), |migrated| {
                reads_as(migrated, next);
            });
        }
    }

    phase every_earlier_entry_comes_up_to_this_builds;
    // A device on the released build, or on any build since, updates straight to this one.
    {
        for (version, golden) in EARLIER {
            let migrated = FORMAT.migrate(bytes(golden), version).unwrap();
            reads_as(&migrated, WORKSPACE_VERSION);
            let fields: serde_json::Value = serde_json::from_slice(&migrated).unwrap();
            assert!(fields.get("publication").is_none(), "{fields}");
        }
    }

    phase an_entry_of_another_build_is_refused;
    // An entry a newer build wrote, or one older than the released build's, is refused by the
    // version it names.
    {
        assert!(
            FORMAT
                .migrate(bytes(CURRENT), WORKSPACE_VERSION + 1)
                .is_err()
        );
        assert_eq!(FORMAT.oldest, EARLIER[0].0);
        assert!(FORMAT.migrate(bytes(EARLIER[0].1), FORMAT.oldest - 1).is_err());
    }
}
