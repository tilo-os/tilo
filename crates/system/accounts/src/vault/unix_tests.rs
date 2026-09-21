use super::*;
use atomic_file::FileIdentity;
use atomic_file::test_support::{
    Checkpoint as C, ExistingCheckpoint as E, ProvisionCheckpoint as P, with_existing_hooks,
    with_failure, with_failure_on_nth, with_observer, with_provision_failure,
    with_provision_observer,
};
use std::{
    cell::RefCell,
    fs,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    process::Command,
    rc::Rc,
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("vault-unix-{}", uuid::Uuid::new_v4())))
    }
    fn dir(&self) -> PathBuf {
        crate::vault_dir_in(&self.0)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn child(root: &Root, action: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "vault::unix_tests::vault_process", "--nocapture"])
        .env("TILO_VAULT_PROCESS_ROOT", &root.0)
        .env("TILO_VAULT_PROCESS_ACTION", action);
    command
}
fn fixture() -> Root {
    let root = Root::new();
    assert!(child(&root, "legacy").status().unwrap().success());
    assert_eq!(fs::metadata(root.dir()).unwrap().mode() & 0o777, 0o755);
    root
}
fn identity(path: &Path) -> FileIdentity {
    FileIdentity::of(&fs::File::open(path).unwrap()).unwrap()
}
fn save(root: &Root) -> Result<(), String> {
    let mut vault = SecretVault::open_in(&root.0);
    vault.put(&AccountId::from("new"), &Secret::password("new-password"));
    vault.try_save()
}

