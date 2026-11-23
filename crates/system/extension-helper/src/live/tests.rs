use std::cell::{Cell, RefCell};
use std::fmt;
use std::os::unix::fs::{PermissionsExt as _, symlink};

use tilo_test_env::SpawnUnderEnvLock as _;

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
name = "package-service"
kind = "userspace"
scope = "session"
user_units = ["tilo-package-service.service"]
files = [{ from = "tilo-package-service", to = "/usr/libexec/tilo-package-service", mode = "0755" }]

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
files = [{ from = "tilo-greeter", to = "/usr/libexec/tilo/tilo-greeter", mode = "0755" }]

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

tilo_test_env::scenario! {
    fn an_update_is_planned_from_bootc_status_the_manifest_and_the_tree_diff;

    phase bootc_reports_both_layouts;
    {
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

    phase the_manifest_says_who_installs_a_path;
    {
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

    phase only_the_kernel_and_bootloader_need_a_restart_of_the_machine;
    {
        let components = components();
        let class = |path: &str| classify(path, components.owner(path));
        assert_eq!(class("/usr/lib/modules/6.17.1/vmlinuz"), Class::Boot);
        assert_eq!(
            class("/usr/lib/modules/6.17.1/kernel/fs/erofs/erofs.ko.xz"),
            Class::Boot
        );
        // A kernel's start-up image is rebuilt with every image; only the next start reads it.
        assert_eq!(
            class("/usr/lib/modules/6.17.1/initramfs.img"),
            Class::NextBoot
        );
        assert_eq!(
            class("/usr/lib/modules/6.17.1/extra/initramfs.img"),
            Class::Boot
        );
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

    phase a_daily_update_applies_by_component_scope_without_a_reboot;
    {
        let components = components();
        let plan = Plan::new(
            vec![
                written("/usr/bin/fs"),
                written("/usr/bin/shell"),
                written("/usr/libexec/tilo-package-service"),
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
        assert_eq!(session.user_units, ["tilo-package-service.service"]);

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

    phase the_trees_differ_by_bytes_mode_links_and_removals;
    {
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
}

fn tree(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
    }
}

#[derive(Default)]
struct Fake {
    roots: RefCell<BTreeMap<String, PathBuf>>,
    status: RefCell<String>,
    restarted: RefCell<Vec<String>>,
    /// Units that fail once they are restarted.
    failing: RefCell<Vec<String>>,
    /// Units that had failed already, whatever the update does.
    failed_already: RefCell<Vec<String>>,
    logins: RefCell<Vec<Login>>,
    desktops: RefCell<Vec<String>>,
    merges: RefCell<u32>,
    mounted: RefCell<BTreeSet<PathBuf>>,
    /// The fake clock: [`LiveHost::sleep`] advances it, so a wait for a desktop costs no time.
    clock: Cell<Duration>,
    /// Whether the login's wrapper runs.
    login_alive: Cell<bool>,
    /// The wrapper says its asked restart never gave a usable desktop.
    restart_failed: Cell<bool>,
    /// The compositor the login's wrapper runs, while it does.
    compositor: Cell<Option<u32>>,
    /// When that compositor goes away, on the fake clock.
    compositor_gone: Cell<Option<Duration>>,
    /// The compositor that published a usable desktop, from the clock reading it becomes one.
    ready: RefCell<Option<(u32, Duration)>>,
}

impl Fake {
    /// Stand in for a sandbox that still reads `layer` in its own mount namespace.
    fn mount(&self, layer: &Path) {
        self.mounted.borrow_mut().insert(layer.to_path_buf());
    }

    fn release(&self, layer: &Path) {
        self.mounted.borrow_mut().remove(layer);
    }
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
        self.roots
            .borrow()
            .get(&key)
            .cloned()
            .ok_or_else(|| Error::Pack(format!("mount failed: {key}")))
    }

    fn close(&self, _at: &Path) {}

    fn whiteout(&self, path: &Path) -> Result<()> {
        fs::write(path, "whiteout").at(path)
    }

    fn is_whiteout(&self, path: &Path) -> bool {
        fs::read_to_string(path).is_ok_and(|text| text == "whiteout")
    }

    fn layer_in_use(&self, layer: &Path) -> bool {
        self.mounted.borrow().contains(layer)
    }

    fn daemon_reload(&self) -> Result<()> {
        Ok(())
    }

    fn reload_user(&self, _login: &Login) -> Result<()> {
        Ok(())
    }

    fn try_restart(&self, login: Option<&Login>, unit: &str) -> Result<()> {
        self.restarted.borrow_mut().push(format!(
            "{}{unit}",
            login.map(|l| format!("{}:", l.user)).unwrap_or_default()
        ));
        Ok(())
    }

    fn unit_failed(&self, login: Option<&Login>, unit: &str) -> bool {
        let named = format!(
            "{}{unit}",
            login.map(|l| format!("{}:", l.user)).unwrap_or_default()
        );
        self.failed_already
            .borrow()
            .iter()
            .any(|failed| failed == unit)
            || (self.failing.borrow().iter().any(|failing| failing == unit)
                && self.restarted.borrow().contains(&named))
    }

    fn start_no_block(&self, _unit: &str) {}

    fn logins(&self) -> Vec<Login> {
        self.logins.borrow().clone()
    }

    fn restart_desktop(&self, login: &Login) -> Result<()> {
        self.desktops.borrow_mut().push(login.user.clone());
        Ok(())
    }

    fn login_alive(&self, _login: &Login) -> bool {
        self.login_alive.get()
    }

    fn compositor(&self, _login: &Login) -> Option<u32> {
        let pid = self.compositor.get()?;
        let now = self.clock.get();
        if self.compositor_gone.get().is_some_and(|gone| now >= gone) {
            None
        } else {
            Some(pid)
        }
    }

    fn restart_failed(&self, _login: &Login) -> bool {
        self.restart_failed.get()
    }

    fn desktop_ready(&self, _login: &Login, pid: u32) -> bool {
        let now = self.clock.get();
        self.ready
            .borrow()
            .is_some_and(|(ready_pid, since)| ready_pid == pid && now >= since)
    }

    fn now(&self) -> Instant {
        // One origin for the process: only the differences the wait reads matter.
        static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        ORIGIN
            .get_or_init(Instant::now)
            .checked_add(self.clock.get())
            .expect("the fake clock stays in range")
    }

    fn sleep(&self, duration: Duration) {
        self.clock.set(
            self.clock
                .get()
                .checked_add(duration)
                .expect("the fake clock stays in range"),
        );
    }
}

/// Stage an image as the device's update service would: its tree at `dir/name`,
/// its composefs verity and its image digest both spelt with `mark`.
fn stage(dir: &Path, fake: &Fake, name: &str, mark: char, files: &[(&str, &str)]) {
    let root = dir.join(name);
    tree(&root, files);
    let verity = mark.to_string().repeat(128);
    fake.roots.borrow_mut().insert(verity.clone(), root);
    *fake.status.borrow_mut() = format!(
        r#"{{"status":{{
            "booted":{{"image":{{"imageDigest":"sha256:{a}"}},"composefs":{{"verity":"{b}"}}}},
            "staged":{{"image":{{"imageDigest":"sha256:{image}"}},"composefs":{{"verity":"{verity}"}}}}}}}}"#,
        a = "a".repeat(64),
        b = "b".repeat(128),
        image = mark.to_string().repeat(64),
    );
}

fn machine() -> (tempfile::TempDir, Layout, Fake) {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path().join("root"));
    let base = dir.path().join("booted");
    tree(
        &base,
        &[
            ("usr/bin/fs", "fs 1"),
            ("usr/bin/shell", "shell 1"),
            ("usr/libexec/tilo/tilo-greeter", "greeter 1"),
            ("usr/lib/old-lib.so", "old"),
        ],
    );
    let fake = Fake::default();
    fake.roots.borrow_mut().insert("b".repeat(128), base);
    stage(
        dir.path(),
        &fake,
        "staged",
        'd',
        &[
            ("usr/bin/fs", "fs 2"),
            ("usr/bin/shell", "shell 2"),
            ("usr/libexec/tilo/tilo-greeter", "greeter 2"),
            ("usr/lib/modules/6.18/vmlinuz", "kernel"),
            ("usr/share/tilo/components.toml", MANIFEST),
        ],
    );
    fake.logins.borrow_mut().push(Login {
        wrapper: 1,
        user: "ana".into(),
        uid: 1000,
        gid: 1000,
    });
    // The default desktop is up and usable, as one is before an update restarts it.
    fake.login_alive.set(true);
    fake.compositor.set(Some(4242));
    *fake.ready.borrow_mut() = Some((4242, Duration::ZERO));
    (dir, layout, fake)
}

