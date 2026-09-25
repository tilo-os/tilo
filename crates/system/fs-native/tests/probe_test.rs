//! The capability probes against the running kernel.

use fs_native::probe::{self, K5Params, Status, Tier};

use crate::support::fanotify_scratch;

#[test]
fn every_probe_reaches_a_verdict_and_the_tier_follows_from_them() {
    let Some(s) = fanotify_scratch("probe") else {
        return;
    };
    let report = probe::probe(
        &s.store,
        Some(K5Params {
            files: 2000,
            dirty_bytes: 0,
            samples: 3,
        }),
    )
    .unwrap();
    eprintln!("{}", report.render());
    for c in &report.checks {
        if c.id != "fuse-passthrough" {
            assert_ne!(
                c.status,
                Status::Unknown,
                "{} could not run: {}",
                c.id,
                c.detail
            );
        }
    }
    let decision = report.decide();
    let prerequisites = [
        "volume",
        "fanotify-fs-mark",
        "journal",
        "pre-content",
        "snapshot",
        "fiemap-diff",
        "reflink",
    ];
    let all = prerequisites.iter().all(|id| {
        report
            .get(id)
            .is_some_and(|c| c.status == Status::Supported)
    });
    assert_eq!(decision.tier == Tier::Native, all, "{decision:?}");
}
