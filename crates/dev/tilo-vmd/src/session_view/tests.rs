use super::*;
use crate::apply::Apply as _;
use crate::systemd::SystemdApply;
use std::process::Command;

fn tree(path: &Path, marker: &str) {
    fs::create_dir_all(path.join("usr/lib")).unwrap();
    fs::create_dir_all(path.join("opt")).unwrap();
    fs::write(path.join("usr/lib/os-release"), "ID=tilo-test\n").unwrap();
    fs::write(path.join("usr").join(marker), marker).unwrap();
    fs::write(path.join("usr/winner"), marker).unwrap();
}

fn protected(path: &Path) {
    let path = text(path).unwrap();
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: valid live CString and output storage; inspect only on success.
    assert_eq!(
        unsafe { libc::statvfs(path.as_ptr(), status.as_mut_ptr()) },
        0
    );
    // SAFETY: successful statvfs initialized the structure.
    let flags = unsafe { status.assume_init() }.f_flag;
    let required = libc::ST_RDONLY | libc::ST_NODEV;
    assert_eq!(flags & required, required);
    assert_eq!(
        flags & libc::ST_NOSUID,
        0,
        "preserve the existing setuid helper policy"
    );
}

fn overlay(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    let empty = source.with_extension("empty");
    fs::create_dir(&empty).unwrap();
    mount(
        Some(Path::new("overlay")),
        target,
        Some("overlay"),
        libc::MS_RDONLY,
        Some(&format!(
            "lowerdir={}:{}",
            source.display(),
            empty.display()
        )),
    )
    .unwrap();
}

