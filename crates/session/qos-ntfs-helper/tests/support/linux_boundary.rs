use qos_ntfs_helper as _;
// Uninstalled Linux-only opt-in tests. Coordinator supplies an already disposable block fixture.
use std::{
    fs::OpenOptions,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{FileTypeExt, OpenOptionsExt},
    },
    process::Command,
};
unsafe extern "C" {
    fn qos_child_prepare(
        input: i32,
        output: i32,
        uid: u32,
        gid: u32,
        production: i32,
        expected_parent: i32,
    ) -> i32;
}
fn authorized() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("QOS_NTFS_NATIVE_AUTHORIZATION")?.as_str() != "DISPOSABLE_FIXTURE_ONLY"
        || unsafe { libc::geteuid() } != 0
    {
        return Err("explicit disposable native Linux root opt-in required".into());
    }
    Ok(())
}
#[test]
#[ignore = "native Linux only; coordinator must already mount the disposable selected NTFS fixture through an alias"]
fn production_claim_busy_mounted_alias() {
    authorized().unwrap();
    let id = std::env::var("QOS_NTFS_NATIVE_INVENTORY_ID").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_qos-ntfs-probe"))
        .args(["inspect-selected", "--inventory-id", &id, "--root-mft", "5"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Busy"),
        "expected kernel claim refusal: {out:?}"
    );
}
#[test]
#[ignore = "native Linux root only; coordinator supplies unmounted disposable block device and dedicated qos-ntfs account"]
fn production_fd_readonly_and_reader_sandbox() {
    authorized().unwrap();
    let path = std::env::var("QOS_NTFS_NATIVE_DEVICE").unwrap();
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_EXCL)
        .open(path)
        .unwrap();
    assert!(file.metadata().unwrap().file_type().is_block_device());
    let pwd = unsafe { libc::getpwnam(c"qos-ntfs".as_ptr()) };
    assert!(!pwd.is_null());
    let (uid, gid) = unsafe { ((*pwd).pw_uid, (*pwd).pw_gid) };
    assert_ne!(uid, 0);
    assert_ne!(gid, 0);
    for attack in 0..5 {
        let mut pipes = [-1; 2];
        assert_eq!(unsafe { libc::pipe(pipes.as_mut_ptr()) }, 0);
        let [read_fd, write_fd] = pipes;
        let read_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(read_fd) };
        let write_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(write_fd) };
        let parent = unsafe { libc::getpid() };
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            if unsafe {
                qos_child_prepare(file.as_raw_fd(), write_fd.as_raw_fd(), uid, gid, 1, parent)
            } != 0
            {
                unsafe { libc::_exit(120) }
            }
            unsafe {
                match attack {
                    0 => {
                        if libc::fcntl(3, libc::F_GETFL) & libc::O_ACCMODE != libc::O_RDONLY {
                            libc::_exit(121);
                        }
                    }
                    1 => {
                        libc::pwrite(3, c"x".as_ptr().cast(), 1, 0);
                    }
                    2 => {
                        libc::open(c"/etc/passwd".as_ptr(), libc::O_RDONLY);
                    }
                    3 => {
                        libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
                    }
                    _ => {
                        let p = libc::mmap(
                            std::ptr::null_mut(),
                            300 * 1024 * 1024,
                            libc::PROT_READ | libc::PROT_WRITE,
                            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                            -1,
                            0,
                        );
                        if p != libc::MAP_FAILED {
                            libc::_exit(122);
                        }
                    }
                }
                libc::_exit(0);
            }
        }
        drop(read_fd);
        drop(write_fd);
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
        if attack == 0 || attack == 4 {
            assert!(libc::WIFEXITED(status));
            assert_eq!(libc::WEXITSTATUS(status), 0);
        } else {
            assert!(libc::WIFSIGNALED(status));
            assert_eq!(libc::WTERMSIG(status), libc::SIGSYS);
        }
    }
}