#[test]
fn vault_process() {
    let Ok(root) = std::env::var("TILO_VAULT_PROCESS_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let dir = crate::vault_dir_in(&root);
    match std::env::var("TILO_VAULT_PROCESS_ACTION").unwrap().as_str() {
        "legacy" => {
            // Isolated process: reproduce the old ordinary mkdir/private-file writer.
            unsafe {
                libc::umask(0o022);
            }
            fs::create_dir_all(&dir).unwrap();
            atomic_file::write_with_mode(&dir.join("vault.key"), &[7; KEY_LEN], 0o600).unwrap();
            let entries = BTreeMap::from([("old".to_string(), Secret::password("old-password"))]);
            let blob = seal(&[7; KEY_LEN], &serde_json::to_vec(&entries).unwrap()).unwrap();
            atomic_file::write(&dir.join("vault.enc"), &blob).unwrap();
        }
        "stage-exhaustion" => {
            fs::create_dir_all(&dir).unwrap();
            for sequence in 0..32 {
                fs::write(
                    dir.join(format!(".vault.key.{}.{sequence}.tmp", std::process::id())),
                    b"owned by another attempt",
                )
                .unwrap();
            }
            let vault = SecretVault::open_in(&root);
            let generated = Rc::new(RefCell::new(0));
            let observed = generated.clone();
            let error = with_key_hook(
                move |point| {
                    if point == KeyPoint::Generate {
                        *observed.borrow_mut() += 1;
                    }
                    assert_ne!(point, KeyPoint::Encrypt);
                    Ok(())
                },
                || vault.try_save(),
            )
            .unwrap_err();
            assert!(error.contains("no key winner exists"), "{error}");
            assert_eq!(*generated.borrow(), 1);
            assert!(!dir.join("vault.key").exists());
            assert!(!dir.join("vault.enc").exists());
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 32);
        }
        "race" => {
            let token = std::env::var("TILO_VAULT_PROCESS_TOKEN").unwrap();
            let mut vault = SecretVault::open_in(&root);
            vault.put(&AccountId::from("race"), &Secret::password("shared-winner"));
            let publications = Rc::new(RefCell::new(0));
            let count = publications.clone();
            let barrier_root = root.clone();
            with_observer(
                move |event| {
                    if event.checkpoint == C::BeforePublish {
                        *count.borrow_mut() += 1;
                        if *count.borrow() == 1 {
                            fs::write(barrier_root.join(format!("ready-{token}")), b"ready")
                                .unwrap();
                            wait_file(&barrier_root.join("release"));
                        } else if token == "two" {
                            // Race key selection, then serialize unrelated ciphertext replacements.
                            wait_file(&barrier_root.join("first-saved"));
                        }
                    }
                },
                || vault.try_save(),
            )
            .unwrap();
            if std::env::var("TILO_VAULT_PROCESS_TOKEN").unwrap() == "one" {
                fs::write(root.join("first-saved"), b"saved").unwrap();
            }
            assert_eq!(
                *publications.borrow(),
                2,
                "each process must attempt key and ciphertext publication"
            );
            assert_eq!(
                SecretVault::open_in(&root)
                    .get(&AccountId::from("race"))
                    .unwrap()
                    .password,
                "shared-winner"
            );
        }
        "restart-data" | "restart-name" | "restart-success" => {
            let action = std::env::var("TILO_VAULT_PROCESS_ACTION").unwrap();
            let failure = match action.as_str() {
                "restart-data" => Some(E::BeforeDataFlush),
                "restart-name" => Some(E::BeforeNamespaceFlush),
                _ => None,
            };
            let seen = Rc::new(RefCell::new(Vec::new()));
            let visits = seen.clone();
            let vault = SecretVault::open_in(&root);
            let result = with_existing_hooks(
                failure,
                move |point| visits.borrow_mut().push(point),
                || vault.try_save(),
            );
            assert_eq!(result.is_err(), failure.is_some());
            assert!(seen.borrow().contains(&E::BeforeDataFlush));
            if failure != Some(E::BeforeDataFlush) {
                assert!(seen.borrow().contains(&E::BeforeNamespaceFlush));
            }
        }
        "service-success" | "service-invalid" | "service-lookup-failure" => {
            let action = std::env::var("TILO_VAULT_PROCESS_ACTION").unwrap();
            let vault = SecretVault {
                dir,
                entries: BTreeMap::from([("service".into(), Secret::password("service-password"))]),
                custody: SecretCustody::SecretService,
                load_error: None,
            };
            let result = with_key_hook(
                move |point| {
                    if action == "service-lookup-failure" {
                        panic!("lookup failure reached local key at {point:?}");
                    }
                    if action == "service-invalid" {
                        assert!(!matches!(point, KeyPoint::Generate | KeyPoint::Encrypt));
                        if std::env::var("TILO_VAULT_KEY_FAULT")
                            .is_ok_and(|fault| fault == format!("{point:?}"))
                        {
                            fs::write(
                                PathBuf::from(std::env::var("TILO_VAULT_PROCESS_ROOT").unwrap())
                                    .join("read-fault-visited"),
                                b"visited",
                            )
                            .unwrap();
                            return Err(std::io::Error::other(
                                "injected service migration key I/O",
                            ));
                        }
                    }
                    Ok(())
                },
                || vault.try_save(),
            );
            let success = std::env::var("TILO_VAULT_PROCESS_ACTION").unwrap() == "service-success";
            assert_eq!(result.is_ok(), success, "{result:?}");
        }
        action => panic!("unknown child action {action}"),
    }
}

#[test]
fn vault_legacy_layout_decrypts_saves_without_changing_directory_or_key() {
    let root = fixture();
    let dir = root.dir();
    let key = fs::read(dir.join("vault.key")).unwrap();
    let key_id = identity(&dir.join("vault.key"));
    let dir_id = identity(&dir);
    assert_eq!(
        SecretVault::open_in(&root.0)
            .get(&AccountId::from("old"))
            .unwrap()
            .password,
        "old-password"
    );
    save(&root).unwrap();
    let reopened = SecretVault::open_in(&root.0);
    assert_eq!(
        reopened.get(&AccountId::from("old")).unwrap().password,
        "old-password"
    );
    assert_eq!(
        reopened.get(&AccountId::from("new")).unwrap().password,
        "new-password"
    );
    assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o777, 0o755);
    assert_eq!(identity(&dir), dir_id);
    assert_eq!(identity(&dir.join("vault.key")), key_id);
    assert_eq!(fs::read(dir.join("vault.key")).unwrap(), key);
    assert_eq!(&fs::read(dir.join("vault.enc")).unwrap()[..5], b"QASV\x01");
}

