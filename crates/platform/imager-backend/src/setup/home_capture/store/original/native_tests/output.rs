//! Genuine selected GPT/UDisks transaction; all missing subjects fail.
use super::*;
use crate::setup::{
    home_capture::{
        OfflineReadAdmission,
        offline_output::{OfflineOutputTransaction, OwnedMediumOutput},
    },
    portable_transfer::{ExportReview, ImportReview, Purpose},
    prepared_identity::PreparedIdentity,
    prepared_setup::{
        self, Manifest, OfflineHomePlan, PortableHomePlan, PortableHomeSelection,
        PreparedSetupReview,
    },
};
use block_devices::{BlockDevice, Lsblk, udisks::Udisks};
use launcher::owned_service::ServiceCancellation;
use std::{
    fs,
    sync::{Arc, atomic::AtomicBool},
};
use tilo_identity_host::provisioning::IdentityStorage;
use zeroize::Zeroizing;

#[path = "output/candidate.rs"]
mod candidate;
#[path = "output/initialize.rs"]
mod initialize;
#[path = "output/mounted.rs"]
mod mounted;
#[path = "output/paired.rs"]
mod paired;
#[path = "output/paired_apps.rs"]
mod paired_apps;

const PASSWORD: &[u8] = b"several isolated synthetic qualification words";
const COLUMNS: &str =
    "NAME,TYPE,SIZE,START,FSTYPE,UUID,PARTUUID,PTUUID,PTTYPE,LOG-SEC,MAJ:MIN,MOUNTPOINTS";

fn inventory() -> Vec<BlockDevice> {
    Lsblk::new(COLUMNS)
        .full_paths(true)
        .list()
        .expect("actual kernel disk inventory required")
}
fn partition(guid: &str) -> String {
    let devices = inventory();
    let found: Vec<_> = devices
        .iter()
        .filter(|disk| disk.kind == "disk")
        .flat_map(|disk| disk.children.iter())
        .filter(|part| {
            part.kind == "part"
                && part
                    .partuuid
                    .as_deref()
                    .is_some_and(|id| id.eq_ignore_ascii_case(guid))
        })
        .collect();
    assert_eq!(
        found.len(),
        1,
        "actual unique direct GPT partition required"
    );
    let selected = found[0];
    selected.name.clone()
}
fn marker(path: &Path, value: &[u8]) {
    let metadata =
        fs::symlink_metadata(path).expect("coordinator-owned disposable marker required");
    assert!(metadata.is_file());
    assert_eq!(metadata.uid(), 0);
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    assert_eq!(fs::read(path).unwrap(), value);
}
fn read_member(directory: &File, name: &str) -> Vec<u8> {
    let mut options = cap_primitives::fs::OpenOptions::new();
    use cap_fs_ext::OpenOptionsFollowExt;
    options
        .read(true)
        .follow(cap_primitives::fs::FollowSymlinks::No);
    let mut file = cap_primitives::fs::open(directory, Path::new(name), &options).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    bytes
}

fn own_source(path: &Path) {
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::lchown(name.as_ptr(), 1000, 1001) }, 0);
    if fs::symlink_metadata(path).unwrap().is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            own_source(&entry.unwrap().path());
        }
    }
}

#[test]
fn signed_original_export_verifies_full_home_publishes_receipt_and_unmounts_owned_medium() {
    original_pipeline(OriginalSelection::NoApplications);
}

#[test]
fn signed_original_home_and_named_applications_complete_and_restore() {
    if paired::child_dispatch() {
        return;
    }
    original_pipeline(OriginalSelection::GuidedStory);
}

#[test]
fn signed_original_mounted_home_and_named_applications_complete_and_restore() {
    if paired::child_dispatch() {
        return;
    }
    original_pipeline(OriginalSelection::MountedSignedSystem);
}

#[test]
fn installed_original_candidate_prepares_once_qualifies_and_reattests_rootlessly() {
    if paired::child_dispatch() {
        return;
    }
    original_pipeline(OriginalSelection::InstalledCandidate);
}

#[test]
fn authenticated_empty_flatpak_inventory_admits_home_output_without_system_package() {
    original_pipeline(OriginalSelection::AuthenticatedEmpty);
}

