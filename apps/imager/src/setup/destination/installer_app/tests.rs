use super::*;
use automerge::AutoCommit;
use automerge::ReadDoc;
use imager_backend::destination::disks::{DiskPartition, EFI_SYSTEM_GUID, LINUX_FILESYSTEM_GUID};
use serde_json::Value;
use tilo_ui_base::widget::find_test_id;

fn find_test_id_mut<'a>(widget: &'a mut dyn Widget, id: &str) -> Option<&'a mut dyn Widget> {
    if widget.test_id() == Some(id) {
        return Some(widget);
    }
    widget
        .children_mut()
        .into_iter()
        .find_map(|child| find_test_id_mut(child, id))
}

fn perform(
    app: &mut InstallerApp,
    doc: &mut AutoCommit,
    id: &str,
    action: &tilo_ui_base::a11y::Action,
) {
    let _ = build(app);
    let composed = app.composed.element_mut().expect("composed step");
    let control = find_test_id_mut(composed, id).unwrap_or_else(|| panic!("no control {id}"));
    assert!(
        control.perform_action(
            action,
            &[],
            &mut tilo_ui_base::widget::Edits::lend(doc),
            &automerge::ROOT
        ),
        "{id} refused {action:?}"
    );
    let composed = app.composed.element_mut().expect("composed step");
    ui::widget::resources::start_pending_control_invocations(
        composed,
        &mut tilo_ui_base::widget::Edits::lend(doc),
        &automerge::ROOT,
    );
    app.sync(doc, &automerge::ROOT);
    assert_eq!(
        doc.length(automerge::ROOT),
        0,
        "a control wrote the document"
    );
}

pub(super) fn activate(app: &mut InstallerApp, doc: &mut AutoCommit, id: &str) {
    perform(app, doc, id, &tilo_ui_base::a11y::Action::Activate);
}

fn run(app: &mut InstallerApp, doc: &AutoCommit, effect: DestinationEffect) {
    app.run_press(Press::new(effect));
    app.sync(doc, &automerge::ROOT);
    assert_eq!(doc.length(automerge::ROOT), 0, "a press wrote the document");
}

fn next(app: &mut InstallerApp, doc: &AutoCommit) {
    run(app, doc, DestinationEffect::Next);
}

fn sample() -> Vec<Disk> {
    Disk::samples()
}

pub(super) fn disk_with_two_guided_extents() -> Disk {
    let gib = 1024_u64 * 1024 * 1024;
    let mut disk = Disk::samples().remove(1);
    disk.partitions.push(DiskPartition {
        name: "/dev/nvme0n1p1".to_string(),
        start_bytes: 100 * gib,
        size_bytes: 20 * gib,
        part_uuid: "30000000-0000-4000-8000-000000000001".to_string(),
        filesystem: "ntfs".to_string(),
        label: "Existing OS".to_string(),
        mountpoint: String::new(),
        part_type: "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7".to_string(),
    });
    disk
}

fn assert_tree_mirrors(widget: &dyn Widget, taffy: &TaffyTree<()>, node: NodeId) {
    let wc = widget.children();
    let tc = taffy.children(node).unwrap();
    assert_eq!(
        wc.len(),
        tc.len(),
        "widget '{}' has {} children but its taffy node has {}",
        widget.name(),
        wc.len(),
        tc.len()
    );
    for (w, &n) in wc.iter().zip(tc.iter()) {
        assert_tree_mirrors(*w, taffy, n);
    }
}

fn find_named<'a>(widget: &'a dyn Widget, name: &str) -> Option<&'a dyn Widget> {
    if widget.name() == name {
        return Some(widget);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| find_named(child, name))
}

fn reading(
    node: tilo_ui_base::a11y::AccessNode,
    text: &str,
) -> Option<tilo_ui_base::a11y::AccessNode> {
    if node.name.as_deref() == Some(text) || node.value.as_deref() == Some(text) {
        return Some(node);
    }
    node.children
        .into_iter()
        .find_map(|child| reading(child, text))
}

fn find_accessible(widget: &dyn Widget, name: &str) -> Option<tilo_ui_base::a11y::AccessNode> {
    if let Some(node) = widget.accessibility().and_then(|node| reading(node, name)) {
        return Some(node);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| find_accessible(child, name))
}

fn find_accessible_role(
    widget: &dyn Widget,
    name: &str,
    role: tilo_ui_base::a11y::Role,
) -> Option<tilo_ui_base::a11y::AccessNode> {
    if let Some(node) = widget.accessibility()
        && node.name.as_deref() == Some(name)
        && node.role == role
    {
        return Some(node);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| find_accessible_role(child, name, role))
}

fn has_test_id(widget: &dyn Widget, id: &str) -> bool {
    find_test_id(widget, id).is_some()
}

fn build(app: &mut InstallerApp) -> (TaffyTree<()>, NodeId) {
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root = app.build_layout(&mut taffy, &theme, &mut node_map);
    (taffy, root)
}

fn layout_at(app: &mut InstallerApp, width: f32, height: f32) {
    layout_with_theme(app, width, height, &Theme::dark());
}

fn layout_with_theme(
    app: &mut InstallerApp,
    width: f32,
    height: f32,
    theme: &Theme,
) -> HashMap<NodeId, *mut dyn Widget> {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let app_node = app.build_layout(&mut taffy, theme, &mut node_map);
    let viewport = taffy
        .new_with_children(
            Style {
                size: Size::from_lengths(width, height),
                ..Default::default()
            },
            &[app_node],
        )
        .expect("viewport node");
    let measure_map = node_map.clone();
    let mut text_renderer = tilo_ui_base::text_render::TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            viewport,
            Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::Definite(height),
            },
            |known, available, node_id, _, _| {
                if let Some(&widget) = measure_map.get(&node_id) {
                    // SAFETY: every pointer belongs to `app`, which remains
                    // alive and stationary for this layout pass.
                    unsafe { (*widget).measure(known, available, &mut text_renderer) }
                } else {
                    Size::ZERO
                }
            },
        )
        .expect("compute compact installer layout");
    app.apply_layout(&taffy, app_node, Point::ORIGIN);
    node_map
}

tilo_test_env::scenario! {
    fn the_account_step_explains_itself_and_keeps_secrets_private;

    phase welcome_and_account_explain_choices_in_person_first_order;
    {
        let mut app = InstallerApp::with_disks(Vec::new());
        layout_at(&mut app, 800.0, 600.0);
        assert!(
            find_accessible(&app, &crate::setup::strings::t("installer-welcome-title")).is_some()
        );
        assert!(
            find_accessible(&app, &crate::setup::strings::t("installer-welcome-note")).is_some()
        );
        let next = find_test_id(&app, "installer.next").expect("first-page continuation");
        assert!(next.get_rect().height() > 0.0);
        assert!(next.get_rect().y0 >= 0.0);
        assert!(
            next.get_rect().y1 <= 600.0,
            "continue must not begin below the fold"
        );

        app.step = Step::Account;
        app.dirty_structure = true;
        layout_at(&mut app, 800.0, 600.0);
        let name = find_test_id(&app, "installer.full_name").expect("display name field");
        let username = find_test_id(&app, "installer.username").expect("sign-in field");
        assert!(name.get_rect().height() > 0.0);
        assert!(username.get_rect().y0 >= name.get_rect().y1);
        assert!(
            find_accessible(
                &app,
                &crate::setup::strings::t("installer-account-name-note")
            )
            .is_some()
        );
        let next = find_test_id(&app, "installer.next").expect("account continuation");
        assert!(
            next.get_rect().y1 <= 600.0,
            "continue must remain visible at normal text size"
        );
    }

    phase account_fields_have_localized_accessible_names_without_exposing_secrets;
    {
        for pin in [false, true] {
            let mut app = InstallerApp::with_disks(Vec::new());
            app.step = Step::Account;
            app.tpm2 = pin;
            app.secure_boot = pin;
            app.kernel_lockdown = pin;
            app.target_verified_boot = pin;
            *app.password.lock().unwrap() = "private-startup-secret".into();
            *app.password_confirmation.lock().unwrap() = "private-startup-secret".into();
            let _ = build(&mut app);
            for (id, key, protected) in [
                ("installer.full_name", "installer-account-name", false),
                ("installer.username", "installer-account-username", false),
                (
                    "installer.password",
                    if pin {
                        "installer-account-pin"
                    } else {
                        "installer-account-password"
                    },
                    true,
                ),
                (
                    "installer.password_confirm",
                    if pin {
                        "installer-account-pin-confirm"
                    } else {
                        "installer-account-password-confirm"
                    },
                    true,
                ),
            ] {
                let access = find_test_id(&app, id).unwrap().accessibility().unwrap();
                assert_eq!(
                    access.name.as_deref(),
                    Some(crate::setup::strings::t(key).as_str()),
                    "{id}"
                );
                assert_eq!(access.state.protected, protected, "{id}");
                if protected {
                    assert!(access.value.is_none(), "a named secret must remain private");
                }
            }
        }
    }

    phase server_destination_asks_for_the_disk_secret_and_no_account;
    {
        let mut app = InstallerApp::with_disks(sample());
        app.purpose = imager_backend::destination::install::SERVER_PURPOSE.into();
        app.step = Step::Account;
        *app.password.lock().unwrap() = "correct horse battery staple".into();
        *app.password_confirmation.lock().unwrap() = "correct horse battery staple".into();
        let _ = build(&mut app);
        assert!(
            find_test_id(&app, "installer.full_name").is_none(),
            "a server has no account name to ask for"
        );
        assert!(
            find_test_id(&app, "installer.username").is_none(),
            "a server has no sign-in name to ask for"
        );
        assert!(
            find_accessible(
                &app,
                &crate::setup::strings::t("installer-account-server-title")
            )
            .is_some()
        );
        let access = find_test_id(&app, "installer.password")
            .expect("disk secret field")
            .accessibility()
            .unwrap();
        assert_eq!(
            access.name.as_deref(),
            Some(crate::setup::strings::t("installer-account-server-secret").as_str())
        );
        assert!(access.state.protected, "the disk secret stays private");
        assert!(app.validate_account_details().is_ok());
        assert!(validate_step(Step::Account, Some(0), "", "tilo", &app.purpose).is_ok());
        let opts = app.install_options();
        assert_eq!(
            opts.purpose,
            imager_backend::destination::install::SERVER_PURPOSE
        );
        assert_eq!(
            opts.pin.as_str(),
            "correct horse battery staple",
            "the one secret unlocks the disk"
        );
    }

    phase account_step_keeps_secrets_out_of_the_crdt_and_waits_for_username;
    {
        {
            let mut app = InstallerApp::with_disks(sample());
            let doc = AutoCommit::new();
            app.sync(&doc, &automerge::ROOT);
            for path in [
                "installer.username",
                "installer.full_name",
                "installer.password",
                "installer.password_confirm",
                "installer.wifi_password",
            ] {
                assert!(doc.get(&automerge::ROOT, path).unwrap().is_none(), "{path}");
            }
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.tpm2 = false;
            app.selected_disk = Some(0);
            app.step = Step::Account;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            activate(&mut app, &mut doc, "installer.next");
            assert_eq!(app.step, Step::Account);
            assert_eq!(app.status, "Enter a username");

            app.username.set("ada".to_string());
            *app.password.lock().unwrap() = "correct horse battery staple".to_string();
            *app.password_confirmation.lock().unwrap() = "correct horse battery staple".to_string();
            activate(&mut app, &mut doc, "installer.next");
            assert_eq!(app.step, Step::Options);
            assert_eq!(local_text(&app.username), "ada");
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Options;
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            *app.password.lock().unwrap() = "correct horse battery".to_string();
            app.sync(&doc, &root);
            assert_eq!(&*secret_copy(&app.password), "correct horse battery");
            assert_eq!(app.install_options().pin.as_str(), "correct horse battery");
        }
    }
}

