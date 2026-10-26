use super::*;

fn capacity_app() -> InstallerApp {
    let mut disk = sample_disks().remove(0);
    disk.partitions.truncate(2);
    disk.size_bytes = 256 << 30;
    let mut app = InstallerApp::with_disks(vec![disk]);
    app.selected_disk = Some(0);
    app.install_method = InstallMethod::Alongside;
    app.step = Step::Disk;
    app
}

fn set_size(app: &mut InstallerApp, gib: i64) -> bool {
    let encoded = app
        .alongside_capacity_binding()
        .unwrap()
        .invoke(ui::num::f64_of_i64(gib));
    let Ok(invocation) = encoded else {
        return false;
    };
    let resources = app.publish();
    let spec = tilo_actions::door::resolve(&resources, &invocation.address).unwrap();
    app.apply_encoded(&spec.build_json(&invocation.arguments).unwrap())
        .is_ok()
}

#[test]
fn capacity_field_and_console_project_exact_backend_write_extent() {
    let mut app = capacity_app();
    let full = app.guided_alongside_plan().unwrap();
    assert_eq!(
        app.install_options().guided_free_size_bytes,
        full.free_size_bytes,
        "no explicit choice retains the legacy full-extent plan"
    );
    app.set_erase_confirmation("/dev/sda".into());
    app.storage_preflight_passed = true;
    app.storage_preflight_request = Some("old-plan".into());
    let mut controls = Vec::new();
    app.compose_alongside_capacity(&mut controls);
    let field = controls
        .iter_mut()
        .find(|w| w.test_id() == Some("installer.alongside-capacity"))
        .unwrap();
    let mut doc = AutoCommit::new();
    assert!(field.perform_action(
        &ui::a11y::Action::SetValue("48".into()),
        &[],
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT
    ));
    let invocation = ui::widget::controls::take_control_invocation(field.as_mut()).unwrap();
    assert_eq!(invocation.arguments["gib"], 48);
    app.invoke_control(
        &invocation.address.resource,
        &invocation.address.action,
        invocation.arguments,
        None,
    );
    let plan = app.guided_alongside_plan().unwrap();
    assert_eq!(plan.free_size_bytes, 48 << 30);
    assert_eq!(plan.root_size_bytes, 45 << 30);
    assert_eq!(
        plan.root_start_bytes + plan.root_size_bytes,
        full.free_start_bytes + (48 << 30)
    );
    assert!(local_text(&app.erase_confirmation).is_empty());
    assert!(!app.storage_preflight_passed);
    assert!(app.storage_preflight_request.is_none());
    let summary = app.alongside_allocation_summary(&plan);
    assert!(summary.contains("remains unused"), "{summary}");
    let opts = app.install_options();
    let command = crate::install::install_command("/dev/sda", "person", &opts);
    let args = command
        .get_args()
        .map(|a| a.to_str().unwrap())
        .collect::<Vec<_>>();
    for (flag, number) in [
        ("--free-start-bytes", plan.free_start_bytes),
        ("--free-size-bytes", 48 << 30),
        ("--root-size-bytes", 45 << 30),
    ] {
        let index = args.iter().position(|arg| *arg == flag).unwrap();
        assert_eq!(args[index + 1], number.to_string());
    }
    // The console resolves the exact same published operation, without a click.
    let reply = invoke(
        &mut app,
        &mut doc,
        "set-size@installer.storage",
        &json!({"gib": 40}),
    );
    assert!(refused(&reply).is_none(), "{reply}");
    assert_eq!(app.install_options().guided_free_size_bytes, 40 << 30);
}

#[test]
fn capacity_rejects_shortage_overflow_wrong_step_and_stale_hardware() {
    let mut app = capacity_app();
    for gib in [0, -1, 34, 1024, i64::MAX] {
        assert!(!set_size(&mut app, gib));
        assert!(app.alongside_capacity.is_none());
    }
    assert!(set_size(&mut app, 35));
    assert_eq!(
        app.guided_alongside_plan().unwrap().root_size_bytes,
        32 << 30
    );
    app.step = Step::Review;
    assert!(
        !set_size(&mut app, 40),
        "size changes require returning to the target screen"
    );
    app.step = Step::Disk;
    app.disks[0].serial = "different-hardware".into();
    assert!(app.guided_alongside_plan().is_none());
    app.disks[0].serial = sample_disks()[0].serial.clone();
    assert!(app.guided_alongside_plan().is_some());
    app.disks[0].partitions[1].size_bytes += 1 << 30;
    assert!(
        app.guided_alongside_plan().is_none(),
        "a stale extent cannot silently grow or move"
    );
    let disks = app.disks.clone();
    app.refresh_disks_with(disks);
    assert!(app.alongside_capacity.is_none());
    assert!(app.selected_guided_extent.is_none());
}

