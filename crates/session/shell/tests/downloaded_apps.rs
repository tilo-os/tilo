//! A downloaded program that names itself only through its window.
//!
//! A loose binary in Downloads has no desktop entry, no folder with one and
//! no embedded image: all the desktop learns about it is the process behind
//! its window (the kernel's peer credentials) and what the window says, its
//! app id and the icon it sets through `xdg_toplevel_icon_v1`. That is
//! enough to give it an entry in the applications model, keyed by the app
//! id, so the same program run from a new file updates the entry instead of
//! adding a second one. A real process runs a copy of a system binary from
//! the test's Downloads folder; the Wayland client is the harness's, given
//! that process's identity as its peer credentials would.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "deadlines are seconds from now, and a test icon's few pixels cannot overflow"
)]

use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use shell::shell::add_app_sheet::Answer;
use shell::shell::app_list::AppSource;
use shell::shell::downloaded_apps;
use shell::shell::state::TileContent;
use wayland_client::delegate_noop;
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_shm, wl_shm_pool, wl_surface};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};
use wayland_protocols::xdg::toplevel_icon::v1::client::{
    xdg_toplevel_icon_manager_v1, xdg_toplevel_icon_v1,
};

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
}

crate::support::wayland::test_client!(Client);

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_shm::WlShm);
delegate_noop!(Client: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Client: ignore wl_buffer::WlBuffer);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Client: ignore xdg_toplevel_icon_v1::XdgToplevelIconV1);
delegate_noop!(Client: ignore xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1);

type Harness = crate::support::wayland::Harness<Client>;

const APP_ID: &str = "org.example.MyTool";
const WAIT: Duration = Duration::from_secs(10);

/// A running copy of a system binary at `path`, killed on drop.
struct Running {
    child: std::process::Child,
}

