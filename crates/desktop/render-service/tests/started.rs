//! The service started the way the session starts it: its own process, handed its socket as
//! stdin. It must come up ready, keep nothing else it inherited, and end with the broker.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "end-to-end test: failures are the assertion"
)]

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    use std::os::fd::AsRawFd as _;

    use tilo_render_protocol::channel::Channel;
    use tilo_render_service::broker::{FromService, MODE};
    use tilo_test_env::SpawnUnderEnvLock as _;

    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = tilo_render_service::dispatch(&args) {
        return code;
    }
    // nextest lists every test target first (`--list --format terse`, and again with `--ignored`)
    // in libtest's format, then runs each case it listed by name; this target's one case is all
    // of `main`.
    if args.iter().any(|arg| arg == "--list") {
        if !args.iter().any(|arg| arg == "--ignored") {
            println!("started: test");
        }
        return std::process::ExitCode::SUCCESS;
    }

    // A descriptor the session holds without close-on-exec: the service must not keep it.
    let scratch =
        std::env::temp_dir().join(format!("render-service-started-{}", std::process::id()));
    let held = std::fs::File::create(&scratch).unwrap();
    // SAFETY: `held` is this process's own descriptor; clearing its flags changes nothing else.
    assert_eq!(
        unsafe { libc::fcntl(held.as_raw_fd(), libc::F_SETFD, 0) },
        0
    );

    let (broker, service) = Channel::pair().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg(MODE)
        .env(tilo_render_service::RENDER_NODE_ENV, "")
        .env(
            tilo_render_service::FONT_ROOTS_ENV,
            tilo_font_catalog::checkout_root(),
        )
        .stdin(std::process::Stdio::from(
            service.as_fd().try_clone_to_owned().unwrap(),
        ))
        .spawn_locked()
        .unwrap();
    drop(service);

    let ready = broker.recv::<FromService>().unwrap().message;
    assert!(
        matches!(ready, FromService::Ready { gpu: false, .. }),
        "the service started on its socket and drew nowhere but the CPU: {ready:?}"
    );
    let kept: Vec<_> = std::fs::read_dir(format!("/proc/{}/fd", child.id()))
        .unwrap()
        .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
        .collect();
    assert!(
        !kept.contains(&scratch),
        "the service kept a descriptor it inherited: {kept:?}"
    );
    let stdin = std::fs::read_link(format!("/proc/{}/fd/0", child.id())).unwrap();
    assert_eq!(
        stdin,
        std::path::Path::new("/dev/null"),
        "its socket was moved off stdin"
    );

    drop(broker);
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "the service ends with its broker: {status}"
    );
    drop(held);
    std::fs::remove_file(&scratch).unwrap();

    // A machine where wgpu offers no adapter (here: only a backend Linux does not have) still
    // gets a service, which draws on the CPU without a device.
    let (broker, service) = Channel::pair().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg(MODE)
        .env(tilo_render_service::RENDER_NODE_ENV, "")
        .env("WGPU_BACKEND", "metal")
        .env(
            tilo_render_service::FONT_ROOTS_ENV,
            tilo_font_catalog::checkout_root(),
        )
        .stdin(std::process::Stdio::from(
            service.as_fd().try_clone_to_owned().unwrap(),
        ))
        .spawn_locked()
        .unwrap();
    drop(service);
    let ready = broker.recv::<FromService>().unwrap().message;
    assert!(
        matches!(&ready, FromService::Ready { gpu: false, device } if device == "CPU rasterizer"),
        "with no adapter the service still starts, drawing without a device: {ready:?}"
    );
    drop(broker);
    let status = child.wait().unwrap();
    assert!(status.success(), "it ends with its broker: {status}");
    println!("the render service starts on the socket it is handed: ok");
    std::process::ExitCode::SUCCESS
}

#[cfg(not(target_os = "linux"))]
fn main() {
    // No case to list or run off Linux, where the service does not exist.
}