tilo_test_env::scenario! {
    fn the_storage_step_explains_and_checks_what_happens_to_the_disk;

    phase storage_explains_missing_drives_and_the_confirmation_boundary;
    {
        let mut app = InstallerApp::with_disks(Vec::new());
        app.step = Step::Disk;
        let _ = build(&mut app);
        assert!(
            find_accessible(
                &app,
                &crate::setup::strings::t("installer-storage-no-drives")
            )
            .is_some()
        );
        assert!(
            find_accessible(
                &app,
                &crate::setup::strings::t("installer-storage-erase-warning")
            )
            .is_some()
        );
        app.install_method = InstallMethod::Alongside;
        app.dirty_structure = true;
        let _ = build(&mut app);
        assert!(
            find_accessible(
                &app,
                &storage_text("installer-storage-keep-guidance")
            )
            .is_some()
        );
    }

    phase disk_step_allows_next_only_on_a_usable_selection;
    {
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Disk;
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            assert_eq!(app.step, Step::Disk);

            next(&mut app, &doc);
            assert_eq!(app.step, Step::Disk, "must not advance without a disk");
            assert_eq!(app.status, "Select a disk to continue");
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Disk;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            activate(&mut app, &mut doc, "/dev/nvme0n1");
            assert_eq!(app.selected_disk, Some(1));

            activate(&mut app, &mut doc, "installer.next");
            assert_eq!(app.step, Step::Account);
            assert!(app.take_focus_reset(), "step change requests focus reset");
        }
        {
            let mut disks = sample();
            disks[0].read_only = true;
            let mut app = InstallerApp::with_disks(disks);
            app.step = Step::Disk;
            app.selected_disk = Some(0);
            app.go_next();
            assert_eq!(app.step, Step::Disk);
            assert_eq!(
                app.status,
                "This disk is read-only and cannot be used for installation"
            );
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Disk;
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            let begin = app.begin_effect();
            run(&mut app, &doc, begin);
            assert_eq!(app.step, Step::Disk);
            assert_eq!(app.install_state, InstallState::Idle);
            assert!(!app.install_launched);
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(1);
            assert_eq!(app.selected_dev().as_deref(), Some("/dev/nvme0n1"));
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Disk;
            app.selected_disk = Some(0);
            app.viewport_offset.set((0.0, 144.0));
            app.set_status("A transient message");
            assert_eq!(app.viewport_offset.get(), (0.0, 144.0));

            app.go_next();
            assert_eq!(app.step, Step::Account);
            assert_eq!(app.viewport_offset.get(), (0.0, 0.0));
        }
    }

    phase linux_guidance_matches_visible_space_state;
    {
        let mut disk = Disk::samples().remove(1);
        disk.partitions.clear();
        let mut linux = Disk::samples().remove(0).partitions.remove(1);
        linux.filesystem = "ext4".into();
        linux.label = "Linux".into();
        linux.part_type = LINUX_FILESYSTEM_GUID.into();
        linux.size_bytes = 64 * 1024 * 1024 * 1024;
        disk.partitions.push(linux);
        let mut app = InstallerApp::with_disks(vec![disk]);
        app.selected_disk = Some(0);
        app.install_method = InstallMethod::Alongside;
        app.step = Step::Disk;
        let full = app
            .guided_alongside_plan()
            .expect("usable unallocated extent");
        for chosen_gib in [None, Some(48)] {
            app.step = Step::Disk;
            if let Some(gib) = chosen_gib {
                let Some(DestinationEffect::SetSize { extent, .. }) = app.capacity_effect() else {
                    panic!("a capacity choice is offered");
                };
                app.apply(DestinationEffect::SetSize {
                    gib: i64::from(gib),
                    extent,
                })
                .unwrap();
            }
            let _ = build(&mut app);
            let plan = app.guided_alongside_plan().unwrap();
            let expected_bytes = chosen_gib.map_or(full.free_size_bytes, |gib| {
                u64::try_from(gib).unwrap() << 30
            });
            assert_eq!(plan.free_size_bytes, expected_bytes);
            let capacity = app.alongside_allocation_summary(&plan);
            assert!(
                capacity.contains(&tilo_ui_base::format::memory(expected_bytes)),
                "missing selected capacity: {capacity}"
            );
            assert!(
                capacity.contains(&tilo_ui_base::format::memory(full.free_size_bytes - expected_bytes)),
                "missing unused space: {capacity}"
            );
            assert!(capacity.contains("remains unused"), "{capacity}");
            assert!(capacity.contains("unallocated space"), "{capacity}");
            assert!(
                find_accessible(&app, &crate::setup::strings::t(LINUX_ALONGSIDE_READY)).is_some()
            );
            assert!(find_accessible(&app, &capacity).is_some());
            assert!(
                find_accessible(&app, &storage_text(LINUX_ALONGSIDE_PREPARATION))
                    .is_none()
            );
            assert!(
                find_accessible(&app, &crate::setup::strings::t(ALONGSIDE_BOOT_GUIDANCE)).is_none()
            );
            let guidance = app.alongside_storage_guidance();
            assert!(guidance.contains(&crate::setup::strings::t(LINUX_ALONGSIDE_READY)));
            assert!(guidance.contains(&capacity));
            let storage = app.published()[0].clone().into_resource();
            let storage = storage
                .children
                .iter()
                .find(|resource| resource.id.0 == crate::setup::destination::effects::STORAGE)
                .unwrap();
            let published_guidance = storage
                .children
                .iter()
                .filter(|resource| resource.kind == "installer.guidance")
                .map(|resource| {
                    resource
                        .presentation
                        .as_ref()
                        .unwrap()
                        .preview
                        .as_ref()
                        .unwrap()
                        .as_str()
                })
                .collect::<Vec<_>>()
                .join(". ");
            assert_eq!(published_guidance, guidance);
            for hidden in ["35 GiB", "offline", "LUKS", "2 GiB", "1 GiB"] {
                assert!(!guidance.contains(hidden), "unexpected advice: {guidance}");
            }
            app.step = Step::Review;
            let _ = build(&mut app);
            assert!(find_accessible(&app, &capacity).is_some());
            assert!(
                find_accessible(&app, &crate::setup::strings::t(ALONGSIDE_BOOT_GUIDANCE)).is_some()
            );
            assert!(
                find_accessible(&app, &crate::setup::strings::t(ALONGSIDE_FILES_GUIDANCE))
                    .is_some()
            );
            assert_eq!(app.install_options().guided_free_size_bytes, expected_bytes);
        }
        let capacity = app.alongside_allocation_summary(&app.guided_alongside_plan().unwrap());

        app.disks[0].partitions[0].size_bytes =
            app.disks[0].size_bytes - app.disks[0].partitions[0].start_bytes - 1_048_576;
        app.step = Step::Disk;
        let _ = build(&mut app);
        let guidance = app.alongside_storage_guidance();
        assert!(guidance.contains(&storage_text(LINUX_ALONGSIDE_PREPARATION)));
        assert!(!guidance.contains(&crate::setup::strings::t(LINUX_ALONGSIDE_READY)));
        assert!(find_accessible(&app, &guidance).is_some());
        assert!(find_accessible(&app, &crate::setup::strings::t(LINUX_ALONGSIDE_READY)).is_none());
        assert!(find_accessible(&app, &capacity).is_none());
    }

    phase alongside_copy_explains_what_changes_before_leaving_storage;
    {
        {
            let guided = alongside_install_impact(true, "/dev/nvme0n1");
            assert!(guided.contains("Three new Tilo partitions (EFI, boot, and encrypted root)"));
            assert!(
                guided.contains("created and formatted only in the reviewed unallocated space")
            );
            assert!(guided.contains("BitLocker recovery prompt"));

            let prepared = alongside_install_impact(false, "/dev/nvme0n1");
            assert!(prepared.contains("prepared Tilo boot and root partitions"));
            assert!(prepared.contains("will be formatted"));
            assert!(
                prepared.contains("existing EFI partition and other OS partitions are preserved")
            );
            assert!(prepared.contains("BitLocker recovery prompt"));
            assert_eq!(
                alongside_confirmation_operation(false),
                "Tilo boot and root format"
            );
            assert_eq!(
                alongside_confirmation_operation(true),
                "guided free-space partition plan"
            );
        }
        {
            let mut app = InstallerApp::with_disks(Disk::samples());
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(1);
            app.step = Step::Review;
            assert!(app.uses_guided_alongside());
            let _ = build(&mut app);
            assert!(
                find_accessible(
                    &app,
                    "Use the firmware boot picker; its current default and existing entries are preserved"
                )
                .is_some()
            );

            app.install_state = InstallState::Done;
            app.step = Step::Install;
            let _ = build(&mut app);
            assert!(
                find_accessible(
                    &app,
                    "To choose Tilo or your existing system, open this computer's firmware boot picker. The installer preserved its previous default."
                )
                .is_some()
            );
        }
        {
            let mut app = InstallerApp::with_disks(Disk::samples());
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(0);
            app.step = Step::Disk;
            let _ = build(&mut app);

            assert!(
                find_accessible(
                    &app,
                    &crate::setup::strings::t(WINDOWS_ALONGSIDE_PREPARATION)
                )
                .is_some(),
                "Windows preparation must be actionable before the user leaves storage selection"
            );

            app.disks[0].partitions[1].filesystem = "ext4".into();
            app.disks[0].partitions[1].label = "Existing Linux".into();
            let _ = build(&mut app);
            assert!(
                find_accessible(
                    &app,
                    &crate::setup::strings::t(WINDOWS_ALONGSIDE_PREPARATION)
                )
                .is_none(),
                "Linux-only alongside disks must not receive Windows-specific instructions"
            );
            assert!(
                find_accessible(&app, &crate::setup::strings::t(LINUX_ALONGSIDE_READY)).is_none()
            );
            assert!(
                find_accessible(&app, &storage_text(LINUX_ALONGSIDE_PREPARATION))
                    .is_none()
            );
            app.step = Step::Review;
            let _ = build(&mut app);
            assert!(
                find_accessible(&app, &crate::setup::strings::t(ALONGSIDE_BOOT_GUIDANCE)).is_some()
            );
            assert!(
                find_accessible(&app, &crate::setup::strings::t(ALONGSIDE_FILES_GUIDANCE))
                    .is_some()
            );
        }
        {
            let mut disk = Disk::samples().remove(0);
            disk.partitions
                .retain(|partition| partition.name != "/dev/sda4"); // remove the valid 2 GiB Tilo ESP, retain prepared boot
            let mut app = InstallerApp::with_disks(vec![disk]);
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(0);
            app.step = Step::Disk;
            let _ = build(&mut app);

            assert!(
                find_accessible(
                    &app,
                    "/dev/sda1 is too small for Tilo boot images; a prepared EFI partition must be at least 2 GiB. The privileged install check will also verify FAT health and at least 1 GiB of free space before formatting anything."
                )
                .is_some(),
                "known ESP failures should be explained before the user leaves storage selection"
            );
        }
    }

    phase guided_alongside_binds_exactly_the_reviewed_extent;
    {
        {
            let mut app = InstallerApp::with_disks(Disk::samples());
            app.step = Step::Disk;
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(1); // empty 1 TB GPT sample
            assert!(app.uses_guided_alongside());
            assert!(app.selected_disk_can_install_alongside());

            let plan = app.guided_alongside_plan().unwrap();
            let opts = app.install_options();
            assert_eq!(opts.guided_free_start_bytes, plan.free_start_bytes);
            assert_eq!(opts.guided_free_size_bytes, plan.free_size_bytes);
            assert_eq!(opts.guided_efi_size_bytes, 2 * 1024 * 1024 * 1024);
            assert_eq!(opts.guided_boot_size_bytes, 1024 * 1024 * 1024);
            assert_eq!(opts.guided_boot_start_bytes, plan.boot_start_bytes);
            assert!(opts.guided_root_size_bytes >= 32 * 1024 * 1024 * 1024);
            assert_eq!(
                opts.disk_partition_table_uuid,
                "22222222-2222-2222-2222-222222222222"
            );
            assert!(opts.expected_partitions.is_empty());
            assert_eq!(
                app.destructive_confirmation_target().as_deref(),
                Some("/dev/nvme0n1")
            );

            app.go_next();
            assert_eq!(app.step, Step::Account);
        }
        {
            let disk = disk_with_two_guided_extents();
            let plans = disk.guided_alongside_plans();
            assert_eq!(plans.len(), 2);
            let mut app = InstallerApp::with_disks(vec![disk]);
            app.step = Step::Disk;
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(0);

            assert!(app.guided_alongside_plan().is_none());
            assert!(!app.selected_disk_can_install_alongside());
            app.go_next();
            assert_eq!(app.step, Step::Disk, "ambiguous free space must block Next");
            assert_eq!(app.install_method, InstallMethod::Alongside);
            assert!(app.destructive_confirmation_target().is_none());

            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            activate(&mut app, &mut doc, "guided_extent_1");
            assert_eq!(app.guided_alongside_plan(), Some(plans[1].clone()));
            let options = app.install_options();
            assert_eq!(options.guided_free_start_bytes, plans[1].free_start_bytes);
            assert_eq!(options.guided_free_size_bytes, plans[1].free_size_bytes);

            let _ = build(&mut app);
            assert!(has_test_id(&app, "disk_refresh"));
            assert!(has_test_id(&app, "guided_extent_0"));
            assert!(has_test_id(&app, "guided_extent_1"));
        }
        {
            let disk = disk_with_two_guided_extents();
            let mut app = InstallerApp::with_disks(vec![disk.clone()]);
            app.step = Step::Disk;
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(0);
            app.selected_guided_extent = Some(1);
            app.erase_confirmation.set(disk.name.clone());
            let old_efi_uuid = app.guided_efi_partuuid.clone();
            let old_root_uuid = app.guided_root_partuuid.clone();
            assert!(app.guided_alongside_plan().is_some());

            app.refresh_disks_with(vec![disk]);
            assert_eq!(app.selected_disk, Some(0));
            assert_eq!(app.selected_guided_extent, None);
            assert!(!app.selected_disk_can_install_alongside());
            assert!(local_text(&app.erase_confirmation).is_empty());
            assert_ne!(app.guided_efi_partuuid, old_efi_uuid);
            assert_ne!(app.guided_root_partuuid, old_root_uuid);

            app.selected_disk = Some(0);
            assert!(
                app.guided_alongside_plan().is_none(),
                "the pre-refresh extent index must not survive rediscovery"
            );
            app.go_next();
            assert_eq!(app.step, Step::Disk, "a stale choice must remain blocked");
            assert_eq!(app.install_method, InstallMethod::Alongside);
            assert!(app.destructive_confirmation_target().is_none());
        }
        {
            let mut disk = Disk::samples().remove(1);
            let gib = 1024_u64 * 1024 * 1024;
            disk.partitions = vec![
                DiskPartition {
                    name: "/dev/nvme0n1p1".into(),
                    start_bytes: 1024 * 1024,
                    size_bytes: 3 * gib,
                    part_uuid: "00000000-0000-4000-8000-000000000001".into(),
                    filesystem: "ntfs".into(),
                    label: "existing OS".into(),
                    mountpoint: String::new(),
                    part_type: LINUX_FILESYSTEM_GUID.into(),
                },
                DiskPartition {
                    name: "/dev/nvme0n1p2".into(),
                    start_bytes: 4 * gib,
                    size_bytes: 2 * gib,
                    part_uuid: "10000000-0000-4000-8000-000000000002".into(),
                    filesystem: String::new(),
                    label: "Tilo EFI".into(),
                    mountpoint: String::new(),
                    part_type: EFI_SYSTEM_GUID.into(),
                },
                DiskPartition {
                    name: "/dev/nvme0n1p3".into(),
                    start_bytes: 6 * gib,
                    size_bytes: disk.size_bytes - 7 * gib,
                    part_uuid: "20000000-0000-4000-8000-000000000003".into(),
                    filesystem: String::new(),
                    label: "Tilo root".into(),
                    mountpoint: String::new(),
                    part_type: LINUX_FILESYSTEM_GUID.into(),
                },
            ];
            let mut restarted = InstallerApp::with_disks(vec![disk]);
            restarted.step = Step::Disk;
            restarted.install_method = InstallMethod::Alongside;
            restarted.selected_disk = Some(0);

            assert!(!restarted.selected_disk_can_install_alongside());
            assert!(!restarted.uses_guided_alongside());
            restarted.erase_confirmation.set("/dev/nvme0n1".into());
            assert!(!restarted.destructive_confirmation_ready());
            restarted.go_next();
            assert_eq!(restarted.step, Step::Disk);
        }
    }

    phase prepared_preflight_checks_leaving_storage;
    {
        {
            let mut app = InstallerApp::with_disks(Disk::samples());
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(0);
            app.step = Step::Disk;
            app.storage_preflight_request = Some("current".into());

            app.receive(&format!("{PREFLIGHT_OK_PREFIX}stale:stale result"));
            assert_eq!(app.step, Step::Disk);
            assert!(!app.storage_preflight_passed);

            app.receive(&format!(
                "{PREFLIGHT_FAIL_PREFIX}current:EFI has less than 1 GiB free"
            ));
            assert_eq!(app.step, Step::Disk);
            assert!(app.status.contains("less than 1 GiB"));

            app.storage_preflight_request = Some("retry".into());
            app.receive(&format!(
                "{PREFLIGHT_OK_PREFIX}retry:prepared storage check passed"
            ));
            assert_eq!(app.step, Step::Account);
            assert!(app.storage_preflight_passed);
        }
        {
            let mut app = InstallerApp::with_disks(Disk::samples());
            app.install_method = InstallMethod::Alongside;
            app.selected_disk = Some(0);
            app.step = Step::Disk;

            app.go_next();

            assert_eq!(app.step, Step::Disk);
            assert!(!app.storage_preflight_passed);
            assert!(app.status.contains("prepared-storage check is unavailable"));
        }
    }

    phase staged_handoff_preselects_the_plan_and_offers_a_way_back_only_when_valid;
    {
        {
            use imager_backend::destination::internal_handoff::tests::TARGET;
            let mut app = staged_app();
            assert_eq!(app.selected_disk, Some(0));
            assert_eq!(app.install_method, InstallMethod::Alongside);
            assert_eq!(local_text(&app.username), "ada");
            assert_eq!(local_text(&app.hostname), "studio");
            assert!(
                app.status
                    .starts_with("Tilo Imager prepared space for Tilo next to Windows.")
            );
            assert!(app.uses_guided_alongside());
            let opts = app.install_options();
            assert_eq!(opts.guided_free_start_bytes, TARGET.0 * 512);
            assert_eq!(
                opts.guided_root_start_bytes + opts.guided_root_size_bytes,
                (TARGET.1 + 1) * 512
            );
            assert_eq!(
                opts.internal_handoff,
                imager_backend::destination::internal_handoff::HANDOFF_PATH
            );
            assert_eq!(
                opts.expected_partitions.len(),
                4,
                "staging partitions stay in the reviewed map"
            );
            assert!(opts.alongside_root.is_empty());

            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            run(
                &mut app,
                &doc,
                DestinationEffect::SetMethod {
                    method: "erase".into(),
                },
            );
            assert_eq!(app.install_method, InstallMethod::Alongside);

            app.step = Step::Disk;
            let _ = build(&mut app);
            assert!(!has_test_id(&app, "install_method"));
            assert!(has_test_id(&app, "return_to_host"));
            assert!(find_accessible(&app, "Return to Windows").is_some());
            app.go_next();
            assert_eq!(
                app.step,
                Step::Account,
                "the staged disk is a valid destination"
            );

            app.step = Step::Review;
            let _ = build(&mut app);
            assert!(
                find_accessible(
                    &app,
                    "Also holds the running installer and your existing Windows system. Windows stays untouched."
                )
                .is_some()
            );
        }
        {
            let mut json = imager_backend::destination::internal_handoff::tests::lsblk_json();
            json["blockdevices"][0]["ptuuid"] = "99999999-2222-4333-8444-555555555555".into();
            let mut app = InstallerApp::with_disks(
                imager_backend::destination::disks::parse_lsblk(&json.to_string()),
            );
            app.apply_internal_handoff(
                imager_backend::destination::internal_handoff::tests::handoff(),
            );
            assert!(app.internal_handoff.is_none());
            assert!(app.internal_host().is_none());
            assert_eq!(app.selected_disk, None);
            assert!(app.disks[0].blocks_install());
            assert!(app.status.contains("no longer matches"));
            assert!(app.install_options().internal_handoff.is_empty());
            app.step = Step::Disk;
            let _ = build(&mut app);
            assert!(!has_test_id(&app, "return_to_host"));
        }
        {
            let mut app = staged_app();
            app.step = Step::Install;
            app.install_state = InstallState::Running;
            let _ = build(&mut app);
            assert!(!has_test_id(&app, "return_to_host"));
            app.install_state = InstallState::Failed;
            let _ = build(&mut app);
            assert!(has_test_id(&app, "return_to_host"));
        }
    }

    phase a_staged_plan_decides_the_server_the_unlock_and_its_network;
    {
        let plan = |changes: Value| {
            let mut document = serde_json::json!({
                "schema": 1,
                "plan": "0123456789abcdef0123456789abcdef",
                "revision": 1,
                "destination": "tilo-server",
                "name": "cupboard",
                "network": {"addressing": "dhcp", "addresses": "", "gateway": "", "dns": ""},
                "disk": {"kind": "serial", "identity": "INTERNAL"},
                "unlock": "owner-device",
                "updates": "stable",
                "hosting": [],
                "open": [],
                "token": {"nonce": "00112233445566778899aabbccddeeff",
                          "expires_at_ns": 1_700_000_000_000_000_000_u64}
            });
            for (name, value) in changes.as_object().expect("an object of changes") {
                document[name.as_str()] = value.clone();
            }
            serde_json::from_value::<install_profile_model::plan::Plan>(document).unwrap()
        };

        let mut app = staged_app();
        app.apply_install_plan(&plan(serde_json::json!({
            "network": {"addressing": "static",
                        "addresses": "65.108.204.148/26,2a01:4f9:1a:a083::2/64",
                        "gateway": "65.108.204.129,fe80::1",
                        "dns": "185.12.64.1,185.12.64.2"}
        })))
        .unwrap();
        assert_eq!(app.purpose, SERVER_PURPOSE);
        assert_eq!(local_text(&app.hostname), "cupboard");
        assert_eq!(app.unlock, "owner-device");
        assert!(app.install_plan);
        let options = app.install_options();
        assert!(options.install_plan);
        assert_eq!(
            options.network_static,
            "65.108.204.148/26,2a01:4f9:1a:a083::2/64"
        );
        assert_eq!(options.network_gateway, "65.108.204.129,fe80::1");
        assert_eq!(options.network_dns, "185.12.64.1,185.12.64.2");

        // A DHCP plan leaves the image's own network alone.
        let mut app = staged_app();
        app.apply_install_plan(&plan(serde_json::json!({}))).unwrap();
        assert!(app.install_options().network_static.is_empty());

        // The plan must name the staged disk, be complete, and not need a person.
        let mut app = staged_app();
        assert!(
            app.apply_install_plan(&plan(serde_json::json!({
                "disk": {"kind": "serial", "identity": "OTHER"}
            })))
            .unwrap_err()
            .contains("names")
        );
        let mut app = staged_app();
        assert!(
            app.apply_install_plan(&plan(serde_json::json!({
                "unlock": "passphrase"
            })))
            .unwrap_err()
            .contains("passphrase")
        );
        let mut app = staged_app();
        let mut open = plan(serde_json::json!({}));
        open.name.clear();
        open.open = vec!["name".to_owned()];
        assert!(app.apply_install_plan(&open).unwrap_err().contains("open"));
    }
}

