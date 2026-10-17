//! End-to-end: an app asks the portal for a file, a human clicks through
//! the dialog, and the app reads the bytes. The exercise covers the D-Bus
//! (`FileChooserBackend` over a socket-pair), the queue
//! (`FilePickerQueue`), the pump (`ShellOverlayHost::pump_file_picker`),
//! the widget (clicks hit-test the rect `ui::widget::FilePicker` computed),
//! and the file reads.
//!
//! The D-Bus call parks on a oneshot, so the test spawns it and turns the
//! pump by hand, as the compositor's tick would.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::state::WorkspaceAuthority as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use shell::portal::{
    self, CallerMap, FilePickerQueue, FilePickerSpawnerHandle, GrantStoreHandle,
    MockScreenCastProvisioner, OBJECT_PATH, PortalCaller, PortalContext, PromptQueueHandle,
    PromptResolver, RemoteInputInjectorHandle, ScreenCastStreamProvisionerHandle,
};
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{Capability, Workspace, WorkspaceId, WorkspaceTree};
use tokio::net::UnixStream;
use zbus::connection::Builder;
use zbus::zvariant::{ObjectPath, OwnedValue};
use zbus::{Connection, Proxy};

const TEST_GUID: &str = "0123456789abcdef0123456789abcdef";
const WS: u64 = 1;
const APP: &str = "org.example.editor";

fn cursor_at(p: vello::kurbo::Point) -> winit::event::WindowEvent {
    ui::synthetic_input::cursor_moved(p.x, p.y)
}

use ui::synthetic_input::mouse_press;

fn key_press(ch: char) -> winit::event::WindowEvent {
    use winit::keyboard::{Key, KeyLocation, NativeKeyCode, PhysicalKey, SmolStr};
    let mut ev = ui::synthetic_input::key_event(
        winit::keyboard::Key::Unidentified(winit::keyboard::NativeKey::Unidentified),
        None,
        winit::event::ElementState::Pressed,
    );
    let s = SmolStr::new(ch.to_string());
    ev.logical_key = Key::Character(s.clone());
    ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = winit::event::ElementState::Pressed;
    ev.repeat = false;
    ev.text = Some(s);
    ui::synthetic_input::keyboard_input(ev)
}

fn key_named(named: winit::keyboard::NamedKey) -> winit::event::WindowEvent {
    use winit::keyboard::Key;
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        Key::Named(named),
        None,
        winit::event::ElementState::Pressed,
    ))
}

/// Goes through the host's own `handle_event` — the modal intercept the
/// compositor installs, not the widget in isolation.
fn dispatch(host: &mut ShellOverlayHost, event: &winit::event::WindowEvent) {
    ui::testing::dispatch(host, event);
}

fn click_at(host: &mut ShellOverlayHost, p: vello::kurbo::Point) {
    dispatch(host, &cursor_at(p));
    dispatch(host, &mouse_press());
}

fn host_with_portal(
    prompts: PromptQueueHandle,
    pickers: FilePickerQueue,
    picker_root: &std::path::Path,
) -> (ShellOverlayHost, GrantStoreHandle, WorkspaceId) {
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::from(WS);
    tree.desktop.workspaces.push(Workspace::new(id, "demo"));
    tree.set_active(id);
    let handle = tree.shared();
    {
        let mut t = handle.write();
        let owner = shell::shell::state::local_user_id();
        t.adopt_ownerless_workspaces(&owner);
    }
    let mut inputs =
        shell::shell::hosted_inputs::HostedTileInputs::fixture().expect("Files fixture");
    inputs.picker_scope = Some(portal::PickerScope {
        initial_dir: picker_root.to_path_buf(),
        roots: vec![picker_root.to_path_buf()],
        home: None,
    });
    let (mut host, _chrome) = shell::shell::build_tilo_overlay_host_with_inputs(
        handle.clone(),
        false,
        ui::record::RecordStore::in_memory(),
        inputs,
    );
    layout(&mut host, VIEW_W, VIEW_H);
    host.set_portal_queues(prompts, pickers);
    (host, GrantStoreHandle::new(handle), id)
}

const VIEW_W: f32 = 1280.0;
const VIEW_H: f32 = 800.0;

