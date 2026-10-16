//! What an unprivileged user namespace allows.
//!
//! The process enters a new user and mount namespace (it must still be
//! single-threaded), maps itself to root inside it, mounts a tmpfs it owns,
//! and tries each fanotify and Btrfs operation the data plane uses.

use std::fs;
use std::io;
use std::path::Path;
use std::process::ExitCode;

use fs_native::fanotify::{self as fan, Group};
use fs_native::{btrfs, sys};

fn errno(e: &io::Error) -> String {
    e.raw_os_error().map_or_else(
        || e.to_string(),
        |n| io::Error::from_raw_os_error(n).to_string(),
    )
}

fn enter_userns() -> io::Result<()> {
    let (uid, gid) = (
        rustix::process::getuid().as_raw(),
        rustix::process::getgid().as_raw(),
    );
    // SAFETY: plain syscall; the process is still single-threaded.
    if unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) } != 0 {
        return Err(io::Error::last_os_error());
    }
    fs::write("/proc/self/setgroups", "deny")?;
    fs::write("/proc/self/uid_map", format!("0 {uid} 1"))?;
    fs::write("/proc/self/gid_map", format!("0 {gid} 1"))?;
    let root = c"/";
    // SAFETY: valid NUL-terminated strings; makes our mounts private.
    let ret = unsafe {
        libc::mount(
            std::ptr::null(),
            root.as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn mount_tmpfs(at: &Path) -> io::Result<()> {
    fs::create_dir_all(at)?;
    let target = std::ffi::CString::new(at.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))?;
    let kind = c"tmpfs";
    // SAFETY: valid NUL-terminated strings.
    let ret = unsafe {
        libc::mount(
            kind.as_ptr(),
            target.as_ptr(),
            kind.as_ptr(),
            0,
            std::ptr::null(),
        )
    };
    if ret == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn line(what: &str, r: Result<String, String>) {
    match r {
        Ok(d) => println!("  [yes] {what:<52} {d}"),
        Err(d) => println!("  [no ] {what:<52} {d}"),
    }
}

fn notif_group() -> io::Result<Group> {
    Group::new(fan::FAN_CLASS_NOTIF | fan::FAN_REPORT_DFID_NAME_TARGET, 0)
}

const DIRENT: u64 = fan::FAN_CREATE | fan::FAN_DELETE | fan::FAN_RENAME | fan::FAN_ONDIR;

fn probes(btrfs_dir: Option<&Path>, tmpfs: &Path, pass: &str) {
    line(
        "FID group (FAN_CLASS_NOTIF | DFID_NAME_TARGET)",
        notif_group().map(|_| String::new()).map_err(|e| errno(&e)),
    );
    line(
        "pre-content group (FAN_CLASS_PRE_CONTENT)",
        Group::new(fan::FAN_CLASS_PRE_CONTENT, 0)
            .map(|_| String::new())
            .map_err(|e| errno(&e)),
    );
    line(
        "filesystem mark on a tmpfs this namespace mounted",
        notif_group()
            .and_then(|g| {
                g.mark(
                    fan::FAN_MARK_ADD | fan::FAN_MARK_FILESYSTEM,
                    DIRENT | fan::FAN_MODIFY | fan::FAN_CLOSE_WRITE,
                    None,
                    Some(tmpfs),
                )
            })
            .map(|()| "journal possible on a userns-owned superblock".to_owned())
            .map_err(|e| errno(&e)),
    );
    let Some(dir) = btrfs_dir else {
        return;
    };
    line(
        "filesystem mark on the Btrfs store (init-ns superblock)",
        notif_group()
            .and_then(|g| {
                g.mark(
                    fan::FAN_MARK_ADD | fan::FAN_MARK_FILESYSTEM,
                    DIRENT,
                    None,
                    Some(dir),
                )
            })
            .map(|()| String::new())
            .map_err(|e| errno(&e)),
    );
    line(
        "inode mark on a Btrfs directory (children only)",
        notif_group()
            .and_then(|g| g.mark(fan::FAN_MARK_ADD, DIRENT, None, Some(dir)))
            .map(|()| "non-recursive: one mark per directory".to_owned())
            .map_err(|e| errno(&e)),
    );
    let sub = dir.join(format!(".userns-probe-{}-{pass}", std::process::id()));
    let snap = dir.join(format!(".userns-probe-{}-{pass}-snap", std::process::id()));
    line(
        "Btrfs subvolume create",
        btrfs::create_subvolume(&sub)
            .map(|()| String::new())
            .map_err(|e| errno(&e)),
    );
    line(
        "Btrfs read-only snapshot of an owned subvolume",
        btrfs::snapshot(&sub, &snap, true)
            .map(|()| String::new())
            .map_err(|e| errno(&e)),
    );
    line(
        "Btrfs snapshot delete (falls back to rw + rm + rmdir)",
        btrfs::delete_subvolume(&snap)
            .and_then(|()| btrfs::delete_subvolume(&sub))
            .map(|()| String::new())
            .map_err(|e| errno(&e)),
    );
}

pub fn run(btrfs_dir: Option<&Path>) -> ExitCode {
    println!(
        "unprivileged, no namespace (CAP_SYS_ADMIN: {}):",
        sys::has_capability(sys::CAP_SYS_ADMIN)
    );
    let tmp = std::env::temp_dir().join(format!("tilo-userns-{}", std::process::id()));
    if let Err(e) = fs::create_dir_all(&tmp) {
        println!("cannot create {}: {}", tmp.display(), errno(&e));
        return ExitCode::FAILURE;
    }
    probes(btrfs_dir, &tmp, "host");
    if let Err(e) = enter_userns() {
        println!("cannot enter a user namespace: {}", errno(&e));
        return ExitCode::FAILURE;
    }
    println!("inside a user namespace mapped to root:");
    if let Err(e) = mount_tmpfs(&tmp) {
        println!("  tmpfs mount failed: {}", errno(&e));
    }
    probes(btrfs_dir, &tmp, "userns");
    if let Ok(target) = std::ffi::CString::new(tmp.as_os_str().as_encoded_bytes()) {
        // SAFETY: a valid NUL-terminated path.
        unsafe {
            libc::umount2(target.as_ptr(), libc::MNT_DETACH);
        }
    }
    let _ = fs::remove_dir(&tmp);
    ExitCode::SUCCESS
}