tilo_test_env::scenario! {
    fn the_wizard_steps_walk_validate_and_return_in_order;

    phase step_navigation_walks_forward_back_and_indexes_labels;
    {
        assert_eq!(STEP_LABELS.len(), 8);
        let cases = [
            (Step::Keyboard, Some(Step::Network), None, 0),
            (Step::Network, Some(Step::Disk), Some(Step::Keyboard), 1),
            (Step::Disk, Some(Step::Account), Some(Step::Network), 2),
            (Step::Account, Some(Step::Options), Some(Step::Disk), 3),
            (Step::Options, Some(Step::Media), Some(Step::Account), 4),
            (Step::Media, Some(Step::Review), Some(Step::Options), 5),
            (Step::Review, Some(Step::Install), Some(Step::Media), 6),
            (Step::Install, None, None, 7),
        ];
        for (step, next, prev, index) in cases {
            assert_eq!(next_step(step), next, "next_step case {step:?}");
            assert_eq!(prev_step(step), prev, "prev_step case {step:?}");
            assert_eq!(step_index(step), index, "step_index case {step:?}");
        }
    }

    phase step_and_field_validators_match_backend_contract;
    {
        let steps = [
            (Step::Keyboard, Some(0), "ada", "tilo", true),
            (Step::Disk, None, "", "tilo", false),
            (Step::Disk, Some(0), "", "tilo", true),
            (Step::Account, Some(0), "", "tilo", false),
            (Step::Account, Some(0), "   ", "tilo", false),
            (Step::Account, Some(0), "ada", "tilo", true),
            (Step::Account, Some(0), "lab-user_2", "tilo", true),
            (Step::Account, Some(0), "2bad", "tilo", false),
            (Step::Account, Some(0), "has space", "tilo", false),
            (Step::Account, Some(0), "bad:name", "tilo", false),
            (Step::Options, Some(0), "q", "tilo", true),
            (Step::Options, Some(0), "q", "", false),
            (Step::Options, Some(0), "q", "-bad", false),
            (Step::Options, Some(0), "q", "bad host", false),
        ];
        for (step, disk, username, hostname, ok) in steps {
            assert_eq!(
                validate_step(step, disk, username, hostname, "computer").is_ok(),
                ok,
                "validate_step case {step:?} {disk:?} {username:?} {hostname:?}"
            );
        }
        // A server has no account name to require; the step only holds the disk secret.
        assert!(validate_step(Step::Account, Some(0), "", "tilo", "server").is_ok());

        assert_eq!(
            validate_passwords("", "").unwrap_err(),
            "Enter an account password"
        );
        assert_eq!(
            validate_passwords("secret", "different").unwrap_err(),
            "The passwords do not match"
        );
        assert!(validate_passwords("secret", "secret").is_ok());

        for (username, ok) in [
            ("q", true),
            ("_service", true),
            ("lab-user_2", true),
            ("", false),
            ("Upper", false),
            ("2start", false),
            ("root", false),
            ("greetd", false),
            ("a_very_long_username_that_exceeds_32_chars", false),
        ] {
            assert_eq!(
                valid_username(username),
                ok,
                "valid_username case {username:?}"
            );
        }
        for (hostname, ok) in [
            ("tilo", true),
            ("lab-1", true),
            ("", false),
            ("-x", false),
            ("x-", false),
            ("has space", false),
            ("under_score", false),
        ] {
            assert_eq!(
                valid_hostname(hostname),
                ok,
                "valid_hostname case {hostname:?}"
            );
        }
        for (locale, ok) in [
            ("en_US.UTF-8", true),
            ("de_DE.UTF-8", true),
            ("zz_ZZ.UTF-8", false),
            ("../../etc", false),
        ] {
            assert_eq!(valid_locale(locale), ok, "valid_locale case {locale:?}");
        }
        for (secret, ok) in [
            ("correct horse battery", true),
            ("correct horse", false),
            ("246810", false),
            ("hunter2", false),
            ("aaaaaaaaaaaa", false),
            ("password1234", false),
            ("abcabcabcabc", false),
            ("123456789012", false),
            ("12345678901234567890", true),
            (" correct horse battery", false),
        ] {
            assert_eq!(
                validate_startup_secret(secret).is_ok(),
                ok,
                "validate_startup_secret case {secret:?}"
            );
        }
    }

    phase back_walks_in_reverse_and_a_failed_install_returns_to_storage;
    {
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.step = Step::Review;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            activate(&mut app, &mut doc, "installer.back");
            assert_eq!(app.step, Step::Media);

            activate(&mut app, &mut doc, "installer.back");
            assert_eq!(app.step, Step::Options);

            activate(&mut app, &mut doc, "installer.back");
            assert_eq!(app.step, Step::Account);

            activate(&mut app, &mut doc, "installer.back");
            assert_eq!(app.step, Step::Disk);

            activate(&mut app, &mut doc, "installer.back");
            assert_eq!(app.step, Step::Network);

            activate(&mut app, &mut doc, "installer.back");
            assert_eq!(app.step, Step::Keyboard);

            assert!(!has_test_id(
                app.build_composition(&Theme::dark()).as_ref(),
                "installer.back"
            ));
            run(&mut app, &doc, DestinationEffect::Back);
            assert_eq!(app.step, Step::Keyboard);
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            let old_efi_partuuid = app.guided_efi_partuuid.clone();
            let old_root_partuuid = app.guided_root_partuuid.clone();
            app.selected_disk = Some(0);
            app.step = Step::Install;
            app.install_state = InstallState::Failed;
            app.install_launched = true;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            activate(&mut app, &mut doc, "installer.review_storage");
            assert_eq!(app.step, Step::Disk);
            assert_eq!(app.selected_disk, None);
            assert_ne!(app.guided_efi_partuuid, old_efi_partuuid);
            assert_ne!(app.guided_root_partuuid, old_root_partuuid);
            assert_eq!(app.install_state, InstallState::Idle);
            assert!(!app.install_launched, "retry must re-arm the launch guard");
            assert!(app.status.contains("will not reinitialize a target"));
        }
    }
}