#[test]
fn a_restarted_desktop_is_back_only_once_it_is_usable() {
    let fake = Fake::default();
    let login = Login {
        wrapper: 1,
        user: "ana".into(),
        uid: 1000,
        gid: 1000,
    };
    fake.login_alive.set(true);
    fake.compositor.set(Some(7));
    *fake.ready.borrow_mut() = Some((7, fake.clock.get()));
    let started = fake.clock.get();
    assert!(
        fake.desktop_back(&login, DESKTOP_STABLE),
        "a desktop that is ready at once is back"
    );
    let took = fake.clock.get().saturating_sub(started);
    assert!(
        took >= DESKTOP_STABLE && took < Duration::from_secs(20),
        "back after the short stability check, not the old 20 s: {took:?}"
    );

    *fake.ready.borrow_mut() = None;
    let started = fake.clock.get();
    assert!(
        !fake.desktop_back(&login, DESKTOP_STABLE),
        "a compositor that is alive and never ready is not back"
    );
    assert_eq!(
        fake.clock.get().saturating_sub(started),
        DESKTOP_BACK,
        "it is given up at the bound"
    );

    // A wrapper that says the restarted desktop never started is believed at once: the login
    // waits for the restart to be asked again, and every second of waiting is a frozen screen.
    fake.restart_failed.set(true);
    let started = fake.clock.get();
    assert!(
        !fake.desktop_back(&login, DESKTOP_STABLE),
        "a restart the wrapper calls failed is not back"
    );
    assert_eq!(
        fake.clock.get().saturating_sub(started),
        Duration::ZERO,
        "and nobody waits for it"
    );
    fake.restart_failed.set(false);

    fake.compositor.set(Some(8));
    *fake.ready.borrow_mut() = Some((8, fake.clock.get()));
    fake.compositor_gone.set(Some(
        fake.clock.get().saturating_add(Duration::from_secs(1)),
    ));
    assert!(
        !fake.desktop_back(&login, DESKTOP_STABLE),
        "a desktop that becomes ready and crashes before it stayed up is not back"
    );

    // A record the previous run left names its own process, never this compositor's.
    fake.compositor_gone.set(None);
    fake.compositor.set(Some(9));
    *fake.ready.borrow_mut() = Some((8, Duration::ZERO));
    assert!(
        !fake.desktop_back(&login, DESKTOP_STABLE),
        "a stale ready record is not this desktop"
    );

    fake.login_alive.set(false);
    assert!(
        !fake.desktop_back(&login, DESKTOP_STABLE),
        "a login that ended is not back"
    );
}