#[test]
fn switching_regions_discards_capacity_and_rejects_a_queued_old_size_action() {
    let mut app = capacity_app();
    let mut middle = app.disks[0].partitions[1].clone();
    middle.name = "/dev/sda3".into();
    middle.part_uuid = "00000000-0000-0000-0000-000000000003".into();
    middle.start_bytes = 208 << 30;
    middle.size_bytes = 1 << 30;
    app.disks[0].partitions.push(middle);
    assert_eq!(app.guided_alongside_plans().len(), 2);
    assert!(
        app.alongside_capacity_binding().is_none(),
        "choose a region before sizing it"
    );
    let select = |index| {
        crate::operation::InstallerEffect::Install(Operation::SelectFreeRegion { index }).encoded()
    };
    assert!(app.apply_encoded(&select(0)).is_ok());
    assert!(set_size(&mut app, 35));
    let invocation = app
        .alongside_capacity_binding()
        .unwrap()
        .invoke(40.0)
        .unwrap();
    let resources = app.publish();
    let queued = tilo_actions::door::resolve(&resources, &invocation.address)
        .unwrap()
        .build_json(&invocation.arguments)
        .unwrap();
    app.set_erase_confirmation("/dev/sda".into());
    app.storage_preflight_passed = true;
    assert!(app.apply_encoded(&select(1)).is_ok());
    assert!(app.alongside_capacity.is_none());
    assert!(!app.storage_preflight_passed);
    assert!(local_text(&app.erase_confirmation).is_empty());
    assert_eq!(
        app.guided_alongside_plan(),
        Some(app.guided_alongside_plans()[1].clone())
    );
    assert!(
        app.apply_encoded(&queued).is_err(),
        "never size a different region implicitly"
    );
}

#[test]
fn backend_validates_the_exact_sized_allocation_without_disk_writes() {
    let mut app = capacity_app();
    assert!(set_size(&mut app, 48));
    let plan = app.guided_alongside_plan().unwrap();
    let disk = app.selected_disk_ref().unwrap();
    let mut args = vec![
        disk.size_bytes.to_string(),
        disk.logical_sector_size.to_string(),
        plan.free_start_bytes.to_string(),
        plan.free_size_bytes.to_string(),
        plan.efi_start_bytes.to_string(),
        plan.efi_size_bytes.to_string(),
        plan.boot_start_bytes.to_string(),
        plan.boot_size_bytes.to_string(),
        plan.root_start_bytes.to_string(),
        plan.root_size_bytes.to_string(),
    ];
    for p in &disk.partitions {
        args.push(format!(
            "{}:{}:{}",
            p.part_uuid, p.start_bytes, p.size_bytes
        ));
    }
    // The exact rule `tilo-guided-partition` asks the compiled helper to apply
    // before it writes any partition table entry. The helper's dispatcher
    // strips its verb before this call, so these are the numbers alone.
    let accepts = |args: &[String]| -> Result<(), String> {
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        tilo_install_helpers::staging_handoff::backend_commands::guided_geometry(&words)
    };
    assert_eq!(
        accepts(&args),
        Ok(()),
        "the backend must accept the smaller, reviewed write extent: {args:?}"
    );
    for (index, value) in [
        (3, (34_u64 << 30).to_string()),
        (3, u64::MAX.to_string()),
        (8, u64::MAX.to_string()),
        (2, (plan.free_start_bytes + 1).to_string()),
        (0, (plan.root_start_bytes + (32 << 30)).to_string()),
    ] {
        let mut invalid = args.clone();
        invalid[index] = value;
        assert!(
            accepts(&invalid).is_err(),
            "backend accepted unsafe geometry: {invalid:?}"
        );
    }
    let mut occupied = args.clone();
    occupied.push(format!(
        "stale-partition:{}:{}",
        plan.free_start_bytes,
        1 << 20
    ));
    assert!(
        accepts(&occupied).is_err(),
        "a stale free extent must fail before partition writes"
    );
}

#[test]
fn capacity_bounds_alignment_and_review_geometry_are_checked() {
    let mut app = capacity_app();
    let plan = app.guided_alongside_plan().unwrap();
    assert!(plan.with_capacity_bytes((35 << 30) + 1).is_none());
    assert!(
        plan.with_capacity_bytes(plan.free_size_bytes + (1 << 20))
            .is_none()
    );
    let mut overflow = plan;
    overflow.free_start_bytes = u64::MAX - (1 << 20);
    assert!(overflow.with_capacity_bytes(35 << 30).is_none());
    let mut invalid_disk = app.disks[0].clone();
    invalid_disk.partitions[1].size_bytes = u64::MAX;
    assert!(invalid_disk.guided_alongside_plans().is_empty());
    invalid_disk.partitions[1].size_bytes = 1 << 30;
    invalid_disk.partitions[1].start_bytes = 1 << 20;
    assert!(
        invalid_disk.guided_alongside_plans().is_empty(),
        "overlapping discovery must fail closed"
    );
    let stale_size = app
        .alongside_capacity_binding()
        .unwrap()
        .invoke(48.0)
        .unwrap();
    let resources = app.publish();
    let stale_size = tilo_actions::door::resolve(&resources, &stale_size.address)
        .unwrap()
        .build_json(&stale_size.arguments)
        .unwrap();
    app.disks[0].serial.push_str("-replaced");
    assert!(
        app.apply_encoded(&stale_size).is_err(),
        "a queued size action is hardware-bound"
    );
    app.disks[0].serial = sample_disks()[0].serial.clone();
    assert!(set_size(&mut app, 48));
    let old = app.guided_alongside_plan();
    assert!(set_size(&mut app, 40));
    app.step = Step::Review;
    let encoded = crate::operation::InstallerEffect::Install(Operation::BeginInstall {
        target: "/dev/sda".into(),
        allocation: old,
    })
    .encoded();
    assert!(
        app.apply_encoded(&encoded).is_err(),
        "old authorization must not install the resized plan"
    );
}
