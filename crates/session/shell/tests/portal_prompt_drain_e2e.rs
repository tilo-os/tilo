//! End-to-end for the prompt queue: a portal request that needs the user's
//! answer gets one, instead of waiting forever.
//!
//! `PromptQueueHandle::drain()` must actually be called in production —
//! nothing else empties the queue, and an unanswered `PendingGrant` hangs
//! the D-Bus caller indefinitely rather than closing the channel, so the
//! "channel closed = deny" defence never fires.
//!
//! `FileChooser` no longer uses the prompt queue (its dialog is the
//! consent), so `Access` is the honest subject — the interface
//! `xdg-desktop-portal` routes `Camera.AccessCamera` through. The chain
//! under test, all production code:
//!
//! ```text
//!   D-Bus AccessDialog
//!     → flow::resolve            enqueues + awaits
//!     → PromptQueueHandle        the queue that was never drained
//!     → drain_portal_prompts     per-frame, on the compositor
//!     → ChromeState::pending_grants
//!     → pump_permission_prompt   latches the widget
//!     → PermissionPrompt         the user clicks
//!     → PendingPrompt::answer    releases the parked D-Bus call
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::state::WorkspaceAuthority as _;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use shell::portal::{
    self, CallerMap, CapabilityKind, FilePickerQueue, FilePickerSpawnerHandle, GrantStoreHandle,
    MockScreenCastProvisioner, NullFilePickerSpawner, OBJECT_PATH, PortalCaller, PortalContext,
    PromptQueueHandle, PromptResolver, RemoteInputInjectorHandle,
    ScreenCastStreamProvisionerHandle,
};
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{Capability, Workspace, WorkspaceId, WorkspaceTree};
use tokio::net::UnixStream;
use ui::widget::PromptChoice;
use zbus::connection::Builder;
use zbus::zvariant::{ObjectPath, OwnedValue};
use zbus::{Connection, Proxy};

const TEST_GUID: &str = "0123456789abcdef0123456789abcdef";
const APP: &str = "so.tilo.camera";

fn host_with_portal(
    prompts: PromptQueueHandle,
) -> (ShellOverlayHost, GrantStoreHandle, WorkspaceId) {
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::from(1);
    tree.desktop.workspaces.push(Workspace::new(id, "demo"));
    tree.set_active(id);
    let handle = tree.shared();
    {
        let mut t = handle.write();
        let owner = shell::shell::state::local_user_id();
        t.adopt_ownerless_workspaces(&owner);
    }
    let (host, _chrome) = shell::shell::build_tilo_overlay_host_with_handle(handle.clone(), false);
    let mut host = host;
    host.set_portal_queues(prompts, FilePickerQueue::new());
    (host, GrantStoreHandle::new(handle), id)
}

async fn access_proxy(ctx: PortalContext) -> (Connection, Connection, Proxy<'static>) {
    let (server_sock, client_sock) = UnixStream::pair().expect("socket pair");
    let server_builder = Builder::unix_stream(server_sock)
        .server(TEST_GUID)
        .expect("guid")
        .p2p()
        .serve_at(OBJECT_PATH, portal::AccessBackend::new(ctx))
        .expect("serve_at");
    let client_builder = Builder::unix_stream(client_sock).p2p();
    let (server, client) = futures_util::try_join!(server_builder.build(), client_builder.build())
        .expect("p2p connection pair");
    let proxy = Proxy::new(
        &client,
        "org.freedesktop.impl.portal.desktop.tilo",
        OBJECT_PATH,
        "org.freedesktop.impl.portal.Access",
    )
    .await
    .expect("proxy");
    (server, client, proxy)
}

