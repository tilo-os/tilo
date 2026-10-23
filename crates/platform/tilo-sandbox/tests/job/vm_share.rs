//! VM steps over virtio-fs: a root made of shared layers, each layer looked at before the
//! overlay takes it, a writable share the host reads, the step's helpers in its cgroup and
//! holding none of the caller's mounts, and the command run by its path. Needs what
//! `vm.rs` needs; without it each test says why it skipped.

use std::os::fd::OwnedFd;
use std::path::Path;
use std::time::{Duration, Instant};

use tilo_sandbox::job::Mount;
use tilo_sandbox::job::helper::{self, Helper};
use tilo_sandbox::job::vm;

use crate::needs_vm;
use crate::vm::wait_for;

fn layer(at: &Path, files: &[(&str, &str)]) {
    for (path, contents) in files {
        let f = at.join(path);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, contents).unwrap();
    }
    std::fs::create_dir_all(at).unwrap();
}

#[test]
fn a_step_boots_on_a_root_made_of_shared_layers() {
    let rig = needs_vm!();
    let lower = rig.busybox_root("lower");
    layer(&lower, &[("etc/os-release", "NAME=lower\n")]);
    let upper = rig.path("upper");
    layer(&upper, &[("bin/x", "x\n")]);
    let (share, _) = rig.share("inputs", &[("upper", &upper), ("lower", &lower)], true);
    let spec = rig.spec(
        share,
        &["upper", "lower"],
        &[
            "/bin/busybox",
            "sh",
            "-c",
            "/bin/busybox cat /etc/os-release; test -f /bin/x && echo has-x",
        ],
    );
    let (qemu, output, prepared) = rig.run(&spec);
    assert!(qemu.success(), "{output}");
    assert_eq!(prepared.finish(None).unwrap().code, Some(0), "{output}");
    assert!(output.contains("NAME=lower\n"), "{output}");
    assert!(output.contains("has-x\n"), "{output}");
}

#[test]
fn each_layer_is_touched_before_the_overlay_mounts_it() {
    let rig = needs_vm!();
    let base = rig.busybox_root("base");
    let mut dirs = vec![("base".to_owned(), base)];
    for n in 1..8 {
        let at = rig.path(&format!("l{n}"));
        layer(&at, &[(&format!("l{n}"), &format!("{n}\n"))]);
        dirs.push((format!("l{n}"), at));
    }
    dirs.reverse();
    let named: Vec<(&str, &Path)> = dirs
        .iter()
        .map(|(n, p)| (n.as_str(), p.as_path()))
        .collect();
    let (share, _) = rig.share("inputs", &named, true);
    let root: Vec<&str> = named.iter().map(|(n, _)| *n).collect();
    let spec = rig.spec(share, &root, &["sh", "-c", "cat /l1 /l7"]);
    let (qemu, output, prepared) = rig.run(&spec);
    assert!(qemu.success(), "{output}");
    assert_eq!(prepared.finish(None).unwrap().code, Some(0), "{output}");
    assert!(output.contains("1\n7\n"), "{output}");
}

#[test]
fn a_writable_share_shows_the_steps_writes_on_the_host() {
    let rig = needs_vm!();
    let root = rig.busybox_root("root");
    let target = rig.path("target");
    std::fs::create_dir_all(&target).unwrap();
    let (share, _) = rig.share("inputs", &[("root", &root)], true);
    let (writes, _) = rig.share("writes", &[("target", &target)], false);
    let mut spec = rig.spec(share, &["root"], &["sh", "-c", "echo hi > /target/x"]);
    spec.writable_share = Some(writes);
    spec.writable = vec![("target".into(), "/target".into())];
    let (qemu, output, prepared) = rig.run(&spec);
    assert!(qemu.success(), "{output}");
    assert_eq!(prepared.finish(None).unwrap().code, Some(0), "{output}");
    assert_eq!(std::fs::read_to_string(target.join("x")).unwrap(), "hi\n");
}