/// A daily update applied live: first what needs no desktop restart, then the
/// desktop; a newer image over it restarts only what changed since, never
/// touching the layer a running sandbox still reads; a failed restart puts the
/// previous layer back; and once nothing is staged the layer comes off.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one update journey asserts every step in order"
)]
fn a_staged_image_applies_live_is_replaced_by_the_next_and_comes_off_when_unstaged() {
    let (dir, layout, fake) = machine();

    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 0);
    let layer = fs::read_link(link(&layout).unwrap()).unwrap();
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
        fs::read_to_string(layer.join(format!(
            "usr/lib/extension-release.d/extension-release.{}",
            extension_name(&layer)
        )))
        .unwrap()
        .contains(&"d".repeat(64))
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

    fake.restarted.borrow_mut().clear();
    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 0);
    assert!(fake.restarted.borrow().is_empty());

    assert_eq!(apply(&fake, &fake, &layout, Phase::Session).unwrap(), 0);
    let first = fs::read_link(link(&layout).unwrap()).unwrap();
    assert_eq!(
        fs::read_to_string(first.join("usr/bin/shell")).unwrap(),
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

    // A sandbox started before the next update keeps the merged /usr it
    // started with, and that mount reads this layer.
    fake.mount(&first);
    stage(
        dir.path(),
        &fake,
        "newer",
        'f',
        &[
            ("usr/bin/fs", "fs 2"),
            ("usr/bin/shell", "shell 2"),
            ("usr/libexec/tilo/tilo-greeter", "greeter 3"),
            ("usr/lib/old-lib.so", "old"),
            ("usr/lib/modules/6.18/vmlinuz", "kernel"),
            ("usr/share/tilo/components.toml", MANIFEST),
        ],
    );
    fake.desktops.borrow_mut().clear();
    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 0);
    assert!(
        fake.restarted.borrow().is_empty(),
        "fs and its recipient already run this build"
    );
    let second = fs::read_link(link(&layout).unwrap()).unwrap();
    assert_ne!(second, first, "an apply builds a layer of its own");
    assert_eq!(
        fs::read_to_string(second.join("usr/bin/shell")).unwrap(),
        "shell 2",
        "the system phase keeps the compositor the desktop already runs"
    );
    assert_eq!(
        fs::read_to_string(second.join("usr/libexec/tilo/tilo-greeter")).unwrap(),
        "greeter 3"
    );
    assert!(
        !second.join("usr/lib/old-lib.so").exists(),
        "the library is back as booted"
    );
    let state = read_state(&layout).unwrap();
    assert_eq!(state.deferred, ["greetd.service"]);
    assert!(
        !state.desktop_pending,
        "nothing the desktop runs changed since it restarted"
    );
    assert!(
        first.is_dir(),
        "a layer a sandbox still mounts is never removed"
    );
    assert_eq!(
        fs::read_to_string(first.join("usr/bin/fs")).unwrap(),
        "fs 2",
        "nor rewritten under the running sandbox"
    );
    assert_eq!(
        fs::read_dir(layers(&layout)).unwrap().count(),
        2,
        "the linked layer and the one still mounted"
    );
    assert_eq!(apply(&fake, &fake, &layout, Phase::Session).unwrap(), 0);
    assert!(
        fake.desktops.borrow().is_empty(),
        "the desktop is not restarted again"
    );

    // The reaping sweep removes only what nothing links or mounts.
    assert_eq!(prune(&fake, &layout), 0);
    assert!(first.is_dir(), "a mounted layer is kept");
    assert!(second.is_dir(), "the linked layer is kept");
    fake.release(&first);
    assert_eq!(prune(&fake, &layout), 0);
    assert!(!first.exists(), "nothing links or mounts it any more");
    assert!(second.is_dir(), "the linked layer stays");

    // The next update's unit fails after its restart: the previous layer is
    // linked again, and is still there to be.
    fake.failing.borrow_mut().push("fs.service".into());
    stage(
        dir.path(),
        &fake,
        "next",
        'c',
        &[
            ("usr/bin/fs", "fs 3"),
            ("usr/bin/shell", "shell 2"),
            ("usr/libexec/tilo/tilo-greeter", "greeter 3"),
            ("usr/lib/old-lib.so", "old"),
            ("usr/lib/modules/6.18/vmlinuz", "kernel"),
            ("usr/share/tilo/components.toml", MANIFEST),
            ("usr/lib/newer", "newer"),
        ],
    );
    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 1);
    assert_eq!(
        fs::read_link(link(&layout).unwrap()).unwrap(),
        second,
        "rollback relinks the previous layer"
    );
    assert!(second.is_dir(), "and it is still there to relink");

    // Nothing staged any more: the stale layer comes off.
    let booted = fake
        .status
        .borrow()
        .replace(r#""staged":"#, r#""ignored":"#);
    *fake.status.borrow_mut() = booted;
    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 0);
    assert!(link(&layout).is_none());
    assert!(!linked(&layout));
    assert!(!merged_marker(&layout).exists());
    let state = read_state(&layout).unwrap();
    assert_eq!(state.image, "");
    assert!(!state.reboot_required && !state.desktop_pending);
}

