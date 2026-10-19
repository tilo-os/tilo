use std::cell::RefCell;
use std::os::unix::fs::{PermissionsExt as _, symlink};

use super::*;

const MANIFEST: &str = r#"
[[component]]
name = "shell"
kind = "userspace"
scope = "session"
files = [
  { from = "shell", to = "/usr/bin/shell", mode = "0755" },
  { from = "x", to = "/usr/share/fonts/tilo/", tree = true },
]

[[component]]
name = "app-host"
kind = "userspace"
scope = "session"
user_units = ["tilo-app-host.service"]
files = [{ from = "tilo-app-host", to = "/usr/libexec/tilo-app-host", mode = "0755" }]

[[component]]
name = "apps+runtime"
kind = "userspace"
scope = "session"
files = [{ from = "/usr/lib/tilo-runtime/", to = "/usr/lib/tilo-runtime/", tree = true }]

[[component]]
name = "fs"
kind = "userspace"
scope = "vm"
units = ["fs.service"]
user_units = ["tilo-fs-recipient.service"]
files = [{ from = "fs", to = "/usr/bin/fs", mode = "0755" }]

[[component]]
name = "greeter"
kind = "userspace"
scope = "vm"
units = ["greetd.service"]
files = [{ from = "tilo-greeter", to = "/usr/bin/tilo-greeter", mode = "0755" }]

[[component]]
name = "kernel"
kind = "boot"
scope = "vm"
"#;

fn written(path: &str) -> Change {
    Change {
        path: path.to_owned(),
        kind: ChangeKind::Written,
    }
}

fn components() -> Components {
    Components::parse(MANIFEST).unwrap()
}

#[test]
fn bootc_reports_both_layouts() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let verity = "b".repeat(128);
    let json = format!(
        r#"{{"status":{{
            "booted":{{"image":{{"imageDigest":"{digest}"}},"composefs":{{"verity":"sha512:{verity}"}}}},
            "staged":{{"image":{{"imageDigest":"{digest}"}},"ostree":{{"checksum":"{c}","deploySerial":2,"stateroot":"default"}}}},
            "rollback":null}}}}"#,
        c = "c".repeat(64)
    );
    let (booted, staged) = deployments(&json).unwrap();
    assert_eq!(
        booted,
        Some(Deployment::Composefs {
            image: digest.clone(),
            verity: verity.clone()
        })
    );
    assert_eq!(
        staged,
        Some(Deployment::Ostree {
            image: digest.clone(),
            stateroot: "default".into(),
            checksum: "c".repeat(64),
            serial: 2,
        })
    );
    let none = format!(
        r#"{{"status":{{"booted":{{"image":{{"imageDigest":"{digest}"}},"composefs":{{"verity":"{verity}"}}}},"staged":null}}}}"#
    );
    assert_eq!(deployments(&none).unwrap().1, None);
    // An identity that could name another path is refused.
    let escape = format!(
        r#"{{"status":{{"staged":{{"image":{{"imageDigest":"{digest}"}},"composefs":{{"verity":"../../etc"}}}}}}}}"#
    );
    assert!(deployments(&escape).is_err());
    assert!(deployments("not json").is_err());
}

#[test]
fn the_manifest_says_who_installs_a_path() {
    let components = components();
    assert_eq!(components.owner("/usr/bin/shell").unwrap().name, "shell");
    assert_eq!(
        components
            .owner("/usr/share/fonts/tilo/a.ttf")
            .unwrap()
            .name,
        "shell"
    );
    assert_eq!(
        components
            .owner("/usr/lib/tilo-runtime/2026.10/lib.so")
            .unwrap()
            .name,
        "apps+runtime"
    );
    assert!(components.owner("/usr/bin/shellfish").is_none());
    assert!(Components::parse("").is_err());
}