/// The same layout sequence `ui::App` runs each frame. The dialog's
/// position derives from the host's viewport rect, so skipping this would
/// click into a zero-sized world.
fn layout(host: &mut ShellOverlayHost, w: f32, h: f32) {
    use ui::Widget;
    let theme = ui::theme::Theme::default();
    let mut taffy = taffy::TaffyTree::<()>::new();
    let mut node_map = std::collections::HashMap::new();
    let node = host.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(w),
                height: taffy::AvailableSpace::Definite(h),
            },
        )
        .expect("layout");
    host.apply_layout(&taffy, node, vello::kurbo::Point::ZERO);
}

/// The host explicitly scopes the dialog to this isolated user directory.
/// The requesting app receives neither the root nor a grant until the user picks.
fn user_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("isolated picker root")
}

async fn portal_proxy(ctx: PortalContext) -> (Connection, Connection, Proxy<'static>) {
    let (server_sock, client_sock) = UnixStream::pair().expect("socket pair");
    let server_builder = Builder::unix_stream(server_sock)
        .server(TEST_GUID)
        .expect("guid")
        .p2p()
        .serve_at(OBJECT_PATH, portal::FileChooserBackend::new(ctx))
        .expect("serve_at");
    let client_builder = Builder::unix_stream(client_sock).p2p();
    // Both sides of a p2p handshake must be driven at once — awaiting the
    // server's `build()` first parks it waiting for a client this function
    // hasn't created yet, hanging the test.
    let (server, client) = futures_util::try_join!(server_builder.build(), client_builder.build())
        .expect("p2p connection pair");
    let proxy = Proxy::new(
        &client,
        "org.freedesktop.impl.portal.desktop.tilo",
        OBJECT_PATH,
        "org.freedesktop.impl.portal.FileChooser",
    )
    .await
    .expect("proxy");
    (server, client, proxy)
}

fn context(
    grants: GrantStoreHandle,
    prompts: PromptQueueHandle,
    pickers: FilePickerQueue,
    ws: WorkspaceId,
) -> PortalContext {
    let caller_map = CallerMap::new();
    // The p2p transport surfaces no unique name, so pin the caller the way
    // the bridge accept path would in production.
    caller_map.force_caller_for_tests(PortalCaller::new(ws, APP));
    let resolver_map = caller_map.clone();
    PortalContext {
        grants,
        prompts,
        caller_map,
        file_picker: FilePickerSpawnerHandle::new(Arc::new(pickers)),
        screencast_provisioner: ScreenCastStreamProvisionerHandle::new(Arc::new(
            MockScreenCastProvisioner::new_with_node(1, "t"),
        )),
        remote_input_injector: RemoteInputInjectorHandle::null(),
        input_capture: shell::portal::InputCaptureCompositorHandle::unavailable(),
        print_spooler: shell::portal::PrintSpoolerHandle::null(),
        documents: portal::documents_for_session(),
        notifications: shell::notifications::NotificationBridge::new(),
        desktop_services: shell::portal::DesktopPortalServices::default(),
        authenticator: Some(shell::portal::PortalAuthenticator::fixed_for_tests(
            ":1.0".try_into().expect("fixed p2p owner"),
            Arc::new(move |app_id: &str, _: Option<&str>| {
                resolver_map
                    .lookup("")
                    .filter(|caller| caller.app_id == app_id)
            }),
        )),
    }
}

