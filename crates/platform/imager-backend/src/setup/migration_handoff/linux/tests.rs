use super::*;

use super::fixture::facts;

#[test]
fn source_locator_maps_through_fresh_mount_and_rejects_changed_or_writable_sources() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let home = base.join("home/person");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("original"), b"source is preserved").unwrap();
    let facts = facts(&base, true);
    let plan = capture_with(
        &home,
        "migration-1",
        Categories {
            home: true,
            ..Default::default()
        },
        &facts,
    )
    .unwrap();
    assert_eq!(
        plan.home.relative.decode().unwrap(),
        Path::new("home/person")
    );
    let source = resolve_with(&plan, &base, &facts, false).unwrap();
    assert_eq!(source.home, home);
    // Injected topology is not enough: production kernel attestation rejects
    // this ordinary writable fixture rather than trusting claimed ro flags.
    assert!(resolve_with(&plan, &base, &facts, true).is_err());
    let writable = self::facts(&base, false);
    assert!(resolve_with(&plan, &base, &writable, false).is_err());
    let mut changed = plan.clone();
    changed.home.volume.filesystem_uuid = "44444444-4444-4444-8444-444444444444".into();
    assert!(resolve_with(&changed, &base, &facts, false).is_err());
    fs::rename(&home, base.join("original-home")).unwrap();
    fs::create_dir(&home).unwrap();
    assert!(resolve_with(&plan, &base, &facts, false).is_err());
    assert_eq!(
        fs::read(base.join("original-home/original")).unwrap(),
        b"source is preserved"
    );
}

#[test]
fn mount_parser_preserves_native_paths_and_requires_both_readonly_flags() {
    let parsed = parse_mounts(
        b"12 1 8:1 /home /mnt/old\\040home ro,nosuid shared:1 - ext4 /dev/testdisk1 ro\n",
    )
    .unwrap();
    assert_eq!(parsed[0].point, Path::new("/mnt/old home"));
    assert!(parsed[0].read_only);
    assert!(!parse_mounts(b"12 1 8:1 / /mnt ro - ext4 /dev/testdisk1 rw\n").unwrap()[0].read_only);
    assert!(parse_mounts(b"12 1 8:1 / /mnt\\999 ro - ext4 /dev/testdisk1 ro\n").is_err());
    assert!(parse_mounts(b"malformed\n").is_err());
}

#[test]
fn reboot_plan_does_not_report_system_flatpak_restoration_without_original_inventory() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let home = base.join("home/person");
    fs::create_dir_all(&home).unwrap();
    let facts = facts(&base, true);
    let plan = capture_with(
        &home,
        "migration-1",
        Categories {
            home: true,
            flatpak: true,
            accounts: false,
        },
        &facts,
    )
    .unwrap();
    let source = resolve_with(&plan, &base, &facts, false).unwrap();
    let destination = base.join("new-home");
    let error = match source.review_transfer(&destination, &base.join("journals")) {
        Ok(_) => panic!("System applications cannot be silently omitted"),
        Err(error) => error,
    };
    assert!(error.contains("original reviewed app and repository inventory"));
    assert!(!destination.exists());
}