#[test]
fn only_the_kernel_bootloader_and_initramfs_need_a_restart_of_the_machine() {
    let components = components();
    let class = |path: &str| classify(path, components.owner(path));
    assert_eq!(class("/usr/lib/modules/6.17.1/vmlinuz"), Class::Boot);
    assert_eq!(class("/usr/lib/modules/6.17.1/initramfs.img"), Class::Boot);
    assert_eq!(
        class("/usr/lib/systemd/boot/efi/systemd-bootx64.efi"),
        Class::Boot
    );
    assert_eq!(class("/usr/lib/bootupd/updates/EFI/x.efi"), Class::Boot);
    assert_eq!(class("/usr/etc/hosts"), Class::NextBoot);
    assert_eq!(class("/usr/lib/sysimage/rpm/rpmdb.sqlite"), Class::NextBoot);
    assert_eq!(class("/usr/lib/os-release"), Class::NextBoot);
    assert_eq!(class("/usr/bin/fs"), Class::System);
    assert_eq!(class("/usr/bin/shell"), Class::Session);
    assert_eq!(
        class("/usr/share/tilo/system-interfaces.toml"),
        Class::Session
    );
    assert_eq!(class("/usr/lib/tilo-runtime/2026.10/x"), Class::Apps);
    assert_eq!(class("/usr/lib64/libc.so.6"), Class::Base);
}

// USER REQUIREMENT (2026-10-18): "it's fine if we break compat with old versions every few days, people just need to update"
#[test]
fn a_daily_update_applies_by_component_scope_without_a_reboot() {
    let components = components();
    let plan = Plan::new(
        vec![
            written("/usr/bin/fs"),
            written("/usr/bin/shell"),
            written("/usr/libexec/tilo-app-host"),
            written("/usr/lib/tilo-runtime/2026.10/lib.so"),
            written("/usr/lib64/libfoo.so"),
        ],
        &components,
    );
    assert!(!plan.reboot, "no kernel, bootloader or initramfs changed");
    assert!(plan.desktop_pending(), "the compositor changed");
    let in_system: Vec<&str> = plan
        .changes
        .iter()
        .filter(|(_, class)| plan.in_layer(*class, Phase::System))
        .map(|(change, _)| change.path.as_str())
        .collect();
    assert_eq!(
        in_system,
        [
            "/usr/bin/fs",
            "/usr/lib/tilo-runtime/2026.10/lib.so",
            "/usr/lib64/libfoo.so"
        ],
        "system services, apps, the runtime and the base apply at once"
    );
    assert!(
        plan.changes
            .iter()
            .all(|(_, class)| plan.in_layer(*class, Phase::Session)),
        "the desktop restart takes the rest"
    );
    let system = plan.restarts(Phase::System, false);
    assert_eq!(system.units, ["fs.service"]);
    assert_eq!(system.user_units, ["tilo-fs-recipient.service"]);
    let session = plan.restarts(Phase::Session, true);
    assert!(
        session.units.is_empty(),
        "already restarted by the system phase"
    );
    assert_eq!(session.user_units, ["tilo-app-host.service"]);

    // Apps built for new interface versions wait for the desktop that
    // speaks them.
    let interfaces = Plan::new(
        vec![
            written("/usr/share/tilo/system-interfaces.toml"),
            written("/usr/lib/tilo-runtime/2026.11/lib.so"),
        ],
        &components,
    );
    assert!(interfaces.interfaces_changed);
    assert!(!interfaces.in_layer(Class::Apps, Phase::System));
    assert!(interfaces.in_layer(Class::Apps, Phase::Session));
    assert!(interfaces.desktop_pending());

    // An apps-only update restarts nothing.
    let apps = Plan::new(
        vec![written("/usr/lib/tilo-runtime/2026.10/x")],
        &components,
    );
    assert!(!apps.desktop_pending());
    assert_eq!(apps.restarts(Phase::System, false), Restarts::default());

    // A kernel waits for a restart of the machine; the rest still applies.
    let kernel = Plan::new(
        vec![
            written("/usr/lib/modules/6.18/vmlinuz"),
            written("/usr/bin/fs"),
        ],
        &components,
    );
    assert!(kernel.reboot);
    assert!(!kernel.in_layer(Class::Boot, Phase::Session));
    assert!(!kernel.desktop_pending());
}

fn tree(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
    }
}

