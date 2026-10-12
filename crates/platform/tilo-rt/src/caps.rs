//! What this crate provides on each target, as data an app can query.

pub use tilo_platform_caps::{Availability, availability};
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

impl tilo_platform_caps::Feature for Feature {
    /// The feature's availability on Linux, macOS and the web, in that
    /// order.
    fn targets(self) -> [Availability; 3] {
        use Availability::{Available, Unsupported};
        match self {
            Self::Threads => [Available, Available, Unsupported(WEB_THREADS)],
            Self::BlockOn => [Available, Available, Unsupported(WEB_BLOCK_ON)],
            Self::DaemonRuntime => [Available, Available, Unsupported(WEB_DAEMON)],
            Self::Io => [Available, Available, Unsupported(WEB_IO)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
