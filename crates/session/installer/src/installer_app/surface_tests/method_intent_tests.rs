use super::*;

fn occupied(filesystem: &str) -> Disk {
    let mut disk = sample_disks().remove(0);
    disk.partitions.truncate(2);
    disk.partitions[1].filesystem = filesystem.into();
    disk.partitions[1].part_type = LINUX_FILESYSTEM_GUID.into();
    disk.partitions[1].label.clear();
    disk
}

fn fixture(filesystem: &str) -> (InstallerApp, AutoCommit, Arc<FakeBackend>) {
    let backend = Arc::new(FakeBackend {
        disks: vec![occupied(filesystem), sample_disks().remove(1)],
        installs: Mutex::new(Vec::new()),
        power: Mutex::new(Vec::new()),
        succeed: true,
    });
    let (mut app, doc) = app(&backend);
    app.step = Step::Disk;
    (app, doc, backend)
}

fn select(app: &mut InstallerApp, doc: &mut AutoCommit, device: &str) {
    let reply = invoke(
        app,
        doc,
        &format!("select@installer.disk:{device}"),
        &json!({}),
    );
    assert!(refused(&reply).is_none(), "{reply}");
}

fn method(app: &mut InstallerApp, doc: &mut AutoCommit, method: &str) {
    let reply = invoke(
        app,
        doc,
        "set-method@installer.storage",
        &json!({"method":method}),
    );
    assert!(refused(&reply).is_none(), "{reply}");
}

macro_rules! filesystem_cases {
    ($module:ident, $filesystem:literal) => {
        mod $module {
            use super::*;

            #[test]
            fn fresh_selection_keeps_existing_data() {
                let (mut app, mut doc, _) = fixture($filesystem);
                select(&mut app, &mut doc, "/dev/sda");
                assert_eq!(app.install_method, InstallMethod::Alongside);
                assert!(app.disk_method_choices.is_empty());
            }

            #[test]
            fn low_space_blocks_next_without_changing_method() {
                let (mut app, mut doc, backend) = fixture($filesystem);
                app.disks[0].partitions[1].size_bytes =
                    app.disks[0].size_bytes - app.disks[0].partitions[1].start_bytes - 1_048_576;
                select(&mut app, &mut doc, "/dev/sda");
                assert!(app.guided_alongside_plans().is_empty());
                let reply = invoke(&mut app, &mut doc, "next@installer", &json!({}));
                assert!(refused(&reply).is_some(), "{reply}");
                assert_eq!(app.step, Step::Disk);
                assert_eq!(app.install_method, InstallMethod::Alongside);
                assert!(backend.installs.lock().unwrap().is_empty());
            }

            #[test]
            fn explicit_erase_survives_authenticated_refresh_but_not_confirmation() {
                let (mut app, mut doc, _) = fixture($filesystem);
                select(&mut app, &mut doc, "/dev/sda");
                method(&mut app, &mut doc, "erase");
                app.erase_confirmation.set("/dev/sda".into());
                let reply = invoke(&mut app, &mut doc, "refresh@installer.storage", &json!({}));
                assert!(refused(&reply).is_none(), "{reply}");
                assert_eq!(app.selected_disk, Some(0));
                assert_eq!(app.install_method, InstallMethod::Erase);
                assert_eq!(local_text(&app.erase_confirmation), "");
                assert!(!app.destructive_confirmation_ready());
            }

            #[test]
            fn another_occupied_disk_does_not_inherit_erase() {
                let (mut app, mut doc, _) = fixture($filesystem);
                let mut other = occupied($filesystem);
                other.name = "/dev/sdb".into();
                other.major_minor = "8:16".into();
                other.serial = "SECOND-DISK".into();
                app.disks.push(other);
                select(&mut app, &mut doc, "/dev/sda");
                method(&mut app, &mut doc, "erase");
                select(&mut app, &mut doc, "/dev/sdb");
                assert_eq!(app.install_method, InstallMethod::Alongside);
                select(&mut app, &mut doc, "/dev/sda");
                assert_eq!(app.install_method, InstallMethod::Erase);
            }

            #[test]
            fn replacement_at_same_path_does_not_inherit_erase() {
                let (mut app, mut doc, _) = fixture($filesystem);
                select(&mut app, &mut doc, "/dev/sda");
                method(&mut app, &mut doc, "erase");
                let mut replacement = app.disks.clone();
                replacement[0].serial = "REPLACEMENT-DISK".into();
                app.refresh_disks_with(replacement);
                assert_eq!(app.selected_disk, None);
                select(&mut app, &mut doc, "/dev/sda");
                assert_eq!(app.install_method, InstallMethod::Alongside);
            }

            #[test]
            fn row_and_method_controls_record_disk_bound_intent() {
                let (mut app, doc, _) = fixture($filesystem);
                app.control(&op::disk_resource("/dev/sda"), "select", Value::Null).fire();
                app.sync(&doc, &automerge::ROOT);
                assert_eq!(app.install_method, InstallMethod::Alongside);
                app.control(op::STORAGE, "set-method", json!({"method":"erase"})).fire();
                app.sync(&doc, &automerge::ROOT);
                assert_eq!(app.install_method, InstallMethod::Erase);
                app.control(op::STORAGE, "refresh", Value::Null).fire();
                app.sync(&doc, &automerge::ROOT);
                assert_eq!(app.selected_disk, Some(0));
                assert_eq!(app.install_method, InstallMethod::Erase);
                assert_eq!(app.disk_method_choices.len(), 1);
            }

            #[test]
            fn automatic_alongside_rechecks_geometry_after_refresh() {
                let (mut app, mut doc, _) = fixture($filesystem);
                select(&mut app, &mut doc, "/dev/sda");
                assert!(!app.guided_alongside_plans().is_empty());
                let mut disks = app.disks.clone();
                disks[0].partitions[1].size_bytes =
                    disks[0].size_bytes - disks[0].partitions[1].start_bytes - 1_048_576;
                app.refresh_disks_with(disks);
                assert_eq!(app.selected_disk, Some(0));
                assert_eq!(app.install_method, InstallMethod::Alongside);
                assert!(app.guided_alongside_plans().is_empty());
                assert!(!app.selected_disk_can_install_alongside());
            }
        }
    };
}