#[test]
fn the_trees_differ_by_bytes_mode_links_and_removals() {
    let dir = tempfile::tempdir().unwrap();
    let (base, new) = (dir.path().join("base"), dir.path().join("new"));
    tree(
        &base,
        &[
            ("usr/bin/same", "x"),
            ("usr/bin/changed", "old"),
            ("usr/bin/mode", "m"),
            ("usr/bin/gone", "g"),
            ("usr/share/old/a", "a"),
            ("usr/share/old/b", "b"),
        ],
    );
    tree(
        &new,
        &[
            ("usr/bin/same", "x"),
            ("usr/bin/changed", "new"),
            ("usr/bin/mode", "m"),
            ("usr/bin/added", "n"),
            ("usr/share/fresh/c", "c"),
        ],
    );
    fs::set_permissions(new.join("usr/bin/mode"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(base.join("usr/bin/mode"), fs::Permissions::from_mode(0o644)).unwrap();
    symlink("same", base.join("usr/bin/link")).unwrap();
    symlink("changed", new.join("usr/bin/link")).unwrap();

    let changes = diff(&base, &new).unwrap();
    let listed: Vec<(&str, ChangeKind)> = changes
        .iter()
        .map(|change| (change.path.as_str(), change.kind))
        .collect();
    assert_eq!(
        listed,
        [
            ("/usr/bin/added", ChangeKind::Written),
            ("/usr/bin/changed", ChangeKind::Written),
            ("/usr/bin/gone", ChangeKind::Removed),
            ("/usr/bin/link", ChangeKind::Written),
            ("/usr/bin/mode", ChangeKind::Written),
            ("/usr/share/fresh/c", ChangeKind::Written),
            ("/usr/share/old", ChangeKind::Removed),
        ]
    );
}

/// The machine, as a test sees it.
#[derive(Default)]
struct Fake {
    roots: RefCell<BTreeMap<String, PathBuf>>,
    status: RefCell<String>,
    restarted: RefCell<Vec<String>>,
    failing: RefCell<Vec<String>>,
    logins: RefCell<Vec<Login>>,
    desktops: RefCell<Vec<String>>,
    merges: RefCell<u32>,
}

impl Host for Fake {
    fn sysext_refresh(&self, _layout: &Layout) -> Result<()> {
        let mut merges = self.merges.borrow_mut();
        *merges = merges.saturating_add(1);
        Ok(())
    }

    fn sysext_unmerge(&self, _layout: &Layout) -> Result<()> {
        Ok(())
    }

    fn sysext_masked(&self) -> Result<bool> {
        Ok(true)
    }

    fn relabel(&self, _tree: &Path) -> Result<()> {
        Ok(())
    }
}

impl LiveHost for Fake {
    fn bootc_status(&self) -> Result<String> {
        Ok(self.status.borrow().clone())
    }

    fn open(&self, _layout: &Layout, deployment: &Deployment, _at: &Path) -> Result<PathBuf> {
        let key = match deployment {
            Deployment::Composefs { verity, .. } => verity.clone(),
            Deployment::Ostree { checksum, .. } => checksum.clone(),
        };
        Ok(self.roots.borrow()[&key].clone())
    }

    fn close(&self, _at: &Path) {}

    fn whiteout(&self, path: &Path) -> Result<()> {
        fs::write(path, "whiteout").at(path)
    }

    fn is_whiteout(&self, path: &Path) -> bool {
        fs::read_to_string(path).is_ok_and(|text| text == "whiteout")
    }

    fn daemon_reload(&self) -> Result<()> {
        Ok(())
    }

    fn reload_user(&self, _user: &str) -> Result<()> {
        Ok(())
    }

    fn try_restart(&self, user: Option<&str>, unit: &str) -> Result<()> {
        self.restarted.borrow_mut().push(format!(
            "{}{unit}",
            user.map(|u| format!("{u}:")).unwrap_or_default()
        ));
        Ok(())
    }

    fn unit_failed(&self, _user: Option<&str>, unit: &str) -> bool {
        self.failing.borrow().iter().any(|failing| failing == unit)
    }

    fn start_no_block(&self, _unit: &str) {}

    fn logins(&self) -> Vec<Login> {
        self.logins.borrow().clone()
    }

    fn restart_desktop(&self, login: &Login) -> Result<()> {
        self.desktops.borrow_mut().push(login.user.clone());
        Ok(())
    }

    fn desktop_back(&self, _login: &Login, _stable: Duration) -> bool {
        true
    }

    fn sleep(&self, _duration: Duration) {}
}

fn machine() -> (tempfile::TempDir, Layout, Fake) {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path().join("root"));
    let base = dir.path().join("booted");
    let new = dir.path().join("staged");
    tree(
        &base,
        &[
            ("usr/bin/fs", "fs 1"),
            ("usr/bin/shell", "shell 1"),
            ("usr/bin/tilo-greeter", "greeter 1"),
            ("usr/lib/old-lib.so", "old"),
        ],
    );
    tree(
        &new,
        &[
            ("usr/bin/fs", "fs 2"),
            ("usr/bin/shell", "shell 2"),
            ("usr/bin/tilo-greeter", "greeter 2"),
            ("usr/lib/modules/6.18/vmlinuz", "kernel"),
            ("usr/share/tilo/components.toml", MANIFEST),
        ],
    );
    let booted_verity = "b".repeat(128);
    let staged_verity = "d".repeat(128);
    let fake = Fake::default();
    fake.roots.borrow_mut().insert(booted_verity.clone(), base);
    fake.roots.borrow_mut().insert(staged_verity.clone(), new);
    *fake.status.borrow_mut() = format!(
        r#"{{"status":{{
            "booted":{{"image":{{"imageDigest":"sha256:{a}"}},"composefs":{{"verity":"{booted_verity}"}}}},
            "staged":{{"image":{{"imageDigest":"sha256:{e}"}},"composefs":{{"verity":"{staged_verity}"}}}}}}}}"#,
        a = "a".repeat(64),
        e = "e".repeat(64),
    );
    fake.logins.borrow_mut().push(Login {
        wrapper: 1,
        user: "ana".into(),
    });
    (dir, layout, fake)
}