/// An update whose units fail, or whose image cannot be opened, leaves the
/// booted system as it was and waits for a restart of the machine; a unit that
/// had failed before the update is not the update's failure.
#[test]
fn an_update_that_cannot_be_applied_waits_for_a_restart_of_the_machine() {
    let (_dir, layout, fake) = machine();
    fake.failed_already.borrow_mut().push("fs.service".into());
    assert_eq!(
        apply(&fake, &fake, &layout, Phase::System).unwrap(),
        0,
        "an old failure is not the update's"
    );
    assert!(link(&layout).is_some(), "the update stays applied");

    let (_dir, layout, fake) = machine();
    fake.failing.borrow_mut().push("fs.service".into());
    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 1);
    assert!(link(&layout).is_none(), "nothing was linked before");
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
    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 0);

    let (_dir, layout, fake) = machine();
    fake.roots.borrow_mut().remove(&"d".repeat(128));
    assert!(apply(&fake, &fake, &layout, Phase::System).is_err());
    assert!(link(&layout).is_none(), "nothing was linked");
    let state = read_state(&layout).unwrap();
    assert_eq!(state.image, format!("sha256:{}", "d".repeat(64)));
    assert!(state.failed);
    assert!(
        state.reboot_required,
        "the desktop asks for the restart that applies it"
    );
    assert!(state.detail.contains("mount failed"));
    assert_eq!(apply(&fake, &fake, &layout, Phase::System).unwrap(), 0);
}

