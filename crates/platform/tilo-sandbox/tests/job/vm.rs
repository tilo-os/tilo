//! Micro-VM steps, end to end: a guest boots from layers shared over virtio-fs, runs a command
//! as root (a loop mount, here), streams its output and returns its exit code and outputs.
//! Needs root (its virtiofsd runs as a helper), a delegated cgroup, `/dev/kvm`, QEMU,
//! virtiofsd, `mkfs.ext4` and a guest: `TILO_VM_GUEST` naming a directory with `kernel` and
//! `busybox` (the outputs of t's `vm-guest-<arch>` check). Without them each test says why it
//! skipped and passes.

use std::io::Read;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tilo_sandbox::job::helper::{self, Helper};
use tilo_sandbox::job::vm::{self, Guest, Prepared, Share, ShareMount, VmRoot, VmSpec};
use tilo_sandbox::job::{ExitStatus, Job, Launch, Mount};

use crate::support::{TestCgroup, is_root, pipe};

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

/// The first of `names` that exists, or the one `var` names.
fn program(var: &str, names: &[&str]) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .into_iter()
        .chain(names.iter().map(PathBuf::from))
        .find(|p| p.is_file())
}

/// What every step test needs: a guest, a cgroup for the step's QEMU and helpers, virtiofsd.
pub struct Rig {
    pub dir: tempfile::TempDir,
    pub cg: TestCgroup,
    pub guest: Guest,
    virtiofsd: PathBuf,
}