// USER REQUIREMENT (2026-10-18): "it's fine if we break compat with old versions every few days, people just need to update"
#[test]
fn the_staged_image_applies_live_first_the_system_then_the_desktop() {
    let (_dir, layout, fake) = machine();

    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 0);
    let layer = fs::read_link(link(&layout)).unwrap();
    assert!(layer.starts_with(layers(&layout)));
    assert_eq!(
        fs::read_to_string(layer.join("usr/bin/fs")).unwrap(),
        "fs 2"
    );
    assert!(
        !layer.join("usr/bin/shell").exists(),
        "the compositor waits for the desktop restart"
    );
    assert!(
        !layer.join("usr/lib/modules").exists(),
        "the kernel waits for a restart of the machine"
    );
    assert_eq!(
        fs::read_to_string(layer.join("usr/lib/old-lib.so")).unwrap(),
        "whiteout",
        "a file the image dropped is hidden"
    );
    assert!(
        fs::read_to_string(
            layer.join("usr/lib/extension-release.d/extension-release.tilo-live-update")
        )
        .unwrap()
        .contains(&"e".repeat(64))
    );
    assert_eq!(
        *fake.restarted.borrow(),
        ["fs.service", "ana:tilo-fs-recipient.service"],
        "the greeter is never restarted under a login"
    );
    assert!(
        fake.desktops.borrow().is_empty(),
        "the desktop is left alone"
    );
    assert!(*fake.merges.borrow() >= 1, "the layer was merged");
    assert!(merged_marker(&layout).exists());
    let state = read_state(&layout).unwrap();
    assert_eq!(state.applied, Some(Phase::System));
    assert!(state.desktop_pending);
    assert!(
        state.reboot_required,
        "the kernel needs a restart of the machine"
    );
    assert_eq!(state.deferred, ["greetd.service"]);

    // Asked again for the same image, nothing happens.
    fake.restarted.borrow_mut().clear();
    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 0);
    assert!(fake.restarted.borrow().is_empty());

    // The desktop's natural moment: the rest, and the desktop restarts.
    assert_eq!(apply(&fake, &fake, &layout, Phase::Session).unwrap(), 0);
    let layer = fs::read_link(link(&layout)).unwrap();
    assert_eq!(
        fs::read_to_string(layer.join("usr/bin/shell")).unwrap(),
        "shell 2"
    );
    assert_eq!(*fake.desktops.borrow(), ["ana"]);
    assert!(
        fake.restarted.borrow().is_empty(),
        "the system services already run the update"
    );
    let state = read_state(&layout).unwrap();
    assert_eq!(state.applied, Some(Phase::Session));
    assert!(!state.desktop_pending);
    assert_eq!(
        fs::read_dir(layers(&layout)).unwrap().count(),
        1,
        "only the linked layer is kept"
    );
}