#[test]
fn signed_original_named_applications_without_home_complete_and_restore() {
    if paired::child_dispatch() {
        return;
    }
    original_pipeline(OriginalSelection::SignedApplicationsOnly);
}

#[test]
fn signed_original_selected_user_applications_without_home_complete_and_restore() {
    if paired::child_dispatch() {
        return;
    }
    original_pipeline(OriginalSelection::SignedUserApplicationsOnly);
}

#[test]
fn installed_original6_user_applications_candidate_qualifies_and_reattests_rootlessly() {
    if paired::child_dispatch() {
        return;
    }
    original_pipeline(OriginalSelection::InstalledApplicationsCandidate);
}

#[derive(Clone, Copy)]
pub enum OriginalSelection {
    NoApplications,
    GuidedStory,
    SignedApplicationsOnly,
    SignedUserApplicationsOnly,
    MountedSignedSystem,
    InstalledCandidate,
    InstalledApplicationsCandidate,
    AuthenticatedEmpty,
}

fn original_pipeline(selection: OriginalSelection) {
    original_pipeline_with_receiver(selection, None);
}

/// The callback borrows the genuinely reopened completed medium after the
/// signed exporter/verifier and output namespace retirement have succeeded.
/// The enclosing GPT subjects, UDisks connection and scratch remain owned.
pub fn original_pipeline_with_receiver(
    selection: OriginalSelection,
    receiver: Option<paired::CompletedReceiver>,
) {
    assert!(
        receiver.is_none() || matches!(selection, OriginalSelection::GuidedStory),
        "first-use callback requires the full guided Home/application producer"
    );
    let mounted = matches!(
        selection,
        OriginalSelection::MountedSignedSystem
            | OriginalSelection::InstalledCandidate
            | OriginalSelection::InstalledApplicationsCandidate
    );
    let paired = matches!(
        selection,
        OriginalSelection::GuidedStory
            | OriginalSelection::SignedApplicationsOnly
            | OriginalSelection::SignedUserApplicationsOnly
            | OriginalSelection::MountedSignedSystem
            | OriginalSelection::InstalledCandidate
            | OriginalSelection::InstalledApplicationsCandidate
    );
    let applications_only = matches!(
        selection,
        OriginalSelection::SignedApplicationsOnly
            | OriginalSelection::SignedUserApplicationsOnly
            | OriginalSelection::InstalledApplicationsCandidate
    );
    let mut qualifier_env = tilo_test_env::ScopedEnv::new();
    if matches!(selection, OriginalSelection::GuidedStory) {
        qualifier_env.set("TILO_ORIGINAL_PAIRED_GUIDED_STORY", "1");
    }
    if applications_only {
        qualifier_env.set("TILO_ORIGINAL_PAIRED_APPLICATIONS_ONLY", "1");
    }
    if matches!(
        selection,
        OriginalSelection::SignedUserApplicationsOnly
            | OriginalSelection::InstalledApplicationsCandidate
    ) {
        qualifier_env.set("TILO_ORIGINAL_PAIRED_USER_APPLICATIONS", "1");
    }
    if matches!(selection, OriginalSelection::InstalledApplicationsCandidate) {
        qualifier_env.set("TILO_ORIGINAL_PAIRED_INSTALLED_APPS", "1");
    }
    if mounted {
        qualifier_env.set("TILO_ORIGINAL_PAIRED_MOUNTED", "1");
    }
    let flatpak = !matches!(selection, OriginalSelection::NoApplications);
    assert_eq!(
        rustix::process::getuid().as_raw(),
        0,
        "isolated privileged qualifier required"
    );
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    // No ambient disk selection: coordinator supplies explicitly disposable
    // kernel subjects (three for the mounted route). Production reattests every serialized fact.
    let source_mount = PathBuf::from(
        std::env::var_os("TILO_ORIGINAL_SOURCE_GPT_STORE")
            .expect("owned synthetic GPT source mount required"),
    );
    marker(
        &source_mount.join(".tilo-original-source-qualification"),
        b"disposable-original-source-only",
    );
    let output_disk = std::env::var("TILO_ORIGINAL_OUTPUT_GPT_DISK")
        .expect("owned synthetic selected output disk required");
    let target = crate::probe::probe()
        .expect("actual target discovery required")
        .into_iter()
        .find(|target| target.dev_path() == output_disk)
        .expect("selected synthetic disk must actually be discovered");
    assert!(!target.is_system);
    if mounted {
        mounted::preflight(&source_mount, &target);
    }
    if paired && std::env::var_os("TILO_ORIGINAL_INITIALIZE_OUTPUT").is_some() {
        initialize::output(&target, &source_mount);
    }
    let daemon = Udisks::system().expect("actual UDisks service required");
    let disk = daemon
        .open_read_only(&output_disk)
        .expect("actual selected block descriptor required");
    crate::write::attest_target_descriptor(&target, &disk).unwrap();
    let binding = crate::media_layout::binding(
        &disk,
        target.size,
        target
            .stable_id
            .as_deref()
            .expect("actual stable hardware identity required"),
    )
    .unwrap();
    let output_partition = partition(&binding.partition_guid);
    let output_points = daemon
        .filesystem(&output_partition)
        .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
        .unwrap();
    assert_eq!(
        output_points.len(),
        1,
        "coordinator's sole marker-verification mount required"
    );
    let output_mount = PathBuf::from(std::ffi::OsStr::from_bytes(
        output_points[0]
            .strip_suffix(&[0])
            .unwrap_or(&output_points[0]),
    ));
    marker(
        &output_mount.join(".tilo-original-output-qualification"),
        b"disposable-original-output-only",
    );
    assert_ne!(
        fs::metadata(&source_mount).unwrap().dev(),
        fs::metadata(&output_mount).unwrap().dev()
    );
    daemon
        .unmount(&output_partition)
        .expect("retire only explicitly marked output fixture mount");
    assert!(
        daemon
            .filesystem(&output_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty()
    );

    // Retain fixture files on its disposable source volume until the external
    // coordinator destroys that volume; do not unlink through a retired mount.
    let temporary = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(&source_mount)
        .unwrap();
    let namespace = temporary.keep();
    let signed_source = paired.then(|| {
        fs::set_permissions(&namespace, fs::Permissions::from_mode(0o755)).unwrap();
        let root = namespace.join("flatpak-fixture");
        fs::create_dir(&root).unwrap();
        crate::setup::flatpak_transfer::portable::native_tests::system::initialize_original_source(
            &root,
        )
    });
    let home = namespace.join("alice");
    fs::create_dir(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let home_c = std::ffi::CString::new(home.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(home_c.as_ptr(), 1000, 1001) }, 0);
    let hidden = home.join(".var/app/so.test/data");
    fs::create_dir_all(&hidden).unwrap();
    fs::write(hidden.join("document"), b"hidden full-home app bytes").unwrap();
    fs::hard_link(hidden.join("document"), home.join("same-document")).unwrap();
    own_source(&home);
    fs::write(
        home.join("group-only"),
        b"actual supplementary-group source bytes",
    )
    .unwrap();
    let group = std::ffi::CString::new(home.join("group-only").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(group.as_ptr(), 1003, 1002) }, 0);
    fs::set_permissions(home.join("group-only"), fs::Permissions::from_mode(0o640)).unwrap();
    // Source-owned writable sentinel: readonly projection refusal must not be
    // explained by another user's DAC protection.
    fs::write(
        home.join("owner-document"),
        b"unchanged original owner bytes",
    )
    .unwrap();
    let owned = std::ffi::CString::new(home.join("owner-document").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(owned.as_ptr(), 1000, 1001) }, 0);
    fs::set_permissions(
        home.join("owner-document"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let mut mounted_source =
        mounted.then(|| mounted::Subjects::new(&namespace, &home, &source_mount, &output_mount));
    let operation = uuid::Uuid::new_v4().to_string();
    let mut plan = crate::setup::migration_handoff::capture(
        &home,
        &operation,
        crate::setup::migration_handoff::Categories {
            home: !applications_only,
            flatpak,
            accounts: false,
        },
    )
    .unwrap();
    if applications_only {
        let app_state = home.join(".var/app/org.tilo.Fixture/data");
        fs::create_dir_all(&app_state).unwrap();
        fs::write(
            app_state.join("selected-document"),
            b"genuine selected application state",
        )
        .unwrap();
        paired_apps::selected_source_state(&home);
        own_source(&home.join(".var"));
        own_source(&home.join(".local"));
    }
    let captured = paired.then(|| paired::capture(&namespace, &home, &operation));
    if let Some(captured) = &captured {
        plan = captured.plan.clone();
    }
    if matches!(selection, OriginalSelection::AuthenticatedEmpty) {
        // Canonical empty metadata, backed by actual peer/Store registration.
        // No fake installed refs and no manufacture of an output proof.
        plan.flatpak_inventory = Some(serde_json::json!({
            "schema":1,"inventory":{"refs":[],"remotes":[]},
            "selected":[],"installations":[],
        }));
    }
    let full_plan = plan.clone();
    let (plan, protected_inventory) = plan.original_inventory_selection().unwrap();
    let source_partition = partition(&plan.home.volume.partition_guid);
    let root = namespace.join("var/lib/tilo-imager/home-capture");
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let (witness, caller, mounted_witness) = if let Some(subjects) = &mut mounted_source {
        let request = namespace.join("mounted-request");
        fs::create_dir(&request).unwrap();
        own_source(&request);
        fs::set_permissions(&request, fs::Permissions::from_mode(0o700)).unwrap();
        subjects.bind_store(&root);
        let (source, mounted, caller) =
            crate::setup::home_capture::rpc::mounted_native::register_for_paired(
                &home, &full_plan, &request,
            );
        (source, caller, Some(mounted))
    } else {
        let (mut child, mut channel) = peer([1002, 1002, 1003]);
        let mut store =
            Store::open(PinnedDirectory::open_absolute(&root).unwrap(), &operation).unwrap();
        store.records_path = Some(root.join("records"));
        let sidecar = protected_inventory.as_ref().map(|bytes| {
            let path = namespace.join("selected-inventory.json");
            fs::write(&path, bytes).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::chown(path_c.as_ptr(), 1000, 1001) }, 0);
            File::open(path).unwrap()
        });
        let registered = store
            .register_original_selection_with_inventory(
                &home,
                &plan,
                &channel,
                captured
                    .as_ref()
                    .and_then(|facts| facts.preparation.as_ref()),
                sidecar.as_ref(),
            )
            .unwrap();
        let witness = registered.witness().unwrap();
        let caller = registered.original_caller().clone();
        drop(store);
        channel.stream().write_all(&[7]).unwrap();
        assert!(child.0.wait().unwrap().success());
        drop(channel);
        drop(child);
        (witness, caller, None)
    };

    let scratch = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let staging = scratch.path().join("setup-stage");
    let mut manifest = if paired::guided_story() {
        let parent = PreparedSetupReview::open(
            &namespace.join("capture/guided-setup.sealed"),
            PASSWORD,
            &scratch.path().join("guided-parent"),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(parent.manifest().account_ids.len(), 2);
        assert!(parent.manifest().offline_home.is_none());
        let manifest = parent.manifest().clone();
        drop(parent);
        fs::rename(scratch.path().join("guided-parent/home"), &staging).unwrap();
        fs::remove_file(staging.join(prepared_setup::MANIFEST_FILE)).unwrap();
        manifest
    } else {
        let identity = PreparedIdentity::create(
            &scratch.path().join("identity"),
            "Synthetic offline qualifier",
            [1, 2, 3],
            IdentityStorage::Files,
        )
        .unwrap();
        fs::create_dir(&staging).unwrap();
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
        identity
            .export(
                &staging.join(prepared_setup::IDENTITY_FILE),
                Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
            )
            .unwrap();
        Manifest::new(
            "synthetic-original-export".into(),
            identity.public_key(),
            Vec::new(),
        )
        .unwrap()
    };
    let reservation = captured
        .as_ref()
        .map_or(16 * 1024 * 1024, |facts| facts.reservation);
    if applications_only {
        let mut draft = crate::imager_model::InstallProfile::default();
        draft.user.username = "fixture".into();
        draft.user.full_name = "Synthetic applications qualifier".into();
        manifest.install_profile =
            Some(crate::setup::prepared_setup::PublicInstallProfile::from_draft(&draft).unwrap());
    }
    if applications_only {
        manifest.offline_applications = Some(
            crate::setup::prepared_setup::OfflineApplicationsPlan::new(
                plan,
                manifest.setup_binding.clone(),
                manifest.identity_public_key,
                binding.clone(),
                caller,
                reservation,
            )
            .unwrap()
            .with_source_witness(witness)
            .unwrap(),
        );
        assert!(mounted_witness.is_none());
    } else {
        manifest.portable_home = Some(PortableHomePlan {
            schema: 1,
            selection: PortableHomeSelection::FullHome,
            applications: flatpak,
        });
        let mut offline = OfflineHomePlan::new(
            plan,
            manifest.setup_binding.clone(),
            manifest.identity_public_key,
            binding.clone(),
            caller,
            reservation,
        )
        .unwrap()
        .with_source_witness(witness)
        .unwrap();
        if let Some(mounted) = mounted_witness {
            offline = offline.with_mounted_witness(mounted).unwrap();
        }
        manifest.offline_home = Some(offline);
    }
    manifest.write(&staging).unwrap();
    let package = scratch.path().join("setup.sealed");
    ExportReview::review(
        &staging,
        &package,
        Purpose::PreparedSetup,
        prepared_setup::LIMITS,
    )
    .unwrap()
    .export(Zeroizing::new(PASSWORD.to_vec()), &AtomicBool::new(false))
    .unwrap();
    let setup = Arc::new(
        PreparedSetupReview::open(
            &package,
            PASSWORD,
            &scratch.path().join("authenticated"),
            &AtomicBool::new(false),
        )
        .unwrap(),
    );
    if flatpak {
        paired::publish_pending(
            &target,
            &disk,
            &binding,
            &full_plan,
            reservation,
            &package,
            mounted_source
                .as_ref()
                .map(|subject| subject.geometry(&operation)),
        );
    }
    drop(signed_source);
    let participant_partition = mounted_source.as_ref().map(|subject| subject.partition());
    if let Some(subject) = &mounted_source {
        subject.verify_original();
    }
    drop(mounted_source.take());
    if mounted {
        assert_eq!(
            fs::read(home.join("mounted-file")).unwrap(),
            b"underlying original file must not replace selected mount"
        );
    }
    if let Some(partition) = &participant_partition {
        daemon
            .unmount(partition)
            .expect("retire exact marked participant GPT source");
        assert!(
            daemon
                .filesystem(partition)
                .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
                .unwrap()
                .is_empty()
        );
    }
    daemon
        .unmount(&source_partition)
        .expect("retire only explicitly marked original fixture mount");
    assert!(
        daemon
            .filesystem(&source_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty()
    );
    // Qualification consumes the same genuinely prepared pending selection.
    // No output attempt/namespace is started by this test before the installed
    // controller independently reopens the protected records and capsule.
    if matches!(
        selection,
        OriginalSelection::InstalledCandidate | OriginalSelection::InstalledApplicationsCandidate
    ) {
        drop(setup);
        drop(scratch);
        candidate::qualify();
        assert!(
            daemon
                .filesystem(&source_partition)
                .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
                .unwrap()
                .is_empty()
        );
        assert!(
            daemon
                .filesystem(&output_partition)
                .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
                .unwrap()
                .is_empty()
        );
        assert!(
            daemon
                .filesystem(participant_partition.as_ref().unwrap())
                .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
                .unwrap()
                .is_empty()
        );
        return;
    }
    let read = Arc::new(
        OfflineReadAdmission::review(
            setup.clone(),
            disk.try_clone().unwrap(),
            target.clone(),
            &AtomicBool::new(false),
        )
        .unwrap(),
    );
    let proof = read
        .admit_application_output(&AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        proof.is_some(),
        flatpak,
        "consented empty selection still binds an output proof"
    );
    assert_eq!(
        read.output_flatpak_required(&AtomicBool::new(false))
            .unwrap(),
        paired
    );
    drop(proof);

    let (transaction, medium) =
        OfflineOutputTransaction::mount(read.clone(), ServiceCancellation::default()).unwrap();
    if applications_only {
        paired_apps::complete_and_restore(
            &setup,
            transaction,
            read,
            medium,
            paired_apps::Subjects {
                target: &target,
                raw: &disk,
                scratch: scratch.path(),
                daemon: &daemon,
                source_partition: &source_partition,
                output_partition: &output_partition,
            },
        );
        return;
    }
    let outcome = transaction.export_and_verify(Zeroizing::new(PASSWORD.to_vec()));
    let expected = outcome
        .result
        .as_ref()
        .expect("actual signed exporter and independent verifier must succeed");
    assert_eq!(expected.operation, operation);
    assert!(expected.sealed_bytes <= reservation);
    let output = medium.files().unwrap().output;
    assert_eq!(
        output.metadata().unwrap().uid(),
        0,
        "verified stage must retire to protected root ownership"
    );
    let receipt = read_member(&output, &format!("{operation}.receipt.json"));
    assert_eq!(receipt, serde_json::to_vec(expected).unwrap());
    // Actual published ciphertext is consumed again by the native importer;
    // neither worker DTO nor public receipt can stand in for contents.
    let ciphertext = read_member(&output, &expected.filename);
    assert_eq!(ciphertext.len() as u64, expected.sealed_bytes);
    let completed = scratch.path().join("completed-home.sealed");
    fs::write(&completed, ciphertext).unwrap();
    let tree = ImportReview::review(
        &completed,
        PASSWORD,
        Purpose::Home,
        expected.limits,
        &AtomicBool::new(false),
    )
    .unwrap()
    .extract(
        Zeroizing::new(PASSWORD.to_vec()),
        &scratch.path().join("imported"),
        &AtomicBool::new(false),
    )
    .unwrap();
    if paired::guided_story() {
        paired::verify_guided_home(&tree.root);
    }
    assert_eq!(
        fs::read(tree.root.join(".var/app/so.test/data/document")).unwrap(),
        b"hidden full-home app bytes"
    );
    assert_eq!(
        fs::read(tree.root.join("group-only")).unwrap(),
        b"actual supplementary-group source bytes"
    );
    assert_eq!(
        fs::read(tree.root.join("owner-document")).unwrap(),
        b"unchanged original owner bytes"
    );
    assert_eq!(
        fs::metadata(tree.root.join("same-document")).unwrap().ino(),
        fs::metadata(tree.root.join(".var/app/so.test/data/document"))
            .unwrap()
            .ino()
    );
    if mounted {
        mounted::verify_import(&tree.root);
    }
    let original = read.output_original_home(&AtomicBool::new(false)).unwrap();
    assert_eq!(
        read_member(&original, "owner-document"),
        b"unchanged original owner bytes"
    );
    assert_eq!(
        read_member(&original, "group-only"),
        b"actual supplementary-group source bytes"
    );
    if paired::guided_story() {
        assert_eq!(
            read_member(&original, "Documents/notes"),
            b"all my actual documents"
        );
        assert_eq!(
            read_member(&original, ".var/app/story.app/state"),
            b"original hidden app state"
        );
    }
    drop(original);
    let committed = paired.then(|| paired::complete(&setup, &transaction, &outcome));
    drop(output);
    drop(transaction);
    // The medium owns the last admission through observed all-source retirement.
    drop(read);
    let (shutdown, key, work) = outcome
        .cleanup_receipt()
        .expect("both actual namespaces must be confirmed retired");
    medium
        .finish(shutdown, key, work)
        .expect("actual owned UDisks unmount must succeed");
    assert!(
        daemon
            .filesystem(&output_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty()
    );
    if let Some(committed) = committed {
        paired::reopen_and_receive(&target, &disk, &committed, scratch.path(), receiver);
    }
    if let Some(partition) = &participant_partition {
        assert!(
            daemon
                .filesystem(partition)
                .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
                .unwrap()
                .is_empty(),
            "mounted participant readonly epoch must retire"
        );
    }
    assert!(
        daemon
            .filesystem(&source_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty(),
        "original readonly mount lease must retire"
    );
}