impl Rig {
    pub fn new() -> Result<Self, String> {
        let guest = guest()?;
        if !is_root() {
            return Err("a step's virtiofsd runs as a helper, which needs root".into());
        }
        let virtiofsd = program(
            "TILO_VIRTIOFSD",
            &[
                "/usr/libexec/virtiofsd",
                "/usr/lib/qemu/virtiofsd",
                "/usr/bin/virtiofsd",
            ],
        )
        .ok_or("no virtiofsd")?;
        Ok(Self {
            dir: tempfile::tempdir().map_err(|e| e.to_string())?,
            cg: TestCgroup::new()?,
            guest,
            virtiofsd,
        })
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// A busybox root at `rel`: busybox and its applets the tests use, and the directories
    /// steps mount on.
    pub fn busybox_root(&self, rel: &str) -> PathBuf {
        let at = self.path(rel);
        std::fs::create_dir_all(at.join("bin")).unwrap();
        std::fs::copy(&self.guest.busybox, at.join("bin/busybox")).unwrap();
        for tool in [
            "sh", "mount", "umount", "losetup", "cp", "cat", "echo", "mkdir", "dd", "id", "uname",
            "test", "sleep", "nc", "ls",
        ] {
            std::os::unix::fs::symlink("busybox", at.join("bin").join(tool)).unwrap();
        }
        for d in ["etc", "tmp", "var", "src", "target", "run"] {
            std::fs::create_dir_all(at.join(d)).unwrap();
        }
        at
    }

    /// Serve `dirs` (name, host directory) as a share tagged `tag`: a virtiofsd helper in the
    /// rig's cgroup, each directory a mount of its own under the shared directory.
    pub fn share<P: AsRef<Path>>(
        &self,
        tag: &str,
        dirs: &[(&str, P)],
        readonly: bool,
    ) -> (Share, i32) {
        let sockets = self.path(&format!("sock-{tag}"));
        std::fs::create_dir_all(&sockets).unwrap();
        let mut binds: Vec<Mount> = dirs
            .iter()
            .map(|(name, dir)| {
                let at = Path::new("/share").join(name);
                if readonly {
                    Mount::read_only(dir.as_ref(), at)
                } else {
                    Mount::read_write(dir.as_ref(), at)
                }
            })
            .collect();
        binds.push(Mount::read_write(&sockets, "/sock"));
        let mut argv =
            vm::virtiofsd_argv(Path::new("/share"), Path::new("/sock/fs.sock"), readonly);
        argv[0] = self.virtiofsd.clone().into();
        let status: OwnedFd = std::fs::File::create(self.path(&format!("virtiofsd-{tag}.status")))
            .unwrap()
            .into();
        let pid = helper::spawn(Helper {
            argv,
            env: vec![],
            binds,
            devices: vec![],
            cgroup: self.cg.fd(),
            netns: None,
            status,
            stdout: None,
            stderr: None,
            nofile_max: true,
        })
        .expect("start virtiofsd");
        let socket = sockets.join("fs.sock");
        wait_for(&socket, "virtiofsd's socket");
        (
            Share {
                socket,
                tag: tag.into(),
            },
            pid,
        )
    }

    /// A step with `root` layers (top first) of `share`, running `argv`.
    pub fn spec(&self, share: Share, root: &[&str], argv: &[&str]) -> VmSpec {
        VmSpec {
            guest: self.guest.clone(),
            root: VmRoot::Share {
                layers: root.iter().map(|s| (*s).to_owned()).collect(),
            },
            share,
            inputs: vec![],
            writable_share: None,
            writable: vec![],
            scratch: 1 << 30,
            output: 16 << 20,
            argv: argv.iter().map(|a| (*a).into()).collect(),
            env: vec![("PATH".into(), "/bin".into())],
            cwd: "/".into(),
            hostname: "step".into(),
            outputs: vec![],
            cpus: 1,
            memory: 256 << 20,
            kvm: false,
            network: None,
            qemu: None,
        }
    }

    /// Start `spec`'s QEMU in the rig's cgroup.
    pub fn start(&self, spec: &VmSpec) -> (Job, std::fs::File, Prepared) {
        let work = self.path("work");
        std::fs::create_dir_all(&work).unwrap();
        let prepared = vm::prepare(spec, &work).expect("prepare the step");
        let (out_r, out_w) = pipe();
        let err_w = out_w.try_clone().unwrap();
        let job = Launch::new(&prepared.job)
            .cgroup_fd(self.cg.fd())
            .stdout(out_w)
            .stderr(err_w)
            .spawn()
            .expect("start QEMU");
        (job, out_r, prepared)
    }

    /// Run `spec` to its end: QEMU's status, its output, and the step.
    pub fn run(&self, spec: &VmSpec) -> (ExitStatus, String, Prepared) {
        let t0 = Instant::now();
        let (mut job, mut out_r, prepared) = self.start(spec);
        let mut output = String::new();
        out_r.read_to_string(&mut output).unwrap();
        let qemu = job.wait().unwrap();
        eprintln!("step: {:?}\n{output}", t0.elapsed());
        (qemu, output, prepared)
    }
}

pub fn wait_for(path: &Path, what: &str) {
    let t0 = Instant::now();
    while !path.exists() {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "no {what} within 10 s"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The rig, or skip the test saying why.
#[macro_export]
macro_rules! needs_vm {
    () => {{
        $crate::needs_sandbox!();
        match $crate::vm::Rig::new() {
            Ok(r) => r,
            Err(why) => {
                eprintln!("skipped: {why}");
                return;
            }
        }
    }};
}

#[test]
fn a_step_runs_as_root_in_its_vm_and_brings_back_its_outputs() {
    let rig = needs_vm!();
    let root = rig.busybox_root("root");
    let src = rig.path("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("hello.txt"), "from the host\n").unwrap();
    // A filesystem for the guest to loop-mount: made here, as busybox builds differ in which
    // mkfs they carry.
    let fs = std::fs::File::create(src.join("disk.img")).unwrap();
    fs.set_len(16 << 20).unwrap();
    drop(fs);
    let made = std::process::Command::new("mkfs.ext4")
        .args(["-q", "-F"])
        .arg(src.join("disk.img"))
        .status()
        .unwrap();
    assert!(made.success());
    let (share, _) = rig.share("inputs", &[("root", &root), ("src", &src)], true);
    let script = "set -e; id -u; cat /src/hello.txt; echo changed > /src/hello.txt; \
        cp /src/disk.img /tmp/disk; \
        loop=$(losetup -f); losetup $loop /tmp/disk; mkdir -p /mnt/loop; mount $loop /mnt/loop; \
        echo looped > /mnt/loop/file; cat /mnt/loop/file; umount /mnt/loop; losetup -d $loop; \
        mkdir -p /target/out; echo built > /target/out/app; echo no > /target/other; exit 7";
    let mut spec = rig.spec(share, &["root"], &["sh", "-c", script]);
    spec.inputs = vec![ShareMount {
        layers: vec!["src".into()],
        dst: "/src".into(),
    }];
    spec.cwd = "/src".into();
    spec.outputs = vec!["/target/out/*".into()];
    let (qemu, output, prepared) = rig.run(&spec);
    assert!(qemu.success(), "QEMU ended with {qemu:?}: {output}");
    let outputs = rig.path("outputs");
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
        std::fs::read_to_string(src.join("hello.txt")).unwrap(),
        "from the host\n"
    );
}