#[test]
fn related_flatpak_roots_require_same_operation_volume_and_real_readonly_grant() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let home = base.join("home/person");
    let installation = base.join("var/lib/flatpak");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&installation).unwrap();
    fs::write(
        installation.join("original-config"),
        b"original repository fixture",
    )
    .unwrap();
    let facts = facts(&base, true);
    let categories = Categories {
        home: true,
        flatpak: true,
        accounts: false,
    };
    let home_plan = capture_with(&home, "migration-1", categories, &facts).unwrap();
    let related = capture_with(&installation, "migration-1", categories, &facts).unwrap();
    let source = resolve_with(&home_plan, &base, &facts, false).unwrap();
    let mut wrong = related.clone();
    wrong.operation_id = "different-operation".into();
    let error = match source.resolve_related_flatpak_source(&wrong.bytes().unwrap()) {
        Err(error) => error,
        Ok(_) => panic!("A different operation cannot share the grant"),
    };
    assert!(error.contains("reviewed operation and source volume"));
    wrong = related.clone();
    wrong.home.volume.filesystem_uuid = "44444444-4444-4444-8444-444444444444".into();
    assert!(
        source
            .resolve_related_flatpak_source(&wrong.bytes().unwrap())
            .is_err()
    );
    let error = match source.resolve_related_flatpak_source(&related.bytes().unwrap()) {
        Err(error) => error,
        Ok(_) => panic!("A claimed topology cannot turn a writable fixture into a readonly grant"),
    };
    assert!(error.contains("Kernel source filesystem is writable"));
    assert_eq!(
        fs::read(installation.join("original-config")).unwrap(),
        b"original repository fixture"
    );
}