filesystem_cases!(ext4, "ext4");
filesystem_cases!(btrfs, "btrfs");
filesystem_cases!(luks, "crypto_LUKS");
filesystem_cases!(ntfs, "ntfs");
filesystem_cases!(lvm, "LVM2_member");

#[test]
fn blank_disk_defaults_to_erase_without_inheriting_alongside() {
    let (mut app, mut doc, _) = fixture("ext4");
    select(&mut app, &mut doc, "/dev/sda");
    method(&mut app, &mut doc, "alongside");
    select(&mut app, &mut doc, "/dev/nvme0n1");
    assert_eq!(app.install_method, InstallMethod::Erase);
}

#[test]
fn blank_disk_manual_alongside_survives_refresh() {
    let (mut app, mut doc, _) = fixture("ext4");
    select(&mut app, &mut doc, "/dev/nvme0n1");
    method(&mut app, &mut doc, "alongside");
    app.refresh_disks_with(app.disks.clone());
    assert_eq!(app.selected_disk, Some(1));
    assert_eq!(app.install_method, InstallMethod::Alongside);
}

#[test]
fn reorder_preserves_identity_not_disk_index() {
    let (mut app, mut doc, _) = fixture("ext4");
    select(&mut app, &mut doc, "/dev/sda");
    method(&mut app, &mut doc, "erase");
    let mut disks = app.disks.clone();
    disks.reverse();
    app.refresh_disks_with(disks);
    assert_eq!(app.selected_disk, Some(1));
    assert_eq!(app.install_method, InstallMethod::Erase);
}

#[test]
fn read_only_refresh_revokes_selection_and_preflight() {
    let (mut app, mut doc, _) = fixture("ext4");
    select(&mut app, &mut doc, "/dev/sda");
    method(&mut app, &mut doc, "erase");
    app.storage_preflight_passed = true;
    let mut disks = app.disks.clone();
    disks[0].read_only = true;
    app.refresh_disks_with(disks);
    assert_eq!(app.selected_disk, None);
    assert!(!app.storage_preflight_passed);
    let reply = invoke(
        &mut app,
        &mut doc,
        "select@installer.disk:/dev/sda",
        &json!({}),
    );
    assert!(refused(&reply).is_some(), "{reply}");
}

#[test]
fn missing_hardware_identity_never_remembers_erase() {
    let (mut app, mut doc, _) = fixture("ext4");
    app.disks[0].serial.clear();
    app.disks[0].wwn.clear();
    select(&mut app, &mut doc, "/dev/sda");
    method(&mut app, &mut doc, "erase");
    assert!(app.disk_method_choices.is_empty());
    app.refresh_disks_with(app.disks.clone());
    select(&mut app, &mut doc, "/dev/sda");
    assert_eq!(app.install_method, InstallMethod::Alongside);
}

#[test]
fn repartitioned_disk_requires_fresh_method_choice() {
    let (mut app, mut doc, _) = fixture("ext4");
    select(&mut app, &mut doc, "/dev/sda");
    method(&mut app, &mut doc, "erase");
    let mut disks = app.disks.clone();
    disks[0].partition_table_uuid = "new-table".into();
    app.refresh_disks_with(disks);
    select(&mut app, &mut doc, "/dev/sda");
    assert_eq!(app.install_method, InstallMethod::Alongside);
}

#[test]
fn remembered_erase_still_requires_a_new_semantic_confirmation() {
    let (mut app, mut doc, backend) = fixture("ext4");
    select(&mut app, &mut doc, "/dev/sda");
    method(&mut app, &mut doc, "erase");
    app.erase_confirmation.set("/dev/sda".into());
    app.refresh_disks_with(app.disks.clone());
    app.step = Step::Review;
    app.username.set("owner".into());
    *app.password.lock().unwrap() = SECRET.into();
    *app.password_confirmation.lock().unwrap() = SECRET.into();
    app.recovery_ack = true;
    let reply = invoke(&mut app, &mut doc, "begin@installer.review", &json!({}));
    assert_eq!(
        refused(&reply),
        Some("action_confirmation_required"),
        "{reply}"
    );
    assert!(backend.installs.lock().unwrap().is_empty());
}

#[test]
fn no_selected_disk_cannot_create_a_global_erase_preference() {
    let (mut app, mut doc, _) = fixture("ext4");
    method(&mut app, &mut doc, "erase");
    select(&mut app, &mut doc, "/dev/sda");
    assert_eq!(app.install_method, InstallMethod::Alongside);
    assert!(app.disk_method_choices.is_empty());
}
