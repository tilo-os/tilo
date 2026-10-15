//! Genuine source-GPT/kernel-peer/encrypted capsule authority regression.
//! This qualifies increment A only; no Files/native scoped launch is claimed.
#[path = "retained/installed_receiver.rs"]
mod installed_receiver;
#[path = "retained/native_staging.rs"]
mod native_staging;
#[path = "retained/selected_grant.rs"]
mod selected_grant;
use super::*;
use crate::setup::{
    home_capture::{RetainedCallerDeclaration, RetainedSourceAuthority},
    migration_handoff::{self, Categories, HomePolicy},
    portable_transfer::{ExportReview, Purpose},
    prepared_identity::PreparedIdentity,
    prepared_setup::{self, Manifest, PreparedSetupReview, RetainedHomePlan},
};
use std::os::fd::AsFd;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use zeroize::Zeroizing;
const PASSWORD: &[u8] = b"owned retained source fixture installation words";
fn capsule(
    root: &Path,
    identity: &PreparedIdentity,
    plan: &Plan,
    declaration: RetainedCallerDeclaration,
    suffix: &str,
) -> PreparedSetupReview {
    let staging = root.join(format!("staging-{suffix}"));
    std::fs::create_dir(&staging).unwrap();
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700)).unwrap();
    identity
        .export(
            &staging.join(prepared_setup::IDENTITY_FILE),
            Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
        )
        .unwrap();
    let mut manifest = Manifest::new(
        "retained-source-qualification".into(),
        identity.public_key(),
        vec![],
    )
    .unwrap();
    manifest.retained_home = Some(RetainedHomePlan::new(plan.clone()).unwrap());
    manifest.retained_caller = Some(declaration);
    manifest.write(&staging).unwrap();
    let path = root.join(format!("{suffix}.sealed"));
    ExportReview::review(
        &staging,
        &path,
        Purpose::PreparedSetup,
        prepared_setup::LIMITS,
    )
    .unwrap()
    .export(Zeroizing::new(PASSWORD.to_vec()), &AtomicBool::new(false))
    .unwrap();
    PreparedSetupReview::open(
        &path,
        PASSWORD,
        &root.join(format!("quarantine-{suffix}")),
        &AtomicBool::new(false),
    )
    .unwrap()
}
fn partition(plan: &Plan) -> String {
    let devices = block_devices::Lsblk::new("NAME,TYPE,PARTUUID,PTTYPE")
        .full_paths(true)
        .list()
        .unwrap();
    let parts: Vec<_> = devices
        .iter()
        .filter(|disk| disk.kind == "disk" && disk.pttype.as_deref() == Some("gpt"))
        .flat_map(|disk| &disk.children)
        .filter(|part| {
            part.kind == "part"
                && part
                    .partuuid
                    .as_deref()
                    .is_some_and(|id| id.eq_ignore_ascii_case(&plan.home.volume.partition_guid))
        })
        .collect();
    assert_eq!(
        parts.len(),
        1,
        "unique actual direct GPT partition required"
    );
    let part = parts[0];
    part.name.clone()
}
const SOURCE_CHILD: &str =
    "setup::home_capture::store::original::native_tests::retained::original_retained_input_child";