#[test]
fn rebuilding_a_layer_name_never_touches_the_one_there() {
    let (dir, _machine, fake) = machine();
    let layout = Layout::new(dir.path().join("build"));
    let linked = dir.path().join("staged");
    let plan = Plan::new(vec![written("/usr/bin/fs")], &components());
    let requested = layers(&layout).join("same");
    let first = build_layer(
        &fake,
        &fake,
        &plan,
        Phase::System,
        &linked,
        &requested,
        "sha256:aaa",
    )
    .unwrap();
    assert_eq!(first, requested);
    let bytes = fs::read_to_string(first.join("usr/bin/fs")).unwrap();
    let second = build_layer(
        &fake,
        &fake,
        &plan,
        Phase::System,
        &linked,
        &requested,
        "sha256:aaa",
    )
    .unwrap();
    assert_ne!(second, first, "a second build takes a fresh name");
    assert_eq!(
        fs::read_to_string(first.join("usr/bin/fs")).unwrap(),
        bytes,
        "the first layer is untouched"
    );
    assert_eq!(
        fs::read_to_string(
            first.join("usr/lib/extension-release.d/extension-release.tilo-live-update-same")
        )
        .unwrap()
        .lines()
        .next(),
        Some("ID=_any"),
        "the release file is named after the layer's link"
    );
}

