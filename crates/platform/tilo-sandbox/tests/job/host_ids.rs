//! A job with host ids of its own (`IdBlock`): it runs as a host user no
//! other job is, while the files it is given stay their owner's. Root only
//! (mapping a block and idmapping mounts take it); elsewhere each test says
//! why it skipped and passes.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use tilo_sandbox::job::{IdBlock, JobSpec, Launch, Mount, MountKind, Root};

use crate::needs_sandbox;
use crate::support::{pipe, run, shell};

/// The files' owner on the host: not the test's own uid, so a write that
/// keeps it was translated, not just made as root.
const OWNER: u32 = 4242;

/// A block far from any user on a test machine.
const BLOCK: IdBlock = IdBlock {
    uid: 0x7100_0000,
    gid: 0x7100_0000,
    count: 65536,
};

macro_rules! needs_root {
    () => {
        needs_sandbox!();
        if !rustix::process::geteuid().is_root() {
            eprintln!("skipped: host ids of a job's own need root");
            return;
        }
    };
}

/// A directory of `OWNER`'s, with one file of theirs in it.
fn owned_dir(root: &Path, name: &str) -> std::path::PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("theirs"), "theirs\n").unwrap();
    for p in [&dir, &dir.join("theirs")] {
        std::os::unix::fs::chown(p, Some(OWNER), Some(OWNER)).unwrap();
    }
    dir
}

fn with_block(s: &mut JobSpec) {
    s.ids.outside_uid = OWNER;
    s.ids.outside_gid = OWNER;
    s.ids.block = Some(BLOCK);
}

fn owner(p: &Path) -> (u32, u32) {
    let m = std::fs::metadata(p).unwrap();
    (m.uid(), m.gid())
}

#[test]
fn a_job_runs_as_a_host_user_of_its_own_and_writes_as_the_files_owner() {
    needs_root!();
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let work = owned_dir(dir.path(), "work");
    let (status, out) = run(|s| {
        with_block(s);
        s.mount(Mount::read_write(&work, "/work"));
        shell(
            s,
            "id -u; id -g; id -G; cat /proc/self/uid_map; stat -c '%u:%g' /work/theirs; \
             echo mine > /work/mine && mkdir /work/d && cat /work/theirs",
        );
    });
    assert!(status.success(), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.first(), Some(&"0"), "{out}");
    assert_eq!(lines.get(1), Some(&"0"), "{out}");
    assert_eq!(lines.get(2), Some(&"0"), "kept the worker's groups: {out}");
    let map: Vec<&str> = lines.get(3).unwrap_or(&"").split_whitespace().collect();
    assert_eq!(map, ["0", &BLOCK.uid.to_string(), "65536"], "{out}");
    assert_eq!(
        lines.get(4),
        Some(&"0:0"),
        "the owner's file isn't the job's: {out}"
    );
    assert!(out.contains("theirs"), "{out}");
    assert_eq!(owner(&work.join("mine")), (OWNER, OWNER));
    assert_eq!(owner(&work.join("d")), (OWNER, OWNER));
}

#[test]
fn the_jobs_processes_are_its_blocks_user_on_the_host() {
    needs_root!();
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/sleep", "5"]);
    with_block(&mut spec);
    let mut job = Launch::new(&spec).spawn().expect("start the job");
    let status = std::fs::read_to_string(format!("/proc/{}/status", job.pid())).unwrap();
    job.kill();
    let _ = job.wait();
    let uid = status.lines().find(|l| l.starts_with("Uid:")).unwrap();
    let ids: Vec<&str> = uid.split_whitespace().skip(1).collect();
    let want = BLOCK.uid.to_string();
    assert!(ids.len() == 4 && ids.iter().all(|i| *i == want), "{uid}");
}

#[test]
fn a_nested_job_with_a_block_writes_as_the_files_owner() {
    needs_root!();
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let work = owned_dir(dir.path(), "work");
    let tmp = owned_dir(dir.path(), "tmp");
    let (status, out) = run(|s| {
        s.allow_nested_sandboxes();
        with_block(s);
        s.tmp = tilo_sandbox::job::Tmp::Dir(tmp.clone());
        s.mount(Mount::read_write(&work, "/work"));
        shell(
            s,
            "id -u; stat -c %u /work/theirs; echo mine > /work/mine; echo t > /tmp/t; \
             unshare --user --map-root-user --mount sh -c 'mount -t tmpfs none /tmp && echo nested' 2>&1",
        );
    });
    assert!(status.success(), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.first(), Some(&"1000"), "{out}");
    assert_eq!(lines.get(1), Some(&"1000"), "{out}");
    assert!(out.contains("nested"), "{out}");
    assert_eq!(owner(&work.join("mine")), (OWNER, OWNER));
    assert_eq!(owner(&tmp.join("t")), (OWNER, OWNER));
}

