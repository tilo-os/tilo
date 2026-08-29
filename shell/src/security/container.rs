use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use uuid::Uuid;

pub struct Container {
    pub project_path: String,
    pub allow_network: bool,
}

impl Container {
    pub fn new(project_path: &str) -> Self {
        Self {
            project_path: project_path.to_string(),
            allow_network: false,
        }
    }

    pub fn build_command(&self, app_command: &str, wayland_socket: &str) -> Command {
        let mut cmd = Command::new("bwrap");

        // 1. Identity & Isolation
        cmd.arg("--unshare-user");
        cmd.arg("--unshare-ipc");
        cmd.arg("--unshare-pid");

        // 2. Base System (Fedora/Bazzite compatibility)
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

        // 4. Filesystem Setup
        cmd.arg("--tmpfs").arg("/tmp");
        cmd.arg("--bind").arg(&self.project_path).arg("/project");
        cmd.arg("--chdir").arg("/project");

        // 5. Wayland Socket
        let host_runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or("/run/user/1000".into());
        let host_socket_path = format!("{host_runtime_dir}/{wayland_socket}");

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

        // Robust environment for Wayland apps
        cmd.env("QT_QPA_PLATFORM", "wayland");
        cmd.env("GDK_BACKEND", "wayland");
        cmd.env("MOZ_ENABLE_WAYLAND", "1");
        cmd.env("WINIT_UNIX_BACKEND", "wayland");
        cmd.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        cmd.env("XDG_CURRENT_DESKTOP", "QuickDesktop");

        if !self.allow_network {
            cmd.arg("--unshare-net");
        }

        cmd.arg("--tmpfs").arg("/home/user");
        cmd.env("HOME", "/home/user");

        // Wrapper to ensure we capture startup errors
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

    pub fn spawn(
        &self,
        app_command: &str,
        wayland_socket: &str,
        tab_id: Uuid,
    ) -> std::io::Result<std::process::Child> {
        let mut cmd = self.build_command(app_command, wayland_socket);

        let log_path = format!("/tmp/quick-desktop-app-{tab_id}.log");
        let log_file = File::create(log_path)?;
        cmd.stdout(Stdio::from(log_file.try_clone()?));
        cmd.stderr(Stdio::from(log_file));

        println!("DEBUG: Spawning Tab {tab_id} Log: /tmp/quick-desktop-app-{tab_id}.log");
        cmd.spawn()
    }
}