impl Running {
    fn start(path: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::copy("/usr/bin/sleep", path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let child = std::process::Command::new(path).arg("60").spawn().unwrap();
        let running = Self { child };
        // Until it has exec'd, the process is still a copy of this test.
        let deadline = Instant::now() + WAIT;
        while std::fs::read_link(format!("/proc/{}/exe", running.pid()))
            .ok()
            .as_deref()
            != Some(path)
        {
            assert!(
                Instant::now() < deadline,
                "{} never started",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        running
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn start_time(&self) -> u64 {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", self.pid())).unwrap();
        stat.rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap()
            .parse()
            .unwrap()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A compositor whose one client is `process`, as an unconfined host
/// program run from a terminal is.
fn harness_for(process: &Running) -> Harness {
    let pid = process.pid();
    let start = process.start_time();
    Harness::with_identity(move |_| {
        let mut client = shell::compositor::state::ClientState::default();
        client.pid = Some(pid);
        client.pid_start_time = Some(start);
        Arc::new(client)
    })
}

fn solid_buffer(h: &Harness, edge: i32) -> wl_buffer::WlBuffer {
    let qh = h.queue.handle();
    let stride = edge * 4;
    let len = usize::try_from(stride * edge).unwrap();
    // SAFETY: a null-terminated name and a valid flag word are the whole
    // contract of `memfd_create`.
    let raw = unsafe { libc::memfd_create(c"tilo-downloaded-app-icon".as_ptr(), 0) };
    assert!(raw >= 0);
    // SAFETY: `memfd_create` just handed us this descriptor.
    let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    file.set_len(u64::try_from(len).unwrap()).unwrap();
    {
        use std::os::unix::fs::FileExt;
        let pixels: Vec<u8> = std::iter::repeat_n([0xc0, 0x80, 0x20, 0xff], len / 4)
            .flatten()
            .collect();
        file.write_at(&pixels, 0).unwrap();
    }
    let shm = h.bind::<wl_shm::WlShm>("wl_shm", 1);
    let pool = shm.create_pool(file.as_fd(), i32::try_from(len).unwrap(), &qh, ());
    pool.create_buffer(0, edge, edge, stride, wl_shm::Format::Argb8888, &qh, ())
}

/// Open a window that sets its icon, then names itself `APP_ID`. Returns
/// its surface id and the objects that keep it open.
fn open_window(h: &mut Harness) -> (u64, impl Sized + use<>) {
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = h.bind("xdg_wm_base", 2);
    let manager: xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1 =
        h.bind("xdg_toplevel_icon_manager_v1", 1);
    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    let buffer = solid_buffer(h, 32);
    let icon = manager.create_icon(&qh, ());
    icon.add_buffer(&buffer, 1);
    manager.set_icon(&toplevel, Some(&icon));
    surface.commit();
    h.pump();
    toplevel.set_app_id(APP_ID.into());
    surface.commit();
    h.pump();
    let surface_id = h
        .state
        .workspace_tree
        .read()
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find_map(|tab| match &tab.tile {
            TileContent::Foreign { app_id, surface_id } if app_id == APP_ID => Some(*surface_id),
            _ => None,
        })
        .expect("the window was adopted into a tab");
    (surface_id, (surface, xdg, toplevel, icon, buffer))
}

fn entry(desktop_id: &str) -> Option<shell::shell::host_apps::HostApp> {
    desktop_entry::cache::refresh();
    shell::shell::host_apps::host_apps()
        .into_iter()
        .find(|app| app.desktop_id == desktop_id)
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !ready() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Closing the question means `file` is not asked about again, while it can
/// still be added from search.
fn declining_is_remembered_for_that_file(file: &Path, desktop_id: &str) {
    let again = Running::start(file);
    let mut h = harness_for(&again);
    let (surface_id, window) = open_window(&mut h);
    assert!(downloaded_apps::settle(WAIT));
    let ask = downloaded_apps::take_ask().expect("with no entry, the file is asked about");
    assert_eq!(ask.surface_id, surface_id);
    downloaded_apps::answer(&Answer::Declined {
        surface_id: ask.surface_id,
    });
    assert!(downloaded_apps::settle(WAIT));
    assert!(entry(desktop_id).is_none(), "declining adds nothing");
    drop(window);
    drop(h);
    drop(again);
    let once_more = Running::start(file);
    let mut h = harness_for(&once_more);
    let (surface_id, _window) = open_window(&mut h);
    assert!(downloaded_apps::settle(WAIT));
    assert!(
        downloaded_apps::take_ask().is_none(),
        "that file is not asked about again"
    );
    assert!(
        downloaded_apps::candidate(surface_id).is_some(),
        "it can still be added from search"
    );
}

#[test]
fn a_bare_binary_named_only_by_its_window_gets_one_entry_that_follows_it() {
    let home = tilo_test_env::TestHome::new().unwrap();
    desktop_entry::cache::invalidate();
    downloaded_apps::follow_moves();
    let first_file: PathBuf = home.home().join("Downloads/mytool");
    let first = Running::start(&first_file);
    let mut h = harness_for(&first);
    let (surface_id, window) = open_window(&mut h);
    assert!(downloaded_apps::settle(WAIT));

    // Phase `the_window_alone_names_the_program`.
    let candidate = downloaded_apps::candidate(surface_id)
        .expect("a program from Downloads with no entry is noticed");
    assert_eq!(candidate.program.path, first_file);
    assert_eq!(candidate.identity.key, APP_ID);
    assert_eq!(candidate.identity.name, "MyTool");
    let desktop_id = "tilo-downloaded-org.example.MyTool";
    assert!(
        entry(desktop_id).is_none(),
        "running it adds nothing to the person's apps by itself"
    );

    // Phase `the_first_run_asks_once_with_no_move_for_a_loose_binary`.
    let ask = downloaded_apps::take_ask().expect("the first run asks");
    assert_eq!(ask.surface_id, surface_id);
    assert_eq!(ask.name, "MyTool");
    assert_eq!(ask.file, first_file);
    assert!(!ask.appimage, "a loose binary is never offered a move");
    assert!(ask.icon.is_some(), "the question shows the window's icon");
    assert!(downloaded_apps::take_ask().is_none(), "asked once");

    // Phase `adding_it_puts_it_in_the_applications_model_with_its_window_icon`.
    downloaded_apps::answer(&Answer::Add {
        surface_id,
        move_to_applications: true,
    });
    assert!(downloaded_apps::settle(WAIT));
    let app = entry(desktop_id).expect("the entry is in the applications model");
    assert_eq!(app.name, "MyTool");
    assert!(app.window_identities.iter().any(|id| id == APP_ID));
    let icon = PathBuf::from(app.icon.clone().expect("the window's icon"));
    assert!(std::fs::read(&icon).unwrap().starts_with(b"\x89PNG"));
    assert_eq!(
        app.downloaded.as_ref().unwrap().program,
        first_file,
        "a loose binary stays where it is even when a move was asked for"
    );
    assert!(app.downloaded.as_ref().unwrap().present);
    drop(window);
    drop(h);
    drop(first);

    // Phase `the_same_app_from_a_new_file_updates_its_entry`.
    let second_file = home.home().join("Downloads/mytool-2/mytool");
    let second = Running::start(&second_file);
    let mut h = harness_for(&second);
    let (surface_id, window) = open_window(&mut h);
    assert!(downloaded_apps::settle(WAIT));
    assert!(
        downloaded_apps::candidate(surface_id).is_none() && downloaded_apps::take_ask().is_none(),
        "an app with an entry is not asked about again"
    );
    let named: Vec<_> = shell::shell::host_apps::host_apps()
        .into_iter()
        .filter(|app| app.name == "MyTool")
        .collect();
    assert_eq!(named.len(), 1, "one app, one entry: {named:?}");
    assert_eq!(named[0].downloaded.as_ref().unwrap().program, second_file);
    assert_eq!(
        named[0].downloaded.as_ref().unwrap().locations,
        std::slice::from_ref(&first_file),
        "the first copy stays known"
    );
    drop(window);
    drop(h);
    drop(second);

    // Phase `a_gone_copy_falls_back_to_the_other_copy`.
    std::fs::remove_file(&second_file).unwrap();
    wait_for("the entry starts the copy still there", || {
        entry(desktop_id)
            .and_then(|app| app.downloaded)
            .is_some_and(|downloaded| downloaded.present && downloaded.program == first_file)
    });

    // Phase `a_deleted_file_shows_as_missing_without_trying_to_start_it`.
    let kept = home.home().join("kept-mytool");
    std::fs::rename(&first_file, &kept).unwrap();
    wait_for("the entry says its file is gone", || {
        shell::shell::host_apps::host_apps()
            .into_iter()
            .find(|app| app.desktop_id == desktop_id)
            .and_then(|app| app.downloaded)
            .is_some_and(|downloaded| !downloaded.present)
    });

    // Phase `locate_points_it_at_the_file_where_it_is_now`.
    std::fs::rename(&kept, &first_file).unwrap();
    downloaded_apps::locate(desktop_id, &first_file);
    assert!(downloaded_apps::settle(WAIT));
    let app = entry(desktop_id).unwrap();
    let downloaded = app.downloaded.unwrap();
    assert!(downloaded.present);
    assert_eq!(downloaded.program, first_file);

    taken_off_the_apps_and_declined(desktop_id, &first_file, &icon);
    desktop_entry::cache::invalidate();
}

/// Taking the entry off the apps through the one action Files, search and
/// agents share leaves the file, and the file is then asked about again.
fn taken_off_the_apps_and_declined(desktop_id: &str, first_file: &Path, icon: &Path) {
    let mut listed = None;
    wait_for("the app list holds the entry", || {
        listed = shell::shell::app_list::find(desktop_id)
            .filter(|item| matches!(item.source, AppSource::Downloaded { .. }));
        listed.is_some()
    });
    shell::shell::app_list::remove_from_apps(listed.unwrap()).unwrap();
    wait_for("the entry is gone", || entry(desktop_id).is_none());
    assert!(first_file.exists(), "the program's file stays where it is");
    assert!(!icon.exists());

    declining_is_remembered_for_that_file(first_file, desktop_id);
}

#[test]
fn an_entry_follows_its_file_when_it_moves_inside_the_persons_files() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    // The person's files on Tilo's filesystem: every file carries a record
    // id, and the root answers where a record is now.
    let files = home.home().join("Files");
    let before = files.join("Downloads/Foo.AppImage");
    std::fs::create_dir_all(before.parent().unwrap()).unwrap();
    std::fs::copy("/usr/bin/sleep", &before).unwrap();
    let record = [0x5a; 16];
    xattr::set(&before, fs_protocol::RECORD_ID_XATTR, &record).unwrap();
    home.env().set("TILO_WORKSPACE_ROOTS", &files);
    desktop_entry::cache::invalidate();
    let store = desktop_entry::downloaded::Store::for_session().unwrap();
    let desktop_id = store
        .add(
            &desktop_entry::downloaded::Identity {
                key: "org.example.Foo".into(),
                name: "Foo".into(),
                icon: None,
                comment: None,
                categories: Vec::new(),
                keywords: Vec::new(),
                mime_types: Vec::new(),
                startup_wm_class: None,
                field_code: None,
            },
            &desktop_entry::downloaded::Program {
                path: before.clone(),
                kind: desktop_entry::downloaded::ProgramKind::Program,
            },
        )
        .unwrap();
    let written = entry(&desktop_id).unwrap().downloaded.unwrap();
    assert_eq!(written.record(), Some(record));

    // Phase `a_moved_and_renamed_file_is_found_by_its_record`.
    let after = files.join("Apps/Foo-renamed.AppImage");
    std::fs::create_dir_all(after.parent().unwrap()).unwrap();
    std::fs::rename(&before, &after).unwrap();
    let uuid = uuid::Uuid::from_bytes(record);
    let exact = serde_json::json!({
        "schema_version": 2,
        "revision": "a".repeat(64),
        "record": {
            "record_id": uuid,
            "path": "/Apps/Foo-renamed.AppImage",
            "is_dir": false,
        },
    });
    xattr::set(
        &files,
        format!(
            "{}{uuid}",
            fs_protocol::projection::RECORD_INDEX_BY_ID_XATTR_PREFIX
        ),
        &serde_json::to_vec(&exact).unwrap(),
    )
    .unwrap();
    desktop_entry::cache::refresh();
    downloaded_apps::follow_moves();
    assert!(downloaded_apps::settle(WAIT));
    wait_for("the entry points at the file where it is now", || {
        entry(&desktop_id)
            .and_then(|app| app.downloaded)
            .is_some_and(|downloaded| downloaded.present && downloaded.program == after)
    });
    desktop_entry::cache::invalidate();
}