#[test]
fn vault_missing_key_with_ciphertext_never_generates_on_load_or_save() {
    let root = fixture();
    let ciphertext = fs::read(root.dir().join("vault.enc")).unwrap();
    fs::remove_file(root.dir().join("vault.key")).unwrap();
    assert!(SecretVault::open_in(&root.0).is_empty());
    assert!(
        !root.dir().join("vault.key").exists(),
        "load generated a key"
    );
    assert!(save(&root).is_err());
    assert!(!root.dir().join("vault.key").exists());
    assert_eq!(fs::read(root.dir().join("vault.enc")).unwrap(), ciphertext);
}

#[test]
fn vault_private_invalid_key_is_never_used_or_replaced() {
    let root = fixture();
    let key_path = root.dir().join("vault.key");
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();
    let key_id = identity(&key_path);
    let ciphertext = fs::read(root.dir().join("vault.enc")).unwrap();
    assert!(SecretVault::open_in(&root.0).is_empty());
    assert!(save(&root).is_err());
    assert_eq!(identity(&key_path), key_id);
    assert_eq!(fs::read(&key_path).unwrap(), vec![7; KEY_LEN]);
    assert_eq!(fs::metadata(&key_path).unwrap().mode() & 0o777, 0o644);
    assert_eq!(fs::read(root.dir().join("vault.enc")).unwrap(), ciphertext);
}

#[test]
fn vault_existing_key_requires_actual_file_and_name_barriers() {
    for checkpoint in [E::BeforeDataFlush, E::BeforeNamespaceFlush] {
        let root = fixture();
        let ciphertext = fs::read(root.dir().join("vault.enc")).unwrap();
        let error = with_existing_hooks(Some(checkpoint), |_| {}, || save(&root));
        assert!(error.is_err(), "existing key bypassed {checkpoint:?}");
        assert_eq!(fs::read(root.dir().join("vault.enc")).unwrap(), ciphertext);
    }
}

