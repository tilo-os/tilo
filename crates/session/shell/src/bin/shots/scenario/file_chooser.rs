//! The file chooser's screenshot fixture: the writer's Document window asks
//! for a file, and the chooser opens as a sheet on it, over a throwaway home
//! filled like a person's.

use shell::shell::chrome_state::ChromeState;

use super::super::driver::ShellDriver;

/// `Action::SeedFilePicker`: the chooser in `mode` (`open`,
/// `open_multiple`, `save` or `folder`), asked for by the writer.
#[allow(
    clippy::too_many_lines,
    reason = "one screenshot fixture staged top to bottom"
)]
pub(super) fn seed(driver: &mut ShellDriver, mode: &str) {
    use shell::portal::{
        FilePickerOptions, FilePickerQueue, FilePickerSpawner, PickerFilter, PortalCaller,
        PromptQueueHandle, PromptResolver,
    };
    use shell::shell::state::WorkspaceId;
    // Only one dialog is on screen at a time — a second request
    // while one is up is answered rather than stacked. So a
    // scenario capturing several shapes has to dismiss the
    // previous one first, the way the user would.
    if let Some(picker) = driver.host_mut().file_picker_mut() {
        picker.click_cancel();
    }
    driver.host_mut().pump_file_picker();
    let home = seed_file_picker_fixture(driver);
    // What the writer was given before: the chooser's Recent.
    let recent: Vec<std::path::PathBuf> = home
        .iter()
        .flat_map(|home| {
            ["Quarterly report.odt", "Meeting notes.md", "Projects"].map(|name| home.join(name))
        })
        .collect();
    // The writer's Document tab is the window that asks: in front,
    // focused, and the window the chooser is a sheet on.
    let mut document = None;
    driver.with_chrome(|chrome| {
        let mut tree = chrome.tree_mut();
        for strip in tree
            .desktop
            .workspaces
            .iter_mut()
            .flat_map(|workspace| &mut workspace.strips)
        {
            for group in &mut strip.window_groups {
                if let Some(tab) = group.tabs.iter_mut().find(|tab| tab.title == "Document") {
                    tab.tile = shell::shell::state::TileContent::Foreign {
                        app_id: "so.tilo.word-processor".into(),
                        surface_id: tab.id,
                    };
                    document = Some((strip.id, group.id, tab.id));
                }
            }
        }
    });
    driver.relayout();
    if let Some((strip, group, tab)) = document {
        driver.host_mut().focus_window_on_strip(strip, group, tab);
    }
    driver.relayout();
    let queue = FilePickerQueue::new();
    driver.host_mut().set_portal_queues(
        PromptQueueHandle::new(PromptResolver::Channel),
        queue.clone(),
    );

    let save = mode == "save";
    let caller = PortalCaller::new(WorkspaceId(1), "so.tilo.word-processor");
    let opts = FilePickerOptions::new(
        caller,
        // The writer gives a purpose for a folder; otherwise the
        // sheet says what it wants in its name.
        match mode {
            "folder" => "Choose where to keep exports",
            _ => "",
        },
        save,
    )
    .multiple(mode == "open_multiple")
    .directory(mode == "folder")
    .filters(vec![
        PickerFilter::from_pairs(
            "Documents",
            &[(0, "*.odt".into()), (0, "*.md".into()), (0, "*.txt".into())],
        ),
        PickerFilter::from_pairs("Images", &[(1, "image/png".into())]),
    ])
    .current_name(save.then(|| "Untitled.md".to_string()));
    // The scenario waits on the answer the way the asking app does:
    // a request whose app has gone is closed at the next frame.
    thread_local! {
        static WAITING: std::cell::RefCell<Vec<tokio::sync::oneshot::Receiver<Option<Vec<std::path::PathBuf>>>>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    WAITING.with(|waiting| waiting.borrow_mut().push(rx));
    queue.spawn(shell::portal::FilePickerRequest {
        caller: opts.caller,
        parent_window: None,
        title: opts.title,
        save: opts.save,
        directory: opts.directory,
        writable_directory: opts.writable_directory,
        multiple: opts.multiple,
        accept_label: opts.accept_label,
        current_name: opts.current_name,
        filters: opts.filters,
        scope: None,
        recent,
        reply: tx,
    });
    driver.host_mut().pump_file_picker();
    // The chooser lists its folder and Recent on a worker: the still is of
    // what a person sees a moment after it opens, with the rows in.
    let deadline = std::time::Instant::now().checked_add(std::time::Duration::from_secs(10));
    while driver
        .host()
        .file_picker()
        .is_some_and(ui::widget::FilePicker::reading)
    {
        if deadline.is_none_or(|deadline| std::time::Instant::now() >= deadline) {
            eprintln!("shots: the file chooser had not listed its folder after 10 s");
            break;
        }
        std::thread::yield_now();
        driver.host_mut().pump_file_picker();
    }
}

/// Fill the picker's fixture folder like a person's home — the usual
/// folders and a few documents with sizes and dates — so the capture shows
/// the sidebar and every column. The fixture is a throwaway directory the
/// capture owns; nothing here exists outside `--shots`.
fn seed_file_picker_fixture(driver: &mut ShellDriver) -> Option<std::path::PathBuf> {
    let mut home = None;
    driver.with_chrome(|st: &mut ChromeState| {
        home = st
            .hosted_inputs()
            .picker_scope
            .as_ref()
            .map(|scope| scope.initial_dir.clone());
    });
    let home = home?;
    for folder in ["Documents", "Downloads", "Pictures", "Music", "Projects"] {
        let _ = std::fs::create_dir_all(home.join(folder));
    }
    for (name, kilobytes) in [
        ("Quarterly report.odt", 84_u64),
        ("Meeting notes.md", 3),
        ("Budget 2027.ods", 212),
        ("Holiday photo.png", 2_480),
        ("Invoice March.pdf", 156),
        ("Reading list.txt", 1),
    ] {
        let path = home.join(name);
        if !path.exists() {
            let _ = std::fs::write(
                &path,
                vec![b'x'; usize::try_from(kilobytes.saturating_mul(1000)).unwrap_or(0)],
            );
        }
    }
    Some(home)
}
