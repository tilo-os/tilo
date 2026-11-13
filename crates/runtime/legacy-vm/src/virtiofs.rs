//! One unprivileged `virtiofsd` per share. It cannot build its own namespace
//! sandbox without a uid map, so `tilo-legacy confine-fs` restricts it with
//! Landlock to the share, the socket's directory and the system's read-only files,
//! then execs it.

use std::path::{Path, PathBuf};

use crate::grants::HostShare;

/// Fedora installs the daemon outside `PATH`.
pub const VIRTIOFSD: &str = "/usr/libexec/virtiofsd";

/// What a confined `virtiofsd` may read besides its share.
pub const SYSTEM_READ_ONLY: &[&str] = &["/usr", "/etc", "/proc", "/sys/devices/system/cpu"];

#[must_use]
#[allow(clippy::similar_names, reason = "uid and gid are the vocabulary here")]
pub fn virtiofsd_argv(
    virtiofsd: &str,
    share: &HostShare,
    socket: &Path,
    guest_uid: u32,
    host_uid: u32,
    host_gid: u32,
) -> Vec<String> {
    let mut argv = vec![
        virtiofsd.to_string(),
        "--socket-path".to_string(),
        socket.display().to_string(),
        "--shared-dir".to_string(),
        share.path.display().to_string(),
        "--sandbox".to_string(),
        "none".to_string(),
        "--cache".to_string(),
        "auto".to_string(),
        "--translate-uid".to_string(),
        format!("map:{guest_uid}:{host_uid}:1"),
        "--translate-gid".to_string(),
        format!("map:{guest_uid}:{host_gid}:1"),
    ];
    if share.read_only {
        argv.push("--readonly".to_string());
    }
    argv
}

#[must_use]
pub fn confined_argv(
    self_exe: &Path,
    share: &HostShare,
    socket: &Path,
    daemon: Vec<String>,
) -> Vec<String> {
    let mut argv = vec![self_exe.display().to_string(), "confine-fs".to_string()];
    for path in SYSTEM_READ_ONLY {
        argv.extend(["--ro".to_string(), (*path).to_string()]);
    }
    let shared = if share.read_only { "--ro" } else { "--rw" };
    argv.extend([shared.to_string(), share.path.display().to_string()]);
    if let Some(dir) = socket.parent() {
        argv.extend(["--rw".to_string(), dir.display().to_string()]);
    }
    argv.push("--".to_string());
    argv.extend(daemon);
    argv
}

/// A `confine-fs` command line: what the program may read, write and must not see, and the program.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Confinement {
    pub ro: Vec<PathBuf>,
    pub rw: Vec<PathBuf>,
    /// Namespaces of its own first: no network, and [`Self::hide`] covered.
    pub isolate: bool,
    /// Directories that are empty in its mount namespace (with `isolate`).
    pub hide: Vec<PathBuf>,
    pub program: Vec<String>,
}

/// The `confine-fs` arguments in `args`: `--ro DIR`, `--rw DIR`,
/// `--isolate`, `--hide DIR`, then `--` and the program.
#[must_use]
pub fn parse_confined(args: &[String]) -> Option<Confinement> {
    let mut confinement = Confinement::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ro" => confinement.ro.push(PathBuf::from(args.next()?)),
            "--rw" => confinement.rw.push(PathBuf::from(args.next()?)),
            "--hide" => confinement.hide.push(PathBuf::from(args.next()?)),
            "--isolate" => confinement.isolate = true,
            "--" => {
                confinement.program = args.cloned().collect();
                return (!confinement.program.is_empty()
                    && (confinement.isolate || confinement.hide.is_empty()))
                .then_some(confinement);
            }
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_share_gets_a_confined_unprivileged_daemon() {
        let share = HostShare {
            path: "/var/home/you/Music".into(),
            read_only: true,
        };
        let socket = Path::new("/run/user/1000/tilo-legacy/linux/fs-1.sock");
        {
            let argv = virtiofsd_argv(VIRTIOFSD, &share, socket, 1000, 4242, 4243);
            assert_eq!(
                argv,
                [
                    VIRTIOFSD,
                    "--socket-path",
                    "/run/user/1000/tilo-legacy/linux/fs-1.sock",
                    "--shared-dir",
                    "/var/home/you/Music",
                    "--sandbox",
                    "none",
                    "--cache",
                    "auto",
                    "--translate-uid",
                    "map:1000:4242:1",
                    "--translate-gid",
                    "map:1000:4243:1",
                    "--readonly"
                ]
            );
            let writable = HostShare {
                read_only: false,
                ..share.clone()
            };
            assert!(
                !virtiofsd_argv(VIRTIOFSD, &writable, socket, 1000, 1, 1)
                    .contains(&"--readonly".to_string())
            );
        }
        {
            let daemon = virtiofsd_argv(VIRTIOFSD, &share, socket, 1000, 1, 1);
            let argv = confined_argv(
                Path::new("/usr/libexec/tilo/tilo-legacy"),
                &share,
                socket,
                daemon.clone(),
            );
            assert_eq!(argv[..2], ["/usr/libexec/tilo/tilo-legacy", "confine-fs"]);
            let Confinement {
                ro, rw, program, ..
            } = parse_confined(&argv[2..]).unwrap();
            assert!(ro.contains(&PathBuf::from("/var/home/you/Music")));
            assert!(ro.contains(&PathBuf::from("/usr")));
            assert_eq!(rw, [PathBuf::from("/run/user/1000/tilo-legacy/linux")]);
            assert_eq!(program, daemon);
            assert_eq!(parse_confined(&["--ro".to_string()]), None);
            assert_eq!(parse_confined(&["--".to_string()]), None);
        }
    }
}
