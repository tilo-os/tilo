//! Running a Wayland client inside `bwrap` against a test compositor's socket,
//! for the sandbox handshake and interaction tests.

use std::fs::File;
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// `app_command` under `bwrap`, with `project_path` bound at `/project` (its
/// working directory), no network, and the host's `wayland_socket` bound into
/// a private runtime directory.
pub fn bwrap_command(project_path: &str, app_command: &str, wayland_socket: &str) -> Command {
    let mut cmd = Command::new("bwrap");

    cmd.arg("--unshare-user");
    cmd.arg("--unshare-ipc");
    cmd.arg("--unshare-pid");
    cmd.arg("--unshare-net");

    cmd.arg("--ro-bind").arg("/usr").arg("/usr");
    cmd.arg("--ro-bind").arg("/etc").arg("/etc");
    cmd.arg("--symlink").arg("usr/lib").arg("/lib");
    cmd.arg("--symlink").arg("usr/lib64").arg("/lib64");
    cmd.arg("--symlink").arg("usr/bin").arg("/bin");
    cmd.arg("--symlink").arg("usr/sbin").arg("/sbin");

    if Path::new("/usr/share").exists() {
        cmd.arg("--ro-bind").arg("/usr/share").arg("/usr/share");
    }

    cmd.arg("--dev").arg("/dev");
    cmd.arg("--proc").arg("/proc");

    if Path::new("/sys").exists() {
        cmd.arg("--ro-bind").arg("/sys").arg("/sys");
    }
    if Path::new("/dev/dri").exists() {
        cmd.arg("--dev-bind").arg("/dev/dri").arg("/dev/dri");
    }
    if Path::new("/run/udev").exists() {
        cmd.arg("--ro-bind").arg("/run/udev").arg("/run/udev");
    }
    if Path::new("/dev/shm").exists() {
        cmd.arg("--bind").arg("/dev/shm").arg("/dev/shm");
    }

    cmd.arg("--tmpfs").arg("/tmp");
    cmd.arg("--bind").arg(project_path).arg("/project");
    cmd.arg("--chdir").arg("/project");

    let host_runtime_dir =
        tilo_paths::xdg::runtime_dir().unwrap_or_else(|| "/run/user/1000".into());
    let host_socket_path = format!("{}/{wayland_socket}", host_runtime_dir.display());
    let sandbox_runtime_dir = "/tmp/runtime";
    cmd.arg("--dir").arg(sandbox_runtime_dir);
    if Path::new(&host_socket_path).exists() {
        cmd.arg("--bind")
            .arg(&host_socket_path)
            .arg(format!("{sandbox_runtime_dir}/{wayland_socket}"));
    }

    cmd.env("XDG_RUNTIME_DIR", sandbox_runtime_dir);
    cmd.env("WAYLAND_DISPLAY", wayland_socket);
    cmd.env("XDG_SESSION_TYPE", "wayland");
    cmd.env("WAYLAND_DEBUG", "1");
    cmd.env("QT_QPA_PLATFORM", "wayland");
    cmd.env("GDK_BACKEND", "wayland");
    cmd.env("MOZ_ENABLE_WAYLAND", "1");
    cmd.env("WINIT_UNIX_BACKEND", "wayland");
    cmd.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    cmd.env("XDG_CURRENT_DESKTOP", "tilo");

    cmd.arg("--tmpfs").arg("/home/user");
    cmd.env("HOME", "/home/user");

    // The wrapper's lines tell a missing socket from a client that failed.
    let wrapped_script = format!(
        "echo '[WRAPPER] Date: $(date)'; \
         echo '[WRAPPER] Socket: {wayland_socket}'; \
         ls -l {sandbox_runtime_dir}/{wayland_socket} || echo '[WRAPPER] Socket NOT FOUND'; \
         echo '[WRAPPER] Executing: {app_command}'; \
         exec {app_command} 2>&1"
    );
    cmd.arg("sh").arg("-c").arg(wrapped_script);
    cmd
}

/// Spawn [`bwrap_command`] with its stdout and stderr written to `log_path`.
pub fn spawn_logged(
    project_path: &str,
    app_command: &str,
    wayland_socket: &str,
    log_path: &Path,
) -> ChildGuard {
    let mut cmd = bwrap_command(project_path, app_command, wayland_socket);
    let log = File::create(log_path).expect("create the sandboxed client's log");
    cmd.stdout(Stdio::from(log.try_clone().expect("clone the log handle")));
    cmd.stderr(Stdio::from(log));
    ChildGuard(cmd.spawn().expect("spawn the sandboxed client"))
}

/// Kills and reaps the sandboxed client on drop, so a panicking `.expect()`
/// or a failing assertion before the explicit `kill`/`wait` can't leak the
/// process.
pub struct ChildGuard(pub Child);

impl std::ops::Deref for ChildGuard {
    type Target = Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