#[test]
fn original_retained_input_child() {
    let Ok(descriptor) = std::env::var("TILO_RETAINED_SOURCE_FIXTURE_SOCKET") else {
        return;
    };
    use std::os::fd::FromRawFd;
    // SAFETY: this fixture's parent transfers exactly this owned socket.
    let mut stream = unsafe { UnixStream::from_raw_fd(descriptor.parse().unwrap()) };
    runtime_portal::process_binding::complete(&mut stream).unwrap();
    // Crucially opened AFTER the real original UID/groups transition. Root
    // cannot provide an ambiently opened Home FD as original consent evidence.
    let source = PinnedDirectory::open_absolute(&PathBuf::from(
        std::env::var_os("TILO_RETAINED_SOURCE_FIXTURE_HOME").unwrap(),
    ))
    .unwrap();
    runtime_portal::fd_transport::send_with_fd(stream.as_fd(), &[4], source.file.as_fd()).unwrap();
    let mut ack = [0];
    std::io::Read::read_exact(&mut stream, &mut ack).unwrap();
    assert_eq!(ack, [4]);
    let mut command = [0];
    std::io::Read::read_exact(&mut stream, &mut command).unwrap();
    assert_eq!(command, [7]);
}
/// The real PAM source login receiver spawns this owned bridge child with
/// the original init-created bootstrap. No source grant is constructed here
/// until that real UID has reopened Home and normal capture is requested.
#[test]
fn original_composite_capture_child() {
    if std::env::var("TILO_RETAINED_SOURCE_CAPTURE_FD")
        .ok()
        .as_deref()
        != Some("87")
    {
        return;
    }
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;
    assert_eq!(rustix::process::getuid().as_raw(), 1000);
    assert_eq!(rustix::process::geteuid().as_raw(), 1000);
    let main = crate::setup::qualification_registration::AdmittedMain::from_receiver().unwrap();
    let mut stream = unsafe { UnixStream::from_raw_fd(87) };
    runtime_portal::process_binding::complete(&mut stream).unwrap();
    // This private readiness endpoint belongs to the actual source receiver
    // that owns this Child. It is not an authentication result or public flag.
    let mut ready = unsafe { File::from_raw_fd(92) };
    ready.write_all(&[92]).unwrap();
    drop(ready);
    let (tag, fd) = runtime_portal::fd_transport::recv_with_fd(stream.as_fd()).unwrap();
    assert_eq!(tag, [75]);
    let held = File::from(fd.expect("actual original Home descriptor"));
    let path = std::fs::read_link(format!("/proc/self/fd/{}", held.as_raw_fd())).unwrap();
    let reopened = PinnedDirectory::open_absolute(&path).unwrap();
    let observed = held.metadata().unwrap();
    let current = reopened.file.metadata().unwrap();
    assert!(observed.is_dir());
    assert_eq!(
        (
            observed.dev(),
            observed.ino(),
            observed.uid(),
            observed.gid(),
            observed.mode()
        ),
        (
            current.dev(),
            current.ino(),
            current.uid(),
            current.gid(),
            current.mode()
        )
    );
    assert_eq!(current.uid(), 1000);
    assert_eq!(current.gid(), 1001);
    assert_eq!(current.mode() & 0o7777, 0o700);
    drop(held);
    let mut header = [0; 5];
    stream.read_exact(&mut header).unwrap();
    assert_eq!(header[0], 77);
    let length = u32::from_le_bytes(header[1..].try_into().unwrap()) as usize;
    assert!((1..=65536).contains(&length));
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    let installation: crate::setup::staging::plan::Plan = serde_json::from_slice(&bytes).unwrap();
    let source =
        crate::setup::flatpak_transfer::portable::native_tests::RunnableFixture::create_in(&path);
    assert_eq!(source.source_home(), path);
    let inventory = crate::setup::flatpak_transfer::discover(
        &crate::setup::flatpak_transfer::NativeRunner,
        &path,
    )
    .unwrap();
    let selected: Vec<_> = inventory
        .refs
        .into_iter()
        .filter(|reference| {
            reference.installation == crate::setup::flatpak_transfer::Installation::User
                && reference.reference == "app/org.tilo.Fixture/x86_64/stable"
        })
        .collect();
    assert_eq!(selected.len(), 1);
    let directory = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(std::env::var_os("XDG_RUNTIME_DIR").expect("actual source PAM runtime"))
        .unwrap();
    let grant = selected_grant::SelectedGrant::capture(
        &main,
        &path,
        directory.path(),
        selected,
        &installation,
    )
    .unwrap();
    let _checked = grant.review(&directory.path().join("checked")).unwrap();
    let package = grant.package().unwrap();
    runtime_portal::fd_transport::send_with_fd(stream.as_fd(), &[76], package.as_fd()).unwrap();
    let mut ack = [0];
    stream.read_exact(&mut ack).unwrap();
    assert_eq!(ack, [76]);
    main.check().unwrap();
    // Every source owner stays alive until actual root copied and acknowledged
    // the encrypted package; cleanup then releases this source epoch normally.
    drop(package);
    drop(grant);
    drop(directory);
    drop(source);
    drop(reopened);
}
fn source_peer(
    home: &Path,
    groups: [libc::gid_t; 3],
) -> (
    Peer,
    HostChannel,
    super::super::super::super::transport::RetainedHomeInput,
) {
    let (host, app) = UnixStream::pair().unwrap();
    let executable = File::open(std::env::current_exe().unwrap()).unwrap();
    let fds = [app.as_raw_fd(), executable.as_raw_fd()];
    let mut command = Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
    command
        .args(["--exact", SOURCE_CHILD, "--nocapture"])
        .env(
            "TILO_RETAINED_SOURCE_FIXTURE_SOCKET",
            app.as_raw_fd().to_string(),
        )
        .env("TILO_RETAINED_SOURCE_FIXTURE_HOME", home);
    // SAFETY: fixed child-only inherited descriptors and credential syscalls.
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
    let mut channel = HostChannel::authenticate(host, child.0.id(), 1000).unwrap();
    let input = channel.receive_retained_home().unwrap();
    (child, channel, input)
}
#[test]
fn retained_groups_require_actual_peer_protected_record_and_owned_rw_epoch() {
    assert_eq!(
        rustix::process::getuid().as_raw(),
        0,
        "privileged isolated native qualification required"
    );
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    let mut receiver = installed_receiver::Receiver::from_inherited().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let mount = PathBuf::from(
        std::env::var_os("TILO_ORIGINAL_SOURCE_GPT_STORE")
            .expect("explicit owned direct GPT native subject required"),
    );
    let marker =
        std::fs::symlink_metadata(mount.join(".tilo-original-source-qualification")).unwrap();
    assert!(marker.is_file());
    assert_eq!(marker.uid(), 0);
    assert_eq!(marker.nlink(), 1);
    assert_eq!(marker.mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::read(mount.join(".tilo-original-source-qualification")).unwrap(),
        b"disposable-original-source-only"
    );
    let directory = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&mount)
        .unwrap();
    let relative = directory.path().strip_prefix(&mount).unwrap().to_path_buf();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o711)).unwrap();
    let home = directory.path().join("alice");
    std::fs::create_dir(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home_c = std::ffi::CString::new(home.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(home_c.as_ptr(), 1000, 1001) }, 0);
    std::fs::write(home.join("document"), b"unchanged original contents").unwrap();
    let original_metadata = std::fs::metadata(home.join("document")).unwrap();
    if receiver.is_some() {
        let editable = home.join("receiver-document.txt");
        std::fs::write(&editable, b"original shared document\n").unwrap();
        let name = std::ffi::CString::new(editable.as_os_str().as_bytes()).unwrap();
        // SAFETY: owned disposable source fixture and genuine original UID/GID.
        assert_eq!(unsafe { libc::chown(name.as_ptr(), 1000, 1001) }, 0);
        std::fs::set_permissions(&editable, std::fs::Permissions::from_mode(0o640)).unwrap();
    }
    if let Some(owner) = receiver
        .as_mut()
        .filter(|owner| owner.has_selected_registration())
    {
        let selected = owner.selected_package(&home).unwrap();
        let plan = selected
            .review
            .manifest()
            .retained_home
            .as_ref()
            .unwrap()
            .as_plan()
            .clone();
        assert_eq!(plan.home.original_uid(), 1000);
        let public_key = selected.review.manifest().identity_public_key;
        // Neither source account/session nor a source mount descriptor may
        // remain after the receiver's observed source completion boundary.
        let source_partition = partition(&plan);
        let daemon = block_devices::udisks::Udisks::system().unwrap();
        let _owned_source_directory = directory.keep();
        daemon.unmount(&source_partition).unwrap();
        assert!(
            daemon
                .filesystem(&source_partition)
                .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
                .unwrap()
                .is_empty()
        );
        receiver
            .take()
            .unwrap()
            .publish(
                &selected.review,
                &public_key,
                &plan,
                &selected.sealed,
                PASSWORD,
            )
            .unwrap();
        return;
    }
    let records_root = directory.path().join("var/lib/tilo-imager/home-capture");
    std::fs::create_dir_all(&records_root).unwrap();
    std::fs::set_permissions(&records_root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let mut plan = migration_handoff::capture(
        &home,
        &operation,
        Categories {
            home: true,
            flatpak: false,
            accounts: false,
        },
    )
    .unwrap();
    plan.home_policy = HomePolicy::AccessReadWrite;
    let (mut child, mut channel, input) = source_peer(&home, [1002, 1002, 1003]);
    let mut store = Store::open(
        PinnedDirectory::open_absolute(&records_root).unwrap(),
        &operation,
    )
    .unwrap();
    store.records_path = Some(records_root.join("records"));
    let target = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let identity = PreparedIdentity::create(
        &target.path().join("identity"),
        "Owned original person",
        [1, 2, 3],
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let declaration = store
        .register_retained_caller(
            (&home, &input),
            &plan,
            &channel,
            "retained-source-qualification",
            &identity.public_key(),
        )
        .unwrap();
    let record_path = records_root
        .join("records")
        .join(format!("{operation}.retained-caller.json"));
    let record = std::fs::read(&record_path).unwrap();
    let metadata = std::fs::metadata(&record_path).unwrap();
    assert_eq!(
        store
            .register_retained_caller(
                (&home, &input),
                &plan,
                &channel,
                "retained-source-qualification",
                &identity.public_key()
            )
            .unwrap(),
        declaration
    );
    assert_eq!(
        std::fs::metadata(&record_path).unwrap().ino(),
        metadata.ino()
    );
    assert_eq!(std::fs::read(&record_path).unwrap(), record);
    let (mut foreign, mut foreign_channel, foreign_input) = source_peer(&home, [1002, 1002, 1004]);
    assert!(
        store
            .register_retained_caller(
                (&home, &foreign_input),
                &plan,
                &foreign_channel,
                "retained-source-qualification",
                &identity.public_key()
            )
            .is_err()
    );
    assert!(
        store
            .register_retained_caller(
                (&home, &input),
                &plan,
                &foreign_channel,
                "retained-source-qualification",
                &identity.public_key()
            )
            .is_err(),
        "one live peer cannot substitute another peer's transferred Home descriptor"
    );
    foreign_channel.stream().write_all(&[7]).unwrap();
    assert!(foreign.0.wait().unwrap().success());
    assert!(
        store
            .register_original_selection(&home, &plan, &channel)
            .is_err(),
        "readonly Copy producer cannot register writable credentials"
    );
    let review = capsule(
        target.path(),
        &identity,
        &plan,
        declaration.clone(),
        "genuine",
    );
    let mut forged = serde_json::to_value(&declaration).unwrap();
    forged["original"]["supplementary_gids"] = serde_json::json!([1002, 1004]);
    let forged: RetainedCallerDeclaration = serde_json::from_value(forged).unwrap();
    let counterfeit = capsule(target.path(), &identity, &plan, forged, "forged-groups");
    let mut unknown = serde_json::to_value(&declaration).unwrap();
    unknown["grant"] = serde_json::json!(true);
    assert!(serde_json::from_value::<RetainedCallerDeclaration>(unknown).is_err());
    channel.stream().write_all(&[7]).unwrap();
    assert!(child.0.wait().unwrap().success());
    assert!(
        store
            .register_retained_caller(
                (&home, &input),
                &plan,
                &channel,
                "retained-source-qualification",
                &identity.public_key()
            )
            .is_err(),
        "exited kernel peer cannot certify a new retained caller"
    );
    drop(store);
    drop(input);
    drop(foreign_input);
    drop(channel);
    drop(child);
    let source_partition = partition(&plan);
    let daemon = block_devices::udisks::Udisks::system().unwrap();
    // No leaked TempDir path can trigger cleanup under a different remount.
    let _owned_source_directory = directory.keep();
    daemon.unmount(&source_partition).unwrap();
    assert!(
        daemon
            .filesystem(&source_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty()
    );
    assert!(
        RetainedSourceAuthority::review(
            &counterfeit,
            &identity.public_key(),
            &AtomicBool::new(false)
        )
        .is_err(),
        "a password-valid capsule cannot substitute groups for protected root source facts"
    );
    assert!(
        daemon
            .filesystem(&source_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty()
    );
    assert!(RetainedSourceAuthority::review(&review, &[7; 32], &AtomicBool::new(false)).is_err());
    // Actual authenticated root publication creates the installed continuation;
    // no password, identity private material or user receipt becomes authority.
    if let Some(receiver) = receiver {
        receiver
            .publish(
                &review,
                &identity.public_key(),
                &plan,
                &target.path().join("genuine.sealed"),
                PASSWORD,
            )
            .unwrap();
        return;
    }
    let installed_root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/run")
        .unwrap();
    let installed_namespace = installed_root.path().join("var/lib/tilo");
    std::fs::create_dir_all(&installed_namespace).unwrap();
    std::fs::set_permissions(&installed_namespace, std::fs::Permissions::from_mode(0o700)).unwrap();
    review
        .publish_enrolled_system(&installed_namespace, PASSWORD)
        .unwrap();
    review
        .publish_enrolled_system(&installed_namespace, PASSWORD)
        .unwrap();
    assert!(
        counterfeit
            .publish_enrolled_system(&installed_namespace, PASSWORD)
            .is_err(),
        "immutable installed continuation refuses even a password-valid changed declaration"
    );
    assert!(!installed_namespace.join("var/lib/tilo").exists());
    let installed = prepared_setup::AuthenticatedInstalledRetained::open_fixture(
        installed_root.path(),
        &operation,
    )
    .unwrap();
    let installed_authority = RetainedSourceAuthority::review_installed(
        Arc::clone(&installed),
        &identity.public_key(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        installed_authority.original().supplementary_gids(),
        &[1002, 1003]
    );
    let installed_projection = installed_authority
        .projection(&AtomicBool::new(false))
        .unwrap();
    // Derive independently cancellable source leases from the genuine owned
    // RW epoch, without remounting or manufacturing an existing-mount grant.
    let fresh_installed = || {
        prepared_setup::AuthenticatedInstalledRetained::open_fixture(
            installed_root.path(),
            &operation,
        )
        .unwrap()
    };
    assert!(
        installed_projection
            .derive_installed(fresh_installed(), &[7; 32], &AtomicBool::new(false),)
            .is_err()
    );
    assert!(
        installed_projection
            .derive_installed(
                fresh_installed(),
                &identity.public_key(),
                &AtomicBool::new(true),
            )
            .is_err()
    );
    installed_authority
        .revalidate(&AtomicBool::new(false))
        .unwrap();
    let first_editor = installed_projection
        .derive_installed(
            fresh_installed(),
            &identity.public_key(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let second_editor = installed_projection
        .derive_installed(
            fresh_installed(),
            &identity.public_key(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let original_root = installed_projection.directory().metadata().unwrap();
    let fresh_projection = second_editor.projection(&AtomicBool::new(false)).unwrap();
    let fresh_root = fresh_projection.directory().metadata().unwrap();
    assert_eq!(
        (original_root.dev(), original_root.ino()),
        (fresh_root.dev(), fresh_root.ino())
    );
    assert_eq!(first_editor.original(), installed_authority.original());
    assert_eq!(second_editor.original(), installed_authority.original());
    installed_authority.revoke();
    assert!(
        installed_projection
            .revalidate(&AtomicBool::new(false))
            .is_err()
    );
    first_editor.revalidate(&AtomicBool::new(false)).unwrap();
    second_editor.revalidate(&AtomicBool::new(false)).unwrap();
    drop(installed_projection);
    installed_authority.retire().unwrap();
    assert_eq!(
        daemon
            .filesystem(&source_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .len(),
        1,
        "Files lease retirement must not unmount a live editor epoch"
    );
    first_editor.revoke();
    first_editor.retire().unwrap();
    fresh_projection
        .revalidate(&AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        daemon
            .filesystem(&source_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .len(),
        1,
        "one editor retirement must not unmount its sibling"
    );
    let installed_authority = second_editor;
    let installed_projection = fresh_projection;
    let installed_record = installed_root
        .path()
        .join("var/lib/tilo/retained-sources")
        .join(format!("{operation}.json"));
    let saved_installed = installed_record.with_extension("saved");
    let installed_bytes = std::fs::read(&installed_record).unwrap();
    std::fs::rename(&installed_record, &saved_installed).unwrap();
    std::fs::write(&installed_record, &installed_bytes).unwrap();
    std::fs::set_permissions(&installed_record, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        installed_authority
            .revalidate(&AtomicBool::new(false))
            .is_err(),
        "live source lease retains exact installed continuation inode custody"
    );
    std::fs::remove_file(&installed_record).unwrap();
    std::fs::rename(&saved_installed, &installed_record).unwrap();
    assert!(
        installed_authority
            .revalidate(&AtomicBool::new(false))
            .is_err(),
        "restoring installed inode cannot revive a revoked source epoch"
    );
    assert!(RetainedSourceAuthority::retire(Arc::clone(&installed_authority)).is_err());
    drop(installed_projection);
    RetainedSourceAuthority::retire(installed_authority).unwrap();
    assert!(
        daemon
            .filesystem(&source_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty()
    );
    let authority =
        RetainedSourceAuthority::review(&review, &identity.public_key(), &AtomicBool::new(false))
            .unwrap();
    assert_eq!(authority.original().uid(), 1000);
    assert_eq!(authority.original().primary_gid(), 1001);
    assert_eq!(authority.original().supplementary_gids(), &[1002, 1003]);
    let projection = authority.projection(&AtomicBool::new(false)).unwrap();
    let source_document = format!(
        "/proc/self/fd/{}/document",
        projection.directory().as_raw_fd()
    );
    assert_eq!(
        std::fs::read(&source_document).unwrap(),
        b"unchanged original contents"
    );
    let after = std::fs::metadata(&source_document).unwrap();
    assert_eq!(
        (after.dev(), after.ino(), after.uid(), after.gid()),
        (
            original_metadata.dev(),
            original_metadata.ino(),
            original_metadata.uid(),
            original_metadata.gid()
        )
    );
    // Root fixture mutates only its own protected source record. A matching
    // replacement cannot preserve an already issued lease's inode custody.
    let mounted_record = PathBuf::from(format!(
        "/proc/self/fd/{}/../var/lib/tilo-imager/home-capture/records/{operation}.retained-caller.json",
        projection.directory().as_raw_fd(),
    ));
    let saved_record = mounted_record.with_extension("saved");
    std::fs::rename(&mounted_record, &saved_record).unwrap();
    std::fs::write(&mounted_record, &record).unwrap();
    std::fs::set_permissions(&mounted_record, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        authority.revalidate(&AtomicBool::new(false)).is_err(),
        "matching bytes in a different root-owned inode cannot replace active record custody"
    );
    std::fs::remove_file(&mounted_record).unwrap();
    std::fs::rename(&saved_record, &mounted_record).unwrap();
    assert!(
        authority.revalidate(&AtomicBool::new(false)).is_err(),
        "a restored inode cannot revive an observed revoked source epoch"
    );
    // This proves custody, not old-group app operations (increment B/C).
    assert!(
        authority.clone().retire().is_err(),
        "live projection prevents positive mount retirement"
    );
    assert!(
        !daemon
            .filesystem(&source_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty()
    );
    authority.revoke();
    assert!(projection.revalidate(&AtomicBool::new(false)).is_err());
    drop(projection);
    authority.retire().unwrap();
    assert!(
        daemon
            .filesystem(&source_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    assert_eq!(rustix::process::getegid().as_raw(), 0);
    assert!(
        migration_handoff::retain_access(&plan, migration_handoff::AccessMode::ReadWrite).is_err(),
        "ordinary root retained access still cannot assume original UID/group authority"
    );
    assert!(
        daemon
            .filesystem(&source_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty()
    );
    // Cleanup only the held, explicitly disposable source fixture through UDisks.
    let mounted = daemon.mount(&source_partition).unwrap();
    std::fs::remove_dir_all(Path::new(&mounted).join(relative)).unwrap();
    daemon.unmount(&source_partition).unwrap();
    assert!(
        daemon
            .filesystem(&source_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty()
    );
}
