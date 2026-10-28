//! Overlays of several layers: the top one wins, whiteouts and opaque
//! directories of a layer hide what lies below it, what the job writes goes
//! to its own upper layer only, and read-only layers can't be written.

use std::path::{Path, PathBuf};

use tilo_sandbox::job::{JobSpec, Launch, Mount, MountKind, Root, SpawnError};

use crate::needs_sandbox;
use crate::support::{run, shell};

/// A directory with `files` (path, contents) in it.
fn layer(root: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = root.join(name);
    for (path, contents) in files {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, contents).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An upper and a work directory under `root/name`.
fn writes(root: &Path, name: &str) -> (PathBuf, PathBuf) {
    let (upper, work) = (root.join(name).join("upper"), root.join(name).join("work"));
    std::fs::create_dir_all(&upper).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    (upper, work)
}

/// Whether overlays mount here at all: a job's overlay needs user extended
/// attributes on the filesystem under the test's temporary directory.
fn overlays_work(root: &Path) -> bool {
    let lower = layer(root, "probe-lower", &[]);
    let (upper, work) = writes(root, "probe");
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/true"]);
    spec.mount(Mount::overlay(vec![lower], &upper, &work, "/o"));
    let ran = Launch::new(&spec)
        .spawn()
        .map_err(|e| e.to_string())
        .and_then(|mut j| j.wait().map_err(|e| e.to_string()));
    match ran {
        Ok(s) if s.success() => true,
        other => {
            eprintln!("skipped: no overlays under {}: {other:?}", root.display());
            false
        }
    }
}

#[test]
fn the_top_layer_wins_and_the_job_writes_only_its_own() {
    needs_sandbox!();
    let dir = tempfile::tempdir().unwrap();
    if !overlays_work(dir.path()) {
        return;
    }
    let bottom = layer(
        dir.path(),
        "bottom",
        &[("a", "bottom\n"), ("b", "bottom\n"), ("d/x", "bottom\n")],
    );
    let top = layer(dir.path(), "top", &[("a", "top\n"), ("d/y", "top\n")]);
    let (upper, work) = writes(dir.path(), "job");
    let (status, out) = run(|s| {
        s.mount(Mount::overlay(
            vec![top.clone(), bottom.clone()],
            &upper,
            &work,
            "/o",
        ));
        shell(
            s,
            "cat /o/a /o/b; ls /o/d | tr '\\n' ' '; echo; echo new > /o/new; echo more >> /o/b; rm /o/d/x",
        );
    });
    assert!(status.success(), "{out}");
    assert_eq!(out, "top\nbottom\nx y \n");
    assert_eq!(std::fs::read_to_string(upper.join("new")).unwrap(), "new\n");
    assert_eq!(
        std::fs::read_to_string(upper.join("b")).unwrap(),
        "bottom\nmore\n"
    );
    // The lowers are as they were.
    assert_eq!(
        std::fs::read_to_string(bottom.join("b")).unwrap(),
        "bottom\n"
    );
    assert!(bottom.join("d/x").exists() && !top.join("new").exists());
}

#[test]
fn a_former_upper_is_a_layer_with_its_whiteouts() {
    needs_sandbox!();
    let dir = tempfile::tempdir().unwrap();
    if !overlays_work(dir.path()) {
        return;
    }
    let base = layer(
        dir.path(),
        "base",
        &[("gone", "base\n"), ("kept", "base\n"), ("d/old", "base\n")],
    );
    let (upper, work) = writes(dir.path(), "first");
    let (status, out) = run(|s| {
        s.mount(Mount::overlay(vec![base.clone()], &upper, &work, "/o"));
        // A whiteout for `gone`, an opaque `d` with only `new` in it.
        shell(
            s,
            "rm /o/gone && rm -r /o/d && mkdir /o/d && echo first > /o/d/new",
        );
    });
    assert!(status.success(), "{out}");
    // The first job's upper, stacked on its lower: as the first job left it.
    let (upper2, work2) = writes(dir.path(), "second");
    let (status, out) = run(|s| {
        s.mount(Mount::overlay(
            vec![upper.clone(), base.clone()],
            &upper2,
            &work2,
            "/o",
        ));
        shell(
            s,
            "ls /o | tr '\\n' ' '; echo; ls /o/d | tr '\\n' ' '; echo; echo second > /o/kept",
        );
    });
    assert!(status.success(), "{out}");
    assert_eq!(out, "d kept \nnew \n");
    assert_eq!(
        std::fs::read_to_string(base.join("kept")).unwrap(),
        "base\n"
    );
    // Read-only, the same view, and no writing.
    let (status, out) = run(|s| {
        s.mount(Mount {
            dst: "/o".into(),
            kind: MountKind::Layers {
                lowers: vec![upper.clone(), base.clone()],
            },
        });
        shell(
            s,
            "ls /o | tr '\\n' ' '; echo; cat /o/kept; echo x > /o/kept && echo wrote",
        );
    });
    assert!(!status.success(), "{out}");
    assert!(out.starts_with("d kept \nbase\n"), "{out}");
    assert!(!out.contains("wrote"), "{out}");
}

#[test]
fn one_read_only_layer_is_refused() {
    needs_sandbox!();
    let dir = tempfile::tempdir().unwrap();
    let only = layer(dir.path(), "only", &[]);
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/true"]);
    spec.mount(Mount {
        dst: "/o".into(),
        kind: MountKind::Layers { lowers: vec![only] },
    });
    let err = Launch::new(&spec).spawn().err();
    assert!(matches!(err, Some(SpawnError::Invalid(_))), "{err:?}");
}

/// An image's layers as a job's root: the top layer's file hides the lower's, the mount points
/// the job needs go into its own upper, and the root itself can't be written.
#[test]
fn a_root_of_layers_shows_the_top_layer_and_cant_be_written() {
    needs_sandbox!();
    let dir = tempfile::tempdir().unwrap();
    if !overlays_work(dir.path()) {
        return;
    }
    // The host's /usr is bound in for a shell; the base layer links into it as a merged-/usr
    // image does.
    let base = layer(
        dir.path(),
        "base",
        &[("etc/hello", "base\n"), ("etc/only-base", "x\n")],
    );
    for (link, to) in [
        ("bin", "usr/bin"),
        ("lib", "usr/lib"),
        ("lib64", "usr/lib64"),
        ("sbin", "usr/sbin"),
    ] {
        std::os::unix::fs::symlink(to, base.join(link)).unwrap();
    }
    let top = layer(dir.path(), "top", &[("etc/hello", "top\n")]);
    let (upper, work) = writes(dir.path(), "root");
    let mut spec = JobSpec::new(
        Root::Layers {
            lowers: vec![top.clone(), base.clone()],
            upper: upper.clone(),
            work,
        },
        ["/bin/sh"],
    );
    spec.mount(Mount::read_only("/usr", "/usr"));
    shell(
        &mut spec,
        // The refused redirection's own message goes to the subshell's stderr, which is
        // silenced: `cmd > /new 2>/dev/null` would print it, since `> /new` fails first.
        "cat /etc/hello /etc/only-base; if (echo x > /new) 2>/dev/null; then echo wrote; fi",
    );
    let (mut out_r, out_w) = crate::support::pipe();
    let err_w = out_w.try_clone().unwrap();
    let mut job = Launch::new(&spec)
        .stdout(out_w)
        .stderr(err_w)
        .spawn()
        .expect("start the job");
    let status = job.wait().unwrap();
    let mut out = String::new();
    std::io::Read::read_to_string(&mut out_r, &mut out).unwrap();
    assert!(status.success(), "{out}");
    assert_eq!(out, "top\nx\n");
    assert!(
        upper.join("usr").is_dir(),
        "the mount point is the job's own"
    );
    assert!(!top.join("usr").exists() && !base.join("usr").exists());
    assert!(!upper.join("new").exists());
}
