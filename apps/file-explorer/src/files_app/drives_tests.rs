use super::*;

fn entry(id: &str, label: &str, internal: bool, open_at: Option<&Path>) -> DriveEntry {
    DriveEntry {
        id: id.into(),
        label: label.into(),
        size_bytes: 512_000_000_000,
        internal,
        open_at: open_at.map(|path| path.display().to_string()),
    }
}

/// The drives the opener below was asked for, across the threads it runs on.
static ASKED: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[expect(
    clippy::unnecessary_wraps,
    reason = "it stands in for the shell's opener, which can refuse"
)]
fn record_open(drive: &str) -> Result<(), String> {
    ASKED.lock().unwrap().push(drive.to_owned());
    Ok(())
}

fn refuse_open(_drive: &str) -> Result<(), String> {
    Err("The person chose not now".into())
}

/// Wait for the answer of a request that runs off the UI thread.
fn settle(files: &mut FileExplorerWidget, done: impl Fn(&FileExplorerWidget) -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while !done(files) {
        assert!(
            std::time::Instant::now() < deadline,
            "the request never answered"
        );
        files.poll_drives();
        std::thread::yield_now();
    }
}

#[test]
fn rows_name_unnamed_drives_by_where_they_are_and_say_when_open() {
    let windows = drive_place(&entry("nvme0n1p3:1", "Windows", true, None));
    assert_eq!(windows.label, "Windows");
    assert!(windows.internal);
    assert_eq!(windows.detail, crate::i18n::format_bytes(512_000_000_000));
    let unnamed = drive_place(&entry(
        "sdb1:7",
        "  ",
        false,
        Some(Path::new("/run/media/u/x")),
    ));
    assert_eq!(unnamed.label, t("files-drive-unnamed-external"));
    assert_eq!(unnamed.detail, t("files-drive-open"));
    assert_eq!(unnamed.open_at, Some(PathBuf::from("/run/media/u/x")));
    let inside = drive_place(&entry("sda2:1", "", true, None));
    assert_eq!(inside.label, t("files-drive-unnamed-internal"));
}

#[test]
fn choosing_a_closed_drive_asks_the_shell_and_never_navigates() {
    let mut files = FileExplorerWidget::new();
    let before = files.current_path();
    files.debug_show_drives(vec![entry("nvme0n1p3:1", "Windows", true, None)]);
    files.set_drive_opener(record_open);
    assert!(files.apply_command(FileCommand::OpenDrive("nvme0n1p3:1".into())));
    settle(&mut files, |_| {
        ASKED.lock().unwrap().iter().any(|id| id == "nvme0n1p3:1")
    });
    assert_eq!(
        files.current_path(),
        before,
        "opening goes through the shell"
    );
    assert!(
        files.apply_command(FileCommand::OpenDrive("gone:9".into())),
        "an unknown drive says so"
    );
    assert_eq!(
        files.status.as_deref(),
        Some(
            t_args(
                "files-drive-open-failed",
                &[arg("error", t("files-drive-gone"))]
            )
            .as_str()
        )
    );
}

#[test]
fn a_refusal_from_the_shell_is_shown() {
    let mut files = FileExplorerWidget::new();
    files.debug_show_drives(vec![entry("sdb1:7", "Stick", false, None)]);
    files.set_drive_opener(refuse_open);
    files.apply_command(FileCommand::OpenDrive("sdb1:7".into()));
    settle(&mut files, |files| files.status.is_some());
    assert_eq!(
        files.status.as_deref(),
        Some(
            t_args(
                "files-drive-open-failed",
                &[arg("error", "The person chose not now")]
            )
            .as_str()
        )
    );
}

#[test]
fn an_open_drive_this_window_can_read_opens_here() {
    let mount = tempfile::tempdir().unwrap();
    std::fs::write(mount.path().join("Readme.txt"), b"x").unwrap();
    let mut files = FileExplorerWidget::new();
    files.debug_show_drives(vec![entry("sdb1:8", "Stick", false, Some(mount.path()))]);
    files.set_drive_opener(refuse_open);
    files.apply_command(FileCommand::OpenDrive("sdb1:8".into()));
    assert_eq!(files.current_path(), mount.path());
    assert!(files.status.is_none(), "the shell was not asked");
}

