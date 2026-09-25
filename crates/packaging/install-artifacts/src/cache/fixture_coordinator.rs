#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixture: an unexpected fixture failure must abort the test that selected it"
)]
use crate::cache::fixture_platform as platform;
#[path = "coordinator.rs"]
pub(crate) mod driver;

impl driver::Worker {
    pub(crate) fn fixture(
        root: &std::path::Path,
        preference: crate::preference::PreferenceStore,
        transport: crate::transport::InternalTransport,
        consumptions: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        use std::sync::atomic::Ordering;

        let root = root.to_owned();
        Self::spawn_runner(move |shared| {
            crate::cache::fixture_platform::ROOT.with(|value| *value.borrow_mut() = root);
            crate::test_support::with_fixture_interval(|| {
                driver::worker_loop(
                    shared,
                    transport,
                    move |selected, cancel, first, second| {
                        consumptions.fetch_add(1, Ordering::AcqRel);
                        crate::refuse_protected_staging(selected, cancel, first, second)
                    },
                    preference,
                );
            });
        })
        .expect("fixture worker")
    }
}