tilo_test_env::scenario! {
    fn the_options_and_media_choices_reach_the_install_options;

    phase options_choices_thread_into_install_options;
    {
        {
            let mut app = InstallerApp::with_disks(sample());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            assert_eq!(app.keymap(), "English (US)");
            let opts = app.install_options();
            assert_eq!(opts.keymap, "us");
            assert!(opts.keyvariant.is_empty());

            perform(
                &mut app,
                &mut doc,
                "installer.keymap",
                &tilo_ui_base::a11y::Action::SetValue("Dvorak".into()),
            );
            assert_eq!(app.keymap(), "Dvorak");
            let opts = app.install_options();
            assert_eq!(opts.keymap, "us", "Dvorak is an xkb us variant");
            assert_eq!(opts.keyvariant, "dvorak");
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Options;
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            assert_eq!(app.selected_fs, 0);
            assert_eq!(app.filesystem(), "btrfs");

            run(
                &mut app,
                &doc,
                DestinationEffect::SetFilesystem {
                    filesystem: "xfs".into(),
                },
            );
            assert_eq!(app.selected_fs, 2);
            assert_eq!(app.filesystem(), "xfs");
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Options;
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            run(
                &mut app,
                &doc,
                DestinationEffect::SetSecurityKey { enabled: true },
            );
            assert!(app.fido2);
            assert!(app.install_options().fido2);
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            assert_eq!(local_text(&app.hostname), "tilo");
            assert_eq!(app.timezone(), "UTC");
            let opts = app.install_options();
            assert_eq!(opts.hostname, "tilo");
            assert_eq!(opts.timezone, "UTC");
            assert_eq!(opts.filesystem, "btrfs");
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.install_method = InstallMethod::Alongside;
            let opts = app.install_options();
            assert_eq!(opts.disk_size_bytes, 256_000_000_000);
            assert_eq!(opts.disk_major_minor, "8:0");
            assert_eq!(opts.disk_wwn, "0x5002538e40abcdef");
            assert_eq!(opts.disk_serial, "TILO-SAMPLE-SATA");
            assert_eq!(opts.alongside_root, "/dev/sda3");
            assert_eq!(
                opts.alongside_root_partuuid,
                "00000000-0000-0000-0000-000000000003"
            );
            assert_eq!(opts.alongside_root_size_bytes, 64_000_000_000);
            assert_eq!(opts.alongside_boot, "/dev/sda5");
            assert_eq!(
                opts.alongside_boot_partuuid,
                "00000000-0000-0000-0000-000000000005"
            );
            assert_eq!(opts.alongside_boot_size_bytes, 1024 * 1024 * 1024);
            assert_eq!(opts.alongside_efi, "/dev/sda4");
            assert_eq!(
                opts.alongside_efi_partuuid,
                "00000000-0000-0000-0000-000000000004"
            );
            assert_eq!(opts.alongside_efi_size_bytes, 2 * 1024 * 1024 * 1024);
        }
    }

    phase media_codecs_are_an_explicit_opt_in_that_threads_into_install_options;
    {
        let mut app = InstallerApp::with_disks(sample());
        app.step = Step::Media;
        let doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&doc, &root);
        assert!(!app.codecs, "media codecs are not selected by default");
        assert!(!app.install_options().codecs);
        let codecs = |enabled: bool| DestinationEffect::SetMediaCodecs { enabled };
        run(&mut app, &doc, codecs(true));
        assert!(app.codecs);
        assert!(app.install_options().codecs);
        assert_eq!(
            app.step,
            Step::Media,
            "Install records the choice and stays"
        );
        run(&mut app, &doc, codecs(false));
        assert!(!app.codecs, "the choice can be withdrawn");
    }

    phase image_filesystem_manifest_bounds_the_choice;
    {
        {
            assert_eq!(
                probes::filesystem_support_from_manifest("ext4\nxfs\n").unwrap(),
                [true, false, false]
            );
            assert_eq!(
                probes::filesystem_support_from_manifest("xfs\n").unwrap(),
                [true, true, false]
            );
            assert_eq!(
                probes::filesystem_support_from_manifest("amdgpu\nnouveau\nxe\nxfs\n").unwrap(),
                [true, true, false],
                "board GPU omissions must not hide supported filesystems"
            );
            assert!(probes::filesystem_support_from_manifest("unknown\n").is_err());
            assert!(probes::filesystem_support_from_manifest("amdgpu\namdgpu\n").is_err());

            let mut app = InstallerApp::with_disks(sample());
            app.set_filesystem_support(probes::filesystem_support_from_manifest("ext4\nxfs\n"));
            assert_eq!(app.available_filesystem_indices(), vec![0]);
            app.set_filesystem_support(probes::filesystem_support_from_manifest("xfs\n"));
            assert_eq!(app.available_filesystem_indices(), vec![0, 1]);
            app.set_filesystem_support(probes::filesystem_support_from_manifest("ext4\nxfs\n"));
            app.step = Step::Options;
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            run(
                &mut app,
                &doc,
                DestinationEffect::SetFilesystem {
                    filesystem: "ext4".into(),
                },
            );
            assert_eq!(
                app.filesystem(),
                "btrfs",
                "a hidden ext4 choice must be rejected"
            );

            app.selected_fs = 1; // stale selection from a media profile or UI rebuild
            assert!(!app.filesystem_policy_ready());
            let mut review = InstallerApp::for_shot(Step::Review);
            review.set_filesystem_support(probes::filesystem_support_from_manifest("ext4\nxfs\n"));
            review.selected_fs = 1;
            review.begin_install();
            assert!(!review.install_launched);
            assert_eq!(review.step, Step::Review);
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.apply_media_profile(imager_backend::destination::media_profile::MediaProfile {
                purpose: install_profile_model::default_purpose(),
                device_label: String::new(),
                hostname: Some("studio".into()),
                username: Some("ada".into()),
                full_name: None,
                locale: Some("en_US.UTF-8".into()),
                keyboard: Some("us".into()),
                timezone: Some("UTC".into()),
                filesystem: Some("ext4".into()),
                repairs: Vec::new(),
            });
            app.set_filesystem_support(probes::filesystem_support_from_manifest("ext4\nxfs\n"));
            assert_eq!(app.filesystem(), "btrfs");
            assert!(!app.profile_completion.unwrap().options_complete());

            app.set_filesystem_support(Err("manifest missing".into()));
            assert!(!app.filesystem_policy_ready());
            app.step = Step::Options;
            assert!(app.validate_current_step_details().is_err());
        }
    }
}