#[test]
fn vault_existing_root_requires_full_ancestry_barriers() {
    let root = fixture();
    let dir = fs::canonicalize(root.dir()).unwrap();
    let ciphertext = fs::read(dir.join("vault.enc")).unwrap();
    assert!(with_provision_failure(P::BeforeSync, dir.parent().unwrap(), || save(&root)).is_err());
    assert_eq!(fs::read(dir.join("vault.enc")).unwrap(), ciphertext);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyPoint {
    Open,
    Identity,
    Read,
    Generate,
    Encrypt,
    CiphertextQuery,
}
type KeyHook = Rc<dyn Fn(KeyPoint) -> std::io::Result<()>>;
thread_local! { static KEY_HOOK: RefCell<Option<KeyHook>> = RefCell::new(None); }
pub(super) fn key_checkpoint(point: KeyPoint) -> std::io::Result<()> {
    let hook = KEY_HOOK.with(|hook| hook.borrow().clone());
    if let Some(hook) = hook {
        hook(point)?;
    }
    Ok(())
}
struct RestoreKeyHook(Option<KeyHook>);
impl Drop for RestoreKeyHook {
    fn drop(&mut self) {
        KEY_HOOK.with(|hook| *hook.borrow_mut() = self.0.take());
    }
}
fn with_key_hook<T>(
    hook: impl Fn(KeyPoint) -> std::io::Result<()> + 'static,
    run: impl FnOnce() -> T,
) -> T {
    let _restore = RestoreKeyHook(KEY_HOOK.with(|old| old.replace(Some(Rc::new(hook)))));
    run()
}

#[test]
fn vault_injected_open_query_read_failures_never_generate_or_publish() {
    for point in [KeyPoint::Open, KeyPoint::Identity, KeyPoint::Read] {
        let root = fixture();
        let key_id = identity(&root.dir().join("vault.key"));
        let ciphertext = fs::read(root.dir().join("vault.enc")).unwrap();
        let visited = Rc::new(RefCell::new(false));
        let seen = visited.clone();
        let result = with_key_hook(
            move |p| {
                assert!(!matches!(p, KeyPoint::Generate | KeyPoint::Encrypt));
                if p == point {
                    *seen.borrow_mut() = true;
                    Err(std::io::Error::other("injected held key I/O"))
                } else {
                    Ok(())
                }
            },
            || {
                with_observer(
                    |_| panic!("key read failure published bytes"),
                    || save(&root),
                )
            },
        );
        assert!(result.is_err());
        assert!(
            *visited.borrow(),
            "actual key reader did not visit {point:?}"
        );
        assert_eq!(identity(&root.dir().join("vault.key")), key_id);
        assert_eq!(fs::read(root.dir().join("vault.enc")).unwrap(), ciphertext);
    }
}

#[test]
fn vault_rejects_malformed_and_nonregular_keys_without_effects() {
    use std::os::unix::fs::symlink;
    for shape in [
        "empty",
        "short",
        "long",
        "oversized",
        "unreadable",
        "symlink",
        "hardlink",
        "directory",
        "fifo",
    ] {
        let root = fixture();
        let key = root.dir().join("vault.key");
        let ciphertext = fs::read(root.dir().join("vault.enc")).unwrap();
        match shape {
            "empty" | "short" | "long" | "oversized" => {
                let length = match shape {
                    "empty" => 0,
                    "short" => 31,
                    "long" => 33,
                    _ => 1_000_000,
                };
                fs::write(&key, vec![7; length]).unwrap();
            }
            // A mode-only denial is ineffective when the test runs as root.
            "unreadable" => {}
            "hardlink" => fs::hard_link(&key, root.dir().join("other-name")).unwrap(),
            "symlink" => {
                fs::rename(&key, root.dir().join("real-key")).unwrap();
                symlink("real-key", &key).unwrap();
            }
            "directory" => {
                fs::remove_file(&key).unwrap();
                fs::create_dir(&key).unwrap();
            }
            "fifo" => {
                fs::remove_file(&key).unwrap();
                use std::os::unix::ffi::OsStrExt;
                let path = std::ffi::CString::new(key.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            }
            _ => unreachable!(),
        }
        let before = fs::symlink_metadata(&key).unwrap();
        let read_error_seen = Rc::new(RefCell::new(false));
        let read_error_observer = read_error_seen.clone();
        with_key_hook(
            move |point| {
                assert!(!matches!(point, KeyPoint::Generate | KeyPoint::Encrypt));
                if shape == "unreadable" && point == KeyPoint::Read {
                    *read_error_observer.borrow_mut() = true;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "injected held key read denial",
                    ));
                }
                Ok(())
            },
            || {
                assert!(SecretVault::open_in(&root.0).is_empty(), "{shape}");
                assert!(
                    with_observer(|_| panic!("invalid key published"), || save(&root)).is_err(),
                    "{shape}"
                );
            },
        );
        if shape == "unreadable" {
            assert!(
                *read_error_seen.borrow(),
                "actual key reader was not reached"
            );
        }
        let after = fs::symlink_metadata(&key).unwrap();
        assert_eq!(
            (after.dev(), after.ino(), after.mode(), after.len()),
            (before.dev(), before.ino(), before.mode(), before.len())
        );
        assert_eq!(fs::read(root.dir().join("vault.enc")).unwrap(), ciphertext);
    }
}

#[test]
fn vault_load_is_read_only_and_first_launch_is_private() {
    let root = fixture();
    with_observer(
        |_| panic!("load published"),
        || {
            with_provision_observer(
                |_| panic!("load provisioned/committed"),
                || {
                    with_existing_hooks(
                        None,
                        |_| panic!("load committed a key"),
                        || {
                            assert!(!SecretVault::open_in(&root.0).is_empty());
                        },
                    )
                },
            )
        },
    );
    let fresh = Root::new();
    assert!(SecretVault::open_in(&fresh.0).is_empty());
    assert!(!fresh.0.exists());
    save(&fresh).unwrap();
    for directory in [&fresh.0, &fresh.0.join("accounts"), &fresh.dir()] {
        assert_eq!(fs::metadata(directory).unwrap().mode() & 0o777, 0o700);
    }
    assert_eq!(
        fs::metadata(fresh.dir().join("vault.key")).unwrap().mode() & 0o777,
        0o600
    );
    assert_eq!(
        SecretVault::open_in(&fresh.0)
            .get(&AccountId::from("new"))
            .unwrap()
            .password,
        "new-password"
    );
}

