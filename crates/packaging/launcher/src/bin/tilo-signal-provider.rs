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
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}
fn install_stop() -> io::Result<()> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = stop as *const () as usize;
    if unsafe { libc::sigemptyset(&raw mut action.sa_mask) } != 0
        || unsafe { libc::sigaction(libc::SIGTERM, &raw const action, std::ptr::null_mut()) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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
    install_stop()?;
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
    supervise(java)
}
fn supervise(mut java: Java) -> io::Result<bool> {
    let mut stopping = None;
    let mut forced = false;
    loop {
        if let Some(status) = java.0.try_wait()? {
            return Ok(clean(status, stopping.is_some(), forced));
        }
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
        if stopping.is_some_and(|deadline| Instant::now() >= deadline) && !forced {
            java.0.kill()?;
            forced = true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn main() -> std::process::ExitCode {
    if run().unwrap_or(false) {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
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
        let deadline = Instant::now().checked_add(Duration::from_secs(5)).unwrap();
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            ready.exists(),
            "fixture must observe the actual live child before shutdown"
        );
        (root, child)
    }
    #[test]
    fn actual_child_normal_hooks_and_forced_shutdown_produce_different_checkpoints() {
        let (_root, child) =
            live("trap 'exit 143' TERM; touch \"$1\"; while :; do sleep 0.02; done");
        STOP.store(true, Ordering::Relaxed);
        assert!(supervise(child).unwrap());
        let (_root, child) = live("trap '' TERM; touch \"$1\"; while :; do sleep 0.02; done");
        assert!(!supervise(child).unwrap());
        STOP.store(false, Ordering::Relaxed);
        let child = Java(
            Command::new("/bin/sh")
                .args(["-c", "exit 143"])
                .spawn()
                .unwrap(),
        );
        assert!(
            !supervise(child).unwrap(),
            "unsolicited 143 exit is never clean"
        );
        let child = Java(
            Command::new("/bin/sh")
                .args(["-c", "kill -TERM $$"])
                .spawn()
                .unwrap(),
        );
        assert!(
            !supervise(child).unwrap(),
            "kernel signal termination is never a normal Java shutdown"
        );
    }
}