#[test]
fn only_a_mount_that_reads_the_layer_holds_it() {
    let layer = Path::new("/var/lib/tilo/live-update/layers/aaaa-system");
    let name = extension_name(layer);
    assert_eq!(name, "tilo-live-update-aaaa-system");
    assert!(!mountinfo_reads("", layer, &name));
    // A sandbox's clone of the merged /usr, as systemd-sysext mounted it.
    let line = "36 25 0:44 / /usr ro,relatime shared:20 - overlay sysext \
                ro,lowerdir=/run/systemd/sysext/meta/usr:\
                /run/systemd/sysext/extensions/tilo-live-update-aaaa-system/usr:/usr\n";
    assert!(mountinfo_reads(line, layer, &name));
    // The resolved layer path, for a sysext that hands over the extension directory directly.
    let line = "36 25 0:44 / /usr ro - overlay sysext \
                ro,lowerdir=/var/lib/tilo/live-update/layers/aaaa-system/usr:/usr\n";
    assert!(mountinfo_reads(line, layer, &name));
    // Another layer's mount, and something that is not an overlay.
    let other = "36 25 0:44 / /usr ro - overlay sysext \
                 ro,lowerdir=/run/systemd/sysext/extensions/tilo-live-update-bbbb-system/usr:/usr\n";
    assert!(!mountinfo_reads(other, layer, &name));
    assert!(!mountinfo_reads(
        "36 25 0:44 / /usr ro - ext4 /dev/sda1 ro\n",
        layer,
        &name
    ));

    let proc = tempfile::tempdir().unwrap();
    assert!(!layer_mounted(proc.path(), layer));
    let pid = proc.path().join("42");
    fs::create_dir(&pid).unwrap();
    fs::write(pid.join("mountinfo"), line).unwrap();
    assert!(layer_mounted(proc.path(), layer));
    assert!(
        layer_mounted(Path::new("/nonexistent-proc"), layer),
        "an unreadable mount table means the layer may be in use"
    );
}

#[test]
fn a_composefs_image_is_checked_by_its_own_digest_algorithm() {
    let sha512 = [0x5e_u8; 64];
    let sha256 = [0x3c_u8; 32];
    assert!(verity_matches(2, &sha512, &hex::encode(sha512)));
    assert!(verity_matches(
        2,
        &sha512,
        &hex::encode(sha512).to_uppercase()
    ));
    assert!(verity_matches(1, &sha256, &hex::encode(sha256)));
    assert!(
        !verity_matches(1, &sha512, &hex::encode(sha512)),
        "wrong algorithm"
    );
    assert!(
        !verity_matches(2, &[0x5f; 64], &hex::encode(sha512)),
        "wrong digest"
    );
    assert!(
        !verity_matches(2, &sha512, &hex::encode(sha256)),
        "wrong length"
    );
    assert!(!verity_matches(2, &[], ""));

    let args = composefs_mount(
        Path::new("/sysroot/composefs/objects"),
        Path::new("/proc/1/fd/3"),
        Path::new("/run/tilo/live-update/mounts/staged"),
    );
    assert_eq!(
        args,
        [
            "mount.composefs",
            "-o",
            "ro,verity,basedir=/sysroot/composefs/objects",
            "/proc/1/fd/3",
            "/run/tilo/live-update/mounts/staged",
        ]
        .map(OsString::from),
        "no digest= option: libcomposefs measures only SHA-256 images"
    );
}

/// The fixture as the machine has it, read when a wait gives up: what the
/// wrapper runs, which children it has and what they run, so a failure names
/// what stood in the compositor's place rather than only that it never came.
struct Fixture(u32);

impl fmt::Display for Fixture {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let children: Vec<String> = children_of(self.0)
            .into_iter()
            .map(|pid| {
                format!(
                    "{pid} runs {}{}",
                    process(pid),
                    if zombie(pid) { " (a zombie)" } else { "" }
                )
            })
            .collect();
        write!(
            out,
            "the fixture's compositor (the wrapper {} runs {}, and is {}; the children file is {}; \
             its children: {})",
            self.0,
            process(self.0),
            if zombie(self.0) { "gone" } else { "running" },
            if fs::metadata(format!("/proc/{}/task/{}/children", self.0, self.0)).is_ok() {
                "there"
            } else {
                "absent"
            },
            if children.is_empty() {
                "none".to_owned()
            } else {
                children.join(", ")
            },
        )
    }
}

