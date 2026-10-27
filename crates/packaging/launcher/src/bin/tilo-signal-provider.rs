//! Thin entrypoint for the normally signed native Signal service package.
//! It owns Java, requests graceful shutdown and reports success only after
//! Java's normal shutdown hooks have finished. The host still proves namespace
//! teardown before sealing the newest private profile.
use std::{
    io,
    process::{Child, Command, ExitStatus},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
static STOP: AtomicBool = AtomicBool::new(false);
/// Rung by the SIGTERM handler, so the supervisor sleeps until it matters.
static STOP_WAKE: std::sync::OnceLock<bounded_command::WakeFd> = std::sync::OnceLock::new();
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
    // `get` is one atomic load and `ring` one write(2): async-signal-safe.
    if let Some(wake) = STOP_WAKE.get() {
        wake.ring();
    }
}
fn install_stop() -> io::Result<&'static bounded_command::WakeFd> {
    // Created before the handler, so no SIGTERM finds it missing.
    let wake = if let Some(wake) = STOP_WAKE.get() {
        wake
    } else {
        let created = bounded_command::WakeFd::new()?;
        STOP_WAKE.get_or_init(|| created)
    };
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = stop as *const () as usize;
    if unsafe { libc::sigemptyset(&raw mut action.sa_mask) } != 0
        || unsafe { libc::sigaction(libc::SIGTERM, &raw const action, std::ptr::null_mut()) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(wake)
}
struct Java(Child);
fn runtime_lib(
    root: &std::path::Path,
    library: &std::path::Path,
) -> io::Result<std::path::PathBuf> {
    let relative = library
        .strip_prefix(root)
        .map_err(|_| io::ErrorKind::InvalidData)?;
    let mut parts = relative.components();
    let digest = parts
        .next()
        .and_then(|part| part.as_os_str().to_str())
        .and_then(|part| part.strip_prefix("2026.10.1-signal+native."))
        .ok_or(io::ErrorKind::InvalidData)?;
    if digest.len() != 32
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || parts.next() != Some(std::path::Component::Normal(std::ffi::OsStr::new("lib")))
        || parts.next().is_some()
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(library.to_path_buf())
}
impl Drop for Java {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn clean(status: ExitStatus, requested: bool, forced: bool) -> bool {
    !forced && (status.success() || (requested && status.code() == Some(143)))
}
fn run() -> io::Result<bool> {
    if std::env::args_os().len() != 1 || std::env::consts::ARCH != "x86_64" {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let wake = install_stop()?;
    // The normal strict launcher verifies this exact runtime and mounts it
    // read-only. There is no host Java, library lookup or alternate version.
    // bwrap supplies the verified bundle's exact library path. Landlock
    // grants that mount, not enumeration of the parent runtime directory.
    let library = std::env::var_os("LD_LIBRARY_PATH").ok_or(io::ErrorKind::InvalidData)?;
    let runtime = runtime_lib(
        std::path::Path::new("/run/tilo-runtime"),
        std::path::Path::new(&library),
    )?;
    let base = runtime.to_str().ok_or(io::ErrorKind::InvalidData)?;
    let provider = format!("{base}/signal-provider");
    let libraries = format!(
        "{base}:{base}/lib64:{base}/usr/lib64:{provider}/java/lib:{provider}/java/lib/server"
    );
    let java = Java(
        Command::new(format!("{base}/lib64/ld-linux-x86-64.so.2"))
            .args([
                "--library-path",
                &libraries,
                &format!("{provider}/java/bin/java"),
                "--enable-native-access=ALL-UNNAMED",
                "-Duser.home=/run/tilo/app",
                "-classpath",
                &format!("{provider}/signal/tilo:{provider}/signal/lib/*"),
                "org.asamk.signal.Main",
                "--data-dir",
                "/run/tilo/app/data",
                "daemon",
                "--socket",
                "/run/tilo/app/provider.sock",
                "--receive-mode",
                "manual",
            ])
            .stdin(std::process::Stdio::null())
            .spawn()?,
    );
    supervise(java, wake)
}
/// Sleep until Java exits, SIGTERM rings `wake`, or the stop grace ends.
fn supervise(mut java: Java, wake: &bounded_command::WakeFd) -> io::Result<bool> {
    let mut stopping = None;
    let mut forced = false;
    // The first wait only probes: STOP may already be set.
    let mut deadline = Some(Instant::now());
    loop {
        let ring = stopping.is_none().then(|| wake.fd());
        match bounded_command::wait_child_or(&mut java.0, ring, deadline)? {
            bounded_command::Wakeup::Exited(status) => {
                let success = clean(status, stopping.is_some(), forced);
                if !success {
                    eprintln!("Signal Java process failed: {status}");
                }
                return Ok(success);
            }
            // Drain before checking, so a signal racing the check rings again.
            bounded_command::Wakeup::Woken => wake.drain(),
            bounded_command::Wakeup::Deadline => {}
        }
        deadline = None;
        if stopping.is_none() && STOP.load(Ordering::Relaxed) {
            // This child remains owned and unreaped, so its PID cannot refer
            // to another process. It is never selected by a name or pid file.
            let pid = i32::try_from(java.0.id()).map_err(io::Error::other)?;
            if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
                return Err(io::Error::last_os_error());
            }
            stopping = Some(
                Instant::now()
                    .checked_add(Duration::from_secs(2))
                    .ok_or(io::ErrorKind::InvalidData)?,
            );
        }
        if stopping.is_some_and(|grace| Instant::now() >= grace) && !forced {
            java.0.kill()?;
            forced = true;
        }
        if !forced {
            deadline = stopping;
        }
    }
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => std::process::ExitCode::FAILURE,
        Err(error) => {
            eprintln!("Signal provider startup failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_requires_one_exact_native_build_path() {
        let root = tempfile::tempdir().unwrap();
        let exact = root
            .path()
            .join(format!("2026.10.1-signal+native.{}/lib", "a".repeat(32)));
        assert_eq!(runtime_lib(root.path(), &exact).unwrap(), exact);
        for wrong in [
            root.path().join("2026.10.1-signal/lib"),
            root.path().join("2026.10.1-signal+native.a/lib"),
            root.path()
                .join(format!("2026.10.1-signal+native.{}/lib", "A".repeat(32))),
            exact.join(".."),
            exact.join("other"),
            root.path().join(format!(
                "2026.10.1-signal+native.{}/lib:/host",
                "a".repeat(32)
            )),
            std::path::PathBuf::from("/host/lib"),
        ] {
            assert_eq!(
                runtime_lib(root.path(), &wrong).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }
    #[test]
    fn runtime_path_works_without_parent_enumeration_under_strict_landlock() {
        const FIXTURE: &str = "TILO_SIGNAL_RUNTIME_RULE_FIXTURE";
        if let Some(root) = std::env::var_os(FIXTURE) {
            let root = std::path::PathBuf::from(root);
            let mount = root.join(format!("2026.10.1-signal+native.{}", "a".repeat(32)));
            let library = mount.join("lib");
            let plan = launcher::landlock::plan_from_paths(&[mount], &[]);
            assert_eq!(
                launcher::landlock::apply_plan_or_log(&plan, false),
                launcher::landlock::LandlockOutcome::FullyEnforced
            );
            assert_eq!(
                std::fs::read_dir(&root).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
            assert_eq!(runtime_lib(&root, &library).unwrap(), library);
            return;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(
            root.path()
                .join(format!("2026.10.1-signal+native.{}/lib", "a".repeat(32))),
        )
        .unwrap();
        let result = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::runtime_path_works_without_parent_enumeration_under_strict_landlock",
                "--nocapture",
            ])
            .env(FIXTURE, root.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "strict runtime fixture failed: stdout={} stderr={}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fn live(script: &str) -> (tempfile::TempDir, Java) {
        let root = tempfile::tempdir().unwrap();
        let ready = root.path().join("ready");
        let child = Command::new("/bin/sh")
            .args(["-c", script, "fixture"])
            .arg(&ready)
            .spawn()
            .unwrap();
        let child = Java(child);
        tilo_test_env::wait_until("the fixture to observe the live child", || ready.exists());
        (root, child)
    }
    #[test]
    fn actual_child_normal_hooks_and_forced_shutdown_produce_different_checkpoints() {
        let wake = install_stop().unwrap();
        let (_root, child) =
            live("trap 'exit 143' TERM; touch \"$1\"; while :; do sleep 0.02; done");
        STOP.store(true, Ordering::Relaxed);
        assert!(supervise(child, wake).unwrap());
        let (_root, child) = live("trap '' TERM; touch \"$1\"; while :; do sleep 0.02; done");
        assert!(!supervise(child, wake).unwrap());
        STOP.store(false, Ordering::Relaxed);
        let child = Java(
            Command::new("/bin/sh")
                .args(["-c", "exit 143"])
                .spawn()
                .unwrap(),
        );
        assert!(
            !supervise(child, wake).unwrap(),
            "unsolicited 143 exit is never clean"
        );
        let child = Java(
            Command::new("/bin/sh")
                .args(["-c", "kill -TERM $$"])
                .spawn()
                .unwrap(),
        );
        assert!(
            !supervise(child, wake).unwrap(),
            "kernel signal termination is never a normal Java shutdown"
        );
        // Phase `sigterm_wakes_the_sleeping_supervisor`: the supervisor waits
        // without a timer, so only the handler's ring can start the shutdown.
        let (root, child) =
            live("trap 'exit 143' TERM; touch \"$1\"; while :; do sleep 0.02; done");
        let pid = i32::try_from(child.0.id()).unwrap();
        let stopped = root.path().join("stopped");
        let watchdog = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            // SAFETY: raises SIGTERM in this process, whose handler is installed.
            assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
            let deadline = tilo_test_env::hang_deadline();
            while !stopped.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if !stopped.exists() {
                // A missed ring would otherwise hang: an unrequested kill fails it.
                // SAFETY: the supervised child is still owned and unreaped here.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        });
        let clean = supervise(child, wake).unwrap();
        std::fs::write(root.path().join("stopped"), b"").unwrap();
        watchdog.join().unwrap();
        assert!(clean, "SIGTERM must reach the sleeping supervisor");
        assert!(STOP.load(Ordering::Relaxed));
        STOP.store(false, Ordering::Relaxed);
    }
}
