//! Helpers: a fresh mount namespace holding none of the caller's mounts, born in the given
//! cgroup, in the given network namespace. They run as the caller in the initial user
//! namespace, so these tests need root; elsewhere each says why it skipped.

use crate::support::{TestCgroup, is_root, reap, start_in_child, status_file};

/// A cgroup for a helper, or why the test skips.
fn ready() -> Result<TestCgroup, String> {
    if !is_root() {
        return Err("helpers need root".into());
    }
    TestCgroup::new()
}

macro_rules! needs_root {
    () => {
        match ready() {
            Ok(cg) => cg,
            Err(why) => {
                eprintln!("skipped: {why}");
                return;
            }
        }
    };
}

/// Run `script` as a helper of `kind`; its output.
fn run(cg: &TestCgroup, dir: &std::path::Path, kind: &str, script: &str) -> String {
    std::fs::write(dir.join("kind"), kind).unwrap();
    std::fs::write(dir.join("script"), script).unwrap();
    std::fs::write(dir.join("cgroup"), cg.path.as_os_str().as_encoded_bytes()).unwrap();
    let pid = start_in_child(dir);
    reap(pid);
    let st = status_file(&dir.join("status")).expect("the helper's status");
    let out = std::fs::read_to_string(dir.join("out")).unwrap();
    assert!(
        libc::WIFEXITED(st) && libc::WEXITSTATUS(st) == 0,
        "{st:#x}: {out}"
    );
    out
}

#[test]
fn a_helper_sees_none_of_the_callers_mounts() {
    let cg = needs_root!();
    let dir = tempfile::tempdir().unwrap();
    let out = run(&cg, dir.path(), "helper-probe", "cat /proc/self/mountinfo");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("probe-seen")).unwrap(),
        "true",
        "the caller's probe mount was not made"
    );
    assert!(!out.contains(" /tmp/t-probe "), "{out}");
    // Nothing of the caller's tree but the system directories and what was bound.
    for line in out.lines() {
        let at = line.split(' ').nth(4).unwrap_or("");
        let expected = [
            "/", "/usr", "/etc", "/bin", "/sbin", "/lib", "/proc", "/dev", "/tmp",
        ]
        .iter()
        .any(|p| at == *p || (*p != "/" && at.starts_with(&format!("{p}/"))));
        assert!(expected, "the helper sees {at}: {line}");
    }
}

#[test]
fn a_helper_runs_in_the_given_cgroup() {
    let cg = needs_root!();
    let dir = tempfile::tempdir().unwrap();
    let out = run(&cg, dir.path(), "helper", "cat /proc/self/cgroup");
    let name = cg.path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(out.trim_end().ends_with(&name), "{out}");
}

#[test]
fn a_helper_joins_the_given_netns() {
    let cg = needs_root!();
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        &cg,
        dir.path(),
        "helper-netns",
        "readlink /proc/self/ns/net",
    );
    let made = std::fs::read_to_string(dir.path().join("netns")).unwrap();
    let own = std::fs::read_link("/proc/self/ns/net").unwrap();
    assert_eq!(out.trim(), made.trim());
    assert_ne!(out.trim(), own.to_string_lossy());
}