#[test]
fn retained_flatpak_inventory_cannot_bypass_home_mount_attestation_or_selection() {
    struct EmptyInventory;
    impl super::super::super::flatpak_transfer::Runner for EmptyInventory {
        fn run(&self, _home: &Path, args: &[String]) -> Result<String, String> {
            if matches!(args.first().map(String::as_str), Some("list" | "remotes")) {
                Ok(String::new())
            } else {
                Err("No fixture installations exist".into())
            }
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let home = base.join("home/person");
    fs::create_dir_all(&home).unwrap();
    let facts = facts(&base, true);
    let plan = capture_with(
        &home,
        "migration-1",
        Categories {
            home: true,
            flatpak: true,
            accounts: false,
        },
        &facts,
    )
    .unwrap();
    let mut source = resolve_with(&plan, &base, &facts, false).unwrap();
    let inventory = super::super::super::flatpak_transfer::HandoffInventory::capture(
        &EmptyInventory,
        &home,
        vec![],
        |_| Err("No fixture roots exist".into()),
    )
    .unwrap();
    let target = base.join("imported-home");
    let journals = base.join("journals");
    let error = match source.review_transfer_with_handoff(&target, &journals, &inventory) {
        Err(error) => error,
        Ok(_) => panic!("No readonly grant was created for this writable fixture"),
    };
    assert!(error.contains("Kernel source filesystem is writable"));
    source.plan.flatpak_inventory =
        Some(serde_json::from_slice(&inventory.bytes().unwrap()).unwrap());
    let error = match source.review_transfer(&target, &journals) {
        Err(error) => error,
        Ok(_) => panic!("Embedded inventory cannot replace an actual readonly source grant"),
    };
    assert!(error.contains("Kernel source filesystem is writable"));
    source.plan.flatpak_inventory = Some(serde_json::json!({"schema": 999}));
    assert!(source.review_transfer(&target, &journals).is_err());
    source.plan.flatpak_inventory = None;
    let error = match source.review_transfer(&target, &journals) {
        Err(error) => error,
        Ok(_) => panic!("A selected inventory cannot be silently omitted"),
    };
    assert!(error.contains("original reviewed"));
    source.plan.categories.flatpak = false;
    let error = match source.review_transfer_with_handoff(&target, &journals, &inventory) {
        Err(error) => error,
        Ok(_) => panic!("Flatpak restoration was not selected"),
    };
    assert!(error.contains("was not selected"));
    assert!(!target.exists());
    assert!(!journals.exists());
}

#[test]
fn cached_secondary_volume_is_reattested_before_resolving_another_root() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let home = base.join("home/person");
    let installation = base.join("var/lib/flatpak");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&installation).unwrap();
    let facts = facts(&base, true);
    let categories = Categories {
        home: true,
        flatpak: true,
        accounts: false,
    };
    let primary_plan = capture_with(&home, "migration-1", categories, &facts).unwrap();
    let mut secondary_plan =
        capture_with(&installation, "migration-1", categories, &facts).unwrap();
    let primary = resolve_with(&primary_plan, &base, &facts, false).unwrap();
    let mut candidate = primary.clone();
    secondary_plan.home.volume.filesystem_uuid = "44444444-4444-4444-8444-444444444444".into();
    candidate.plan = secondary_plan.clone();
    candidate.home = installation;
    candidate.pin = PinnedDirectory::open_absolute(&candidate.home).unwrap();
    // This is deliberately an unattested writable candidate. Matching cached
    // metadata must never waive the production kernel read-only check.
    let mut cache = vec![candidate];
    let error = match primary.resolve_inventory_root(&secondary_plan.bytes().unwrap(), &mut cache) {
        Err(error) => error,
        Ok(_) => panic!("A cached entry cannot fabricate a read-only grant"),
    };
    assert!(error.contains("Kernel source filesystem is writable"));
    assert_eq!(cache.len(), 1);
}
#[test]
fn retained_access_holds_mount_without_copying_and_keeps_tilo_state_separate() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Release(Arc<AtomicUsize>);
    impl MountRelease for Release {
        fn release(&self, _: &str) -> io::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let home = base.join("home/person");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("document"), b"original bytes").unwrap();
    let target = base.join("new-tilo-home");
    fs::create_dir(&target).unwrap();
    let topology = facts(&base, true);
    let plan = capture_with(
        &home,
        "retained-access",
        Categories {
            home: true,
            ..Default::default()
        },
        &topology,
    )
    .unwrap();
    let mut source = resolve_with(&plan, &base, &topology, false).unwrap();
    let released = Arc::new(AtomicUsize::new(0));
    source.mount = Some(Arc::new(MountedVolume {
        service: Box::new(Release(Arc::clone(&released))),
        device: "/dev/testdisk1".into(),
        automatic_release: true,
    }));
    let retained = RetainedSource {
        root_permit: None,
        source,
        mode: AccessMode::ReadOnly,
    };
    retained.validate_with(&topology, false).unwrap();
    retained.validate_plan(&plan).unwrap();
    retained.validate_target(&target).unwrap();
    assert!(retained.validate_target(&home).is_err());
    assert!(
        retained.validate_with(&topology, true).is_err(),
        "ordinary fixture cannot assert a real kernel readonly mount"
    );
    assert_eq!(retained.path(), home);
    assert_eq!(released.load(Ordering::SeqCst), 0);
    assert_eq!(fs::read(home.join("document")).unwrap(), b"original bytes");
    assert_eq!(
        fs::read_dir(&target).unwrap().count(),
        0,
        "access creates no imported home, journals, configs or file copies"
    );
    let mut changed = plan;
    changed.categories.accounts = true;
    assert!(retained.validate_plan(&changed).is_err());
    drop(retained);
    assert_eq!(released.load(Ordering::SeqCst), 1);
}
#[test]
fn shared_access_is_distinct_from_copy_grant_and_refuses_unverified_kernel_safety() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let home = base.join("home/person");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("document"), b"shared original").unwrap();
    let writable = facts(&base, false);
    let plan = capture_with(
        &home,
        "shared-access",
        Categories {
            home: true,
            ..Default::default()
        },
        &writable,
    )
    .unwrap();
    let source = resolve_mode_with(&plan, &base, &writable, false, AccessMode::ReadWrite).unwrap();
    let retained = RetainedSource {
        root_permit: None,
        source,
        mode: AccessMode::ReadWrite,
    };
    retained.validate_with(&writable, false).unwrap();
    assert!(
        resolve_with(&plan, &base, &writable, false).is_err(),
        "copy never accepts writable access grants"
    );
    assert!(
        retained.validate_with(&writable, true).is_err(),
        "kernel nodev/nosuid/noexec attestation cannot be faked by inventory"
    );
    assert!(
        retained.validate_with(&facts(&base, true), false).is_err(),
        "readonly source must not claim edits appear in both OSes"
    );
    assert_eq!(fs::read(home.join("document")).unwrap(), b"shared original");
}
#[test]
fn retained_policy_cannot_enter_copy_or_escalate_legacy_grant_to_writes() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let home = base.join("home/person");
    fs::create_dir_all(&home).unwrap();
    let topology = facts(&base, true);
    let mut plan = capture_with(
        &home,
        "retained-policy",
        Categories {
            home: true,
            ..Default::default()
        },
        &topology,
    )
    .unwrap();
    assert!(
        retain_access(&plan, AccessMode::ReadWrite)
            .err()
            .expect("legacy write refusal")
            .to_string()
            .contains("explicit retained-files policy")
    );
    plan.home_policy = HomePolicy::AccessReadWrite;
    let source = resolve_with(&plan, &base, &topology, false).unwrap();
    let target = base.join("no-imported-home");
    let receipts = base.join("no-receipts");
    assert!(source.review_transfer(&target, &receipts).is_err());
    assert!(!target.exists());
    assert!(!receipts.exists());
    assert!(
        retain_access(&plan, AccessMode::ReadOnly)
            .err()
            .expect("readonly downgrade refusal")
            .to_string()
            .contains("cannot silently become")
    );
}