#[test]
fn sample_drive_opens_the_capture_folder_without_asking_the_shell() {
    let capture = tempfile::tempdir().unwrap();
    let mut files = FileExplorerWidget::new();
    files.open_path(capture.path().to_path_buf());
    files.debug_show_sample_drives();
    files.set_drive_opener(refuse_open);
    assert_eq!(files.drives.shown.len(), 2);
    assert_eq!(
        files.drives.shown[1].open_at.as_deref(),
        capture.path().to_str()
    );
    let elsewhere = tempfile::tempdir().unwrap();
    files.open_path(elsewhere.path().to_path_buf());
    assert!(files.apply_command(FileCommand::OpenDrive("sdb1:7".into())));
    assert_eq!(files.current_path(), capture.path());
    assert!(files.status.is_none());
}

#[test]
fn every_drive_is_a_resource_with_an_open_action() {
    // Drives are the host person's, like cloud accounts: published only
    // inside their own callback.
    crate::files_app::tests::with_owner_callback(drives_are_published);
}

fn drives_are_published() {
    let mut files = FileExplorerWidget::new();
    files.debug_show_drives(vec![
        entry("nvme0n1p3:1", "Windows", true, None),
        entry("sdb1:7", "", false, Some(Path::new("/run/media/u/Stick"))),
    ]);
    let document = automerge::AutoCommit::new();
    let resources: Vec<Resource> = crate::files_app::published::tree(&files, &document)
        .into_iter()
        .flat_map(|resource| resource.children)
        .filter(|resource| resource.kind == "files.drive")
        .collect();
    assert_eq!(resources.len(), 2);
    assert_eq!(resources[0].id.as_str(), "files.drive:nvme0n1p3:1");
    assert_eq!(resources[0].kind, "files.drive");
    assert_eq!(resources[0].label, "Windows");
    assert!(
        resources
            .iter()
            .all(|resource| resource.actions.iter().any(|action| action.id == "open"))
    );
    assert!(
        resources[1]
            .presentation
            .as_ref()
            .and_then(|presentation| presentation.badge.clone())
            .is_some(),
        "an open drive says so"
    );
}

#[test]
fn the_window_starts_with_no_drives_and_tests_never_ask_a_shell() {
    let mut files = FileExplorerWidget::new();
    assert!(files.drives.shown.is_empty());
    assert!(!files.poll_drives());
    assert!(files.drives.next_wake().is_none());
}

/// How often the lister below was asked, and whether it may answer yet.
static LISTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ANSWER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Answers once the test lets it, as a shell busy elsewhere would; a
/// test that never lets it gets an error, which shows no drives.
fn held_lister() -> Result<Vec<DriveEntry>, String> {
    use std::sync::atomic::Ordering;
    LISTED.fetch_add(1, Ordering::SeqCst);
    let deadline = tilo_test_env::hang_deadline();
    while !ANSWER.load(Ordering::SeqCst) {
        if std::time::Instant::now() >= deadline {
            return Err("the test never let the lister answer".into());
        }
        std::thread::yield_now();
    }
    Ok(vec![entry("sdb1:7", "Stick", false, None)])
}

#[test]
fn an_idle_window_never_asks_for_drives_and_each_announced_change_asks_once() {
    use std::sync::atomic::Ordering;
    let mut files = FileExplorerWidget::new();
    files.set_drive_lister(held_lister);
    for _ in 0..3 {
        assert!(!files.poll_drives());
    }
    assert_eq!(files.drives.next_wake(), None, "no deadline while idle");
    assert_eq!(LISTED.load(Ordering::SeqCst), 0, "nothing asked while idle");

    // The shell says the drives changed (the first answer of the wait does
    // too): the window wakes at once and asks once.
    let changed = ui::collaboration::CollabEvent::DrivesChanged;
    ui::widget::Collaborative::handle_collab_event(&mut files, &changed);
    assert!(
        files
            .drives
            .next_wake()
            .is_some_and(|due| due <= Instant::now())
    );
    files.poll_drives();
    assert_eq!(files.drives.next_wake(), None, "waits for the answer");
    // Two more changes while that ask is unanswered become one more ask.
    ui::widget::Collaborative::handle_collab_event(&mut files, &changed);
    ui::widget::Collaborative::handle_collab_event(&mut files, &changed);
    files.poll_drives();
    ANSWER.store(true, Ordering::SeqCst);
    settle(&mut files, |files| !files.drives.shown.is_empty());
    assert_eq!(files.drives.shown[0].id, "sdb1:7");
    settle(&mut files, |_| LISTED.load(Ordering::SeqCst) == 2);
    settle(&mut files, |files| {
        !files.drives.listing && files.drives.next_wake().is_none()
    });
    for _ in 0..3 {
        files.poll_drives();
    }
    assert_eq!(
        LISTED.load(Ordering::SeqCst),
        2,
        "one ask for the first change, one for those during it"
    );
    assert_eq!(files.drives.next_wake(), None, "idle again");
}