#[test]
fn a_newer_image_over_an_applied_one_restarts_only_what_changed_since() {
    let (dir, layout, fake) = machine();
    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 0);
    assert_eq!(apply(&fake, &fake, &layout, Phase::Session).unwrap(), 0);

    // A newer image is staged before the machine restarts: the services and
    // the compositor as the applied one has them, a new greeter, and the
    // library the applied one dropped back as booted.
    let newer = dir.path().join("newer");
    tree(
        &newer,
        &[
            ("usr/bin/fs", "fs 2"),
            ("usr/bin/shell", "shell 2"),
            ("usr/bin/tilo-greeter", "greeter 3"),
            ("usr/lib/old-lib.so", "old"),
            ("usr/lib/modules/6.18/vmlinuz", "kernel"),
            ("usr/share/tilo/components.toml", MANIFEST),
        ],
    );
    let verity = "f".repeat(128);
    fake.roots.borrow_mut().insert(verity.clone(), newer);
    let status = fake
        .status
        .borrow()
        .replace(&"d".repeat(128), &verity)
        .replace(&"e".repeat(64), &"f".repeat(64));
    *fake.status.borrow_mut() = status;
    fake.restarted.borrow_mut().clear();
    fake.desktops.borrow_mut().clear();

    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 0);
    assert!(
        fake.restarted.borrow().is_empty(),
        "fs and its recipient already run this build"
    );
    let layer = fs::read_link(link(&layout)).unwrap();
    assert_eq!(
        fs::read_to_string(layer.join("usr/bin/shell")).unwrap(),
        "shell 2",
        "the system phase keeps the compositor the desktop already runs"
    );
    assert_eq!(
        fs::read_to_string(layer.join("usr/bin/tilo-greeter")).unwrap(),
        "greeter 3"
    );
    assert!(
        !layer.join("usr/lib/old-lib.so").exists(),
        "the library is back as booted"
    );
    let state = read_state(&layout).unwrap();
    assert_eq!(state.deferred, ["greetd.service"]);
    assert!(
        !state.desktop_pending,
        "nothing the desktop runs changed since it restarted"
    );
    assert_eq!(apply(&fake, &fake, &layout, Phase::Session).unwrap(), 0);
    assert!(
        fake.desktops.borrow().is_empty(),
        "the desktop is not restarted again"
    );
}

#[test]
fn a_unit_that_fails_after_the_update_rolls_it_back() {
    let (_dir, layout, fake) = machine();
    fake.failing.borrow_mut().push("fs.service".into());
    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 1);
    assert!(!link(&layout).exists(), "nothing was linked before");
    assert!(
        !merged_marker(&layout).exists(),
        "the layer was merged away"
    );
    assert_eq!(
        *fake.restarted.borrow(),
        [
            "fs.service",
            "ana:tilo-fs-recipient.service",
            "fs.service",
            "ana:tilo-fs-recipient.service"
        ],
        "the same units restart again on the old build"
    );
    let state = read_state(&layout).unwrap();
    assert!(state.failed);
    assert!(
        state.reboot_required,
        "it applies with the next restart of the machine"
    );
    assert!(state.detail.contains("fs.service"));
    // A failed image is not retried live.
    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 0);
}

#[test]
fn nothing_staged_takes_a_stale_layer_off() {
    let (_dir, layout, fake) = machine();
    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 0);
    assert!(linked(&layout));
    let booted = fake
        .status
        .borrow()
        .replace(r#""staged":"#, r#""ignored":"#);
    *fake.status.borrow_mut() = booted;
    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 0);
    assert!(!link(&layout).exists());
    assert!(!merged_marker(&layout).exists());
    let state = read_state(&layout).unwrap();
    assert_eq!(state.image, "");
    assert!(!state.reboot_required && !state.desktop_pending);
}