#[test]
fn virtiofsd_and_passt_run_in_the_steps_cgroup() {
    let rig = needs_vm!();
    let Some(passt) = ["/usr/bin/passt", "/usr/local/bin/passt"]
        .iter()
        .map(Path::new)
        .find(|p| p.is_file())
    else {
        eprintln!("skipped: no passt");
        return;
    };
    let root = rig.busybox_root("root");
    let (share, _) = rig.share("inputs", &[("root", &root)], true);
    let net = rig.path("net");
    std::fs::create_dir_all(&net).unwrap();
    let mut argv = vm::passt_argv(Path::new("/net/passt.sock"), false);
    argv[0] = passt.into();
    let status: OwnedFd = std::fs::File::create(rig.path("passt.status"))
        .unwrap()
        .into();
    helper::spawn(Helper {
        argv,
        env: vec![],
        binds: vec![Mount::read_write(&net, "/net")],
        devices: vec![],
        cgroup: rig.cg.fd(),
        netns: None,
        status,
        stdout: None,
        stderr: None,
        nofile_max: false,
    })
    .expect("start passt");
    wait_for(&net.join("passt.sock"), "passt's socket");
    let mut spec = rig.spec(share, &["root"], &["sh", "-c", "sleep 5"]);
    spec.network = Some(net.join("passt.sock"));
    let (mut job, _out, _prepared) = rig.start(&spec);
    let t0 = Instant::now();
    let wanted = ["qemu", "virtiofsd", "passt"];
    let mut seen = Vec::new();
    while t0.elapsed() < Duration::from_secs(4) {
        seen = rig
            .cg
            .procs()
            .iter()
            .filter_map(|p| std::fs::read_to_string(format!("/proc/{p}/comm")).ok())
            .collect();
        if wanted.iter().all(|w| seen.iter().any(|c| c.starts_with(w))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    job.kill();
    let _ = job.wait();
    for w in wanted {
        assert!(
            seen.iter().any(|c| c.starts_with(w)),
            "no {w} in the step's cgroup: {seen:?}"
        );
    }
}

#[test]
fn a_helper_sees_none_of_the_callers_mounts() {
    let rig = needs_vm!();
    let root = rig.busybox_root("root");
    let (_, pid) = rig.share("inputs", &[("root", &root)], true);
    let mounts = std::fs::read_to_string(format!("/proc/{pid}/mountinfo")).unwrap();
    for line in mounts.lines() {
        let at = line.split(' ').nth(4).unwrap_or("");
        let ours = [
            "/usr", "/etc", "/bin", "/sbin", "/lib", "/proc", "/dev", "/tmp", "/share", "/sock",
        ]
        .iter()
        .any(|p| at == *p || at.starts_with(&format!("{p}/")));
        assert!(at == "/" || ours, "virtiofsd holds {at}: {line}");
    }
    assert!(
        !mounts.contains(&rig.dir.path().to_string_lossy().into_owned()),
        "{mounts}"
    );
}

#[test]
fn the_guest_runs_its_command_by_path() {
    let rig = needs_vm!();
    let base = rig.busybox_root("base");
    // A `sh` of the step's own above busybox's link: run by its path, it is this script; a
    // busybox looking up a bare `sh` would run its own applet instead.
    let top = rig.path("top");
    layer(&top, &[("bin/sh", "#!/bin/busybox sh\necho by-path\n")]);
    std::fs::set_permissions(
        top.join("bin/sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let (share, _) = rig.share("inputs", &[("top", &top), ("base", &base)], true);
    let spec = rig.spec(share, &["top", "base"], &["sh", "-c", "echo ok"]);
    let (qemu, output, prepared) = rig.run(&spec);
    assert!(qemu.success(), "{output}");
    assert_eq!(prepared.finish(None).unwrap().code, Some(0), "{output}");
    assert!(output.contains("by-path\n"), "{output}");
    assert!(!output.contains("ok\n"), "{output}");
}
