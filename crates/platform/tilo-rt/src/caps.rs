//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::Availability;
/// One capability of this crate, for [`availability`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// Named OS threads: [`spawn_thread`](crate::spawn_thread) and the
    /// [`ThreadPool`](crate::background::ThreadPool) executor.
    Threads,
    /// Waiting on a future synchronously: [`block_on`](crate::block_on) and
    /// [`AppRuntime::block_on`](crate::AppRuntime::block_on).
    BlockOn,
    /// Multi-threaded service runtimes: `daemon()`.
    DaemonRuntime,
    /// The file-I/O pool: [`io`](crate::io).
    Io,
}

const WEB_THREADS: &str = "A browser page has no threads of its own.";
const WEB_BLOCK_ON: &str = "A browser page's one thread cannot wait on work.";
const WEB_DAEMON: &str = "A browser page cannot run a multi-threaded runtime.";
const WEB_IO: &str = "A browser page has no file-I/O threads.";

/// Every feature with its availability on Linux, macOS and the web, in that
/// order.
pub const CAPABILITIES: &[(Feature, [Availability; 3])] = {
    use Availability::{Available, Unsupported};
    &[
        (
            Feature::Threads,
            [Available, Available, Unsupported(WEB_THREADS)],
        ),
        (
            Feature::BlockOn,
            [Available, Available, Unsupported(WEB_BLOCK_ON)],
        ),
        (
            Feature::DaemonRuntime,
            [Available, Available, Unsupported(WEB_DAEMON)],
        ),
        (Feature::Io, [Available, Available, Unsupported(WEB_IO)]),
    ]
};

/// Whether `feature` can be used on this target.
pub fn availability(feature: Feature) -> Availability {
    tilo_platform_caps::availability(CAPABILITIES, feature)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Feature; 4] = [
        Feature::Threads,
        Feature::BlockOn,
        Feature::DaemonRuntime,
        Feature::Io,
    ];

    #[test]
    fn every_feature_is_listed_once() {
        for feature in ALL {
            let rows = CAPABILITIES.iter().filter(|(listed, _)| *listed == feature);
            assert_eq!(rows.count(), 1, "{feature:?}");
        }
    }

    #[test]
    fn availability_matches_the_table_for_this_target() {
        for (feature, row) in CAPABILITIES {
            assert_eq!(availability(*feature), row[tilo_platform_caps::COLUMN]);
        }
    }

    #[test]
    fn spawn_thread_agrees_with_threads_availability() {
        let spawned = crate::spawn_thread("tilo-rt-caps-test", || 5);
        match availability(Feature::Threads) {
            Availability::Available | Availability::Prompt => {
                assert_eq!(spawned.unwrap().join().unwrap(), 5);
            }
            Availability::Unsupported(_) => assert!(spawned.is_err()),
        }
    }

    #[test]
    fn block_on_agrees_with_block_on_availability() {
        let result = crate::block_on(async { 3 });
        match availability(Feature::BlockOn) {
            Availability::Available | Availability::Prompt => assert_eq!(result.unwrap(), 3),
            Availability::Unsupported(_) => assert!(result.is_err()),
        }
    }
}
