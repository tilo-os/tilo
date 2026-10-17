//! Original-user native host process. Only the private handoff socket survives
//! exec; approved subtree and output rights arrive after kernel authentication.
use super::super::{
    failure,
    transport::{CHANNEL_FD, HostChannel},
};
use std::{
    io,
    os::{
        fd::AsRawFd,
        unix::{net::UnixStream, process::CommandExt},
    },
    path::Path,
    process::{Child, Command, Stdio},
};

use super::ARG;

/// The caller supplies its canonical, authenticated native helper executable.
/// This is not an app-provided executable or serialized launch specification.
pub(super) fn spawn(executable: &Path, original_uid: u32) -> io::Result<(Child, HostChannel)> {
    if rustix::process::geteuid().as_raw() != 0 || original_uid == 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let credentials = credentials(original_uid)?;
    let (parent, worker) = UnixStream::pair()?;
    let descriptor = worker.as_raw_fd();
    let parent_pid = unsafe { libc::getpid() };
    let mut command = Command::new(executable);
    clean_environment(&mut command);
    command
        .arg(ARG)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // NSS-derived original groups preserve group-readable personal files.
    // Neither the helper's root groups nor request-provided IDs are inherited.
    unsafe {
        command.pre_exec(move || {
            // This namespace is initially blank: approved source rights still
            // cross only after the broker durably records this host incarnation.
            if libc::unshare(libc::CLONE_NEWNS) < 0
                || libc::mount(
                    std::ptr::null(),
                    c"/".as_ptr(),
                    std::ptr::null(),
                    libc::MS_REC | libc::MS_PRIVATE,
                    std::ptr::null(),
                ) < 0
                || libc::setgroups(credentials.groups.len(), credentials.groups.as_ptr()) < 0
                || libc::setresgid(credentials.gid, credentials.gid, credentials.gid) < 0
                || libc::setresuid(original_uid, original_uid, original_uid) < 0
                || libc::dup2(descriptor, CHANNEL_FD) < 0
                || libc::fcntl(CHANNEL_FD, libc::F_SETFD, 0) < 0
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) < 0
            {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent_pid {
                return Err(io::ErrorKind::Interrupted.into());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    drop(worker);
    match HostChannel::authenticate(parent, child.id(), original_uid) {
        Ok(channel) => Ok((child, channel)),
        Err(error) => {
            // No source/output descriptor has been sent; killing this failed
            // admission cannot interrupt an export or justify snapshot cleanup.
            let _ = child.kill();
            child.wait()?;
            Err(error)
        }
    }
}

/// Native admission carries its own paths and identity through authenticated
/// descriptors. Root's loader, temporary directory and account environment
/// confer no authority on the credential-dropped host.
fn clean_environment(command: &mut Command) {
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .current_dir("/");
}

struct Credentials {
    gid: u32,
    groups: Vec<libc::gid_t>,
}

fn credentials(uid: u32) -> io::Result<Credentials> {
    let mut storage = vec![0u8; 64 * 1024];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            &raw mut passwd,
            storage.as_mut_ptr().cast(),
            storage.len(),
            &raw mut result,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    if result.is_null() || passwd.pw_uid != uid {
        return Err(failure("original user primary group is unavailable"));
    }
    if passwd.pw_name.is_null() {
        return Err(failure("original account name missing"));
    }
    let mut groups = vec![0; 65_536];
    let mut count = i32::try_from(groups.len()).map_err(io::Error::other)?;
    let status = unsafe {
        libc::getgrouplist(
            passwd.pw_name,
            passwd.pw_gid,
            groups.as_mut_ptr(),
            &raw mut count,
        )
    };
    let count = usize::try_from(count).map_err(io::Error::other)?;
    if status < 0 || count == 0 || count > groups.len() {
        return Err(failure("original supplementary groups exceed native bound"));
    }
    groups.truncate(count);
    groups.sort_unstable();
    groups.dedup();
    if !groups.contains(&passwd.pw_gid) {
        return Err(failure("original primary group missing"));
    }
    Ok(Credentials {
        gid: passwd.pw_gid,
        groups,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_host_does_not_inherit_privileged_paths_or_loader_overrides() {
        let privileged = tempfile::tempdir().unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .env("TMPDIR", privileged.path().join("root-only"))
            .env("HOME", privileged.path())
            .env("XDG_RUNTIME_DIR", privileged.path())
            .env("LD_PRELOAD", "/untrusted-loader.so")
            .env("LD_LIBRARY_PATH", privileged.path())
            .current_dir(privileged.path());
        clean_environment(&mut command);
        let output = command
            .args([
                "-c",
                "test \"$PWD\" = / && test -z \"${TMPDIR+x}${HOME+x}${XDG_RUNTIME_DIR+x}${LD_PRELOAD+x}${LD_LIBRARY_PATH+x}\" && /usr/bin/mktemp -d",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "loader overrides reached child");
        let path = std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        assert!(path.is_absolute());
        assert!(!path.starts_with(privileged.path()));
        std::fs::remove_dir(path).unwrap();
    }
}