#[test]
fn capture_unsupported_filesystem_has_explicit_kind_without_persisting_plan() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let home = base.join("home");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("document"), b"original").unwrap();
    let mut topology = facts(&base, false);
    topology.devices[0].children[0].fstype = Some("vfat".into());
    let error = capture_with(
        &home,
        "migration-1",
        Categories {
            home: true,
            ..Default::default()
        },
        &topology,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_eq!(fs::read(home.join("document")).unwrap(), b"original");
    assert_eq!(fs::read_dir(&base).unwrap().count(), 1);
}

#[test]
fn unrelated_cgroup_namespace_root_preserves_storage_capture_and_cannot_be_selected() {
    if rustix::process::geteuid().is_root() {
        tilo_test_env::require_capability!(
            Chown,
            tilo_test_env::chown_to(1),
            "as root the fixture gives the original Home to a non-root owner"
        );
    }
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let home = base.join("home");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("document"), b"preserved original").unwrap();
    let _caller = crate::setup::home_capture::original_caller::fixture_source_owner(&home);
    let namespace = base.join("cgroup");
    fs::create_dir(&namespace).unwrap();
    let parsed = parse_mounts(
        format!(
            "99 1 0:30 /../../../../../.. {} rw - cgroup2 cgroup rw\n",
            namespace.display()
        )
        .as_bytes(),
    )
    .unwrap();
    let mut reviewed = facts(&base, false);
    reviewed.mounts.extend(parsed);
    let categories = Categories {
        home: true,
        flatpak: false,
        accounts: false,
    };
    capture_with(
        &home,
        &uuid::Uuid::new_v4().to_string(),
        categories,
        &reviewed,
    )
    .unwrap();
    assert_eq!(
        capture_with(
            &namespace,
            &uuid::Uuid::new_v4().to_string(),
            categories,
            &reviewed
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::Unsupported
    );
    assert!(parse_mounts(b"99 1 8:1 /../home /mnt rw - ext4 /dev/testdisk1 rw\n").is_err());
    assert!(parse_mounts(b"99 1 0:30 /../../.. /../cgroup rw - cgroup2 cgroup rw\n").is_err());
    assert_eq!(
        fs::read(home.join("document")).unwrap(),
        b"preserved original"
    );
}

