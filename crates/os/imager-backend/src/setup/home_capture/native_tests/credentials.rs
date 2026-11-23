//! Kernel-observed peer identity for the signed service qualification sockets.
use std::{fs::File, io::Read as _, os::fd::AsRawFd as _, os::unix::net::UnixStream};

pub(super) fn assert_kernel_credentials(stream: &UnixStream, uid: u32, gid: u32, groups: &[u32]) {
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    assert_eq!(
        unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut peer).cast(),
                &raw mut length,
            )
        },
        0
    );
    assert_eq!(length as usize, std::mem::size_of::<libc::ucred>());
    assert_eq!((peer.uid, peer.gid), (uid, gid));
    assert!(peer.pid > 1);
    let mut status = String::new();
    File::open(format!("/proc/{}/status", peer.pid))
        .unwrap()
        .take(16385)
        .read_to_string(&mut status)
        .unwrap();
    assert!(status.len() <= 16384);
    let observed: Vec<u32> = status
        .lines()
        .find_map(|line| line.strip_prefix("Groups:"))
        .expect("actual live peer group list observable")
        .split_whitespace()
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(observed, groups);
    for (name, id) in [("Uid:", uid), ("Gid:", gid)] {
        let ids: Vec<u32> = status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap()
            .split_whitespace()
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(ids, [id; 4], "real/effective/saved/filesystem IDs agree");
    }
    for name in ["CapInh:", "CapPrm:", "CapEff:", "CapAmb:"] {
        let value = status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap()
            .trim();
        assert_eq!(
            u64::from_str_radix(value, 16).unwrap(),
            0,
            "worker retained {name}"
        );
    }
    assert_eq!(
        status
            .lines()
            .find_map(|line| line.strip_prefix("NoNewPrivs:"))
            .unwrap()
            .trim(),
        "1"
    );
    assert_eq!(
        status
            .lines()
            .find_map(|line| line.strip_prefix("Seccomp:"))
            .unwrap()
            .trim(),
        "2"
    );
}