#[test]
fn the_media_step_is_one_install_button_and_review_repeats_it() {
    let title = crate::setup::strings::t("installer-media-title");
    let note = crate::setup::strings::t("installer-media-note");
    let chosen_copy = crate::setup::strings::t("installer-media-chosen");
    let mut app = InstallerApp::for_shot(Step::Media);
    let _ = build(&mut app);
    let composed = app.composed.element().unwrap();
    assert!(has_test_id(composed, "codecs_install"));
    assert!(has_test_id(composed, "codecs_skip"));
    assert!(
        !has_test_id(composed, "codecs_undo"),
        "nothing is chosen yet"
    );
    for absent in ["RPM Fusion", "Cisco", "patent", "licence"] {
        assert!(!note.contains(absent), "{absent}");
    }
    assert!(
        find_accessible(composed, &note).is_some(),
        "the rendered codec control must expose the formats source explanation"
    );
    let published = app.published()[0].clone().into_resource();
    let install = published
        .actions
        .iter()
        .find(|action| action.id == "set-media-codecs")
        .expect("published codec choice");
    let codecs = crate::setup::strings::t("imager-destination-set-media-codecs");
    assert!(
        install
            .description
            .as_deref()
            .is_some_and(|said| said.contains(&codecs)),
        "{:?}",
        install.description
    );
    let mut options = InstallerApp::for_shot(Step::Options);
    let _ = build(&mut options);
    assert!(
        find_accessible(options.composed.element().unwrap(), &title).is_none(),
        "Options no longer carries the formats"
    );

    app.codecs = true;
    app.dirty_structure = true;
    let _ = build(&mut app);
    let chosen = app.composed.element().unwrap();
    assert!(has_test_id(chosen, "codecs_undo"));
    assert!(!has_test_id(chosen, "codecs_install"));
    assert!(find_accessible(chosen, &chosen_copy).is_some());

    app.step = Step::Review;
    app.dirty_structure = true;
    let _ = build(&mut app);
    let review = app.composed.element().unwrap();
    assert!(
        find_accessible(review, "Install once the new system is online").is_some(),
        "Review repeats the codecs choice"
    );
}

fn hardware_report(state: hardware_report::SupportState) -> hardware_report::SupportReport {
    use hardware_report::{
        Applicability, ComponentKind, ComponentSupport, DestinationIdentity, REPORT_SCHEMA,
        ReportSource, SupportReason, SupportReport,
    };
    SupportReport {
        schema: REPORT_SCHEMA.into(),
        identity: DestinationIdentity {
            source: ReportSource::LocalScan,
            manufacturer: Some("Example".into()),
            model: Some("Laptop 14".into()),
            variant: Some("BOARD-1".into()),
            board: None,
            sku: None,
            firmware: None,
        },
        kernel: Some("6.17.0".into()),
        architecture: Some("x86_64".into()),
        components: vec![ComponentSupport {
            kind: ComponentKind::Storage,
            name: "Observed 9000".into(),
            present: true,
            state,
            applicability: Applicability::ObservationOnly,
            reason: SupportReason::NotDetected,
            evidence: Vec::new(),
            remedy: None,
        }],
    }
}

