//! Exact native GPT source/peer/protected record; unavailable subject FAILS.
use super::*;
use std::{
    io::Write,
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, PermissionsExt},
            net::UnixStream,
            process::CommandExt,
        },
    },
    process::{Child, Command},
};

struct Peer(Child);
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        self.0.wait().expect("reap original fixture peer");
    }
}
struct ReadOnly(PathBuf);
impl ReadOnly {
    fn enter(path: &Path) -> Self {
        remount(path, true);
        Self(path.to_path_buf())
    }
}
impl Drop for ReadOnly {
    fn drop(&mut self) {
        remount(&self.0, false);
    }
}
fn remount(path: &Path, readonly: bool) {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: root qualification's expressly supplied disposable native GPT
    // mount only; no source discovery selects or modifies a host filesystem.
    assert_eq!(
        unsafe {
            libc::mount(
                std::ptr::null(),
                path.as_ptr(),
                std::ptr::null(),
                libc::MS_REMOUNT | if readonly { libc::MS_RDONLY } else { 0 },
                std::ptr::null(),
            )
        },
        0,
        "actual disposable remount required: {}",
        io::Error::last_os_error()
    );
}
fn peer(groups: [libc::gid_t; 3]) -> (Peer, HostChannel) {
    let (host, app) = UnixStream::pair().unwrap();
    let executable = File::open(std::env::current_exe().unwrap()).unwrap();
    let fds = [app.as_raw_fd(), executable.as_raw_fd()];
    let mut command = Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
    command
        .args([
            "--exact",
            "setup::home_capture::original_caller::tests::original_caller_fixture_child",
            "--nocapture",
        ])
        .env(
            "TILO_ORIGINAL_CALLER_FIXTURE_FD",
            app.as_raw_fd().to_string(),
        );
    // SAFETY: child-only async-signal-safe exact inherited FDs/credentials.
    unsafe {
        command.pre_exec(move || {
            for fd in fds {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            if libc::setgroups(groups.len(), groups.as_ptr()) != 0
                || libc::setgid(1001) != 0
                || libc::setuid(1000) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = Peer(command.spawn().unwrap());
    let channel = HostChannel::authenticate(host, child.0.id(), 1000).unwrap();
    (child, channel)
}
#[test]
fn actual_same_gpt_source_record_reopens_ext4_xfs_peer_facts_and_refuses_tampering() {
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "privileged isolated qualifier required"
    );
    // Unlike live fallback's whole-loop filesystem fixture, this must be an
    // actual uniquely visible GPT `disk`/`part` source admitted by production.
    // The root coordinator must provision it explicitly; absence is failure.
    let mount = PathBuf::from(
        std::env::var_os("TILO_ORIGINAL_SOURCE_GPT_STORE")
            .expect("disposable production-admissible GPT source mount required"),
    );
    let marker = mount.join(".tilo-original-source-qualification");
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        b"disposable-original-source-only"
    );
    assert_eq!(std::fs::metadata(&marker).unwrap().uid(), 0);
    assert_eq!(std::fs::metadata(&marker).unwrap().mode() & 0o777, 0o600);
    let temporary = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&mount)
        .unwrap();
    let home = temporary.path().join("alice");
    std::fs::create_dir(&home).unwrap();
    let home_c = std::ffi::CString::new(home.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(home_c.as_ptr(), 1000, 1001) }, 0);
    std::fs::write(home.join(".hidden"), b"original complete user bytes").unwrap();
    let root = temporary.path().join("var/lib/tilo-imager/home-capture");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let plan = crate::setup::migration_handoff::capture(
        &home,
        &operation,
        crate::setup::migration_handoff::Categories {
            home: true,
            flatpak: false,
            accounts: false,
        },
    )
    .expect("actual kernel mount+lsblk GPT source proof");
    assert!(matches!(
        plan.home.volume.filesystem,
        crate::setup::migration_handoff::Filesystem::Ext4
            | crate::setup::migration_handoff::Filesystem::Xfs
    ));
    let (mut child, mut channel) = peer([1002, 1002, 1003]);
    let mut store =
        Store::open(PinnedDirectory::open_absolute(&root).unwrap(), &operation).unwrap();
    // Test-private injection of the Store's OWN real path, never source facts
    // or a locator to a second filesystem; production Store.system fixes it.
    store.records_path = Some(root.join("records"));
    let intent = store
        .register_original_selection(&home, &plan, &channel)
        .unwrap();
    let record_path = root.join("records").join(format!("{operation}.json"));
    let original = std::fs::read(&record_path).unwrap();
    let before = std::fs::metadata(&record_path).unwrap();
    let repeated = store
        .register_original_selection(&home, &plan, &channel)
        .unwrap();
    assert_eq!(
        repeated, intent,
        "lost-ack retry must return the same selection"
    );
    assert_eq!(repeated.witness().unwrap(), intent.witness().unwrap());
    let after = std::fs::metadata(&record_path).unwrap();
    assert_eq!(
        (before.ino(), before.mtime(), before.ctime()),
        (after.ino(), after.mtime(), after.ctime()),
        "matching retry must not replace or write the record"
    );
    assert_eq!(std::fs::read(&record_path).unwrap(), original);
    let (mut changed_peer, mut changed_channel) = peer([1002, 1002, 1004]);
    assert!(
        store
            .register_original_selection(&home, &plan, &changed_channel)
            .is_err(),
        "freshly admitted different kernel groups cannot reuse this record"
    );
    assert_eq!(std::fs::read(&record_path).unwrap(), original);
    changed_channel.stream().write_all(&[7]).unwrap();
    assert!(changed_peer.0.wait().unwrap().success());
    let mut altered: serde_json::Value = serde_json::from_slice(&original).unwrap();
    altered["original"]["supplementary_gids"] = serde_json::json!([1002, 1004]);
    let altered_bytes = serde_json::to_vec(&altered).unwrap();
    std::fs::write(&record_path, &altered_bytes).unwrap();
    assert!(
        store
            .register_original_selection(&home, &plan, &channel)
            .is_err(),
        "tampered protected bytes must never be repaired by registration retry"
    );
    assert_eq!(std::fs::read(&record_path).unwrap(), altered_bytes);
    std::fs::write(&record_path, &original).unwrap();
    let separate = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    assert_ne!(
        std::fs::metadata(separate.path()).unwrap().dev(),
        std::fs::metadata(&home).unwrap().dev(),
        "fixture requires an independently backed outside filesystem"
    );
    let foreign_root = separate.path().join("var/lib/tilo-imager/home-capture");
    std::fs::create_dir_all(&foreign_root).unwrap();
    std::fs::set_permissions(&foreign_root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut foreign = Store::open(
        PinnedDirectory::open_absolute(&foreign_root).unwrap(),
        &operation,
    )
    .unwrap();
    foreign.records_path = Some(foreign_root.join("records"));
    assert!(
        foreign
            .register_original_selection(&home, &plan, &channel)
            .is_err(),
        "a record on another filesystem must not certify this actual source"
    );
    assert!(
        !foreign_root
            .join("records")
            .join(format!("{operation}.json"))
            .exists()
    );
    drop(foreign);
    let namespace = intent.namespace.decode().unwrap();
    drop(store);
    for case in 0..4 {
        if case == 1 {
            let mut changed = intent.clone();
            changed.home.operation_id = uuid::Uuid::new_v4().to_string();
            std::fs::write(&record_path, serde_json::to_vec(&changed).unwrap()).unwrap();
        } else if case == 2 {
            std::fs::write(&record_path, &original).unwrap();
            std::fs::set_permissions(&record_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        } else if case == 3 {
            std::fs::set_permissions(&record_path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let path = std::ffi::CString::new(record_path.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::chown(path.as_ptr(), 1000, 1001) }, 0);
        }
        let readonly = ReadOnly::enter(&mount);
        let source = crate::setup::migration_handoff::resolve(&plan, &mount)
            .expect("actual readonly source without writable alias");
        assert!(
            source
                .open_original_record(Path::new("../other-records"))
                .is_err()
        );
        assert!(
            source
                .open_original_record(Path::new("/var/lib/tilo-imager/home-capture/records"))
                .is_err()
        );
        let result = SourceReadContext::review(source, &namespace);
        if case == 0 {
            let context = result.unwrap();
            let witness = intent.witness().unwrap();
            witness.validate_plan(&plan).unwrap();
            context
                .verify_requested(&plan, &witness, context.original())
                .unwrap();
            let mut forged_facts = serde_json::to_value(context.original()).unwrap();
            forged_facts["primary_gid"] = serde_json::json!(1006);
            let forged_facts: OriginalCallerFacts = serde_json::from_value(forged_facts).unwrap();
            forged_facts.validate(plan.home_uid()).unwrap();
            assert!(
                context
                    .verify_requested(&plan, &witness, &forged_facts)
                    .is_err(),
                "valid password-request shape must not replace actual protected source credentials"
            );
            let mut different_selection = plan.clone();
            different_selection.categories.accounts = !different_selection.categories.accounts;
            assert!(
                witness.validate_plan(&different_selection).is_err(),
                "the same operation must not authorize different selected categories"
            );
            let reopened = crate::setup::migration_handoff::resolve(&plan, &mount).unwrap();
            SourceReadContext::review_witness(reopened, &witness).unwrap();
            let mut altered = witness.clone();
            altered.record_sha256[0] ^= 1;
            let reopened = crate::setup::migration_handoff::resolve(&plan, &mount).unwrap();
            assert!(SourceReadContext::review_witness(reopened, &altered).is_err());
            assert_eq!(context.original().uid(), 1000);
            assert_eq!(context.original().primary_gid(), 1001);
            assert_eq!(context.original().supplementary_gids(), &[1002, 1003]);
            assert_eq!(context.plan(), &plan);
            context.revalidate().unwrap();
        } else {
            assert!(
                result.is_err(),
                "tampered op/privacy/owner cannot become original source evidence"
            );
        }
        drop(readonly);
    }
    let path = std::ffi::CString::new(record_path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(path.as_ptr(), 0, 0) }, 0);
    std::fs::set_permissions(&record_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&record_path, original).unwrap();
    assert_eq!(
        std::fs::read(home.join(".hidden")).unwrap(),
        b"original complete user bytes"
    );
    channel.stream().write_all(&[7]).unwrap();
    assert!(child.0.wait().unwrap().success());
}

pub mod output;

#[test]
fn actual_large_original_inventory_registers_and_reopens_protected_source_custody() {
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "privileged isolated qualifier required"
    );
    let mount = PathBuf::from(
        std::env::var_os("TILO_ORIGINAL_SOURCE_GPT_STORE").expect("disposable GPT source required"),
    );
    let marker = mount.join(".tilo-original-source-qualification");
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        b"disposable-original-source-only"
    );
    assert_eq!(std::fs::metadata(&marker).unwrap().uid(), 0);
    let temporary = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&mount)
        .unwrap();
    let home = temporary.path().join("alice");
    std::fs::create_dir(&home).unwrap();
    let home_c = std::ffi::CString::new(home.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(home_c.as_ptr(), 1000, 1001) }, 0);
    std::fs::write(home.join("document"), b"all user data unchanged").unwrap();
    let root = temporary.path().join("var/lib/tilo-imager/home-capture");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let mut plan = crate::setup::migration_handoff::capture(
        &home,
        &operation,
        crate::setup::migration_handoff::Categories {
            home: true,
            flatpak: true,
            accounts: false,
        },
    )
    .unwrap();
    // Actual canonical Handoff parser accepts public metadata beyond64KiB.
    // No provider credentials, fake deployments or native exporter is needed
    // to qualify original registration and protected source custody itself.
    let remotes:Vec<_>=(0..1024).map(|index|serde_json::json!({"installation":{"System":"system"},"name":format!("public-{index}"),"url":format!("https://example.invalid/{}", "p".repeat(128)),"options":""})).collect();
    plan.flatpak_inventory = Some(
        serde_json::json!({"schema":1,"inventory":{"refs":[],"remotes":remotes},"selected":[],"installations":[]}),
    );
    plan.validate().unwrap();
    assert!(
        plan.bytes().is_err(),
        "ordinary inline bound must remain unchanged"
    );
    let (compact, bytes) = plan.original_inventory_selection().unwrap();
    let bytes = bytes.unwrap();
    assert!(bytes.len() > crate::setup::migration_handoff::MAX_BYTES);
    assert!(compact.bytes().unwrap().len() < crate::setup::migration_handoff::MAX_BYTES);
    let file_path = temporary.path().join("caller-inventory.json");
    std::fs::write(&file_path, &bytes).unwrap();
    std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let file_c = std::ffi::CString::new(file_path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(file_c.as_ptr(), 1000, 1001) }, 0);
    let original = File::open(&file_path).unwrap();
    let (mut child, mut channel) = peer([1002, 1002, 1003]);
    let mut store =
        Store::open(PinnedDirectory::open_absolute(&root).unwrap(), &operation).unwrap();
    store.records_path = Some(root.join("records"));
    let intent = store
        .register_original_selection_with_inventory(
            &home,
            &compact,
            &channel,
            None,
            Some(&original),
        )
        .unwrap();
    let witness = intent.witness().unwrap();
    witness.validate_plan(&compact).unwrap();
    let inventory_path = root
        .join("records")
        .join(format!("{operation}.flatpak-inventory.json"));
    let before = std::fs::metadata(&inventory_path).unwrap();
    assert_eq!(before.uid(), 0);
    assert_eq!(before.nlink(), 1);
    assert_eq!(std::fs::read(&inventory_path).unwrap(), bytes);
    let repeated = store
        .register_original_selection_with_inventory(
            &home,
            &compact,
            &channel,
            None,
            Some(&original),
        )
        .unwrap();
    assert_eq!(repeated.witness().unwrap(), witness);
    assert_eq!(
        std::fs::metadata(&inventory_path).unwrap().ino(),
        before.ino()
    );
    assert!(
        store
            .register_original_selection_with_inventory(&home, &compact, &channel, None, None)
            .is_err()
    );
    // Genuine root registration without an inventory is distinct from a
    // protected canonical empty selection; the grant must reject it.
    drop(store);
    let mut missing_plan = compact.clone();
    missing_plan.operation_id = uuid::Uuid::new_v4().to_string();
    missing_plan.flatpak_inventory = None;
    let mut missing_store = Store::open(
        PinnedDirectory::open_absolute(&root).unwrap(),
        &missing_plan.operation_id,
    )
    .unwrap();
    missing_store.records_path = Some(root.join("records"));
    let missing_intent = missing_store
        .register_original_selection_with_inventory(&home, &missing_plan, &channel, None, None)
        .unwrap();
    let missing_witness = missing_intent.witness().unwrap();
    drop(missing_store);
    channel.stream().write_all(&[7]).unwrap();
    assert!(child.0.wait().unwrap().success());
    drop(channel);
    drop(child);
    drop(original);
    {
        let _readonly = ReadOnly::enter(&mount);
        let source = crate::setup::migration_handoff::resolve(&compact, &mount).unwrap();
        let context = SourceReadContext::review_witness(source, &witness).unwrap();
        let (held, len, digest) = context.original_flatpak_inventory().unwrap().unwrap();
        assert_eq!(len, bytes.len() as u64);
        assert_eq!(held.metadata().unwrap().ino(), before.ino());
        use sha2::Digest;
        assert_eq!(digest, <[u8; 32]>::from(sha2::Sha256::digest(&bytes)));
        context
            .verify_requested(&compact, &witness, intent.original_caller())
            .unwrap();
        let grant =
            crate::setup::flatpak_transfer::offline_original::OriginalInventoryGrant::review(
                &context,
            )
            .unwrap()
            .expect("authenticated empty inventory retains a grant");
        assert!(grant.inventory().selected().is_empty());
        grant.matches_plan(&compact).unwrap();
        assert!(
            crate::setup::flatpak_transfer::offline_original::OriginalFlatpakSources::review(
                &context
            )
            .unwrap()
            .is_none(),
            "Home carries user state; no invented system package"
        );
        let missing_source =
            crate::setup::migration_handoff::resolve(&missing_plan, &mount).unwrap();
        let missing_context =
            SourceReadContext::review_witness(missing_source, &missing_witness).unwrap();
        let error =
            match crate::setup::flatpak_transfer::offline_original::OriginalInventoryGrant::review(
                &missing_context,
            ) {
                Ok(_) => panic!("missing selected inventory must not become authenticated empty"),
                Err(error) => error,
            };
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("inventory"));
        assert_eq!(
            std::fs::read(home.join("document")).unwrap(),
            b"all user data unchanged"
        );
    }
    // Missing exact protected sibling is rejected before a source grant can
    // confuse the declared empty selection with absence of authority.
    let retained_inventory = inventory_path.with_extension("retained");
    std::fs::rename(&inventory_path, &retained_inventory).unwrap();
    {
        let _readonly = ReadOnly::enter(&mount);
        let source = crate::setup::migration_handoff::resolve(&compact, &mount).unwrap();
        assert!(SourceReadContext::review_witness(source, &witness).is_err());
    }
    std::fs::rename(&retained_inventory, &inventory_path).unwrap();
    assert_eq!(
        std::fs::metadata(&inventory_path).unwrap().ino(),
        before.ino()
    );
    // Same named inode with changed bytes must not reuse the authenticated
    // record witness when the source is mounted readonly again.
    let mut changed = bytes;
    changed[20] ^= 1;
    std::fs::write(&inventory_path, &changed).unwrap();
    {
        let _readonly = ReadOnly::enter(&mount);
        let source = crate::setup::migration_handoff::resolve(&compact, &mount).unwrap();
        assert!(SourceReadContext::review_witness(source, &witness).is_err());
    }
    assert_eq!(
        std::fs::read(home.join("document")).unwrap(),
        b"all user data unchanged"
    );
}

mod mounted;

#[path = "native_tests/retained.rs"]
mod retained;