#[test]
fn vault_uncertain_ciphertext_query_is_not_initialization() {
    let root = fixture();
    fs::remove_file(root.dir().join("vault.key")).unwrap();
    fs::remove_file(root.dir().join("vault.enc")).unwrap();
    let failed = Rc::new(RefCell::new(false));
    let observed = failed.clone();
    assert!(
        with_key_hook(
            move |point| {
                assert!(!matches!(point, KeyPoint::Generate | KeyPoint::Encrypt));
                if point == KeyPoint::CiphertextQuery {
                    *observed.borrow_mut() = true;
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "injected query denial",
                    ))
                } else {
                    Ok(())
                }
            },
            || save(&root)
        )
        .is_err()
    );
    assert!(*failed.borrow());
    assert!(!root.dir().join("vault.key").exists());
    assert!(!root.dir().join("vault.enc").exists());
    fs::create_dir(root.dir().join("vault.enc")).unwrap();
    assert!(save(&root).is_err());
    assert!(!root.dir().join("vault.key").exists());
}

#[test]
fn vault_key_publication_phases_retry_only_known_commit_and_preserve_restart() {
    for point in [C::BeforePublish, C::AfterPublish, C::BeforeCommit] {
        let root = Root::new();
        let result = with_failure(point, || save(&root));
        let error = result.unwrap_err();
        assert!(
            error.contains(match point {
                C::BeforePublish => "NotPublished",
                C::AfterPublish => "PublicationUnknown",
                _ => "PublishedDurabilityUnknown",
            }),
            "{error}"
        );
        assert!(!root.dir().join("vault.enc").exists());
        assert_eq!(
            root.dir().join("vault.key").exists(),
            point != C::BeforePublish
        );
        let key_before = root.dir().join("vault.key").exists().then(|| {
            (
                identity(&root.dir().join("vault.key")),
                fs::read(root.dir().join("vault.key")).unwrap(),
            )
        });
        save(&root).unwrap();
        if let Some((id, bytes)) = key_before {
            assert_eq!(identity(&root.dir().join("vault.key")), id);
            assert_eq!(fs::read(root.dir().join("vault.key")).unwrap(), bytes);
        }
    }
    let root = Root::new();
    with_failure_on_nth(C::BeforeCommit, 1, || save(&root)).unwrap();
    assert!(!SecretVault::open_in(&root.0).is_empty());
}

#[test]
fn vault_equal_bytes_replacement_and_namespace_change_during_commit_fail() {
    for mutation in ["key", "root", "mode"] {
        let root = fixture();
        let dir = root.dir();
        let changed = Rc::new(RefCell::new(false));
        let flag = changed.clone();
        let original_ciphertext = fs::read(dir.join("vault.enc")).unwrap();
        let altered_dir = dir.clone();
        let result = with_existing_hooks(
            None,
            move |_| {
                if *flag.borrow() {
                    return;
                }
                *flag.borrow_mut() = true;
                match mutation {
                    "key" => {
                        fs::rename(
                            altered_dir.join("vault.key"),
                            altered_dir.join("original-key"),
                        )
                        .unwrap();
                        let mut file = fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .open(altered_dir.join("vault.key"))
                            .unwrap();
                        std::io::Write::write_all(&mut file, &[7; KEY_LEN]).unwrap();
                    }
                    "root" => {
                        fs::rename(&altered_dir, altered_dir.with_file_name("moved-vault"))
                            .unwrap();
                        fs::create_dir(&altered_dir).unwrap();
                    }
                    "mode" => fs::set_permissions(&altered_dir, fs::Permissions::from_mode(0o777))
                        .unwrap(),
                    _ => unreachable!(),
                }
            },
            || {
                with_key_hook(
                    |point| {
                        assert_ne!(point, KeyPoint::Encrypt);
                        Ok(())
                    },
                    || save(&root),
                )
            },
        );
        assert!(result.is_err(), "{mutation}");
        assert!(*changed.borrow());
        if mutation == "root" {
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        } else {
            assert_eq!(
                fs::read(dir.join("vault.enc")).unwrap(),
                original_ciphertext
            );
        }
        if mutation == "mode" {
            assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o777, 0o777);
        }
    }
}

