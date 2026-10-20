//! Production account handoff in a private mount namespace and disposable chroot.
//! Requires root (or passwordless sudo), mount namespaces and a disk-backed
//! TMPDIR, which the ordinary test sandbox never gives, so it is ignored there
//! and the `sandbox-host-ids` check runs it as root in a micro-VM. Missing
//! isolation fails the test; this is not a VM installation or boot test.
#[cfg(test)]
mod tests {
    use account_custody::portable::{self, PortableBinding};
    use accounts::{Account, AccountId, AccountStore, AccountTypeId, Secret, SecretVault};
    use installer::account_handoff::InstallAccounts;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};
    use tilo_seal::SymmetricKey;

    const OPERATION: &str = "49b48a8c-9dd0-46ad-b1d2-abc241d8e002";
    const TARGET: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const USER: &str = "alice";
    const CREDENTIAL: &str = "native handoff fixture credential";
    const CHILD: &str = "TILO_ACCOUNTS_NATIVE_NAMESPACE";

    fn selected_secret() -> Secret {
        account_custody::oauth_secret(&account_custody::TokenSet {
            access_token: CREDENTIAL.into(),
            refresh_token: "native fixture renewable grant".into(),
            scope: Some("https://www.googleapis.com/auth/drive".into()),
            expires_in: 3600,
            obtained_at: 1,
        })
    }

    fn checked(command: &mut Command) {
        let output = command.output().expect("execute isolation command");
        assert!(
            output.status.success(),
            "isolation command failed: {output:?}"
        );
    }

    struct Fixture {
        // Mounts must be removed before TemporaryDirectory removes the chroot.
        mounts: Vec<PathBuf>,
        directory: tempfile::TempDir,
    }
    impl Fixture {
        fn new() -> Self {
            let scratch = std::env::var_os("TMPDIR").expect("disk-backed TMPDIR is required");
            let directory = tempfile::Builder::new()
                .prefix("accounts-native-")
                .tempdir_in(scratch)
                .unwrap();
            let mut fixture = Self {
                mounts: Vec::new(),
                directory,
            };
            fs::set_permissions(fixture.directory.path(), fs::Permissions::from_mode(0o755))
                .unwrap();
            for path in [
                "usr",
                "proc",
                "dev",
                "etc/tilo",
                "root",
                "var/home/alice",
                "source",
            ] {
                fs::create_dir_all(fixture.path(path)).unwrap();
            }
            fixture.bind_read_only("/usr", "usr");
            fixture.bind_read_only("/proc", "proc");
            fs::write(fixture.path("dev/null"), []).unwrap();
            fixture.bind_read_only("/dev/null", "dev/null");
            // Fedora's merged /usr layout, including the dynamic loader.
            for name in ["lib", "lib64", "bin", "sbin"] {
                std::os::unix::fs::symlink(format!("usr/{name}"), fixture.path(name)).unwrap();
            }
            fs::copy(
                env!("CARGO_BIN_EXE_tilo-firstboot-home-seeds"),
                fixture.path("helper"),
            )
            .unwrap();
            fs::set_permissions(fixture.path("helper"), fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(
                fixture.path("etc/nsswitch.conf"),
                "passwd: files\ngroup: files\n",
            )
            .unwrap();
            fixture.passwd(1000);
            fixture.chown("var/home/alice", 1000);
            fixture
        }
        fn path(&self, path: &str) -> PathBuf {
            self.directory.path().join(path)
        }
        fn bind_read_only(&mut self, source: &str, target: &str) {
            let target = self.path(target);
            checked(Command::new("mount").arg("--bind").arg(source).arg(&target));
            self.mounts.push(target.clone());
            checked(
                Command::new("mount")
                    .args(["-o", "remount,bind,ro"])
                    .arg(target),
            );
        }
        fn chown(&self, path: &str, uid: u32) {
            checked(
                Command::new("chown")
                    .arg(format!("{uid}:{uid}"))
                    .arg(self.path(path)),
            );
        }
        fn passwd(&self, uid: u32) {
            fs::write(
                self.path("etc/passwd"),
                format!(
                    "root:x:0:0::/root:/bin/sh\n{USER}:x:{uid}:1000::/var/home/{USER}:/bin/sh\n"
                ),
            )
            .unwrap();
            fs::write(self.path("etc/group"), "root:x:0:\nalice:x:1000:\n").unwrap();
        }
        fn run(&self, args: &[&str], frame: &[u8], uid: u32, success: bool) -> Output {
            let root = std::ffi::CString::new(self.directory.path().as_os_str().as_encoded_bytes())
                .unwrap();
            let mut command = Command::new("/helper");
            command
                .args(args)
                .env_clear()
                .env("HOME", "/root")
                .env("XDG_CONFIG_HOME", "/root/.config")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            // SAFETY: only async-signal-safe credential/chroot syscalls run before exec;
            // all paths and arguments were prepared in the parent.
            unsafe {
                command.pre_exec(move || {
                    if libc::chroot(root.as_ptr()) != 0
                        || libc::chdir(c"/".as_ptr()) != 0
                        || libc::setgroups(0, std::ptr::null()) != 0
                        || libc::setgid(uid) != 0
                        || libc::setuid(uid) != 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command
                .spawn()
                .expect("execute production helper in chroot");
            // A helper that refuses before reading closes its input: its
            // status and stderr below say why, not the broken pipe.
            if let Err(error) = child.stdin.take().unwrap().write_all(frame) {
                assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe, "{error}");
            }
            let output = child.wait_with_output().unwrap();
            assert_eq!(
                output.status.success(),
                success,
                "helper {args:?} returned {output:?}"
            );
            assert!(output.stdout.is_empty(), "unexpected helper output");
            if success {
                assert!(output.stderr.is_empty(), "{output:?}");
            } else {
                assert_eq!(output.status.code(), Some(1));
                assert!(
                    output.stderr.starts_with(b"firstboot home defaults: "),
                    "{output:?}"
                );
            }
            output
        }
        fn stage(&self, frame: &[u8], uid: &str, success: bool) {
            self.run(
                &[
                    "--accounts-stage",
                    OPERATION,
                    TARGET,
                    USER,
                    uid,
                    "/etc/tilo/online-accounts",
                ],
                frame,
                0,
                success,
            );
        }
        fn adopt(&self, success: bool) {
            self.run(&["--accounts-adopt", USER], &[], 0, success);
        }
        fn absent_destination(&self) {
            assert!(!self.path("var/home/alice/.config/tilo/accounts").exists());
            self.no_root_vault();
        }
        fn no_root_vault(&self) {
            assert!(!self.path("root/.config").exists());
            assert!(!self.path("etc/tilo/accounts").exists());
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            for mount in self.mounts.iter().rev() {
                checked(Command::new("umount").arg(mount));
            }
        }
    }

    type FileSnapshot = (PathBuf, Vec<u8>, u32, u64, i64, i64);

    fn snapshot(path: &Path) -> Vec<FileSnapshot> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                entries.extend(snapshot(&path));
            } else {
                let stat = fs::metadata(&path).unwrap();
                entries.push((
                    path.clone(),
                    fs::read(path).unwrap(),
                    stat.uid(),
                    stat.ino(),
                    stat.mtime(),
                    stat.ctime(),
                ));
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    #[test]
    #[ignore = "needs root and mount namespaces: run by the sandbox-host-ids check in a micro-VM"]
    fn production_stage_adopt_accounts() {
        if std::env::var(CHILD).as_deref() == Ok("verify") {
            assert_eq!(rustix::process::getuid().as_raw(), 1000);
            let destination = Path::new("/var/home/alice/.config/tilo");
            assert_eq!(AccountStore::load_in(destination).len(), 1);
            let vault = SecretVault::open_read_only_in(destination);
            assert_eq!(
                vault.get(&AccountId::new("selected-native")).unwrap(),
                selected_secret()
            );
            assert!(vault.get(&AccountId::new("unselected-native")).is_none());
            return;
        }
        if std::env::var_os(CHILD).is_none() {
            let scratch = std::env::var_os("TMPDIR").expect("disk-backed TMPDIR is required");
            // Already root (the micro-VM), unshare directly; otherwise
            // through passwordless sudo.
            let root = rustix::process::geteuid().is_root();
            let mut command = Command::new(if root { "unshare" } else { "sudo" });
            if !root {
                command.args(["-n", "unshare"]);
            }
            command
                .args(["--mount", "--fork", "--propagation", "private", "env"])
                .arg(format!("{CHILD}=1"))
                .arg(format!("TMPDIR={}", PathBuf::from(scratch).display()))
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::production_stage_adopt_accounts",
                    "--ignored",
                    "--nocapture",
                ]);
            checked(&mut command);
            return;
        }
        assert_eq!(
            rustix::process::getuid().as_raw(),
            0,
            "fixture requires root"
        );
        assert_ne!(
            fs::read_link("/proc/self/ns/mnt").unwrap(),
            fs::read_link("/proc/1/ns/mnt").unwrap(),
            "fixture must never mount in the host namespace"
        );
        let fixture = Fixture::new();
        let (source, frame) = prepare_source(&fixture);
        verify_handoff(&fixture, &source, &frame);
    }

    fn prepare_source(fixture: &Fixture) -> (PathBuf, Vec<u8>) {
        let source = fixture.path("source");
        let selected = AccountId::new("selected-native");
        let unselected = AccountId::new("unselected-native");
        let mut vault = SecretVault::open_in(&source);
        vault.put(&selected, &selected_secret());
        vault.put(&unselected, &Secret::password("must remain only at source"));
        vault.try_save().unwrap();
        let mut store = AccountStore::new();
        for id in [&selected, &unselected] {
            store.upsert(Account::new(
                id.clone(),
                AccountTypeId::new(account_custody::OAuthCloud::GoogleDrive.type_id()),
            ));
        }
        store.save_in(&source).unwrap();
        let binding = PortableBinding::new(OPERATION, TARGET, USER, 1000).unwrap();
        let key = SymmetricKey::generate().unwrap();
        let sealed = portable::seal_selected_accounts(
            &source,
            std::slice::from_ref(&selected),
            &key,
            binding.as_bytes(),
        )
        .unwrap();
        let mut frame = Vec::new();
        InstallAccounts::new(&sealed, key)
            .write_to(&mut frame)
            .unwrap();

        (source, frame)
    }

    fn verify_handoff(fixture: &Fixture, source: &Path, frame: &[u8]) {
        // The source is deliberately writable by the destination UID: a custody
        // alias regression must be caught by the guard, not by Unix permissions.
        checked(Command::new("chown").args(["-R", "1000:1000"]).arg(source));
        let before = snapshot(source);
        fixture.run(
            &["--accounts-consume", OPERATION, TARGET, USER, "1000"],
            frame,
            1001,
            false,
        );
        fixture.absent_destination();
        fixture.stage(frame, "1001", false);
        assert!(!fixture.path("etc/tilo/online-accounts").exists());
        fixture.absent_destination();
        fixture.stage(frame, "1000", true);
        let transport = fixture.path("etc/tilo/online-accounts/transport");
        assert_eq!(fs::read(&transport).unwrap(), frame);
        for path in [
            "etc/tilo/online-accounts",
            "etc/tilo/online-accounts/transport",
            "etc/tilo/online-accounts/binding.json",
        ] {
            let stat = fs::metadata(fixture.path(path)).unwrap();
            assert_eq!(stat.uid(), 0);
            assert_eq!(
                stat.mode() & 0o777,
                if stat.is_dir() { 0o700 } else { 0o600 }
            );
        }
        // Root must not be able to bypass the destination-user consumer boundary.
        fixture.run(
            &["--accounts-consume", OPERATION, TARGET, USER, "1000"],
            frame,
            0,
            false,
        );
        fixture.absent_destination();
        fixture.passwd(1001);
        fixture.adopt(false);
        fixture.passwd(1000);
        fixture.absent_destination();

        let binding_path = fixture.path("etc/tilo/online-accounts/binding.json");
        let original_binding = fs::read(&binding_path).unwrap();
        let mut wrong: serde_json::Value = serde_json::from_slice(&original_binding).unwrap();
        wrong["target"] = "b".repeat(64).into();
        fs::write(&binding_path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        fixture.adopt(false);
        fixture.absent_destination();
        assert_eq!(fs::read(&transport).unwrap(), frame);
        fs::write(&binding_path, &original_binding).unwrap();
        // Wrong transport key is well framed but must authenticate before vault writes.
        let mut corrupt = frame.to_vec();
        corrupt[12] ^= 1;
        fs::write(&transport, &corrupt).unwrap();
        fixture.adopt(false);
        fixture.absent_destination();
        assert_eq!(fs::read(&transport).unwrap(), corrupt);
        fs::write(&transport, frame).unwrap();

        // A migrated custody alias must not redirect writes into source credentials.
        let config = fixture.path("var/home/alice/.config");
        fs::create_dir(&config).unwrap();
        fixture.chown("var/home/alice/.config", 1000);
        std::os::unix::fs::symlink("/source", config.join("tilo")).unwrap();
        fixture.adopt(false);
        assert_eq!(snapshot(source), before);
        fs::remove_file(config.join("tilo")).unwrap();
        fixture.adopt(true);
        let destination = config.join("tilo");
        assert_eq!(AccountStore::load_in(&destination).len(), 1);
        // Read canonical custody as the installed user, not root: custody readers
        // deliberately refuse keys owned by another real user.
        fs::copy(std::env::current_exe().unwrap(), fixture.path("verify")).unwrap();
        fs::set_permissions(fixture.path("verify"), fs::Permissions::from_mode(0o755)).unwrap();
        checked(
            Command::new("chroot")
                .args(["--userspec=1000:1000", "--groups=1000"])
                .arg(fixture.directory.path())
                .args([
                    "/verify",
                    "--exact",
                    "tests::production_stage_adopt_accounts",
                    "--ignored",
                ])
                .env_clear()
                .env(CHILD, "verify"),
        );
        assert!(!transport.exists());
        assert_eq!(
            fs::read(fixture.path("etc/tilo/online-accounts/adopted")).unwrap(),
            original_binding
        );
        for (_, _, uid, _, _, _) in snapshot(&destination) {
            assert_eq!(uid, 1000);
        }
        assert_ne!(
            fs::read(accounts::vault_dir_in(source).join("vault.key")).unwrap(),
            fs::read(accounts::vault_dir_in(&destination).join("vault.key")).unwrap()
        );
        let installed_before = snapshot(&destination);
        fixture.adopt(true);
        assert_eq!(snapshot(&destination), installed_before);
        assert_eq!(snapshot(source), before);
        fixture.no_root_vault();
    }
}