fn subject(root: &Path) {
    // Isolate the fixture from the builder container's own overlay depth.
    mount(
        Some(Path::new("tmpfs")),
        root,
        Some("tmpfs"),
        libc::MS_NODEV,
        Some("mode=0700"),
    )
    .unwrap();
    let image = root.join("image");
    let boot = root.join("boot");
    let vm = root.join("vm");
    let app = root.join("app");
    tree(&image, "base-only");
    fs::set_permissions(image.join("opt"), fs::Permissions::from_mode(0o775)).unwrap();
    for name in ["proc", "proof", "vm"] {
        fs::create_dir(image.join(name)).unwrap();
    }
    tree(&vm, "vm-only");
    tree(&app, "app-only");
    // A signed/imported hierarchy may be group-writable in its readonly
    // presentation without making the protected backing mountpoint mutable.
    fs::set_permissions(app.join("opt"), fs::Permissions::from_mode(0o775)).unwrap();
    overlay(&image, &boot); // Bootc-like base: filesystem depth one.
    mount(None, &boot, None, libc::MS_SHARED, None).unwrap();
    let global = format!(
        "lowerdir={}:{}",
        vm.join("usr").display(),
        boot.join("usr").display()
    );
    mount(
        Some(Path::new("overlay")),
        &boot.join("usr"),
        Some("overlay"),
        libc::MS_RDONLY,
        Some(&global),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(boot.join("usr/winner")).unwrap(),
        "vm-only"
    );
    let legacy = root.join("legacy");
    fs::create_dir(&legacy).unwrap();
    let old = mount(
        Some(Path::new("overlay")),
        &legacy,
        Some("overlay"),
        libc::MS_RDONLY,
        Some(&format!(
            "lowerdir={}:{}",
            app.join("usr").display(),
            boot.join("usr").display()
        )),
    );
    assert!(
        old.is_err(),
        "a third nested overlay must reproduce the kernel limit"
    );
    let views = root.join("views");
    let layers = vec![vm.clone(), app.clone()];
    let view = prepare_from(&views, "proof", &layers, &boot).unwrap();
    assert_eq!(
        fs::read_to_string(view.join("usr/winner")).unwrap(),
        "app-only"
    );
    for marker in ["base-only", "vm-only", "app-only"] {
        assert!(view.join("usr").join(marker).is_file());
    }
    protected(&view.join("usr"));
    protected(&view.join("opt"));
    assert!(fs::write(view.join("usr/forbidden"), b"write").is_err());
    refresh_shared_base(root, &boot, &views, &layers, &global);
    // A new generation replaces the fixed mount points without accumulating
    // overlays; removing the slot releases its active mounted views.
    prepare_from(&views, "proof", std::slice::from_ref(&vm), &boot).unwrap();
    assert!(!view.join("usr/app-only").exists());
    assert_eq!(
        fs::read_to_string(view.join("usr/winner")).unwrap(),
        "vm-only"
    );
    remove(&views, "proof").unwrap();
    assert!(!mounted(&view.join("usr")).unwrap());
    // The exception is never applied to a mutable backing mountpoint.
    fs::set_permissions(view.join("opt"), fs::Permissions::from_mode(0o775)).unwrap();
    assert!(prepare_from(&views, "proof", &layers, &boot).is_err());
    fs::set_permissions(view.join("opt"), fs::Permissions::from_mode(0o700)).unwrap();
    let escape = root.join("escape");
    std::os::unix::fs::symlink(&image, &escape).unwrap();
    assert!(prepare_from(&views, "proof", &[escape], &boot).is_err());
    fs::set_permissions(&app, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(prepare_from(&views, "proof", &[app], &boot).is_err());
    assert!(prepare_from(&views, "../outside", &layers, &boot).is_err());

    actual_apply(root, &boot, &vm);
}

fn refresh_shared_base(root: &Path, boot: &Path, views: &Path, layers: &[PathBuf], global: &str) {
    // Reproduce an already-installed older daemon's shared base. Its
    // migration must disconnect peers before removing propagated children.
    let old_views = root.join("old-views");
    directory(&old_views).unwrap();
    let old_base = old_views.join("base");
    directory(&old_base).unwrap();
    mount(Some(boot), &old_base, None, libc::MS_BIND, None).unwrap();
    mount(
        None,
        &old_base,
        None,
        libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NODEV,
        None,
    )
    .unwrap();
    // A normal global sysext refresh must not propagate into the retained
    // boot root, even when the original boot mount belongs to a shared group.
    unmount(&boot.join("usr")).unwrap();
    mount(
        Some(Path::new("overlay")),
        &boot.join("usr"),
        Some("overlay"),
        libc::MS_RDONLY,
        Some(global),
    )
    .unwrap();
    assert!(
        !mounted(&views.join("base/usr")).unwrap(),
        "global sysext refresh must never overmount the retained base"
    );
    assert!(mounted(&old_base.join("usr")).unwrap());
    let repaired = prepare_from(&old_views, "migrated", layers, boot).unwrap();
    assert!(!mounted(&old_base.join("usr")).unwrap());
    assert!(mounted(&boot.join("usr")).unwrap());
    assert_eq!(
        fs::read_to_string(boot.join("usr/winner")).unwrap(),
        "vm-only"
    );
    assert_eq!(
        fs::read_to_string(old_base.join("usr/winner")).unwrap(),
        "base-only"
    );
    assert_eq!(
        fs::read_to_string(repaired.join("usr/winner")).unwrap(),
        "app-only"
    );
    protected(&repaired.join("usr"));
    remove(&old_views, "migrated").unwrap();
}

fn actual_apply(root: &Path, boot: &Path, vm: &Path) {
    // Exercise SystemdApply's fixed / source in a real Bootc-shaped root,
    // independent of however many overlays the CI container already stacks.
    // All namespace/chroot capabilities are confined to this test process.
    let actual = root.join("actual");
    fs::create_dir(&actual).unwrap();
    mount(
        Some(&actual),
        &boot.join("proof"),
        None,
        libc::MS_BIND,
        None,
    )
    .unwrap();
    mount(Some(vm), &boot.join("vm"), None, libc::MS_BIND, None).unwrap();
    mount(
        Some(Path::new("/proc")),
        &boot.join("proc"),
        None,
        libc::MS_BIND,
        None,
    )
    .unwrap();
    let boot = text(boot).unwrap();
    // SAFETY: this private mount/user namespace owns the NUL-terminated root.
    assert_eq!(unsafe { libc::chroot(boot.as_ptr()) }, 0);
    std::env::set_current_dir("/").unwrap();
    let mut apply = SystemdApply::new(Path::new("/proof"));
    let dropin = apply
        .session_layer_dropin("actual", 7, &[PathBuf::from("/vm")])
        .unwrap();
    assert!(dropin.contains("ExtensionDirectories=\nBindReadOnlyPaths="));
    assert!(dropin.contains("/slot-actual/usr:/usr"));
    assert!(dropin.contains("/slot-actual/opt:/opt"));
    assert_eq!(
        fs::read_to_string("/proof/views/slot-actual/usr/winner").unwrap(),
        "vm-only"
    );
    protected(Path::new("/proof/views/slot-actual/usr"));
    protected(Path::new("/proof/views/slot-actual/opt"));
    apply.remove_session_view("actual").unwrap();
}

#[test]
fn session_views_flatten_vm_and_slot_layers_in_real_mount_namespace() {
    const SUBJECT: &str = "TILO_VMD_SESSION_VIEW_SUBJECT";
    if let Ok(path) = std::env::var(SUBJECT) {
        subject(Path::new(&path));
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let output = Command::new("timeout")
        .args([
            "30",
            "bwrap",
            "--unshare-all",
            "--cap-add",
            "CAP_SYS_ADMIN",
            "--cap-add",
            "CAP_SYS_CHROOT",
            "--uid",
            "0",
            "--gid",
            "0",
            "--bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--setenv",
            SUBJECT,
        ])
        .arg(root.path())
        .arg(std::env::current_exe().unwrap())
        .args([
            "session_view::tests::session_views_flatten_vm_and_slot_layers_in_real_mount_namespace",
            "--exact",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
