//! The `tilo-runtime` program of a test under Wine: the same daemon, allowed to start a tilolang
//! process with no confinement at all where Windows has no `AppContainer` to start it in, so a
//! walk through the daemon reaches an app there. It proves nothing about the sandbox: every
//! confinement and refusal test runs on a Windows worker, confined, against `tilo-runtime`
//! itself. Built only with `test-support`.

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    tilo_runtime::device::capabilities::allow_unconfined_test_launch();
    tilo_runtime::device::capabilities::mount(tilo_daemon::capabilities());
    tilo_runtime::cli::host("tilo-runtime")
}