#[test]
fn original_source_retirement_closes_descriptors_confirms_release_and_quarantines_failure() {
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Release {
        watched: Vec<i32>,
        calls: Arc<AtomicUsize>,
        drops: Arc<AtomicUsize>,
        fail: bool,
    }
    impl MountRelease for Release {
        fn release(&self, _: &str) -> io::Result<()> {
            // Actual FD lifetime, not a copied metadata assertion.
            for fd in &self.watched {
                assert_eq!(unsafe { libc::fcntl(*fd, libc::F_GETFD) }, -1);
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(io::ErrorKind::PermissionDenied.into())
            } else {
                Ok(())
            }
        }
    }
    impl Drop for Release {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let home = base.join("home/person");
    fs::create_dir_all(&home).unwrap();
    let topology = facts(&base, true);
    let plan = capture_with(
        &home,
        &uuid::Uuid::new_v4().to_string(),
        Categories {
            home: true,
            ..Default::default()
        },
        &topology,
    )
    .unwrap();
    for fail in [false, true] {
        let source = resolve_with(&plan, &base, &topology, false).unwrap();
        let record = tempfile::tempfile().unwrap();
        let inventory = tempfile::tempfile().unwrap();
        let watched = vec![
            source.pin.file.as_raw_fd(),
            record.as_raw_fd(),
            inventory.as_raw_fd(),
        ];
        let calls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let mut source = source;
        source.mount = Some(Arc::new(MountedVolume {
            service: Box::new(Release {
                watched,
                calls: calls.clone(),
                drops: drops.clone(),
                fail,
            }),
            device: "/dev/owned-test".into(),
            automatic_release: true,
        }));
        let result =
            crate::setup::home_capture::retire_original_source(source, record, Some(inventory));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        if fail {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(
                drops.load(Ordering::SeqCst),
                0,
                "failed actual guard must remain retained"
            );
        } else {
            result.unwrap();
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no silent Drop second release"
        );
    }
    // Ordinary borrowed resolvers confer no mount ownership and release none.
    let source = resolve_with(&plan, &base, &topology, false).unwrap();
    assert!(source.mount.is_none());
    source.retire().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let mut source = resolve_with(&plan, &base, &topology, false).unwrap();
    source.mount = Some(Arc::new(MountedVolume {
        service: Box::new(Release {
            watched: vec![],
            calls: calls.clone(),
            drops: drops.clone(),
            fail: false,
        }),
        device: "/dev/owned-shared-test".into(),
        automatic_release: true,
    }));
    let shared = source.clone();
    assert!(source.retire().is_err());
    drop(shared);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no unmount with shared source custody"
    );
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "retain nonunique custody after failure"
    );
}

#[test]
fn owned_source_mount_witness_refuses_replacement_alias_and_namespace_change() {
    let temp = tempfile::tempdir().unwrap();
    let mut current = facts(temp.path(), true);
    let namespace = (4, 8);
    let witness = OwnedMountWitness::capture(&current, temp.path(), namespace).unwrap();
    witness.validate(&current, namespace).unwrap();
    assert!(witness.validate(&current, (4, 9)).is_err());
    current.mounts[0].id += 1;
    assert!(witness.validate(&current, namespace).is_err());
    current.mounts[0].id -= 1;
    let mut alias = current.mounts[0].clone();
    alias.id += 1;
    alias.point = temp.path().join("alias");
    current.mounts.push(alias);
    assert!(witness.validate(&current, namespace).is_err());
    assert!(OwnedMountWitness::capture(&current, temp.path(), namespace).is_err());
    current.mounts.pop();
    let mut stacked = current.mounts[0].clone();
    stacked.id += 1;
    stacked.number = "8:2".into();
    current.mounts.push(stacked);
    assert!(witness.validate(&current, namespace).is_err());
}