#[allow(
    clippy::future_not_send,
    reason = "ShellOverlayHost and the generic FnMut closure aren't Send; every call site \
              awaits this in place on the test's own task, never spawning it"
)]
async fn pump_until(
    host: &mut ShellOverlayHost,
    what: &str,
    mut f: impl FnMut(&mut ShellOverlayHost) -> bool,
) {
    let deadline = std::time::Instant::now()
        .checked_add(crate::support::HANG_BOUND)
        .expect("the hang bound fits in an Instant");
    while std::time::Instant::now() < deadline {
        host.pump_file_picker();
        if f(host) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Await a spawned portal call while still turning the pump — the pump is
/// what answers the call, so awaiting one without running the other
/// deadlocks.
#[allow(
    clippy::future_not_send,
    reason = "ShellOverlayHost isn't Send; every call site awaits this in place on the \
              test's own task, never spawning it"
)]
async fn await_response(
    host: &mut ShellOverlayHost,
    what: &str,
    handle: tokio::task::JoinHandle<(u32, std::collections::HashMap<String, OwnedValue>)>,
) -> (u32, std::collections::HashMap<String, OwnedValue>) {
    let deadline = std::time::Instant::now()
        .checked_add(crate::support::HANG_BOUND)
        .expect("the hang bound fits in an Instant");
    while std::time::Instant::now() < deadline {
        host.pump_file_picker();
        if handle.is_finished() {
            return handle.await.expect("join");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the portal call for {what} never answered — the caller is hung");
}

fn picker_rect() -> vello::kurbo::Rect {
    shell::shell::overlay_host::file_picker_rect(vello::kurbo::Rect::new(0.0, 0.0, 1280.0, 800.0))
}

fn uris_of(results: &std::collections::HashMap<String, OwnedValue>) -> Vec<String> {
    let v = results.get("uris").expect("uris key");
    Vec::<String>::try_from(v.try_clone().expect("clone")).expect("Vec<String>")
}

fn path_of_uri(uri: &str) -> PathBuf {
    PathBuf::from(uri.strip_prefix("file://").expect("file:// uri"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
async fn file_chooser_opens_and_saves_what_the_user_picked() {
    // Phase `an_app_opens_a_file_the_user_picked_and_reads_it`.
    {
        const BYTES: &str = "the bytes the app must be able to read\n";

        let dir = user_dir();
        let target = dir.path().join("report.txt");
        std::fs::write(&target, BYTES).expect("seed file");
        std::fs::write(dir.path().join("other.txt"), "not this one").expect("seed decoy");

        let prompts = PromptQueueHandle::new(PromptResolver::Channel);
        let pickers = FilePickerQueue::new();
        let (mut host, grants, ws) = host_with_portal(prompts.clone(), pickers.clone(), dir.path());
        let (_srv, _cli, proxy) = portal_proxy(context(grants.clone(), prompts, pickers, ws)).await;

        // The app's request. Parks on the dialog, so it runs concurrently.
        let call = tokio::spawn(async move {
            let handle = ObjectPath::try_from("/org/freedesktop/portal/desktop/request/1").unwrap();
            proxy
                .call::<_, _, (u32, std::collections::HashMap<String, OwnedValue>)>(
                    "OpenFile",
                    &(
                        handle,
                        APP.to_string(),
                        String::new(),
                        "Open a document".to_string(),
                        std::collections::HashMap::<String, OwnedValue>::new(),
                    ),
                )
                .await
                .expect("OpenFile")
        });

        pump_until(&mut host, "the dialog to appear", |h| {
            h.file_picker().is_some()
        })
        .await;

        // Point it at the seeded directory, the way a user browsing there would.
        let rect = picker_rect();
        {
            let picker = host.file_picker_mut().expect("dialog");
            picker.set_rect(rect);
            picker.navigate_to(dir.path());
            picker.set_rect(rect);
        }
        let row = {
            let picker = host.file_picker().expect("dialog");
            let idx = picker
                .entries()
                .iter()
                .position(|e| e.name == "report.txt")
                .expect("report.txt must be listed");
            picker.entry_rect(idx).expect("row must be laid out")
        };
        // Two clicks on the row: select, then activate — through the widget's own
        // hit-test path, so a layout regression breaks this test.
        click_at(&mut host, row.center());
        click_at(&mut host, row.center());

        // Confirm. Again by rect, not by calling `click_open`.
        let confirm = host.file_picker().expect("dialog").open_rect();
        click_at(&mut host, confirm.center());

        pump_until(&mut host, "the dialog to close", |h| {
            h.file_picker().is_none()
        })
        .await;

        let (code, results) = await_response(&mut host, "the open", call).await;

        assert_eq!(code, 0, "a completed pick is response=0");
        let uris = uris_of(&results);
        assert_eq!(uris.len(), 1, "OpenFile returns exactly one uri: {uris:?}");

        let handed = path_of_uri(&uris[0]);
        let read_back = std::fs::read_to_string(&handed)
            .unwrap_or_else(|e| panic!("the app cannot read {}: {e}", handed.display()));
        assert_eq!(
            read_back, BYTES,
            "the app must get the file the user picked"
        );

        // The implementation backend returns the host selection only to the
        // trusted xdg-desktop-portal frontend, which exports it through the
        // document portal. It must not turn a personal pick into replicated room
        // filesystem authority.
        let tree = grants.tree_handle();
        let paths: Vec<PathBuf> = {
            let guard = tree.read();
            guard
                .desktop
                .workspaces
                .iter()
                .find(|w| w.id == ws)
                .expect("workspace")
                .app_capabilities(APP)
                .into_iter()
                .filter_map(|c| match Capability::from_capability(c) {
                    Some(Capability::Filesystem { path, .. }) => Some(path),
                    _ => None,
                })
                .collect()
        };
        assert!(
            paths.is_empty(),
            "a file pick must remain private; got {paths:?}"
        );
    }

    // Phase `an_app_saves_into_a_directory_it_did_not_previously_have`.
    {
        const BYTES: &str = "saved from the app\n";

        let dir = user_dir();
        let fresh = dir.path().join("untouched");
        std::fs::create_dir(&fresh).expect("mkdir");

        let prompts = PromptQueueHandle::new(PromptResolver::Channel);
        let pickers = FilePickerQueue::new();
        let (mut host, grants, ws) = host_with_portal(prompts.clone(), pickers.clone(), dir.path());

        // Prove the starting condition rather than assuming it: nothing in
        // this workspace grants the app anything under `fresh` yet.
        let caller = PortalCaller::new(ws, APP);
        assert!(
            !grants.filesystem_path_granted(&caller, &fresh),
            "precondition: the app must start with no access to the save directory"
        );

        let (_srv, _cli, proxy) = portal_proxy(context(grants.clone(), prompts, pickers, ws)).await;

        let call = tokio::spawn(async move {
            let handle = ObjectPath::try_from("/org/freedesktop/portal/desktop/request/2").unwrap();
            proxy
                .call::<_, _, (u32, std::collections::HashMap<String, OwnedValue>)>(
                    "SaveFile",
                    &(
                        handle,
                        APP.to_string(),
                        String::new(),
                        "Save as".to_string(),
                        std::collections::HashMap::<String, OwnedValue>::new(),
                    ),
                )
                .await
                .expect("SaveFile")
        });

        pump_until(&mut host, "the save dialog to appear", |h| {
            h.file_picker().is_some()
        })
        .await;

        let rect = picker_rect();
        {
            let picker = host.file_picker_mut().expect("dialog");
            picker.set_rect(rect);
            picker.navigate_to(&fresh);
            picker.set_rect(rect);
        }
        // Type the filename, character by character, through the host's
        // modal keyboard route.
        for ch in "notes.md".chars() {
            dispatch(&mut host, &key_press(ch));
        }
        assert_eq!(
            host.file_picker().expect("dialog").filename(),
            "notes.md",
            "the typed filename must reach the dialog through the modal route"
        );

        // Save starts on the filename control; Enter accepts only after the name
        // made the primary action valid. This goes through the host modal route,
        // not a widget-only helper or a pointer click.
        dispatch(&mut host, &key_named(winit::keyboard::NamedKey::Enter));

        pump_until(&mut host, "the save dialog to close", |h| {
            h.file_picker().is_none()
        })
        .await;

        let (code, results) = await_response(&mut host, "the save", call).await;

        assert_eq!(code, 0);
        assert!(
            results.contains_key("writable"),
            "a Save response must be marked writable: {:?}",
            results.keys().collect::<Vec<_>>()
        );
        let handed = path_of_uri(&uris_of(&results)[0]);
        assert_eq!(handed, fresh.join("notes.md"));

        std::fs::write(&handed, BYTES)
            .unwrap_or_else(|e| panic!("the app cannot write {}: {e}", handed.display()));
        assert_eq!(
            std::fs::read_to_string(&handed).expect("read back"),
            BYTES,
            "the bytes must land on disk at the path the user chose"
        );

        // Writable access is conferred by the frontend's document export, never
        // by adding a collaborative workspace capability.
        let tree = grants.tree_handle();
        let replicated = {
            let guard = tree.read();
            guard
                .desktop
                .workspaces
                .iter()
                .find(|w| w.id == ws)
                .expect("workspace")
                .app_capabilities(APP)
                .into_iter()
                .any(|c| {
                    matches!(
                        Capability::from_capability(c),
                        Some(Capability::Filesystem { .. })
                    )
                })
        };
        assert!(
            !replicated,
            "a private Save choice must not add collaborative filesystem authority"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
async fn file_chooser_answers_cancelled_and_concurrent_requests_instead_of_hanging() {
    // Phase `cancelling_the_dialog_answers_the_app_instead_of_hanging_it`.
    {
        let dir = user_dir();
        let prompts = PromptQueueHandle::new(PromptResolver::Channel);
        let pickers = FilePickerQueue::new();
        let (mut host, grants, ws) = host_with_portal(prompts.clone(), pickers.clone(), dir.path());
        let (_srv, _cli, proxy) = portal_proxy(context(grants, prompts, pickers, ws)).await;

        let call = tokio::spawn(async move {
            let handle = ObjectPath::try_from("/org/freedesktop/portal/desktop/request/3").unwrap();
            proxy
                .call::<_, _, (u32, std::collections::HashMap<String, OwnedValue>)>(
                    "OpenFile",
                    &(
                        handle,
                        APP.to_string(),
                        String::new(),
                        "Open".to_string(),
                        std::collections::HashMap::<String, OwnedValue>::new(),
                    ),
                )
                .await
                .expect("OpenFile")
        });

        pump_until(&mut host, "the dialog to appear", |h| {
            h.file_picker().is_some()
        })
        .await;

        // Escape is routed to the modal, not through to the requesting app, and
        // resolves the parked portal call exactly like the visible Cancel button.
        dispatch(&mut host, &key_named(winit::keyboard::NamedKey::Escape));

        pump_until(&mut host, "the dialog to close", |h| {
            h.file_picker().is_none()
        })
        .await;

        let (code, results) = await_response(&mut host, "the cancel", call).await;
        assert_eq!(code, 1, "cancel is response=1");
        assert!(results.is_empty());
    }

    // Phase `a_second_request_while_a_dialog_is_up_is_answered_not_stacked`.
    // Two modals for two apps, with no way to tell which is which, is worse
    // than a cancel the app can retry.
    {
        let dir = user_dir();
        let prompts = PromptQueueHandle::new(PromptResolver::Channel);
        let pickers = FilePickerQueue::new();
        let (mut host, grants, ws) = host_with_portal(prompts.clone(), pickers.clone(), dir.path());
        let ctx = context(grants, prompts, pickers.clone(), ws);
        let (_srv, _cli, proxy) = portal_proxy(ctx).await;
        let proxy = Arc::new(proxy);

        let first = {
            let proxy = Arc::clone(&proxy);
            tokio::spawn(async move {
                let handle =
                    ObjectPath::try_from("/org/freedesktop/portal/desktop/request/4").unwrap();
                proxy
                    .call::<_, _, (u32, std::collections::HashMap<String, OwnedValue>)>(
                        "OpenFile",
                        &(
                            handle,
                            APP.to_string(),
                            String::new(),
                            "First".to_string(),
                            std::collections::HashMap::<String, OwnedValue>::new(),
                        ),
                    )
                    .await
                    .expect("first OpenFile")
            })
        };

        pump_until(&mut host, "the first dialog", |h| h.file_picker().is_some()).await;

        let second = {
            let proxy = Arc::clone(&proxy);
            tokio::spawn(async move {
                let handle =
                    ObjectPath::try_from("/org/freedesktop/portal/desktop/request/5").unwrap();
                proxy
                    .call::<_, _, (u32, std::collections::HashMap<String, OwnedValue>)>(
                        "OpenFile",
                        &(
                            handle,
                            APP.to_string(),
                            String::new(),
                            "Second".to_string(),
                            std::collections::HashMap::<String, OwnedValue>::new(),
                        ),
                    )
                    .await
                    .expect("second OpenFile")
            })
        };

        // The second request is refused while the first dialog holds the
        // screen; the first is still up.
        let (code2, _) = await_response(&mut host, "the second request", second).await;
        assert_eq!(code2, 1, "a request we will not show is cancelled");
        assert!(
            host.file_picker().is_some(),
            "the first dialog must survive the second request"
        );

        // And the first still resolves normally.
        let rect = picker_rect();
        let cancel = {
            let picker = host.file_picker_mut().expect("dialog");
            picker.set_rect(rect);
            picker.cancel_rect()
        };
        click_at(&mut host, cancel.center());
        pump_until(&mut host, "the first dialog to close", |h| {
            h.file_picker().is_none()
        })
        .await;
        let (code1, _) = await_response(&mut host, "the first request", first).await;
        assert_eq!(code1, 1);
    }
}

fn directory_options(write: bool) -> std::collections::HashMap<String, OwnedValue> {
    let mut options = std::collections::HashMap::from([
        ("directory".to_owned(), OwnedValue::from(true)),
        (
            "accept_label".to_owned(),
            OwnedValue::try_from(zbus::zvariant::Value::from("Open")).unwrap(),
        ),
    ]);
    if write {
        let choices = vec![(
            tilo_dialogs::portal::WRITABLE_DIRECTORY_CHOICE.to_owned(),
            "Open".to_owned(),
            Vec::<(String, String)>::new(),
            "true".to_owned(),
        )];
        options.insert(
            "choices".into(),
            OwnedValue::try_from(zbus::zvariant::Value::new(choices)).unwrap(),
        );
    }
    options
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writable_directory_backend_requires_visible_consent_and_keeps_room_private() {
    let dir = user_dir();
    let selected = dir.path().join("selected");
    let sibling = dir.path().join("sibling");
    std::fs::create_dir(&selected).unwrap();
    std::fs::create_dir(&sibling).unwrap();
    let prompts = PromptQueueHandle::new(PromptResolver::Channel);
    let pickers = FilePickerQueue::new();
    let (_host, grants, ws) = host_with_portal(prompts.clone(), pickers.clone(), dir.path());
    let (_srv, _cli, proxy) =
        portal_proxy(context(grants.clone(), prompts, pickers.clone(), ws)).await;
    for (write, cancelled) in [(true, false), (false, false), (true, true)] {
        let proxy = proxy.clone();
        let call = tokio::spawn(async move {
            let options = directory_options(write);
            proxy
                .call::<_, _, (u32, std::collections::HashMap<String, OwnedValue>)>(
                    "OpenFile",
                    &(
                        ObjectPath::try_from("/org/freedesktop/portal/desktop/request/destination")
                            .unwrap(),
                        APP.to_owned(),
                        String::new(),
                        "Open folder".to_owned(),
                        options,
                    ),
                )
                .await
                .unwrap()
        });
        let request = tokio::time::timeout(crate::support::HANG_BOUND, async {
            loop {
                if let Some(request) = pickers.drain().pop() {
                    break request;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("backend must enqueue a real picker");
        assert_eq!(request.writable_directory, write);
        assert!(request.directory);
        assert!(!request.save);
        if write {
            assert_eq!(
                request.display_title(),
                shell::shell::i18n::t("portal-file-grant-write-folder")
            );
            assert_eq!(
                request.confirm_label(),
                shell::shell::i18n::t("portal-file-allow-write-folder")
            );
        }
        assert!(
            !call.is_finished(),
            "caller assertion cannot acknowledge consent"
        );
        request
            .reply
            .send((!cancelled).then(|| vec![selected.clone()]))
            .unwrap();
        let (code, results) = call.await.unwrap();
        if cancelled {
            assert_eq!(code, 1);
            assert!(results.is_empty());
        } else {
            assert_eq!(code, 0);
            assert_eq!(
                uris_of(&results),
                vec![url::Url::from_file_path(&selected).unwrap().to_string()]
            );
            assert_eq!(
                bool::try_from(results["writable"].try_clone().unwrap()).unwrap(),
                write
            );
            assert_eq!(results.contains_key("choices"), write);
        }
        // This is the store connected to the production backend and overlay,
        // not an unrelated empty store constructed solely for an assertion.
        let caller = PortalCaller::new(ws, APP);
        assert!(grants.granted_paths(&caller).is_empty());
        for path in [&selected, &sibling, dir.path()] {
            assert!(!grants.filesystem_path_allows(
                &caller,
                path,
                collab_types::caps::Access::Write
            ));
        }
    }
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use shell::shell::state::WorkspaceExt as _;
