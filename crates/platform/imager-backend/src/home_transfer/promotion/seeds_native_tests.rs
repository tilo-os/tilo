use super::*;
#[path = "native_test_artifact.rs"]
mod native_test_artifact;
use std::os::unix::fs::MetadataExt as _;
use std::process::Command;
const TEST: &str = "home_transfer::promotion::seeds_native_tests::actual_firstboot_seeds_are_retained_only_while_unchanged";
fn command(command: &mut Command) -> std::process::Output {
    let result = command.output().expect("native fixture command starts");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    result
}
fn mount(source: &Path, target: &str) {
    command(
        Command::new("mount")
            .args(["--bind"])
            .arg(source)
            .arg(target),
    );
}
fn production_labels(synthetic: &Path) -> Vec<(PathBuf, String)> {
    if std::fs::read("/sys/fs/selinux/enforce").is_err() {
        return Vec::new();
    }
    // Query public policy before hiding /etc/selinux. Explicit modes prevent
    // matchpathcon from inspecting any original host passwd/shadow inode.
    let mut pending = vec![synthetic.to_owned()];
    let mut labels = Vec::new();
    while let Some(path) = pending.pop() {
        let info = std::fs::symlink_metadata(&path).unwrap();
        let target = Path::new("/etc").join(path.strip_prefix(synthetic).unwrap());
        assert!(
            info.is_dir() || info.is_file(),
            "synthetic policy input is regular"
        );
        let result = Command::new("/usr/bin/matchpathcon")
            .args(["-n", "-m", if info.is_dir() { "dir" } else { "file" }])
            .arg(&target)
            .output()
            .expect("native SELinux public-policy query required");
        assert!(
            result.status.success(),
            "public policy query failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let context = String::from_utf8(result.stdout).unwrap().trim().to_owned();
        assert!(!context.is_empty() && !context.chars().any(char::is_whitespace));
        labels.push((target, context));
        if info.is_dir() {
            pending.extend(
                std::fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path()),
            );
        }
    }
    labels
}
#[test]
fn actual_firstboot_seeds_are_retained_only_while_unchanged() {
    // SAFETY: geteuid only reads current credentials.
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "privileged native qualifier required"
    );
    if std::env::var_os("TILO_NATIVE_SEED_NAMESPACE").is_none() {
        let store = PathBuf::from(
            std::env::var_os("TILO_FS_NATIVE_STORE").expect("canonical native volume required"),
        );
        let temp = tempfile::Builder::new()
            .prefix("firstboot-seeds-")
            .permissions(std::fs::Permissions::from_mode(0o755))
            .tempdir_in(store)
            .unwrap();
        let child = command(
            Command::new("unshare")
                .args(["--mount", "--propagation", "private"])
                // Keep the admitted test inode addressable even for sealed-memfd
                // qualification; /proc/self/exe would be unshare at exec time.
                .arg(format!("/proc/{}/exe", std::process::id()))
                .args(["--exact", TEST, "--nocapture"])
                .env("TILO_NATIVE_SEED_NAMESPACE", "1")
                .env("TILO_NATIVE_SEED_ROOT", temp.path()),
        );
        assert!(
            String::from_utf8_lossy(&child.stdout).contains("test result: ok. 1 passed;"),
            "namespace child must execute exactly one native fixture: {}",
            String::from_utf8_lossy(&child.stdout)
        );
        return;
    }
    let original_root = PathBuf::from(std::env::var_os("TILO_NATIVE_SEED_ROOT").unwrap());
    // Fedora's /mnt is a symlink. Pin the real mountpoint before overlaying
    // anything and use its actual path throughout no-follow adoption.
    let mountpoint = Path::new("/mnt").canonicalize().unwrap();
    mount(&original_root, mountpoint.to_str().unwrap());
    let root = mountpoint.as_path();
    let etc = root.join("etc");
    let state = root.join("lib");
    let homes = root.join("homes");
    for path in [&etc, &state, &homes] {
        std::fs::create_dir(path).unwrap();
    }
    std::fs::create_dir(state.join("tilo")).unwrap();
    std::fs::create_dir_all(etc.join("default")).unwrap();
    std::fs::create_dir_all(etc.join("tilo")).unwrap();
    std::fs::create_dir_all(etc.join("skel")).unwrap();
    for name in [".bashrc", ".profile", ".edited", ".replacement"] {
        std::fs::write(
            etc.join("skel").join(name),
            b"generated firstboot default\n",
        )
        .unwrap();
    }
    std::fs::write(etc.join("passwd"), "root:x:0:0:root:/root:/bin/bash\n").unwrap();
    std::fs::write(
        etc.join("group"),
        "root:x:0:\nwheel:x:10:\nvideo:x:39:\ntilo-identity:x:989:\n",
    )
    .unwrap();
    std::fs::write(etc.join("shadow"), "root:!:20000:0:99999:7:::\n").unwrap();
    std::fs::write(
        etc.join("gshadow"),
        "root:!::\nwheel:!::\nvideo:!::\ntilo-identity:!::\n",
    )
    .unwrap();
    std::fs::set_permissions(etc.join("shadow"), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(etc.join("gshadow"), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(
        etc.join("nsswitch.conf"),
        "passwd: files\ngroup: files\nshadow: files\n",
    )
    .unwrap();
    std::fs::write(
        etc.join("login.defs"),
        "UID_MIN 99\nUID_MAX 99\nGID_MIN 99\nGID_MAX 99\nUSERGROUPS_ENAB yes\nUMASK 077\n",
    )
    .unwrap();
    std::fs::write(
        etc.join("default/useradd"),
        "GROUP=0\nHOME=/var/home\nSHELL=/bin/bash\nSKEL=/etc/skel\nCREATE_MAIL_SPOOL=no\n",
    )
    .unwrap();
    native_test_artifact::install(
        "tilo-firstboot-home-seeds",
        &etc.join("tilo/firstboot-home-seeds"),
    );
    native_test_artifact::install(
        "tilo-firstboot-files",
        &etc.join("tilo/tilo-firstboot-files"),
    );
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../os/usr/libexec/tilo-firstboot-user"
        ),
        etc.join("tilo/firstboot-user"),
    )
    .unwrap();
    std::fs::write(
        etc.join("tilo/install-user.conf"),
        format!("alice:$6$fixture${}\n", "A".repeat(86)),
    )
    .unwrap();
    std::fs::set_permissions(
        etc.join("tilo/install-user.conf"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    // The password tools transition into confined SELinux domains. Give this
    // private synthetic tree production labels rather than unlabeled scratch
    // labels; never relabel or read the original host account databases.
    std::fs::write(etc.join(".pwd.lock"), b"").unwrap();
    std::fs::set_permissions(
        etc.join(".pwd.lock"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let policy = if Path::new("/sys/fs/selinux/enforce").exists() {
        // Pin only public policy in this private namespace before /etc is
        // hidden. Password tools must see policy themselves when replacing
        // database inodes; relabeling their output afterwards is not a test
        // of real firstboot behavior.
        assert!(std::fs::symlink_metadata("/etc/selinux").unwrap().is_dir());
        let held = root.join("public-selinux");
        std::fs::create_dir(&held).unwrap();
        std::fs::create_dir(etc.join("selinux")).unwrap();
        mount(Path::new("/etc/selinux"), held.to_str().unwrap());
        command(
            Command::new("mount")
                .args(["-o", "remount,bind,ro"])
                .arg(&held),
        );
        Some(held)
    } else {
        None
    };
    let labels = production_labels(&etc);
    mount(&etc, "/etc");
    for (path, context) in labels {
        command(Command::new("/usr/bin/chcon").arg(context).arg(path));
    }
    if let Some(held) = policy {
        mount(&held, "/etc/selinux");
        command(Command::new("mount").args(["-o", "remount,bind,ro", "/etc/selinux"]));
    }
    mount(&state, "/var/lib");
    mount(&homes, "/var/home");
    let firstboot = Command::new("bash")
        .arg("/etc/tilo/firstboot-user")
        .output()
        .expect("actual firstboot producer starts");
    if !firstboot.status.success() {
        // Only namespace/lock custody metadata is diagnostic. Never include
        // passwd/shadow contents, credentials, or the firstboot pipe input.
        for name in [
            "",
            "passwd",
            "shadow",
            ".pwd.lock",
            "passwd.lock",
            "shadow.lock",
        ] {
            let path = Path::new("/etc").join(name);
            eprintln!(
                "firstboot fixture metadata {}: {:?}",
                path.display(),
                std::fs::symlink_metadata(&path).map(|info| (
                    info.uid(),
                    info.gid(),
                    info.mode(),
                    info.len(),
                    info.nlink()
                ))
            );
        }
        let diagnosis = Command::new("/usr/bin/python3")
            .args([
                "-c",
                r#"
import ctypes, os
from pathlib import Path
for path in ('/etc', '/etc/passwd', '/etc/shadow', '/etc/.pwd.lock'):
    try:
        print('fixture SELinux', path, repr(os.getxattr(path, 'security.selinux')[:512]))
    except OSError as error:
        print('fixture SELinux', path, 'errno', error.errno)
for path in ('/proc/self/attr/current', '/sys/fs/selinux/enforce'):
    try:
        print('fixture security', path, repr(Path(path).read_bytes()[:512]))
    except OSError as error:
        print('fixture security', path, 'errno', error.errno)
libc = ctypes.CDLL(None, use_errno=True)
result = libc.lckpwdf()
print('fixture actual glibc lckpwdf', result, 'errno', ctypes.get_errno())
if result == 0:
    print('fixture actual glibc ulckpwdf', libc.ulckpwdf())
"#,
            ])
            .output()
            .expect("private namespace lock diagnosis starts");
        eprintln!("{}", String::from_utf8_lossy(&diagnosis.stdout));
        eprintln!("{}", String::from_utf8_lossy(&diagnosis.stderr));
        panic!(
            "actual firstboot producer failed: {}",
            String::from_utf8_lossy(&firstboot.stderr)
        );
    }
    let home = Path::new("/var/home/alice");
    assert_eq!(std::fs::metadata(home).unwrap().uid(), 99);
    let proof = Path::new("/var/lib/tilo/firstboot-home-seeds/99.json");
    let proof_info = std::fs::symlink_metadata(proof).unwrap();
    assert!(proof_info.is_file());
    assert_eq!(proof_info.uid(), 0);
    assert_eq!(proof_info.mode() & 0o022, 0);
    assert_eq!(proof_info.nlink(), 1);
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(proof).unwrap()).unwrap();
    assert_eq!(receipt["schema"], 1);
    assert_eq!(receipt["uid"], 99);
    assert_eq!(
        receipt["home_inode"],
        std::fs::metadata(home).unwrap().ino()
    );
    for name in [".bashrc", ".profile", ".edited", ".replacement"] {
        let source = std::fs::read(Path::new("/etc/skel").join(name)).unwrap();
        let destination = home.join(name);
        assert_eq!(std::fs::read(&destination).unwrap(), source);
        assert_eq!(std::fs::metadata(&destination).unwrap().uid(), 99);
        let entry = receipt["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["path"] == name)
            .expect("every copied seed is attested");
        use sha2::{Digest as _, Sha256};
        assert_eq!(entry["sha256"], hex::encode(Sha256::digest(&source)));
    }
    assert!(!Path::new("/etc/tilo/install-user.conf").exists());
    // SAFETY: this process is a disposable namespace child; the parent retains its credentials.
    assert_eq!(unsafe { libc::setgroups(0, std::ptr::null()) }, 0);
    // SAFETY: fixed disposable fixture GID, in the namespace child only.
    assert_eq!(unsafe { libc::setgid(99) }, 0);
    // SAFETY: fixed disposable fixture UID, in the namespace child only.
    assert_eq!(unsafe { libc::setuid(99) }, 0);
    for path in [".config/tilo", ".local/state/tilo"] {
        std::fs::create_dir_all(home.join(path)).unwrap();
        std::fs::set_permissions(home.join(path), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    // Match the actual first-login receiver: quarantine and its journal are
    // beneath Home's protected portable-home namespace, on the same mount as
    // their adoption target. Separate bind aliases cannot support rename.
    let workspace = home.join(".local/state/tilo/portable-home");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700)).unwrap();
    let imported = workspace.join("imported");
    std::fs::create_dir(&imported).unwrap();
    std::fs::set_permissions(&imported, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        imported.join(".bashrc"),
        b"my original bash configuration\n",
    )
    .unwrap();
    let journal = workspace.join("adoption.journal");
    let review = PromotionReview::review(&imported, home, &journal).unwrap();
    assert_eq!(review.preview().unwrap().collisions.len(), 0);
    let entry = review
        .plan
        .entries
        .iter()
        .map(Result::unwrap)
        .find(|entry| decoded(&entry.path).unwrap() == Path::new(".bashrc"))
        .unwrap();
    let pin = PinnedDirectory::open_absolute(home).unwrap();
    // Actual durable partial-move boundary, then recover through the production journal.
    let (_, _, log_parent) = roots(&imported, home, &journal).unwrap();
    let log = Log::create(
        &log_parent,
        journal.file_name().unwrap(),
        review.plan.clone(),
    )
    .unwrap();
    seeds::preserve(&pin, Path::new(".bashrc"), entry.seed.as_ref().unwrap()).unwrap();
    drop(log);
    let recovered = PromotionReview::recover(&imported, home, &journal).unwrap();
    recovered.witness().unwrap();
    let report = recovered.run(true, &AtomicBool::new(false)).unwrap();
    assert!(report.complete(), "{:?}", report.failed);
    recovered.verify_completed().unwrap();
    assert_eq!(
        std::fs::read(home.join(".bashrc")).unwrap(),
        b"my original bash configuration\n"
    );
    let preserved = home.join(".local/state/tilo/firstboot-preserved");
    let paths: Vec<_> = std::fs::read_dir(&preserved)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(paths.len(), 1);
    assert_eq!(
        std::fs::read(&paths[0]).unwrap(),
        b"generated firstboot default\n"
    );
    std::fs::write(&paths[0], b"edited retained default").unwrap();
    assert!(recovered.verify_completed().is_err());
    let other = workspace.join("other");
    std::fs::create_dir(&other).unwrap();
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o700)).unwrap();
    for name in [".edited", ".replacement", ".profile"] {
        std::fs::write(other.join(name), b"imported original").unwrap();
    }
    std::fs::write(home.join(".edited"), b"my target edit").unwrap();
    std::fs::remove_file(home.join(".replacement")).unwrap();
    std::fs::write(home.join(".replacement"), b"generated firstboot default\n").unwrap();
    let second = PromotionReview::review(&other, home, &workspace.join("second.journal")).unwrap();
    assert_eq!(second.preview().unwrap().collisions.len(), 2);
    std::fs::write(home.join(".profile"), b"edit after review").unwrap();
    let report = second.run(false, &AtomicBool::new(false)).unwrap();
    assert!(!report.failed.is_empty());
    assert_eq!(
        std::fs::read(home.join(".profile")).unwrap(),
        b"edit after review"
    );
    assert_eq!(
        std::fs::read(other.join(".profile")).unwrap(),
        b"imported original"
    );
    // The root parent owns cleanup after this private namespace exits.
}