#[test]
fn vault_owned_key_retry_revalidates_namespace_before_retry() {
    let root = Root::new();
    let dir = root.dir();
    let seen = Rc::new(RefCell::new(0));
    let visited = seen.clone();
    let changed_dir = dir.clone();
    let result = with_observer(
        move |event| {
            if event.checkpoint == C::BeforeCommit {
                *visited.borrow_mut() += 1;
                fs::set_permissions(&changed_dir, fs::Permissions::from_mode(0o777)).unwrap();
            }
        },
        || with_failure(C::BeforeCommit, || save(&root)),
    );
    let error = result.unwrap_err();
    assert!(error.contains("namespace"), "{error}");
    assert!(
        error.contains("PublishedDurabilityUnknown"),
        "namespace refusal must preserve the already-published key phase: {error}"
    );
    assert_eq!(*seen.borrow(), 1);
    assert!(dir.join("vault.key").exists());
    assert!(!dir.join("vault.enc").exists());
    assert_eq!(fs::metadata(dir).unwrap().mode() & 0o777, 0o777);
}

#[test]
fn vault_ciphertext_final_commit_failure_preserves_phase_and_visible_bytes() {
    let root = fixture();
    let key_id = identity(&root.dir().join("vault.key"));
    let error = with_failure(C::BeforeCommit, || save(&root)).unwrap_err();
    assert!(
        error.contains("ciphertext publication PublishedDurabilityUnknown"),
        "{error}"
    );
    assert_eq!(identity(&root.dir().join("vault.key")), key_id);
    assert!(
        SecretVault::open_in(&root.0)
            .get(&AccountId::from("new"))
            .is_some()
    );
}

fn wait_file(path: &Path) {
    let start = std::time::Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "process rendezvous timed out: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn vault_two_process_initialization_selects_one_immutable_durable_winner() {
    let root = fixture();
    fs::remove_file(root.dir().join("vault.key")).unwrap();
    fs::remove_file(root.dir().join("vault.enc")).unwrap();
    let mut first = ChildGuard(
        child(&root, "race")
            .env("TILO_VAULT_PROCESS_TOKEN", "one")
            .spawn()
            .unwrap(),
    );
    let mut second = ChildGuard(
        child(&root, "race")
            .env("TILO_VAULT_PROCESS_TOKEN", "two")
            .spawn()
            .unwrap(),
    );
    wait_file(&root.0.join("ready-one"));
    wait_file(&root.0.join("ready-two"));
    fs::write(root.0.join("release"), b"go").unwrap();
    assert!(first.0.wait().unwrap().success());
    assert!(second.0.wait().unwrap().success());
    let key_id = identity(&root.dir().join("vault.key"));
    let key_bytes = fs::read(root.dir().join("vault.key")).unwrap();
    assert_eq!(key_bytes.len(), KEY_LEN);
    assert_eq!(
        SecretVault::open_in(&root.0)
            .get(&AccountId::from("race"))
            .unwrap()
            .password,
        "shared-winner"
    );
    save(&root).unwrap();
    assert_eq!(identity(&root.dir().join("vault.key")), key_id);
    assert_eq!(fs::read(root.dir().join("vault.key")).unwrap(), key_bytes);
}

#[test]
fn vault_dropped_key_error_fresh_process_repeats_actual_existing_barriers() {
    for point in [C::AfterPublish, C::BeforeCommit] {
        let root = Root::new();
        assert!(with_failure(point, || save(&root)).is_err());
        let key_id = identity(&root.dir().join("vault.key"));
        let key_bytes = fs::read(root.dir().join("vault.key")).unwrap();
        for action in ["restart-data", "restart-name"] {
            assert!(child(&root, action).status().unwrap().success());
            assert!(!root.dir().join("vault.enc").exists());
        }
        assert!(child(&root, "restart-success").status().unwrap().success());
        assert!(root.dir().join("vault.enc").exists());
        assert_eq!(identity(&root.dir().join("vault.key")), key_id);
        assert_eq!(fs::read(root.dir().join("vault.key")).unwrap(), key_bytes);
    }
}