#[test]
fn an_overlay_with_a_block_keeps_the_owner_on_both_layers() {
    needs_root!();
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let lower = owned_dir(dir.path(), "lower");
    let layer = owned_dir(dir.path(), "layer");
    let (upper, work) = (layer.join("upper"), layer.join("work"));
    for d in [&upper, &work] {
        std::fs::create_dir(d).unwrap();
        std::os::unix::fs::chown(d, Some(OWNER), Some(OWNER)).unwrap();
    }
    let (status, out) = run(|s| {
        with_block(s);
        s.mount(Mount {
            dst: "/o".into(),
            kind: MountKind::Overlay {
                lower: lower.clone(),
                upper: upper.clone(),
                work: work.clone(),
            },
        });
        shell(
            s,
            "stat -c %u /o/theirs; echo more >> /o/theirs; echo mine > /o/mine",
        );
    });
    assert!(status.success(), "{out}");
    assert_eq!(out.lines().next(), Some("0"), "{out}");
    assert_eq!(owner(&upper.join("mine")), (OWNER, OWNER));
    assert_eq!(owner(&upper.join("theirs")), (OWNER, OWNER));
    assert_eq!(
        std::fs::read_to_string(lower.join("theirs")).unwrap(),
        "theirs\n"
    );
}

#[test]
fn ids_outside_the_block_are_refused() {
    needs_root!();
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/true"]);
    with_block(&mut spec);
    spec.ids.inside_uid = BLOCK.count;
    let (_r, w) = pipe();
    let err = Launch::new(&spec).stdout(w).spawn().err();
    assert!(
        matches!(err, Some(tilo_sandbox::job::SpawnError::Invalid(_))),
        "{err:?}"
    );
}

#[test]
fn a_job_with_a_block_opens_its_output_pipe_by_path() {
    needs_root!();
    // QEMU opens its console at /dev/stdout, and scripts write to /dev/stderr: both reopen the
    // worker's pipe, which must be the job's.
    let (status, out) = run(|s| {
        with_block(s);
        shell(
            s,
            "echo to-stdout > /dev/stdout && echo to-stderr > /dev/stderr",
        );
    });
    assert!(status.success(), "{out}");
    assert!(
        out.contains("to-stdout") && out.contains("to-stderr"),
        "{out}"
    );
}

#[test]
fn a_job_with_a_block_connects_to_a_socket_made_for_it() {
    needs_root!();
    if !Path::new("/usr/bin/python3").exists() {
        eprintln!("skipped: no python3 to connect with");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    // As passt makes its socket: by root, mode 0755 under the umask, so only its owner may
    // connect.
    let sock = dir.path().join("sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o755)).unwrap();
    let accept = std::thread::spawn(move || {
        use std::io::Write as _;
        let (mut c, _) = listener.accept().unwrap();
        c.write_all(b"hello\n").unwrap();
    });
    let (status, out) = run(|s| {
        with_block(s);
        s.mount(Mount::read_write(&sock, "/run/sock"));
        shell(
            s,
            "python3 -c 'import socket; s=socket.socket(socket.AF_UNIX); s.connect(\"/run/sock\"); print(s.recv(6).decode(), end=\"\")'",
        );
    });
    accept.join().unwrap();
    assert!(status.success() && out.contains("hello"), "{out}");
}

#[test]
fn a_writable_path_that_cannot_be_idmapped_is_bound_and_named() {
    needs_root!();
    // devtmpfs takes no idmapped mounts.
    let dir = Path::new("/dev").join(format!("job-ids-test-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("theirs"), "theirs\n").unwrap();
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/cat", "/d/theirs"]);
    with_block(&mut spec);
    spec.mount(Mount::read_write(&dir, "/d"));
    let (mut out_r, out_w) = pipe();
    let started = Launch::new(&spec).stdout(out_w).spawn();
    let result = started.map(|mut job| {
        let unmapped = job.unmapped().to_vec();
        let status = job.wait().unwrap();
        let mut out = String::new();
        std::io::Read::read_to_string(&mut out_r, &mut out).unwrap();
        (unmapped, status, out)
    });
    std::fs::remove_dir_all(&dir).unwrap();
    let (unmapped, status, out) = result.expect("the job starts");
    assert_eq!(unmapped, std::slice::from_ref(&dir));
    assert!(status.success() && out == "theirs\n", "{out}");
}