fn context(grants: GrantStoreHandle, prompts: PromptQueueHandle, ws: WorkspaceId) -> PortalContext {
    let caller_map = CallerMap::new();
    caller_map.force_caller_for_tests(PortalCaller::new(ws, APP));
    let resolver_map = caller_map.clone();
    PortalContext {
        grants,
        prompts,
        caller_map,
        file_picker: FilePickerSpawnerHandle::new(Arc::new(NullFilePickerSpawner::cancel())),
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

/// Spawn a camera-shaped `AccessDialog` call.
fn spawn_access(
    proxy: Proxy<'static>,
) -> tokio::task::JoinHandle<(u32, HashMap<String, OwnedValue>)> {
    tokio::spawn(async move {
        let handle = ObjectPath::try_from("/org/freedesktop/portal/desktop/request/1").unwrap();
        proxy
            .call::<_, _, (u32, HashMap<String, OwnedValue>)>(
                "AccessDialog",
                &(
                    handle,
                    APP.to_string(),
                    String::new(),
                    "Camera".to_string(),
                    "Allow access to your camera?".to_string(),
                    String::new(),
                    HashMap::<String, OwnedValue>::new(),
                ),
            )
            .await
            .expect("AccessDialog")
    })
}

/// Turn the compositor's per-frame pumps once.
fn tick(host: &mut ShellOverlayHost) {
    host.drain_portal_prompts();
    host.pump_permission_prompt();
}

/// Wait for the prompt widget to appear, turning the pumps as the compositor would.
#[allow(
    clippy::future_not_send,
    reason = "ShellOverlayHost isn't Send; every call site awaits this in place on the \
              test's own task, never spawning it"
)]
async fn pump_until_prompt(host: &mut ShellOverlayHost) {
    let deadline = std::time::Instant::now()
        .checked_add(crate::support::HANG_BOUND)
        .expect("the hang bound fits in an Instant");
    while std::time::Instant::now() < deadline {
        tick(host);
        if host.permission_prompt_mut_for_tests().is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!(
        "no permission prompt ever appeared — the portal's queue is not \
         being drained, which is the hang this test exists for"
    );
}

/// Await the parked D-Bus call while still turning the pumps.
#[allow(
    clippy::future_not_send,
    reason = "ShellOverlayHost isn't Send; every call site awaits this in place on the \
              test's own task, never spawning it"
)]
async fn await_response(
    host: &mut ShellOverlayHost,
    handle: tokio::task::JoinHandle<(u32, HashMap<String, OwnedValue>)>,
) -> (u32, HashMap<String, OwnedValue>) {
    let deadline = std::time::Instant::now()
        .checked_add(crate::support::HANG_BOUND)
        .expect("the hang bound fits in an Instant");
    while std::time::Instant::now() < deadline {
        tick(host);
        if handle.is_finished() {
            return handle.await.expect("join");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the portal call never answered — the caller is hung");
}

/// Answer the live prompt with `choice`, the way a click does.
fn answer(host: &mut ShellOverlayHost, choice: PromptChoice) {
    let prompt = host
        .permission_prompt_mut_for_tests()
        .expect("a prompt must be up");
    prompt.pending_choice = Some(choice);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "caps borrows out of guard (app_capabilities returns Vec<&CapOverride>) and is \
used by the assert right after; ending the borrow earlier would mean cloning \
the capability records for no real gain"
)]
async fn allowing_or_denying_a_prompt_releases_the_portal_call() {
    // Phase `allowing_a_prompt_releases_the_portal_call`.
    {
        let prompts = PromptQueueHandle::new(PromptResolver::Channel);
        let (mut host, grants, ws) = host_with_portal(prompts.clone());
        let (_s, _c, proxy) = access_proxy(context(grants, prompts.clone(), ws)).await;

        let call = spawn_access(proxy);
        pump_until_prompt(&mut host).await;

        // The queue handed its entry over rather than keeping it.
        assert_eq!(
            prompts.queued_len(),
            0,
            "the portal queue must be emptied by the drain, not merely read"
        );

        answer(&mut host, PromptChoice::Allow);
        let (code, _) = await_response(&mut host, call).await;
        assert_eq!(code, 0, "Allow must resolve the request as granted");
    }

    // Phase `denying_a_prompt_releases_the_portal_call_too`.
    {
        let prompts = PromptQueueHandle::new(PromptResolver::Channel);
        let (mut host, grants, ws) = host_with_portal(prompts.clone());
        let (_s, _c, proxy) = access_proxy(context(grants.clone(), prompts, ws)).await;

        let call = spawn_access(proxy);
        pump_until_prompt(&mut host).await;
        answer(&mut host, PromptChoice::Deny);

        let (code, _) = await_response(&mut host, call).await;
        assert_eq!(code, 1, "Deny must answer, not strand, the caller");

        // And nothing was recorded for an app the user said no to.
        let tree = grants.tree_handle();
        let guard = tree.read();
        let caps = guard
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == ws)
            .expect("workspace")
            .app_capabilities(APP);
        assert!(
            caps.is_empty(),
            "a denied prompt must record nothing; got {caps:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_grants_persist_and_unshowable_requests_are_denied() {
    // Phase `a_session_grant_answers_and_persists`.
    // Session-scoped approval answers the call and persists locally, so the next
    // request does not ask again without leaking the choice to workspace peers.
    {
        let prompts = PromptQueueHandle::new(PromptResolver::Channel);
        let (mut host, grants, ws) = host_with_portal(prompts.clone());
        let (_s, _c, proxy) = access_proxy(context(grants.clone(), prompts, ws)).await;

        let call = spawn_access(proxy);
        pump_until_prompt(&mut host).await;
        answer(&mut host, PromptChoice::Session);
        let (code, _) = await_response(&mut host, call).await;
        assert_eq!(code, 0);

        assert_eq!(
            grants.lookup(&PortalCaller::new(ws, APP), CapabilityKind::Camera),
            Some(shell::shell::state::GRANT_SESSION)
        );
        let tree = grants.tree_handle();
        let has_replicated_camera = {
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
                        Some(Capability::Camera { .. })
                    )
                })
        };
        assert!(
            !has_replicated_camera,
            "a private portal decision must not enter the collaborative workspace"
        );
    }

    // Phase `a_request_that_cannot_be_shown_is_denied_rather_than_parked`.
    // The compositor with no chrome cannot show a prompt. It must say so
    // by answering, rather than holding a sender nobody will ever fulfil.
    {
        async fn phase() {
            let prompts = PromptQueueHandle::new(PromptResolver::Channel);
            let grants = {
                let mut tree = WorkspaceTree::default();
                let id = WorkspaceId::from(1);
                tree.desktop.workspaces.push(Workspace::new(id, "demo"));
                tree.set_active(id);
                GrantStoreHandle::new(tree.shared())
            };
            let ws = WorkspaceId::from(1);
            let (_s, _c, proxy) = access_proxy(context(grants, prompts.clone(), ws)).await;

            let call = spawn_access(proxy);

            // Drop the queue's contents the way a compositor with no chrome does.
            let deadline = std::time::Instant::now()
                .checked_add(crate::support::HANG_BOUND)
                .expect("the hang bound fits in an Instant");
            while std::time::Instant::now() < deadline {
                if prompts.queued_len() > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(prompts.queued_len() > 0, "the request must reach the queue");
            for pending in prompts.drain() {
                drop(pending);
            }

            let deadline = std::time::Instant::now()
                .checked_add(crate::support::HANG_BOUND)
                .expect("the hang bound fits in an Instant");
            while std::time::Instant::now() < deadline {
                if call.is_finished() {
                    let (code, _) = call.await.expect("join");
                    assert_eq!(
                        code, 1,
                        "a dropped prompt must read as deny, not as a pending request"
                    );
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("dropping the prompt left the caller hung — the sender was not released");
        }
        Box::pin(phase()).await;
    }
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use shell::shell::state::WorkspaceExt as _;
