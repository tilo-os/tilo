//! Micro-VM steps, end to end: a guest boots from a root image made without privileges, runs a
//! command as root (a loop mount, here), streams its output and returns its exit code and
//! outputs. Needs `/dev/kvm`, QEMU, `mkfs.ext4` and a guest: `TILO_VM_GUEST` naming a
//! directory with `kernel` and `busybox` (the outputs of t's `vm-guest-<arch>` check). Without
//! them each test says why it skipped and passes.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use tilo_sandbox::job::Launch;
use tilo_sandbox::job::vm::{self, Disk, DiskMount, Guest, VmSpec};

use crate::needs_sandbox;
use crate::support::pipe;

fn guest() -> Result<Guest, String> {
    let dir = std::env::var_os("TILO_VM_GUEST").ok_or("TILO_VM_GUEST is not set")?;
    let dir = PathBuf::from(dir);
    let guest = Guest {
        kernel: dir.join("kernel"),
        busybox: dir.join("busybox"),
    };
    if !guest.kernel.is_file() || !guest.busybox.is_file() {
        return Err(format!("{} has no kernel and busybox", dir.display()));
    }
    if std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_err()
    {
        return Err("no usable /dev/kvm".into());
    }
    Ok(guest)
}

/// A root with busybox as its every tool.
fn busybox_root(guest: &Guest, at: &Path) {
    std::fs::create_dir_all(at.join("bin")).unwrap();
    std::fs::copy(&guest.busybox, at.join("bin/busybox")).unwrap();
    for tool in [
        "sh", "mount", "umount", "losetup", "cp", "cat", "echo", "mkdir", "dd", "id", "uname",
    ] {
        std::os::unix::fs::symlink("busybox", at.join("bin").join(tool)).unwrap();
    }
    for d in ["etc", "tmp", "var", "src", "target"] {
        std::fs::create_dir_all(at.join(d)).unwrap();
    }
}

#[test]
fn a_step_runs_as_root_in_its_vm_and_brings_back_its_outputs() {
    needs_sandbox!();
    let guest = match guest() {
        Ok(g) => g,
        Err(why) => {
            eprintln!("skipped: {why}");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    busybox_root(&guest, &tree);
    let root = dir.path().join("root.img");
    let t0 = Instant::now();
    vm::make_image(&tree, &root).expect("make the root image");
    eprintln!("root image: {:?}", t0.elapsed());

    let inputs = dir.path().join("inputs");
    std::fs::create_dir_all(inputs.join("src")).unwrap();
    std::fs::write(inputs.join("src/hello.txt"), "from the host\n").unwrap();
    // A filesystem for the guest to loop-mount: made here, as busybox builds differ in which
    // mkfs they carry.
    let fs = std::fs::File::create(inputs.join("src/disk.img")).unwrap();
    fs.set_len(16 << 20).unwrap();
    drop(fs);
    let made = std::process::Command::new("mkfs.ext4")
        .args(["-q", "-F"])
        .arg(inputs.join("src/disk.img"))
        .status()
        .unwrap();
    assert!(made.success());
    let inputs_img = dir.path().join("inputs.img");
    vm::make_image(&inputs, &inputs_img).expect("make the inputs image");

    let work = dir.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let script = "set -e; id -u; cat /src/hello.txt; echo changed > /src/hello.txt; \
        cp /src/disk.img /tmp/disk; \
        loop=$(losetup -f); losetup $loop /tmp/disk; mkdir -p /mnt/loop; mount $loop /mnt/loop; \
        echo looped > /mnt/loop/file; cat /mnt/loop/file; umount /mnt/loop; losetup -d $loop; \
        mkdir -p /target/out; echo built > /target/out/app; echo no > /target/other; exit 7";
    let spec = VmSpec {
        guest,
        root,
        disks: vec![Disk {
            image: inputs_img,
            mounts: vec![DiskMount {
                path: "src".into(),
                dst: "/src".into(),
                writable: true,
            }],
        }],
        scratch: 1 << 30,
        output: 16 << 20,
        argv: vec!["sh".into(), "-c".into(), script.into()],
        env: vec![("PATH".into(), "/bin".into())],
        cwd: "/src".into(),
        hostname: "step".into(),
        outputs: vec!["/target/out/*".into()],
        cpus: 1,
        memory: 256 << 20,
        kvm: false,
        network: None,
        qemu: None,
    };
    let prepared = vm::prepare(&spec, &work).expect("prepare the step");
    let (mut out_r, out_w) = pipe();
    let err_w = out_w.try_clone().unwrap();
    let t0 = Instant::now();
    let mut job = Launch::new(&prepared.job)
        .stdout(out_w)
        .stderr(err_w)
        .spawn()
        .expect("start QEMU");
    let mut output = String::new();
    out_r.read_to_string(&mut output).unwrap();
    let qemu = job.wait().unwrap();
    eprintln!("step: {:?}\n{output}", t0.elapsed());
    assert!(qemu.success(), "QEMU ended with {qemu:?}: {output}");
    let outputs = dir.path().join("outputs");
    std::fs::create_dir(&outputs).unwrap();
    let status = prepared.finish(Some(&outputs)).expect("the step's status");
    assert_eq!(status.code, Some(7), "{output}");
    assert!(output.contains("0\nfrom the host\n"), "{output}");
    assert!(output.contains("looped\n"), "{output}");
    assert_eq!(
        std::fs::read_to_string(outputs.join("target/out/app")).unwrap(),
        "built\n"
    );
    assert!(!outputs.join("target/other").exists());
    // The guest's changes to its sources stayed in the guest.
    assert_eq!(
        std::fs::read_to_string(inputs.join("src/hello.txt")).unwrap(),
        "from the host\n"
    );
}