tilo_test_env::scenario! {
    fn the_review_begins_only_with_acknowledged_protection;

    phase review_begin_refuses_a_report_missing_an_essential_component;
    {
        use hardware_report::SupportState;
        for (state, blocked) in [
            (SupportState::Unverified, false),
            (SupportState::Unsupported, true),
        ] {
            let mut app =
                InstallerApp::with_disks(sample()).with_hardware_for_test(hardware_report(state));
            app.selected_disk = Some(0);
            app.step = Step::Review;
            app.tpm2 = false;
            app.recovery_ack = true;
            app.erase_confirmation.set("/dev/sda".to_string());
            *app.password.lock().unwrap() = "correct horse battery staple".to_string();
            let _ = build(&mut app);
            let begin_enabled = find_test_id(&app, "installer.next").map(Widget::is_focusable);
            assert_eq!(begin_enabled, Some(!blocked), "{state:?}");
            assert_eq!(
                find_test_id(&app, "installer.hardware.blocked").is_some(),
                blocked,
                "{state:?}"
            );
        }
    }

    phase review_begin_requires_ack_confirmation_and_power;
    {
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.step = Step::Review;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            assert!(!app.recovery_ack);

            let begin = app.begin_effect();
            run(&mut app, &doc, begin);
            assert_eq!(app.install_state, InstallState::Idle);
            assert_eq!(
                app.step,
                Step::Review,
                "blocked until the key is acknowledged"
            );

            perform(
                &mut app,
                &mut doc,
                "recovery_ack",
                &tilo_ui_base::a11y::Action::ToggleCheck,
            );
            assert!(app.recovery_ack, "ack flips on");

            activate(&mut app, &mut doc, "recovery_ack");
            assert!(!app.recovery_ack, "ack flips back off");
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.step = Step::Review;
            app.tpm2 = true;
            app.secure_boot = true;
            app.kernel_lockdown = true;
            app.target_verified_boot = true;
            let _ = build(&mut app);
            let begin_disabled = find_test_id(&app, "installer.next").map(|b| !b.is_focusable());
            assert_eq!(begin_disabled, Some(true), "Begin disabled until ack");

            let mut app2 = InstallerApp::with_disks(sample());
            app2.selected_disk = Some(0);
            app2.step = Step::Review;
            app2.tpm2 = true;
            app2.secure_boot = true;
            app2.kernel_lockdown = true;
            app2.target_verified_boot = true;
            app2.recovery_ack = true;
            app2.erase_confirmation.set("/dev/sda".to_string());
            *app2.password.lock().unwrap() = "804927".to_string();
            let _ = build(&mut app2);
            let begin_enabled = find_test_id(&app2, "installer.next").map(Widget::is_focusable);
            assert_eq!(begin_enabled, Some(true), "Begin enabled once acknowledged");
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.step = Step::Review;
            app.tpm2 = true;
            app.secure_boot = true;
            app.kernel_lockdown = true;
            app.target_verified_boot = true;
            app.recovery_ack = true;
            *app.password.lock().unwrap() = "804927".to_string();
            *app.password_confirmation.lock().unwrap() = "804927".to_string();
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            let begin = app.begin_effect();
            run(&mut app, &doc, begin);
            assert_eq!(app.step, Step::Review);
            assert_eq!(app.install_state, InstallState::Idle);
            assert!(app.status.contains("Type /dev/sda exactly"));

            app.erase_confirmation.set("/dev/sda".to_string());
            app.sync(&doc, &root);
            assert!(app.destructive_confirmation_ready());
            assert!(app.status.is_empty(), "typing clears the stale refusal");
            assert!(
                app.take_structure_dirty(),
                "typing the confirmation must rebuild the disabled Begin button"
            );
            let _ = build(&mut app);
            let begin_enabled = find_test_id(&app, "installer.next").map(Widget::is_focusable);
            assert_eq!(begin_enabled, Some(true));
            let field = find_test_id(&app, "installer.erase_confirmation")
                .and_then(Widget::accessibility)
                .expect("confirmation field");
            assert_eq!(field.value.as_deref(), Some("/dev/sda"));
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.step = Step::Review;
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            let _ = build(&mut app);
            assert!(!app.take_structure_dirty());
            for (index, character) in "/dev/sd".chars().enumerate() {
                app.erase_confirmation.update(|typed| typed.push(character));
                app.sync(&doc, &root);
                assert!(
                    !app.take_structure_dirty(),
                    "partial input rebuilt at {index}"
                );
            }
            app.erase_confirmation.update(|typed| typed.push('a'));
            app.sync(&doc, &root);
            assert!(
                app.take_structure_dirty(),
                "exact confirmation must enable Begin"
            );
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.install_method = InstallMethod::Alongside;
            let root = app
                .selected_disk_ref()
                .and_then(|disk| disk.alongside_root())
                .map(|partition| partition.name.clone())
                .expect("sample disk has one prepared alongside root");

            assert!(!app.destructive_confirmation_ready());
            app.erase_confirmation
                .set(app.selected_dev().expect("selected disk"));
            assert!(!app.destructive_confirmation_ready());
            app.erase_confirmation.set(root);
            assert!(app.destructive_confirmation_ready());
        }
        {
            let mut app = InstallerApp::for_shot(Step::Review);
            app.power_probe = || Some(7);
            let (_taffy, _root) = build(&mut app);
            let message = connect_power_message(7);
            assert!(
                find_accessible(&app, &message).is_some(),
                "Review must explain why destructive work cannot start"
            );

            app.begin_install();
            assert_eq!(app.step, Step::Review);
            assert_eq!(app.install_state, InstallState::Idle);
            assert!(!app.install_launched);
            assert_eq!(app.status, message);
            assert_eq!(
                &*secret_copy(&app.password),
                "correct horse battery staple",
                "the account secret stays editable because installation never started"
            );
        }
    }

    phase encryption_factors_name_every_enrolled_unlock;
    {
        {
            let mut app = InstallerApp::with_disks(sample());
            app.tpm2 = true;
            app.secure_boot = true;
            app.kernel_lockdown = true;
            app.target_verified_boot = true;
            assert_eq!(app.encryption_factors(), ["recovery key"]);

            *app.password.lock().unwrap() = "804927".to_string();
            assert_eq!(
                app.encryption_factors(),
                ["recovery key", "TPM-protected startup PIN"]
            );

            app.fido2 = true;
            assert_eq!(
                app.encryption_factors(),
                ["recovery key", "TPM-protected startup PIN", "FIDO2"]
            );
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.tpm2 = false;
            *app.password.lock().unwrap() = "correct horse battery".to_string();
            assert_eq!(
                app.encryption_factors(),
                ["recovery key", "boot passphrase"],
                "the review step must not claim a TPM factor this machine cannot provide"
            );

            clear_secret_buffer(&app.password);
            assert_eq!(app.encryption_factors(), ["recovery key"]);

            assert!(
                encrypt_description(VerifiedBootState::NoTpm).contains("no TPM2 chip"),
                "the chooser has to say the machine cannot unlock itself"
            );
            assert!(
                !encrypt_description(VerifiedBootState::TargetVerified)
                    .contains("Requires a TPM2 device"),
                "encryption no longer requires a TPM, and the copy must not claim it does"
            );
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Options;
            let _ = build(&mut app);
            let card = find_test_id(&app, "installer.content").expect("options content");
            let has_fido2 = card
                .children()
                .iter()
                .any(|c| c.test_id() == Some("fido2_choice"));
            assert!(
                has_fido2,
                "mandatory encryption exposes its optional extra factor"
            );
        }
    }

    phase encryption_readiness_follows_the_verified_boot_policy;
    {
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.tpm2 = false;
            app.recovery_ack = true;
            assert!(
                !app.encryption_ready(),
                "no TPM and no passphrase must not be installable"
            );

            *app.password.lock().unwrap() = "correct horse battery".to_string();
            assert!(app.encryption_ready(), "a passphrase unblocks it");

            *app.password.lock().unwrap() = "   ".to_string();
            assert!(
                !app.encryption_ready(),
                "blank passphrase is still no passphrase"
            );
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.tpm2 = true;
            app.secure_boot = true;
            app.kernel_lockdown = true;
            app.target_verified_boot = true;
            app.recovery_ack = true;
            assert!(
                !app.encryption_ready(),
                "silent TPM-only unlock is not a hardened startup boundary"
            );
            *app.password.lock().unwrap() = "123456".to_string();
            assert!(
                !app.encryption_ready(),
                "a trivial sequence must not become a TPM PIN"
            );
            *app.password.lock().unwrap() = "804927".to_string();
            assert!(app.encryption_ready());
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.tpm2 = true;
            app.secure_boot = false;
            app.kernel_lockdown = false;
            app.recovery_ack = true;

            *app.password.lock().unwrap() = "246810".to_string();
            assert!(
                !app.encryption_ready(),
                "an unverified TPM must not enable short-PIN mode"
            );
            *app.password.lock().unwrap() = "correct horse battery".to_string();
            assert!(app.encryption_ready());
            assert!(
                encrypt_description(VerifiedBootState::SecureBootInactive)
                    .contains("Secure Boot is not active")
            );
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.tpm2 = true;
            app.secure_boot = true;
            app.kernel_lockdown = false;
            app.recovery_ack = true;

            *app.password.lock().unwrap() = "246810".to_string();
            assert!(!app.encryption_ready());
            *app.password.lock().unwrap() = "correct horse battery".to_string();
            assert!(app.encryption_ready());
            assert!(
                encrypt_description(VerifiedBootState::KernelLockdownInactive)
                    .contains("kernel lockdown is not")
            );
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.tpm2 = true;
            app.secure_boot = true;
            app.kernel_lockdown = true;
            app.target_verified_boot = false;
            app.recovery_ack = true;

            *app.password.lock().unwrap() = "246810".to_string();
            assert!(
                !app.encryption_ready(),
                "the live medium's boundary cannot vouch for the installed target"
            );
            *app.password.lock().unwrap() = "correct horse battery".to_string();
            assert!(app.encryption_ready());
            assert!(
                encrypt_description(VerifiedBootState::TargetUnverified)
                    .contains("Verified boot is unavailable for this installation")
            );
        }
    }

    phase recovery_key_stays_out_of_shared_state_and_checks_power;
    {
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Install;
            app.install_state = InstallState::Running;

            app.receive("::tilo-recovery-key::abcd-efgh-ijkl-mnop");
            assert_eq!(app.recovery_key.as_deref(), Some("abcd-efgh-ijkl-mnop"));
            assert!(
                app.status_lines.is_empty(),
                "secret marker is not a log line"
            );
        }
        {
            let (services, recorder) = ui::process::ProcessServices::recording();
            let _host = ui::process::enter(&services);
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Install;
            app.install_state = InstallState::Running;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            assert!(
                !has_test_id(
                    app.build_composition(&Theme::dark()).as_ref(),
                    "recovery_key_copy"
                ),
                "the action must not exist before a key has been received"
            );

            let key = "abcd-efgh-ijkl-mnop";
            app.receive(&format!("{RECOVERY_KEY_PREFIX}{key}"));
            assert!(has_test_id(
                app.build_composition(&Theme::dark()).as_ref(),
                "recovery_key_copy"
            ));

            activate(&mut app, &mut doc, "recovery_key_copy");
            assert!(app.recovery_key_copied);
            assert_eq!(tilo_ui_base::clipboard_paste().as_deref(), Some(key));
            assert_eq!(
                recorder.secret_copies(),
                [key],
                "the key is copied as a secret, out of history and device sync"
            );
            assert!(
                !String::from_utf8_lossy(&doc.save()).contains(key),
                "the recovery secret must never be replicated through the CRDT"
            );

            app.receive(&format!("{RECOVERY_KEY_PREFIX}replacement-key"));
            assert!(
                !app.recovery_key_copied,
                "a newly emitted key needs its own copy confirmation"
            );
            assert!(
                !app.recovery_key_saved,
                "a newly emitted key needs its own post-generation saved confirmation"
            );
        }
        {
            // The live installer on its kiosk: no session or launcher handed it a clipboard.
            let services = ui::process::ProcessServices::null()
                .with_clipboard(ui::clipboard::unavailable());
            let _host = ui::process::enter(&services);
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Install;
            app.install_state = InstallState::Running;
            let key = "abcd-efgh-ijkl-mnop";
            app.receive(&format!("{RECOVERY_KEY_PREFIX}{key}"));
            let composition = app.build_composition(&Theme::dark());
            assert!(
                !has_test_id(composition.as_ref(), "recovery_key_copy"),
                "no Copy is offered where every copy is refused"
            );
            assert!(has_test_id(composition.as_ref(), "recovery_key_saved"));
            let reason = crate::setup::strings::t("installer-blocked-no-clipboard");
            assert_eq!(
                app.blocked(&DestinationEffect::CopyRecoveryKey),
                Some(("no_clipboard", reason.clone())),
                "an agent asking to copy is told why it cannot"
            );
            assert_eq!(
                app.apply(DestinationEffect::CopyRecoveryKey).err(),
                Some(reason)
            );
            assert!(!app.recovery_key_copied);
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Install;
            app.install_state = InstallState::Running;
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);

            app.receive(&format!("{RECOVERY_KEY_PREFIX}failure-safe-key"));
            app.receive(CTRL_FAIL);
            assert!(has_test_id(
                app.build_composition(&Theme::dark()).as_ref(),
                "recovery_key_copy"
            ));
            assert!(has_test_id(
                app.build_composition(&Theme::dark()).as_ref(),
                "recovery_key_saved"
            ));

            app.install_state = InstallState::Done;
            run(&mut app, &doc, DestinationEffect::Reboot);
            assert!(app.status.contains("Save the recovery key"));

            activate(&mut app, &mut doc, "recovery_key_saved");
            assert!(app.recovery_key_saved);
            assert!(
                !app.recovery_key_copied,
                "explicit saved acknowledgement must not be inferred from clipboard state"
            );
        }
    }
}

fn staged_app() -> InstallerApp {
    let disk = imager_backend::destination::disks::parse_lsblk(
        &imager_backend::destination::internal_handoff::tests::lsblk_json().to_string(),
    );
    let mut app = InstallerApp::with_disks(disk);
    app.apply_internal_handoff(imager_backend::destination::internal_handoff::tests::handoff());
    app
}

