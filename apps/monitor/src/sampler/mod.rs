//! Where Monitor's readings come from: the system information service
//! (`service`, through `tilo_system_info`). On Linux the session reads
//! the machine and pushes it, since the sandbox shows Monitor no other
//! process; elsewhere (macOS) the same service runs in Monitor's process
//! over the platform's own interfaces. The sampler folds a reading into the
//! [`crate::model::MonitorModel`] the panes draw, and a sample (`--shots`, a
//! past version) never reads anything.

pub(crate) mod service;

pub use service::Sampler;

#[cfg(test)]
use std::cell::Cell;

/// How an end request asks a process to go. `Term` asks it to shut down
/// cleanly (catchable); `Kill` is the un-catchable hard stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopKind {
    /// `SIGTERM` — "End process".
    Term,
    /// `SIGKILL` — "Force end".
    Kill,
}

#[cfg(test)]
thread_local! {
    static PROVIDER_READ_COUNT: Cell<usize> = const { Cell::new(0) };
}

/// Count a read of the machine, so a test sees whether one happened.
fn note_provider_read() {
    #[cfg(test)]
    PROVIDER_READ_COUNT.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(test)]
pub(crate) fn reset_provider_read_count() {
    PROVIDER_READ_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn provider_read_count() -> usize {
    PROVIDER_READ_COUNT.with(Cell::get)
}

/// The session's service, in this test process: a real hub reading this
/// machine, served over a socket pair the way the launcher hands one over,
/// installed as the process's connection. The person whose processes may
/// be ended is this test's user, and their "app" is the test's own cgroup,
/// so a test can end a child it started and nothing outside it.
/// Outside Linux the service runs in this process already, over this
/// machine: there is no session to stand in for.
#[cfg(all(test, not(target_os = "linux")))]
pub(crate) fn install_test_session() {}

#[cfg(all(test, target_os = "linux"))]
#[allow(
    clippy::expect_used,
    reason = "a test process that cannot make a socket pair cannot test anything"
)]
pub(crate) fn install_test_session() {
    use std::sync::OnceLock;
    use tilo_system_stats::apps::{EndPolicy, unified_path};
    use tilo_system_stats::protocol::{Grants, Handoff, PROTOCOL};

    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let own = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
        let policy = EndPolicy {
            app_root: unified_path(&own).unwrap_or("/").to_owned(),
            ..EndPolicy::current()
        };
        let hub = tilo_system_stats::hub::Hub::new(
            Box::new(tilo_system_stats::probe::KernelProbe::new()),
            policy,
            Box::new(tilo_system_stats::end::PidfdSignaller),
        );
        let (app, service) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        let handoff = Handoff {
            protocol: PROTOCOL,
            app_id: "so.tilo.monitor".into(),
            workspace_id: None,
            grants: Grants::IMAGE_DEFAULT,
            ceiling: Grants::ALL,
        };
        std::thread::spawn(move || {
            let reader = service.try_clone().expect("a second descriptor");
            tilo_system_stats::server::serve(
                reader,
                service,
                &handoff,
                &hub,
                &tilo_system_stats::server::NoPrompter,
            );
        });
        tilo_system_stats::client::Client::install_shared(
            tilo_system_stats::client::Client::from_stream(app).expect("the client"),
        );
    });
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::model::{MonitorModel, ProcessDataStatus};

    /// The session's readings fold into the model the live app draws from.
    #[test]
    fn the_sessions_readings_fill_the_metrics_and_the_process_list() {
        install_test_session();
        let mut sampler = Sampler::new();
        let mut model = MonitorModel::empty();
        assert!(sampler.refresh_now(&mut model));
        let m = &model.metrics;
        assert!(!m.cores.is_empty(), "this machine's cores");
        assert!(m.mem_total > 0, "this machine's memory");
        assert_eq!(model.process_status, ProcessDataStatus::Ready);
        let own = std::process::id();
        let me = model
            .processes
            .iter()
            .find(|p| p.pid == own)
            .expect("the test process itself is listed");
        assert!(me.cmd.is_empty(), "no command line without its grant");
        assert!(m.net_rx >= 0.0 && m.net_rx.is_finite());
        assert_eq!(m.net_rx_hist.last(), m.net_rx);
        assert_eq!(m.disk_read_hist.last(), m.disk_read);
        assert!(sampler.refresh_now(&mut model));
        assert!(model.metrics.disk_write.is_finite());
    }

    /// An end request for a process that is not there reaches nothing.
    #[test]
    fn ending_an_unknown_or_stale_process_reaches_nothing() {
        install_test_session();
        let mut sampler = Sampler::new();
        assert!(!sampler.signal_if_current(u32::MAX, 0, StopKind::Term));
        assert!(!sampler.signal_if_current(u32::MAX, 0, StopKind::Kill));
        let own = std::process::id();
        assert!(
            !sampler.signal_if_current(own, 0, StopKind::Term),
            "a lifetime that is not this one"
        );
        assert!(sampler.end_app("so.tilo.nothing-runs-this").is_err());
    }
}