#[test]
fn vault_secret_service_migration_uses_validated_local_reader_and_refuses_lookup_failure() {
    for (action, shape) in [
        ("service-success", "valid"),
        ("service-invalid", "mode"),
        ("service-invalid", "short"),
        ("service-invalid", "Open"),
        ("service-invalid", "Identity"),
        ("service-invalid", "Read"),
        ("service-lookup-failure", "valid"),
    ] {
        let root = fixture();
        let bin = root.0.join("bin");
        fs::create_dir(&bin).unwrap();
        let tool = bin.join("secret-tool");
        fs::write(&tool, r#"#!/bin/sh
case "$1" in
lookup)
  if [ "$TILO_VAULT_PROCESS_ACTION" = service-lookup-failure ]; then echo denied >&2; exit 1; fi
  if [ -f "$TILO_VAULT_PROCESS_ROOT/remote-key" ]; then cat "$TILO_VAULT_PROCESS_ROOT/remote-key"; else exit 1; fi
  ;;
store) cat > "$TILO_VAULT_PROCESS_ROOT/remote-key" ;;
*) exit 1 ;;
esac
"#).unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
        let original = fs::read(root.dir().join("vault.enc")).unwrap();
        if shape == "mode" {
            fs::set_permissions(
                root.dir().join("vault.key"),
                fs::Permissions::from_mode(0o644),
            )
            .unwrap();
        } else if shape == "short" {
            fs::write(root.dir().join("vault.key"), [7; 31]).unwrap();
        }
        let key_id = identity(&root.dir().join("vault.key"));
        assert!(
            child(&root, action)
                .env("TILO_VAULT_KEY_FAULT", shape)
                .env("PATH", format!("{}:/bin:/usr/bin", bin.display()))
                .status()
                .unwrap()
                .success()
        );
        if matches!(shape, "Open" | "Identity" | "Read") {
            assert!(root.0.join("read-fault-visited").exists());
        }
        if action == "service-success" {
            assert!(!root.dir().join("vault.key").exists());
            let remote = fs::read_to_string(root.0.join("remote-key")).unwrap();
            assert_eq!(decode_key(&remote).unwrap().as_ref(), &[7; KEY_LEN]);
            let plaintext = open(
                &[7; KEY_LEN],
                &fs::read(root.dir().join("vault.enc")).unwrap(),
            )
            .unwrap();
            assert!(String::from_utf8_lossy(&plaintext).contains("service-password"));
        } else {
            assert!(!root.0.join("remote-key").exists());
            assert_eq!(identity(&root.dir().join("vault.key")), key_id);
            assert_eq!(fs::read(root.dir().join("vault.enc")).unwrap(), original);
        }
    }
}

#[test]
fn vault_equal_bytes_replacement_between_read_and_commit_is_refused() {
    let root = fixture();
    let path = root.dir().join("vault.key");
    let old_id = identity(&path);
    let old_ciphertext = fs::read(root.dir().join("vault.enc")).unwrap();
    let reads = Rc::new(RefCell::new(0));
    let count = reads.clone();
    let admissions = Rc::new(RefCell::new(0));
    let admission_count = admissions.clone();
    // try_save first authenticates existing ciphertext, then admits the held
    // key for commit. Replace that second admission's name after its byte read.
    let vault = SecretVault::open_in(&root.0);
    let result = with_key_hook(
        move |point| {
            assert_ne!(point, KeyPoint::Encrypt);
            if point == KeyPoint::Identity {
                *admission_count.borrow_mut() += 1;
            }
            if point == KeyPoint::Read && *admission_count.borrow() == 2 {
                *count.borrow_mut() += 1;
                if *count.borrow() == 2 {
                    fs::rename(&path, path.with_file_name("original-key")).unwrap();
                    let mut replacement = fs::OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .mode(0o600)
                        .open(&path)
                        .unwrap();
                    std::io::Write::write_all(&mut replacement, &[7; KEY_LEN]).unwrap();
                }
            }
            Ok(())
        },
        || vault.try_save(),
    );
    assert!(result.unwrap_err().contains("existing key commit"));
    assert_eq!(*admissions.borrow(), 2);
    assert_eq!(*reads.borrow(), 2);
    assert_ne!(identity(&root.dir().join("vault.key")), old_id);
    assert_eq!(
        fs::read(root.dir().join("vault.key")).unwrap(),
        vec![7; KEY_LEN]
    );
    assert_eq!(
        fs::read(root.dir().join("vault.enc")).unwrap(),
        old_ciphertext
    );
}