tilo_test_env::scenario! {
    fn an_install_reports_its_progress_and_end_state;

    phase install_events_stream_human_progress_and_set_terminal_state;
    {
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Install;
            app.install_state = InstallState::Running;

            app.receive("==> step one");
            app.receive("==> step two");
            assert!(app.status_lines.iter().any(|l| l == "==> step one"));
            assert!(app.status_lines.iter().any(|l| l == "==> step two"));
            assert_eq!(app.install_state, InstallState::Running);

            app.receive(CTRL_DONE);
            assert_eq!(app.install_state, InstallState::Done);
            assert!(
                app.take_focus_reset(),
                "completion must focus the first recovery action on the new Finish surface"
            );
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Install;
            app.install_state = InstallState::Running;
            app.receive("error: no space left on device");
            app.receive(CTRL_FAIL);
            assert_eq!(app.install_state, InstallState::Failed);
            assert_eq!(
                app.status_lines.last().map(String::as_str),
                Some("error: no space left on device"),
                "the useful backend failure must remain the newest visible log line"
            );
            assert!(
                app.take_focus_reset(),
                "failure must focus the first recovery action on the new error surface"
            );
        }
        {
            let lines = vec![
                "Starting install to /dev/vdb for user tilotest (btrfs).".to_string(),
                "Copying blob 0123456789abcdef".to_string(),
                "Copying blob fedcba9876543210".to_string(),
                "==> Preparing an encrypted root on /dev/vdb".to_string(),
                "Formatting LUKS2 with argon2id".to_string(),
            ];
            assert_eq!(
                friendly_install_status(&lines),
                vec![
                    "Installation choices verified",
                    "Copying verified Tilo system files…",
                    "Preparing encrypted storage…",
                ]
            );
        }
        {
            let lines = vec![
                "==> Enrolling disk-encryption unlock factors".to_string(),
                "  enrolling a FIDO2 security key (touch it now if prompted)…".to_string(),
            ];
            assert_eq!(
                friendly_install_status(&lines),
                vec![
                    "Securing disk unlock and recovery…",
                    "Touch your security key to continue…",
                ]
            );
        }
    }

    phase a_finished_install_reports_tilo_ready_whether_staged_or_from_a_medium;
    {
        for (mut app, last_line) in [
            (InstallerApp::with_disks(sample()), INSTALLED_REMOVE_MEDIUM),
            (staged_app(), INSTALLED_RESTART),
        ] {
            app.step = Step::Install;
            app.install_state = InstallState::Running;
            for line in [
                "Starting install to /dev/vdb for user ada (btrfs).",
                CTRL_DONE,
            ] {
                app.receive(line);
            }
            assert_eq!(app.install_state, InstallState::Done);
            assert_eq!(app.status_lines.last().map(String::as_str), Some(last_line));
            let state = app.console_state();
            let stages = state["install"]["stages"].as_array().unwrap();
            assert_eq!(
                stages.last().and_then(Value::as_str),
                Some("Tilo is ready"),
                "{last_line}: {stages:?}"
            );
        }
    }

    phase status_and_terminal_surfaces_have_live_accessible_semantics;
    {
        {
            let mut done = InstallerApp::for_shot(Step::Install);
            done.shot_finished();
            done.recovery_key = Some("test-recovery-key".to_string());
            let _ = build(&mut done);
            let success = find_accessible(&done, "Tilo is installed").expect("success title");
            assert_eq!(success.live, Some(Politeness::Polite));
            let reboot = find_accessible(&done, "Reboot now").expect("reboot action");
            assert_eq!(reboot.role, tilo_ui_base::a11y::Role::Button);
            assert!(
                !reboot.state.focusable,
                "power stays guarded until the key is saved"
            );
            assert!(find_accessible(&done, "Power off").is_some());
            done.recovery_key_saved = true;
            let _ = build(&mut done);
            assert!(
                find_accessible(&done, "Reboot now")
                    .expect("enabled reboot action")
                    .state
                    .focusable
            );

            let mut failed = InstallerApp::for_shot(Step::Install);
            failed.shot_failed();
            let _ = build(&mut failed);
            let failure = find_accessible(&failed, "Installation failed").expect("failure title");
            assert_eq!(failure.live, Some(Politeness::Assertive));
            assert!(find_accessible(&failed, "Review storage").is_some());
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Review;
            app.selected_disk = Some(0);
            app.set_status("Type /dev/sda exactly to confirm formatting it");
            let _ = build(&mut app);

            let validation =
                find_accessible(&app, "Type /dev/sda exactly to confirm formatting it")
                    .expect("validation status");
            assert_eq!(validation.live, Some(Politeness::Assertive));
            assert!(!app.focus_reset);

            let ack = find_accessible(&app, "I will save my recovery key")
                .expect("recovery acknowledgement");
            assert_eq!(ack.role, tilo_ui_base::a11y::Role::Checkbox);
            assert_eq!(ack.state.checked, Some(false));
            assert!(ack.state.focusable);
        }
    }
}

tilo_test_env::scenario! {
    fn the_step_layouts_fit_and_focus_only_usable_controls;

    phase step_layouts_mirror_taffy_and_fit_compact_viewports;
    {
        {
            for step in [
                Step::Network,
                Step::Disk,
                Step::Account,
                Step::Keyboard,
                Step::Options,
                Step::Review,
                Step::Install,
            ] {
                let mut app = InstallerApp::for_shot(step);
                let (taffy, root) = build(&mut app);
                assert_tree_mirrors(&app, &taffy, root);
            }
            let mut done = InstallerApp::for_shot(Step::Install);
            done.shot_finished();
            let (t, r) = build(&mut done);
            assert_tree_mirrors(&done, &t, r);

            let mut failed = InstallerApp::for_shot(Step::Install);
            failed.shot_failed();
            let (t, r) = build(&mut failed);
            assert_tree_mirrors(&failed, &t, r);
        }
        {
            let mut app = InstallerApp::for_shot(Step::Review);
            layout_at(&mut app, 320.0, 480.0);

            let content = find_test_id(&app, "installer.content").expect("review content");
            assert!(
                content.get_rect().x0 > 0.0 && content.get_rect().x1 < 320.0,
                "responsive content escaped the page gutter of a compact viewport: {:?}",
                content.get_rect()
            );
            assert!(
                content.get_rect().height() > 480.0,
                "representative Review must exercise vertical overflow"
            );

            let scroll = find_named(&app, "Scrollable").expect("installer scroll viewport");
            assert!(
                scroll.is_focusable(),
                "overflowing installer must expose Page Up/Down, Home/End and arrow scrolling"
            );
        }
        {
            let mut app = InstallerApp::for_shot(Step::Review);
            layout_at(&mut app, 1280.0, 768.0);
            let card = find_test_id(&app, "installer.content").expect("review content");
            let input = find_named(&app, "TextInput").expect("destructive confirmation input");
            let controls = card.children();
            let ack = controls
                .iter()
                .find(|child| child.test_id() == Some("recovery_ack"))
                .expect("recovery-key acknowledgement");
            let footer = find_test_id(&app, "installer.actions").expect("pinned review actions");
            let nav = footer
                .children()
                .into_iter()
                .next()
                .expect("review navigation");
            let scroll = find_named(&app, "Scrollable").expect("review content viewport");
            let full = ui::widget::ControlSize::Regular.height(&Theme::dark());
            assert!(
                input.get_rect().height() >= full - 0.5,
                "confirmation input collapsed: {:?}",
                input.get_rect()
            );
            assert!(
                card.get_rect().y1 >= input.get_rect().y1,
                "confirmation input escaped the review content"
            );
            assert!(
                ack.get_rect().height() > 0.0,
                "acknowledgement collapsed: {:?}",
                ack.get_rect()
            );
            assert!(
                ack.get_rect().y0 >= input.get_rect().y1,
                "acknowledgement overlaps confirmation input"
            );
            assert!(
                nav.get_rect().y0 >= scroll.get_rect().y1,
                "navigation overlaps the scrolling confirmation region"
            );
            assert!(
                nav.get_rect().y1 <= 768.0,
                "pinned navigation escaped the viewport"
            );
        }
    }

    phase pinned_actions_fit_large_text_and_focus_reveals_only_step_content;
    {
        for base in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            for scale in [1.0, 2.0] {
                let theme = base.clone().with_text_scale(scale);
                for step in [
                    Step::Keyboard,
                    Step::Network,
                    Step::Disk,
                    Step::Account,
                    Step::Options,
                    Step::Media,
                    Step::Review,
                    Step::Install,
                ] {
                    let mut app = InstallerApp::for_shot(step);
                    if step == Step::Install {
                        app.shot_finished();
                        app.recovery_key_saved = true;
                    }
                    app.set_status("Check your choices before continuing.");
                    layout_with_theme(&mut app, 800.0, 600.0, &theme);
                    let footer = find_test_id(&app, "installer.actions").expect("pinned actions");
                    let scroll = find_named(&app, "Scrollable").expect("content viewport");
                    assert!(
                        footer.get_rect().y0 >= scroll.get_rect().y1,
                        "{step:?}/{scale}: actions overlap content"
                    );
                    assert!(
                        footer.get_rect().y0 >= 0.0 && footer.get_rect().y1 <= 600.0,
                        "{step:?}/{scale}: actions outside viewport: {:?}",
                        footer.get_rect()
                    );
                    assert!(
                        scroll.get_rect().height() > 0.0,
                        "content viewport collapsed"
                    );
                    let status = find_accessible(&app, "Check your choices before continuing.")
                        .expect("validation line");
                    assert_eq!(status.live, Some(Politeness::Assertive));
                    let validation =
                        find_test_id(footer, "installer.validation").expect("pinned validation");
                    assert!(
                        validation.get_rect().height() > 0.0 && validation.get_rect().y1 <= 600.0
                    );
                    assert!(!has_test_id(scroll, "installer.actions"));
                }

                for variant in 0..3 {
                    let mut app = staged_app();
                    let action = match variant {
                        0 => {
                            app.step = Step::Account;
                            "return_to_host"
                        }
                        1 => {
                            app.shot_failed();
                            "installer.review_storage"
                        }
                        _ => {
                            app.step = Step::Media;
                            app.codecs = true;
                            "installer.next"
                        }
                    };
                    layout_with_theme(&mut app, 800.0, 600.0, &theme);
                    let footer = find_test_id(&app, "installer.actions").expect("variant footer");
                    let button = find_test_id(footer, action).expect("retained pinned action");
                    assert!(
                        button.get_rect().height() > 0.0 && button.get_rect().y1 <= 600.0,
                        "{action}/{scale} escaped viewport: {:?}",
                        button.get_rect()
                    );
                }

                assert_account_focus_reveal(&theme);
            }
        }
    }

    phase step_controls_are_focusable_only_when_usable;
    {
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.step = Step::Account;
            let (_taffy, _root) = build(&mut app);

            assert_eq!(
                count_focusable(&app),
                6,
                "4 inputs and 2 nav buttons must be focusable"
            );
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.selected_disk = Some(0);
            app.step = Step::Keyboard;
            let (_taffy, _root) = build(&mut app);

            assert_eq!(
                count_focusable(&app),
                2,
                "layout dropdown + Next must be focusable (no Back on the first step)"
            );

            let card = find_test_id(&app, "installer.content").expect("keyboard content");
            assert!(
                card.children().iter().any(|c| c.name() == "KeyboardView"),
                "the Keyboard step renders the visual keyboard preview"
            );
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Disk;
            let _ = build(&mut app);
            let footer = find_test_id(&app, "installer.actions").expect("disk actions");
            let next_disabled = footer
                .children()
                .last()
                .and_then(|nav| nav.children().last().map(|b| !b.is_focusable()));
            assert_eq!(next_disabled, Some(true), "Next disabled with no disk");

            let mut app2 = InstallerApp::with_disks(sample());
            app2.step = Step::Disk;
            app2.selected_disk = Some(0);
            let _ = build(&mut app2);
            let footer2 = find_test_id(&app2, "installer.actions").expect("disk actions");
            let next_enabled = footer2
                .children()
                .last()
                .and_then(|nav| nav.children().last().map(|b| b.is_focusable()));
            assert_eq!(
                next_enabled,
                Some(true),
                "Next enabled once a disk is selected"
            );
        }
        {
            let mut app = InstallerApp::with_disks(sample());
            app.step = Step::Disk;
            let _ = build(&mut app);
            let disk_rows: Vec<_> = app
                .disks
                .iter()
                .filter_map(|disk| find_test_id(&app, &disk.name))
                .collect();
            assert_eq!(disk_rows.len(), 2, "both sample disks render as rows");
            assert!(disk_rows.iter().any(|c| c.is_focusable()));
        }
        {
            let mut disks = sample();
            disks[0].is_system = true;
            let mut app = InstallerApp::with_disks(disks);
            app.step = Step::Disk;
            let _ = build(&mut app);
            let disk_rows: Vec<_> = app
                .disks
                .iter()
                .filter_map(|disk| find_test_id(&app, &disk.name))
                .collect();
            assert_eq!(disk_rows.len(), 2);
            assert!(!disk_rows[0].is_focusable());
            assert!(disk_rows[1].is_focusable());
        }
    }
}