mod retirement_batch {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Release {
        watched: Vec<i32>,
        calls: Arc<AtomicUsize>,
        drops: Arc<AtomicUsize>,
        fail: bool,
    }
    impl MountRelease for Release {
        fn release(&self, _: &str) -> io::Result<()> {
            for fd in &self.watched {
                assert_eq!(unsafe { libc::fcntl(*fd, libc::F_GETFD) }, -1);
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(io::ErrorKind::PermissionDenied.into())
            } else {
                Ok(())
            }
        }
    }
    impl Drop for Release {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn source(base: &Path, name: &str) -> ResolvedSource {
        let home = base.join(name);
        fs::create_dir_all(&home).unwrap();
        let topology = facts(base, true);
        let plan = capture_with(
            &home,
            &uuid::Uuid::new_v4().to_string(),
            Categories {
                home: true,
                ..Default::default()
            },
            &topology,
        )
        .unwrap();
        resolve_with(&plan, base, &topology, false).unwrap()
    }
    fn mount(
        watched: Vec<i32>,
        fail: bool,
    ) -> (Arc<MountedVolume>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let mount = Arc::new(MountedVolume {
            service: Box::new(Release {
                watched,
                calls: calls.clone(),
                drops: drops.clone(),
                fail,
            }),
            device: "/dev/exact-owned-batch-test".into(),
            automatic_release: true,
        });
        (mount, calls, drops)
    }
    #[test]
    fn identical_resolver_arcs_retire_once_after_actual_pin_close() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let mut source = source(&base, "home");
        let (mount, calls, drops) = mount(vec![source.pin.file.as_raw_fd()], false);
        source.mount = Some(mount);
        let source = Arc::new(source);
        ResolvedSource::retire_many(vec![source.clone(), source]).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn distinct_roots_sharing_mount_close_all_pins_before_single_release() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        for fail in [false, true] {
            let mut home = source(&base, "home");
            let mut apps = source(&base, "applications");
            assert_ne!(home.pin.file.as_raw_fd(), apps.pin.file.as_raw_fd());
            let (mount, calls, drops) = mount(
                vec![home.pin.file.as_raw_fd(), apps.pin.file.as_raw_fd()],
                fail,
            );
            home.mount = Some(mount.clone());
            apps.mount = Some(mount);
            let result = ResolvedSource::retire_many(vec![Arc::new(home), Arc::new(apps)]);
            if fail {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
                assert_eq!(drops.load(Ordering::SeqCst), 0, "failed guard retained");
            } else {
                result.unwrap();
                assert_eq!(drops.load(Ordering::SeqCst), 1);
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1, "no second Drop release");
        }
    }
    #[test]
    fn external_same_volume_custody_refuses_that_release_and_retires_unrelated_mount() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let mut held = source(&base, "held");
        let mut alias = source(&base, "alias");
        let mut unrelated = source(&base, "unrelated");
        let (held_mount, held_calls, held_drops) = mount(vec![], false);
        let (other_mount, other_calls, other_drops) =
            mount(vec![unrelated.pin.file.as_raw_fd()], false);
        held.mount = Some(held_mount.clone());
        alias.mount = Some(held_mount);
        unrelated.mount = Some(other_mount);
        let held = Arc::new(held);
        let external = held.clone();
        assert!(
            ResolvedSource::retire_many(vec![held, Arc::new(alias), Arc::new(unrelated)]).is_err()
        );
        assert!(
            external.pin.file.metadata().is_ok(),
            "external pin remains live"
        );
        assert_eq!(
            other_calls.load(Ordering::SeqCst),
            1,
            "independent owned mount retired"
        );
        assert_eq!(other_drops.load(Ordering::SeqCst), 1);
        drop(external);
        assert_eq!(
            held_calls.load(Ordering::SeqCst),
            0,
            "no release with same-volume external custody"
        );
        assert_eq!(
            held_drops.load(Ordering::SeqCst),
            0,
            "retain exact shared mount custody"
        );
    }
}

#[test]
fn mount_parser_preserves_propagation_instead_of_treating_shared_source_as_private() {
    let parsed = parse_mounts(b"12 1 8:1 / /run rw shared:7 - ext4 /dev/testdisk1 rw\n13 1 8:2 / /private rw - xfs /dev/testdisk2 rw\n14 1 8:3 / /slave rw master:7 - btrfs /dev/testdisk3 rw\n").unwrap();
    assert!(!parsed[0].private);
    assert_eq!(parsed[0].filesystem, "ext4");
    assert!(parsed[1].private);
    assert_eq!(parsed[1].filesystem, "xfs");
    assert!(!parsed[2].private);
}
