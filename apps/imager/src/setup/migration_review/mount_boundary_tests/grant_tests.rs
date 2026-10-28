use super::*;
use crate::setup::migration_review::{HomeRootSet, MountedRootSelection, RootKind};
use std::{fs::OpenOptions, io::Read, sync::mpsc};

struct Picker {
    selected: PathBuf,
    cancel_first: bool,
}
impl ui::host::FilePicker for Picker {
    fn pick(&self, request: ui::PickRequest) -> ui::PortalChannel {
        assert!(request.is_directory());
        let (sender, receiver) = mpsc::channel();
        if self.cancel_first {
            sender.send(ui::PortalOutcome::Cancelled).unwrap();
        }
        sender
            .send(ui::PortalOutcome::Opened(self.selected.clone()))
            .unwrap();
        receiver
    }
    fn open_file(&self, request: ui::PickRequest) -> ui::portal::OpenFileChannel {
        assert!(!request.is_directory());
        let (sender, receiver) = mpsc::channel();
        if self.cancel_first {
            sender.send(ui::portal::OpenFileOutcome::Cancelled).unwrap();
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.selected)
            .unwrap();
        assert!(file.metadata().unwrap().is_file());
        sender
            .send(ui::portal::OpenFileOutcome::Opened(
                ui::portal::OpenedFile::new(file, "display name is not path authority"),
            ))
            .unwrap();
        receiver
    }
    fn pick_multiple(&self, _: ui::PickRequest) -> ui::portal::MultiChannel {
        let (sender, receiver) = mpsc::channel();
        sender.send(ui::portal::MultiOutcome::Cancelled).unwrap();
        receiver
    }
    fn launch(&self, _: ui::portal::LaunchRequest) -> ui::portal::LaunchChannel {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(ui::portal::LaunchOutcome::Failed(
                "no external launch in fixture".into(),
            ))
            .unwrap();
        receiver
    }
}
fn root_set(locations: &Arc<MountedHomeLocations>, cancel: &AtomicBool) -> HomeRootSet {
    let refusal = locations.require_single_root(cancel).unwrap_err();
    HomeRootSet::from_native(
        crate::setup::portable_home_media::HomeSource::mounted_root_set(&refusal).unwrap(),
    )
}
pub(super) fn pending_grant(
    locations: Arc<MountedHomeLocations>,
    selected: &Path,
    cancel: &AtomicBool,
) -> (HomeRootSet, MountedRootSelection) {
    let services = ui::host::HostServices::null().with_files(Arc::new(Picker {
        selected: selected.into(),
        cancel_first: false,
    }));
    let _host = ui::host::enter(&services);
    let mut roots = root_set(&locations, cancel);
    let id = roots.locations()[0].0.clone();
    let mut chooser = roots
        .request(&id, "Mounted location", "Allow", cancel)
        .unwrap();
    let selection = chooser.poll().unwrap().unwrap();
    (roots, selection)
}
pub(super) fn exercise_matching_platform_grants(
    locations: Arc<MountedHomeLocations>,
    selected: &Path,
    wrong: &Path,
    cancel: &AtomicBool,
) {
    let (mut roots, selection) = pending_grant(locations.clone(), selected, cancel);
    assert!(!roots.locations()[0].2);
    roots.approve(selection, cancel).unwrap();
    roots.validate(cancel).unwrap();
    assert!(
        roots.locations()[0].2,
        "actual matching platform answer must retain permission custody"
    );

    // The same bytes/name on a different inode/mount do not authorize this entry.
    let bundle = roots.into_granted_roots(cancel).unwrap();
    bundle
        .with_roots(cancel, |granted| {
            assert_eq!(granted.len(), 2);
            assert_eq!(granted[0].placement(), Path::new(""));
            assert_eq!(granted[0].kind(), RootKind::Directory);
            assert_eq!(
                granted[0].file().metadata().unwrap().ino(),
                std::fs::metadata(wrong).unwrap().ino()
            );
            assert_eq!(
                granted[1].placement(),
                selected.strip_prefix(wrong).unwrap()
            );
            let expected = std::fs::metadata(selected).unwrap();
            let actual = granted[1].file().metadata().unwrap();
            assert_eq!(
                (actual.dev(), actual.ino()),
                (expected.dev(), expected.ino())
            );
            assert_eq!(granted[1].mount_id(), target_mount(selected));
            let data = if expected.is_file() {
                assert_eq!(granted[1].kind(), RootKind::File);
                let mut file = granted[1].file().try_clone().unwrap();
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).unwrap();
                bytes
            } else {
                assert_eq!(granted[1].kind(), RootKind::Directory);
                cap_std::fs::Dir::from_std_file(granted[1].file().try_clone().unwrap())
                    .read("private")
                    .unwrap()
            };
            assert_eq!(data, b"unapproved sibling bytes");
            Ok(())
        })
        .unwrap();
    let cancelled_after_callback = AtomicBool::new(false);
    assert_eq!(
        bundle
            .with_roots(&cancelled_after_callback, |_| {
                cancelled_after_callback.store(true, std::sync::atomic::Ordering::Release);
                Ok(())
            })
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    bundle.validate(cancel).unwrap();
    assert!(
        root_set(&locations, cancel)
            .into_granted_roots(cancel)
            .is_err()
    );
    let (pending, answer) = pending_grant(locations.clone(), selected, cancel);
    assert!(pending.into_granted_roots(cancel).is_err());
    drop(answer);
    drop(bundle);

    let wrong_path = if selected.is_file() {
        wrong.join(".hidden")
    } else {
        wrong.to_path_buf()
    };
    let (mut refused, selection) = pending_grant(locations.clone(), &wrong_path, cancel);
    assert_eq!(
        refused.approve(selection, cancel).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(!refused.locations()[0].2);

    let (mut retired, selection) = pending_grant(locations.clone(), selected, cancel);
    retired.cancel_request();
    assert!(retired.approve(selection, cancel).is_err());
    assert!(!retired.locations()[0].2);

    let (other, selection) = pending_grant(locations.clone(), selected, cancel);
    let mut replacement = root_set(&locations, cancel);
    assert!(
        replacement.approve(selection, cancel).is_err(),
        "another root-set generation must not accept the old answer"
    );
    drop(other);

    let services = ui::host::HostServices::null().with_files(Arc::new(Picker {
        selected: selected.into(),
        cancel_first: false,
    }));
    let _host = ui::host::enter(&services);
    let mut sequential = root_set(&locations, cancel);
    let id = sequential.locations()[0].0.clone();
    let mut first = sequential
        .request(&id, "Mounted location", "Allow", cancel)
        .unwrap();
    let first_selection = first.poll().unwrap().unwrap();
    let mut second = sequential
        .request(&id, "Mounted location", "Allow", cancel)
        .unwrap();
    let second_selection = second.poll().unwrap().unwrap();
    assert!(
        sequential.approve(first_selection, cancel).is_err(),
        "late previous request cannot replace current permission"
    );
    sequential.approve(second_selection, cancel).unwrap();
    assert!(
        sequential
            .request(
                "not-an-observed-location",
                "Mounted location",
                "Allow",
                cancel
            )
            .is_err()
    );

    let cancelled_services = ui::host::HostServices::null().with_files(Arc::new(Picker {
        selected: selected.into(),
        cancel_first: true,
    }));
    let _cancelled_host = ui::host::enter(&cancelled_services);
    let mut cancelled = root_set(&locations, cancel);
    let id = cancelled.locations()[0].0.clone();
    let mut chooser = cancelled
        .request(&id, "Mounted location", "Allow", cancel)
        .unwrap();
    assert_eq!(
        chooser.poll().err().unwrap().kind(),
        io::ErrorKind::Interrupted
    );
    assert!(
        chooser.poll().is_err(),
        "a late opened answer after cancellation must not become a grant"
    );
    assert!(!cancelled.locations()[0].2);
    assert!(
        cancelled
            .request(&id, "Mounted location", "Allow", &AtomicBool::new(true))
            .is_err()
    );
    assert!(
        locations.require_single_root(cancel).is_err(),
        "permission alone never enables whole-home export"
    );
}