#[test]
fn vault_root_alias_replacement_during_ancestry_commit_never_rebases() {
    use std::os::unix::fs::symlink;
    let root = fixture();
    let target = root.dir().with_file_name("original-vault");
    fs::rename(root.dir(), &target).unwrap();
    symlink("original-vault", root.dir()).unwrap();
    let replacement = target.with_file_name("replacement-vault");
    fs::create_dir(&replacement).unwrap();
    let original = fs::read(target.join("vault.enc")).unwrap();
    let done = Rc::new(RefCell::new(false));
    let changed = done.clone();
    let alias = root.dir();
    let result = with_provision_observer(
        move |event| {
            if event.checkpoint == P::BeforeSync && !*changed.borrow() {
                *changed.borrow_mut() = true;
                fs::remove_file(&alias).unwrap();
                symlink("replacement-vault", &alias).unwrap();
            }
        },
        || save(&root),
    );
    assert!(result.is_err());
    assert!(*done.borrow());
    assert_eq!(fs::read_dir(replacement).unwrap().count(), 0);
    assert_eq!(fs::read(target.join("vault.enc")).unwrap(), original);
}

#[test]
fn vault_namespace_rejection_has_no_effect_and_reconciliation_visits_all_ancestors() {
    for mode in [0o775, 0o757] {
        let root = fixture();
        fs::set_permissions(root.dir(), fs::Permissions::from_mode(mode)).unwrap();
        let key_id = identity(&root.dir().join("vault.key"));
        let ciphertext = fs::read(root.dir().join("vault.enc")).unwrap();
        let result = with_observer(
            |_| panic!("namespace rejection published"),
            || with_provision_observer(|_| panic!("namespace rejection committed"), || save(&root)),
        );
        assert!(result.is_err());
        assert_eq!(fs::metadata(root.dir()).unwrap().mode() & 0o777, mode);
        assert_eq!(identity(&root.dir().join("vault.key")), key_id);
        assert_eq!(fs::read(root.dir().join("vault.enc")).unwrap(), ciphertext);
    }
    let root = fixture();
    let physical = fs::canonicalize(root.dir()).unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let visited = seen.clone();
    with_provision_observer(
        move |event| {
            assert!(!matches!(event.checkpoint, P::BeforeCreate | P::Created));
            if event.checkpoint == P::Synced {
                visited.borrow_mut().push(event.path.clone());
            }
        },
        || save(&root),
    )
    .unwrap();
    assert_eq!(
        *seen.borrow(),
        physical.ancestors().map(Path::to_owned).collect::<Vec<_>>()
    );
}

#[test]
fn vault_failed_private_provision_preserves_effects_and_fresh_admission_repeats_barriers() {
    let root = Root::new();
    // Resolve the existing temp ancestor; requested descendants do not yet exist.
    let physical = fs::canonicalize(root.0.parent().unwrap())
        .unwrap()
        .join(root.0.file_name().unwrap());
    let error = with_provision_failure(P::BeforeSync, &physical, || save(&root)).unwrap_err();
    assert!(error.contains("CreatedDurabilityUnknown"), "{error}");
    assert!(root.dir().is_dir());
    assert_eq!(fs::read_dir(root.dir()).unwrap().count(), 0);
    assert!(
        with_provision_failure(P::BeforeSync, &physical, || save(&root))
            .unwrap_err()
            .contains("existing ancestry")
    );
    assert_eq!(fs::read_dir(root.dir()).unwrap().count(), 0);
    save(&root).unwrap();
    assert!(!SecretVault::open_in(&root.0).is_empty());
}

#[test]
fn vault_staging_name_exhaustion_is_not_a_key_winner() {
    let root = Root::new();
    assert!(child(&root, "stage-exhaustion").status().unwrap().success());
}

#[test]
fn vault_persistent_owned_commit_fault_is_bounded_and_never_encrypts() {
    let root = Root::new();
    let seen = Rc::new(RefCell::new(0));
    let visited = seen.clone();
    let error = with_key_hook(
        |point| {
            assert_ne!(point, KeyPoint::Encrypt);
            Ok(())
        },
        || {
            with_observer(
                move |event| {
                    if event.checkpoint == C::BeforeCommit {
                        *visited.borrow_mut() += 1;
                    }
                },
                || with_failure(C::BeforeCommit, || save(&root)),
            )
        },
    )
    .unwrap_err();
    assert!(
        error.contains("key commit retry PublishedDurabilityUnknown"),
        "{error}"
    );
    assert_eq!(*seen.borrow(), 2, "initial commit and one owned retry");
    assert!(root.dir().join("vault.key").exists());
    assert!(!root.dir().join("vault.enc").exists());
}