fn process(pid: u32) -> String {
    let binary = fs::read_link(format!("/proc/{pid}/exe"))
        .map_or_else(|_| "unreadable".to_owned(), |exe| exe.display().to_string());
    let args = fs::read(format!("/proc/{pid}/cmdline")).map_or_else(
        |_| "unreadable".to_owned(),
        |args| {
            String::from_utf8_lossy(&args)
                .replace('\0', " ")
                .trim()
                .to_owned()
        },
    );
    let state = fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next().map(str::to_owned))
        })
        .unwrap_or_else(|| "unreadable".to_owned());
    let waiting = fs::read_to_string(format!("/proc/{pid}/wchan")).map_or_else(
        |_| "unreadable".to_owned(),
        |waiting| waiting.trim().to_owned(),
    );
    format!(
        "{:?} from {binary:?} (`{args}`, state {state}, waiting in {waiting:?})",
        comm(pid)
    )
}

/// The compositor and the login wrapper share a process name, because the
/// wrapper that runs the compositor is a script of the same name: what each
/// process runs is what tells them apart, so the update restarts a desktop
/// through its wrapper and never signals a compositor as if it were one.
#[test]
fn the_compositor_is_told_from_the_login_wrapper_by_its_binary() {
    let dir = tempfile::tempdir().unwrap();
    let binaries = dir.path().join("bin");
    fs::create_dir(&binaries).unwrap();
    let binary = binaries.join(COMPOSITOR);
    fs::copy("/bin/sleep", &binary).unwrap();
    let wrapper = dir.path().join(LOGIN_WRAPPER);
    fs::write(
        &wrapper,
        format!("#!/bin/sh\n\"{}\" 600 &\nwait\n", binary.display()),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let mut login = std::process::Command::new(&wrapper).spawn_locked().unwrap();
    let wrapper_pid = login.id();
    let compositor_pid = tilo_test_env::wait_for(Fixture(wrapper_pid), || {
        // A wrapper that ended started no compositor, and never will.
        if let Some(status) = login.try_wait().unwrap() {
            panic!("the wrapper ended ({status}) before its compositor started");
        }
        compositor_of(wrapper_pid)
    });

    assert_eq!(
        comm(wrapper_pid).as_deref(),
        Some(LOGIN_WRAPPER),
        "the wrapper runs under the compositor's name"
    );
    assert!(
        !is_compositor(wrapper_pid),
        "a script named so is the wrapper, not the compositor"
    );
    assert!(
        is_compositor(compositor_pid),
        "the binary is the compositor"
    );
    let logins = crate::System.logins();
    assert!(
        logins.iter().any(|login| login.wrapper == wrapper_pid),
        "the login is found, and restarted through its wrapper"
    );
    assert!(
        !logins.iter().any(|login| login.wrapper == compositor_pid),
        "the compositor is never taken for a login of its own"
    );
    assert_eq!(
        compositor_pid,
        compositor_of(wrapper_pid).unwrap(),
        "the running compositor is the wrapper's"
    );

    let _ = std::process::Command::new("kill")
        .arg(compositor_pid.to_string())
        .status_locked();
    let _ = login.kill();
    let _ = login.wait();
}

#[test]
fn an_accounts_service_manager_is_asked_as_that_account_on_its_own_bus() {
    let login = Login {
        wrapper: 1,
        user: "ana".into(),
        uid: 1000,
        gid: 1001,
    };
    assert_eq!(
        systemctl(Some(&login), &["try-restart", "tilo-ush.service"]),
        [
            "setpriv",
            "--reuid=1000",
            "--regid=1001",
            "--init-groups",
            "env",
            "XDG_RUNTIME_DIR=/run/user/1000",
            "systemctl",
            "--user",
            "try-restart",
            "tilo-ush.service",
        ]
    );
    // The system's own manager is asked directly, with nothing handed over a bus.
    assert_eq!(
        systemctl(None, &["daemon-reload"]),
        ["systemctl", "daemon-reload"]
    );
}