fn assert_account_focus_reveal(theme: &Theme) {
    let mut account = InstallerApp::for_shot(Step::Account);
    let nodes = layout_with_theme(&mut account, 800.0, 600.0, theme);
    let next = find_test_id(&account, "installer.next")
        .expect("Next")
        .get_rect();
    let confirm =
        find_test_id(&account, "installer.password_confirm").expect("password confirmation");
    assert!(confirm.is_focusable());
    let target = nodes
        .values()
        .copied()
        .find(|&widget| {
            // SAFETY: the production node map belongs to the still-live,
            // unchanged app tree. No pointer survives the next rebuild.
            unsafe { (*widget).test_id() == Some("installer.password_confirm") }
        })
        .expect("confirmation layout node");
    assert!(
        // SAFETY: as above, the node is in the live, unchanged tree.
        account.reveal_descendant(target, unsafe { (*target).get_rect() }),
        "focus reveal must find the input"
    );
    let nodes = layout_with_theme(&mut account, 800.0, 600.0, theme);
    let confirm = find_test_id(&account, "installer.password_confirm")
        .unwrap()
        .get_rect();
    let viewport = find_named(&account, "Scrollable").unwrap().get_rect();
    assert!(
        confirm.y0 >= viewport.y0 && confirm.y1 <= viewport.y1,
        "focused confirmation not revealed: {confirm:?} in {viewport:?}"
    );
    assert_eq!(
        find_test_id(&account, "installer.next").unwrap().get_rect(),
        next,
        "revealing a field moved pinned navigation"
    );
    let offset = account.viewport_offset.get();
    let next_target = nodes
        .values()
        .copied()
        .find(|&widget| {
            // SAFETY: the current production node map, as above.
            unsafe { (*widget).test_id() == Some("installer.next") }
        })
        .expect("Next layout node");
    // SAFETY: as above.
    assert!(account.reveal_descendant(next_target, unsafe { (*next_target).get_rect() }));
    assert_eq!(
        account.viewport_offset.get(),
        offset,
        "footer focus scrolled content"
    );
}

fn count_focusable(widget: &dyn Widget) -> usize {
    let mut n = usize::from(widget.is_focusable());
    for c in widget.children() {
        n = n.saturating_add(count_focusable(c));
    }
    n
}

tilo_test_env::scenario! {
    fn a_media_profile_skips_configured_screens;

    phase media_profile_skips_configured_screens_and_routes_repairs;
    {
        {
            let mut app = InstallerApp::with_disks(Disk::samples());
            let object = serde_json::json!({
                "schema": 1, "purpose": "server", "device_label": "Travel computer",
                "hostname": "travel", "username": "owner", "full_name": "Owner",
                "locale": "en_US.UTF-8", "keyboard": "us", "timezone": "UTC",
                "filesystem": "btrfs",
            });
            app.apply_media_profile(
                imager_backend::destination::media_profile::validate_object(
                    object.as_object().unwrap(),
                )
                .unwrap(),
            );
            for selected in [Some(0), Some(1)] {
                app.selected_disk = selected;
                let options = app.install_options();
                assert_eq!(options.device_label, "Travel computer");
                assert_eq!(options.purpose, "server");
            }
        }
        {
            let mut app = InstallerApp::with_disks(Disk::samples());
            app.apply_media_profile(imager_backend::destination::media_profile::MediaProfile {
                purpose: install_profile_model::default_purpose(),
                device_label: String::new(),
                hostname: Some("studio".into()),
                username: Some("ada".into()),
                full_name: Some("Ada Example".into()),
                locale: Some("de_DE.UTF-8".into()),
                keyboard: Some("de".into()),
                timezone: Some("Europe/Berlin".into()),
                filesystem: Some("ext4".into()),
                repairs: Vec::new(),
            });
            app.tpm2 = false;
            assert_eq!(app.step, Step::Disk);
            assert_eq!(local_text(&app.hostname), "studio");
            assert_eq!(local_text(&app.username), "ada");
            assert_eq!(app.keymap(), "German");
            assert_eq!(app.filesystem(), "ext4");

            app.selected_disk = Some(0);
            app.go_next();
            assert_eq!(app.step, Step::Account);
            let _ = build(&mut app);
            assert!(find_accessible(&app, "Secure your account").is_some());
            assert!(find_accessible(&app, "Username").is_none());
            *app.password.lock().unwrap() = "correct horse battery staple".into();
            *app.password_confirmation.lock().unwrap() = "correct horse battery staple".into();
            app.go_next();
            assert_eq!(
                app.step,
                Step::Media,
                "configured Options is not repeated; the formats are still the owner's choice"
            );
            assert!(!app.codecs);
            app.go_back();
            assert_eq!(app.step, Step::Options);
            let _ = build(&mut app);
            assert!(
                has_test_id(app.composed.element().unwrap(), "fido2_choice"),
                "Review Back must keep on-device security choices reachable"
            );
            app.go_back();
            assert_eq!(app.step, Step::Account);
            app.go_back();
            assert_eq!(app.step, Step::Disk);
            app.go_back();
            assert_eq!(app.step, Step::Network);
        }
        {
            let mut app = InstallerApp::with_disks(Disk::samples());
            app.apply_media_profile(imager_backend::destination::media_profile::MediaProfile {
                purpose: install_profile_model::default_purpose(),
                device_label: String::new(),
                hostname: None,
                username: Some("ada".into()),
                full_name: Some("Ada Example".into()),
                locale: Some("de_DE.UTF-8".into()),
                keyboard: Some("de".into()),
                timezone: Some("Europe/Berlin".into()),
                filesystem: Some("ext4".into()),
                repairs: vec![
                    imager_backend::destination::media_profile::MediaProfileField::Hostname,
                ],
            });
            app.tpm2 = false;
            assert_eq!(app.step, Step::Disk, "sound keyboard is retained");
            assert_eq!(local_text(&app.username), "ada");
            assert_eq!(app.filesystem(), "ext4");

            app.selected_disk = Some(0);
            app.go_next();
            assert_eq!(app.step, Step::Account);
            *app.password.lock().unwrap() = "correct horse battery staple".into();
            *app.password_confirmation.lock().unwrap() = "correct horse battery staple".into();
            app.go_next();
            assert_eq!(app.step, Step::Options, "invalid hostname needs one repair");
            let _ = build(&mut app);
            assert!(find_accessible(&app, "Hostname").is_some());
            assert!(find_accessible(&app, "Filesystem").is_none());
            assert!(find_accessible(&app, "Time zone").is_none());
        }
        {
            let mut app = InstallerApp::with_disks(Disk::samples());
            app.apply_media_profile(imager_backend::destination::media_profile::MediaProfile {
                purpose: install_profile_model::default_purpose(),
                device_label: String::new(),
                hostname: Some("studio".into()),
                username: Some("ada".into()),
                full_name: Some("Ada Example".into()),
                locale: None,
                keyboard: Some("de".into()),
                timezone: Some("Europe/Berlin".into()),
                filesystem: Some("ext4".into()),
                repairs: vec![
                    imager_backend::destination::media_profile::MediaProfileField::Locale,
                ],
            });
            app.tpm2 = false;
            app.selected_disk = Some(0);
            app.go_next();
            *app.password.lock().unwrap() = "correct horse battery staple".into();
            *app.password_confirmation.lock().unwrap() = "correct horse battery staple".into();
            app.go_next();
            assert_eq!(app.step, Step::Options);
            let _ = build(&mut app);
            assert!(
                find_accessible_role(&app, "Language & locale", tilo_ui_base::a11y::Role::ComboBox).is_some(),
                "locale repair must be a bounded chooser"
            );
            assert!(find_accessible(&app, "Hostname").is_none());
            assert!(find_accessible(&app, "Filesystem").is_none());
            assert!(find_accessible(&app, "Time zone").is_none());
        }
        {
            let mut app = InstallerApp::with_disks(Disk::samples());
            app.apply_media_profile(imager_backend::destination::media_profile::MediaProfile {
                purpose: install_profile_model::default_purpose(),
                device_label: String::new(),
                hostname: Some("studio".into()),
                username: Some("ada".into()),
                full_name: Some("Ada Example".into()),
                locale: Some("de_DE.UTF-8".into()),
                keyboard: None,
                timezone: Some("Europe/Berlin".into()),
                filesystem: Some("ext4".into()),
                repairs: vec![
                    imager_backend::destination::media_profile::MediaProfileField::Keyboard,
                ],
            });
            assert_eq!(app.step, Step::Keyboard);
            assert_eq!(local_text(&app.hostname), "studio");
            assert_eq!(local_text(&app.username), "ada");
            assert!(app.status.contains("keyboard layout"));
        }
    }
}
