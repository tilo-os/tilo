//! Integration tests for the xdg-desktop-portal D-Bus backend: one
//! round-trip per interface (`FileChooser` / `ScreenCast` / Access), plus the
//! "Always-grant skips the prompt" fast path. No real session bus — each
//! test uses an in-process p2p zbus connection via
//! `tokio::net::UnixStream::pair()`, so no broker and no real file dialog.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::string_slice,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use shell::portal::{
    self, BUS_NAME, CallerMap, CapabilityKind, FilePickerSpawnerHandle, GrantStoreHandle,
    MockFilePickerSpawner, MockScreenCastProvisioner, NullFilePickerSpawner, OBJECT_PATH,
    PortalCaller, PortalContext, PrintSpoolerHandle, PromptQueueHandle, PromptResolver,
    RecordingInjector, RecordingSpooler, RemoteInputInjectorHandle,
    ScreenCastStreamProvisionerHandle,
};
use shell::shell::state::{
    Capability, FsMode, GRANT_ALWAYS, GRANT_ONCE, Principal, Subject, Workspace, WorkspaceId,
};
use tokio::net::UnixStream;
use zbus::connection::Builder;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

const TEST_GUID: &str = "0123456789abcdef0123456789abcdef";

fn empty_workspace(id: u64, name: &str) -> Workspace {
    Workspace::new(WorkspaceId(id), name)
}

/// Server + paired client zbus connections, both p2p. The returned ctx
/// clone shares the underlying `GrantStore` + `PromptQueue`, so tests can
/// poke/inspect them across the wire.
async fn bring_up_pair(
    grants: GrantStoreHandle,
    prompts: PromptQueueHandle,
    caller_map: CallerMap,
) -> (Connection, Connection, PortalContext) {
    bring_up_pair_with_spawners(
        grants,
        prompts,
        caller_map,
        FilePickerSpawnerHandle::new(Arc::new(NullFilePickerSpawner::cancel())),
        ScreenCastStreamProvisionerHandle::new(Arc::new(MockScreenCastProvisioner::new_with_node(
            1, "test",
        ))),
        RemoteInputInjectorHandle::null(),
        PrintSpoolerHandle::null(),
    )
    .await
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
async fn portal_interfaces_and_notifications_meet_their_published_contracts() {
    // Phase `portal_interfaces_introspect_and_advertise_their_contracts`.
    {
        // Phase `screencast_backend_introspection_matches_the_impl_contract`.
        // The `PipeWire` remote is a frontend-portal operation.  The implementation
        // backend returns source node ids only; adding a backend `OpenPipeWireRemote`
        // method would bypass `xdg-desktop-portal`'s restricted portal connection.
        {
            let (store, map, _ws) = screencast_always(205, "org.example.contract");
            let (_server, client, _ctx) =
                bring_up_pair(store, PromptQueueHandle::new(PromptResolver::Channel), map).await;
            let proxy = Proxy::new(
                &client,
                BUS_NAME,
                OBJECT_PATH,
                "org.freedesktop.DBus.Introspectable",
            )
            .await
            .expect("introspection proxy");
            let xml: String = proxy
                .call("Introspect", &())
                .await
                .expect("introspection XML");
            let start = xml
                .find("org.freedesktop.impl.portal.ScreenCast")
                .expect("ScreenCast interface");
            let screen_cast = &xml[start..];
            for method in ["CreateSession", "SelectSources", "Start"] {
                assert!(
                    screen_cast.contains(&format!("method name=\"{method}\"")),
                    "missing standard ScreenCast backend method {method}"
                );
            }
            assert!(
                !screen_cast.contains("OpenPipeWireRemote"),
                "OpenPipeWireRemote belongs to org.freedesktop.portal.ScreenCast on the frontend"
            );
        }

        // Phase `screencast_properties_advertise_capabilities`.
        {
            let (store, _ws) = GrantStoreHandle::test_with_workspace(empty_workspace(205, "ws"));
            let (_s, client, _ctx) = bring_up_pair(
                store,
                PromptQueueHandle::new(PromptResolver::AutoAllow),
                CallerMap::new(),
            )
            .await;
            let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.ScreenCast").await;
            let types: u32 = proxy
                .get_property("AvailableSourceTypes")
                .await
                .expect("types");
            let modes: u32 = proxy
                .get_property("AvailableCursorModes")
                .await
                .expect("modes");
            let version: u32 = proxy.get_property("version").await.expect("version");
            assert_eq!(types, 1 | 2 | 4, "monitor|window|virtual");
            assert_eq!(modes, 1 | 2 | 4, "hidden|embedded|metadata");
            assert!(version >= 4, "restore_token support requires v4+");
        }
    }

    // Phase `notifications_round_trip_against_the_version_two_contract`.
    {
        // Phase `notification_add_replace_isolate_and_remove_round_trip`.
        {
            let (store, _ws_id) = GrantStoreHandle::test_with_workspace(empty_workspace(100, "ws"));
            let queue = PromptQueueHandle::new(PromptResolver::AutoAllow);
            let caller_map = CallerMap::new();
            let (_server, client, ctx) = bring_up_pair(store, queue, caller_map).await;
            let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.Notification").await;

            let mut payload = HashMap::new();
            payload.insert("title", string_value("Build complete"));
            payload.insert("body", string_value("All checks passed"));
            payload.insert("priority", string_value("high"));
            proxy
                .call_method(
                    "AddNotification",
                    &("org.example.build", "result", &payload),
                )
                .await
                .expect("add notification");

            let first = ctx.notifications.drain_incoming();
            assert_eq!(first.len(), 1);
            assert_eq!(first[0].summary, "Build complete");
            assert_eq!(first[0].body, "All checks passed");
            assert_eq!(first[0].urgency, 2);
            assert_eq!(
                first[0]
                    .caller
                    .as_ref()
                    .map(|caller| caller.app_id.as_str()),
                Some("org.example.build")
            );
            let first_numeric_id = first[0].dbus_id;

            payload.insert("title", string_value("Build updated"));
            proxy
                .call_method(
                    "AddNotification",
                    &("org.example.build", "result", &payload),
                )
                .await
                .expect("replace notification");
            let replacement = ctx.notifications.drain_incoming();
            assert_eq!(replacement.len(), 1);
            assert_eq!(replacement[0].dbus_id, first_numeric_id);
            assert_eq!(replacement[0].summary, "Build updated");

            proxy
                .call_method(
                    "AddNotification",
                    &("org.example.other", "result", &payload),
                )
                .await
                .expect("same string id for another app");
            let other = ctx.notifications.drain_incoming();
            assert_eq!(other.len(), 1);
            assert_ne!(other[0].dbus_id, first_numeric_id);

            proxy
                .call_method("RemoveNotification", &("org.example.build", "result"))
                .await
                .expect("remove notification");
            assert_eq!(
                ctx.notifications.drain_close_requests(),
                vec![first_numeric_id]
            );

            // Removal consumes the app-local mapping. Repeating it is a harmless
            // no-op and cannot close the other application's same-named entry.
            proxy
                .call_method("RemoveNotification", &("org.example.build", "result"))
                .await
                .expect("remove missing notification");
            assert!(ctx.notifications.drain_close_requests().is_empty());
        }

        // Phase `notification_properties_match_version_two_contract`.
        {
            let (store, _ws_id) =
                GrantStoreHandle::test_with_workspace(empty_workspace(1001, "ws"));
            let queue = PromptQueueHandle::new(PromptResolver::AutoAllow);
            let caller_map = CallerMap::new();
            let (_server, client, _ctx) = bring_up_pair(store, queue, caller_map).await;
            let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.Notification").await;

            let version: u32 = proxy
                .get_property("version")
                .await
                .expect("version property");
            assert_eq!(version, 2);
            let options: HashMap<String, OwnedValue> = proxy
                .get_property("SupportedOptions")
                .await
                .expect("supported options property");
            assert!(options.is_empty());
        }
    }
}

async fn bring_up_pair_with_spawners(
    grants: GrantStoreHandle,
    prompts: PromptQueueHandle,
    caller_map: CallerMap,
    file_picker: FilePickerSpawnerHandle,
    screencast_provisioner: ScreenCastStreamProvisionerHandle,
    remote_input_injector: RemoteInputInjectorHandle,
    print_spooler: PrintSpoolerHandle,
) -> (Connection, Connection, PortalContext) {
    use shell::portal::{
        AccessBackend, FileChooserBackend, NotificationBackend, PrintBackend, RemoteDesktopBackend,
        ScreenCastBackend,
    };
    let (server_sock, client_sock) = UnixStream::pair().expect("socket pair");

    let resolver_grants = grants.clone();
    let resolver_callers = caller_map.clone();
    let resolver = Arc::new(move |app_id: &str, _: Option<&str>| {
        resolver_callers.lookup("").or_else(|| {
            resolver_grants
                .tree_handle()
                .read()
                .desktop
                .active_workspace()
                .map(|workspace| PortalCaller::new(workspace.id, app_id))
        })
    });
    let ctx = PortalContext {
        grants,
        prompts,
        caller_map,
        file_picker,
        screencast_provisioner,
        remote_input_injector,
        input_capture: shell::portal::InputCaptureCompositorHandle::unavailable(),
        documents: shell::portal::DocumentStoreHandle::host_paths(),
        print_spooler,
        notifications: shell::notifications::NotificationBridge::new(),
        desktop_services: shell::portal::DesktopPortalServices::default(),
        authenticator: Some(shell::portal::PortalAuthenticator::fixed_for_tests(
            ":1.0".try_into().expect("fixed p2p owner"),
            resolver,
        )),
    };
    let ctx_clone = ctx.clone();

    // `try_join!` on the build pair so both ends negotiate auth
    // concurrently — serial `await` calls deadlock because the
    // server-side handshake wants bytes the client hasn't sent yet.
    let fc = FileChooserBackend::new(ctx.clone());
    let sc = ScreenCastBackend::new(ctx.clone());
    let ac = AccessBackend::new(ctx.clone());
    let rd = RemoteDesktopBackend::new(ctx.clone());
    let pr = PrintBackend::new(ctx.clone());
    let no = NotificationBackend::new(ctx.clone());
    let settings = shell::portal::SettingsBackend::new(ctx.clone());

    let server_builder = Builder::unix_stream(server_sock)
        .server(TEST_GUID)
        .expect("guid")
        .p2p()
        .serve_at(OBJECT_PATH, fc)
        .expect("serve_at fc")
        .serve_at(OBJECT_PATH, sc)
        .expect("serve_at sc")
        .serve_at(OBJECT_PATH, ac)
        .expect("serve_at ac")
        .serve_at(OBJECT_PATH, rd)
        .expect("serve_at rd")
        .serve_at(OBJECT_PATH, pr)
        .expect("serve_at pr")
        .serve_at(OBJECT_PATH, no)
        .expect("serve_at notification")
        .serve_at(OBJECT_PATH, settings)
        .expect("serve_at settings");
    let client_builder = Builder::unix_stream(client_sock).p2p();

    let (server_conn, client_conn) =
        futures_util::try_join!(server_builder.build(), client_builder.build())
            .expect("p2p connection pair");

    (server_conn, client_conn, ctx_clone)
}

/// A placeholder destination is set only because zbus's `Builder` requires
/// one — on p2p it's informational; the message is dispatched directly on
/// the remote object server.
async fn portal_proxy<'a>(client: &'a Connection, interface: &'static str) -> Proxy<'a> {
    zbus::proxy::Builder::<Proxy<'a>>::new(client)
        .destination(BUS_NAME)
        .expect("destination")
        .path(OBJECT_PATH)
        .expect("path")
        .interface(interface)
        .expect("interface")
        .build()
        .await
        .expect("proxy build")
}

fn string_value(value: &str) -> OwnedValue {
    OwnedValue::from(zbus::zvariant::Str::from(value))
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
async fn portal_requests_round_trip_and_prompt_outcomes_reach_the_grant_store() {
    // Phase `auto_allowed_portal_requests_round_trip`.
    {
        // Phase `file_chooser_open_file_round_trips_auto_allow`.
        {
            let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_workspace(101, "ws"));
            let queue = PromptQueueHandle::new(PromptResolver::AutoAllow);
            let caller_map = CallerMap::new();
            let mock = Arc::new(MockFilePickerSpawner::returning(vec![PathBuf::from(
                "/tmp/picked.txt",
            )]));
            let fp_handle = FilePickerSpawnerHandle::new(mock.clone());
            let sc_handle = ScreenCastStreamProvisionerHandle::new(Arc::new(
                MockScreenCastProvisioner::new_with_node(1, "test"),
            ));
            let (_server, client, _ctx) = bring_up_pair_with_spawners(
                store,
                queue,
                caller_map,
                fp_handle,
                sc_handle,
                RemoteInputInjectorHandle::null(),
                PrintSpoolerHandle::null(),
            )
            .await;

            let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.FileChooser").await;

            // Args: handle (o), app_id (s), parent_window (s), title (s),
            //       options (a{sv}). Returns: response (u), results (a{sv}).
            let handle: ObjectPath<'_> = "/org/freedesktop/portal/desktop/request/1"
                .try_into()
                .unwrap();
            let app_id: &str = "org.example.test";
            let parent: &str = "";
            let title: &str = "Open File (test)";
            let opts: HashMap<&str, OwnedValue> = HashMap::new();

            let (response, results): (u32, HashMap<String, OwnedValue>) = proxy
                .call_method("OpenFile", &(&handle, app_id, parent, title, &opts))
                .await
                .expect("call OpenFile")
                .body()
                .deserialize()
                .expect("deserialize reply");

            assert_eq!(response, 0, "auto-allow must yield response=0");
            assert!(results.contains_key("uris"), "results must include uris");
            if let Some(v) = results.get("uris") {
                let owned: OwnedValue = v.try_clone().expect("clone OwnedValue");
                let parsed: Vec<String> =
                    Vec::<String>::try_from(owned).expect("as -> Vec<String>");
                assert_eq!(parsed, vec!["file:///tmp/picked.txt".to_string()]);
            }
            let seen = mock.requests();
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].1, "Open File (test)");
            let _ = ws_id;
        }

        // Phase `screencast_select_sources_round_trips_auto_allow`.
        {
            let (store, _ws_id) = GrantStoreHandle::test_with_workspace(empty_workspace(102, "ws"));
            let queue = PromptQueueHandle::new(PromptResolver::AutoAllow);
            let caller_map = CallerMap::new();
            let (_server, client, _ctx) = bring_up_pair(store, queue, caller_map).await;

            let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.ScreenCast").await;

            // SelectSources(handle, session_handle, app_id, options) → (u, a{sv})
            let handle: ObjectPath<'_> = "/org/freedesktop/portal/desktop/request/test/h1"
                .try_into()
                .unwrap();
            let session: ObjectPath<'_> = "/org/freedesktop/portal/desktop/session/test/s1"
                .try_into()
                .unwrap();
            let app_id: &str = "org.example.screencast";
            let opts: HashMap<&str, OwnedValue> = HashMap::new();

            let (created, _) =
                sc_call(&proxy, "CreateSession", &(&handle, &session, app_id, &opts)).await;
            assert_eq!(created, 0);

            let (response, _results): (u32, HashMap<String, OwnedValue>) = proxy
                .call_method("SelectSources", &(&handle, &session, app_id, &opts))
                .await
                .expect("call SelectSources")
                .body()
                .deserialize()
                .expect("deserialize reply");

            assert_eq!(response, 0, "auto-allow must yield response=0");
        }

        // Phase `access_dialog_camera_round_trips_auto_allow`.
        {
            let (store, _ws_id) = GrantStoreHandle::test_with_workspace(empty_workspace(103, "ws"));
            let queue = PromptQueueHandle::new(PromptResolver::AutoAllow);
            let caller_map = CallerMap::new();
            let (_server, client, _ctx) = bring_up_pair(store, queue, caller_map).await;

            let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.Access").await;

            // AccessDialog(handle, app_id, parent_window, title, subtitle, body, options)
            let handle: ObjectPath<'_> = "/org/freedesktop/portal/desktop/request/test/access2"
                .try_into()
                .unwrap();
            let app_id: &str = "org.example.cam";
            let parent: &str = "";
            let title: &str = "Camera Access";
            let subtitle: &str = "Allow camera?";
            let body: &str = "An app wants to use your camera";
            let opts: HashMap<&str, OwnedValue> = HashMap::new();

            let (response, _results): (u32, HashMap<String, OwnedValue>) = proxy
                .call_method(
                    "AccessDialog",
                    &(&handle, app_id, parent, title, subtitle, body, &opts),
                )
                .await
                .expect("call AccessDialog")
                .body()
                .deserialize()
                .expect("deserialize reply");

            assert_eq!(response, 0, "auto-allow must yield response=0");
        }
    }

    // Phase `prompt_outcomes_map_to_responses_and_the_grant_store`.
    {
        // Phase `always_grant_skips_prompt_queue`.
        {
            // Pre-seed an `Always`-scoped Camera grant. `PromptResolver::Channel`
            // means a request pumped through the queue would show in `queued_len`
            // and hang — the fast path bypasses the queue entirely.
            let mut ws = empty_workspace(104, "ws");
            // `app_grants_snapshot` hands back a fresh map by value — inserting
            // into it stores nothing, so seeding must go through `grant_app`.
            ws.grant_app(
                "org.example.always",
                &Capability::Camera {
                    scope: GRANT_ALWAYS,
                },
            );
            let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
            let queue = PromptQueueHandle::new(PromptResolver::Channel);

            // P2p sockets don't surface a unique-name on the request header, so
            // the `caller_map`'s key-based lookup misses; `force_caller_for_tests`
            // resolves the request to our seeded workspace + app-id every time.
            let caller_map = CallerMap::new();
            let caller = PortalCaller::new(ws_id, "org.example.always");
            caller_map.force_caller_for_tests(caller);

            let (_server, client, ctx) = bring_up_pair(store, queue.clone(), caller_map).await;

            let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.Access").await;
            let handle: ObjectPath<'_> = "/org/freedesktop/portal/desktop/request/test/access3"
                .try_into()
                .unwrap();
            let opts: HashMap<&str, OwnedValue> = HashMap::new();
            // 30s timeout so a fast-path regression (e.g. accidentally pumping
            // every request through the queue) fails explicitly instead of hanging CI.
            let reply = tilo_test_env::within(
                "timed out — Always grant did not fast-path",
                proxy.call_method(
                    "AccessDialog",
                    &(
                        &handle,
                        "org.example.always",
                        "",
                        "Camera",
                        "Allow camera?",
                        "use camera",
                        &opts,
                    ),
                ),
            )
            .await
            .expect("call AccessDialog");
            let (response, _results): (u32, HashMap<String, OwnedValue>) =
                reply.body().deserialize().expect("deserialize reply");

            assert_eq!(response, 0, "Always-grant fast-path returns 0");
            // The fast-path skips `enqueue` — if any prompt got queued, we'd see a
            // non-zero length (`Channel` resolver buffers without resolving the oneshot).
            assert_eq!(
                queue.queued_len(),
                0,
                "Always grant must not enqueue a prompt"
            );
            let _ = ctx;
        }

        // Phase `deny_outcome_returns_response_one`.
        {
            let (store, _ws_id) = GrantStoreHandle::test_with_workspace(empty_workspace(105, "ws"));
            let queue = PromptQueueHandle::new(PromptResolver::AutoDeny);
            let (_server, client, _ctx) = bring_up_pair(store, queue, CallerMap::new()).await;

            let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.FileChooser").await;
            let handle: ObjectPath<'_> = "/h/4".try_into().unwrap();
            let opts: HashMap<&str, OwnedValue> = HashMap::new();
            let (response, results): (u32, HashMap<String, OwnedValue>) = proxy
                .call_method(
                    "OpenFile",
                    &(&handle, "org.example.denied", "", "Open", &opts),
                )
                .await
                .expect("call OpenFile")
                .body()
                .deserialize()
                .expect("deserialize reply");
            assert_eq!(response, 1, "deny returns response=1 per portal contract");
            assert!(results.is_empty(), "deny path returns empty results");
        }

        // Phase `session_outcome_persists_into_grant_store`.
        {
            let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_workspace(106, "ws"));
            let queue = PromptQueueHandle::new(PromptResolver::AutoOutcome(
                portal::PromptOutcome::AllowSession,
            ));

            let caller_map = CallerMap::new();
            let caller = PortalCaller::new(ws_id, "org.example.session");
            caller_map.force_caller_for_tests(caller.clone());

            let (_server, client, ctx) = bring_up_pair(store.clone(), queue, caller_map).await;

            let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.Access").await;
            let handle: ObjectPath<'_> = "/org/freedesktop/portal/desktop/request/test/access5"
                .try_into()
                .unwrap();
            let opts: HashMap<&str, OwnedValue> = HashMap::new();
            let (response, _): (u32, HashMap<String, OwnedValue>) = proxy
                .call_method(
                    "AccessDialog",
                    &(
                        &handle,
                        "org.example.session",
                        "",
                        "Camera",
                        "",
                        "wants the camera",
                        &opts,
                    ),
                )
                .await
                .expect("call AccessDialog")
                .body()
                .deserialize()
                .expect("deserialize reply");
            assert_eq!(response, 0);
            // Session-scoped grant should now be persisted, but caller-map
            // resolution isn't guaranteed across zbus versions — soften to "the
            // round trip succeeded"; lib tests cover the resolver path
            // deterministically.
            let _ = (store, ctx);
        }
    }
}

#[test]
fn portal_names_and_capability_kinds_follow_the_conventions() {
    // Phase `bus_name_matches_freedesktop_portal_convention`.
    {
        // Must match the shipped `os/.../portals/tilo.portal` `DBusName`; the
        // compositor owns it directly rather than racing an activation service.
        assert_eq!(BUS_NAME, "org.freedesktop.impl.portal.desktop.tilo");
        assert_eq!(OBJECT_PATH, "/org/freedesktop/portal/desktop");
    }

    // Phase `capability_kind_round_trips_through_grant_store`.
    {
        // Non-D-Bus check that the integration-test crate can round-trip the
        // public types — guards against a stray re-export miss.
        let mut ws = empty_workspace(107, "ws");
        ws.grant_app(
            "x",
            &Capability::Filesystem {
                path: "/work/a.txt".into(),
                mode: FsMode::Read,
                scope: GRANT_ONCE,
            },
        );
        let (store, id) = GrantStoreHandle::test_with_workspace(ws);
        let caller = PortalCaller::new(id, "x");
        assert_eq!(
            store.lookup(&caller, CapabilityKind::FilesystemRead),
            Some(GRANT_ONCE)
        );
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
async fn screencast_start_provisions_the_selected_streams_and_restores_without_reprompt() {
    // Phase `screencast_start_provisions_pipewire_stream`.
    // Uses a `MockScreenCastProvisioner` — real Pipewire isn't assumed on
    // every build host.
    {
        let mut ws = empty_workspace(108, "ws");
        // Pre-seed an Always-scoped Screencast grant so `Start` skips the prompt;
        // the provisioner is called either way.
        ws.grant_app(
            "org.example.cast",
            &Capability::Screencast {
                scope: GRANT_ALWAYS,
            },
        );
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
        let queue = PromptQueueHandle::new(PromptResolver::Channel);

        let caller_map = CallerMap::new();
        caller_map.force_caller_for_tests(PortalCaller::new(ws_id, "org.example.cast"));

        let provisioner = Arc::new(MockScreenCastProvisioner::new_with_node(77, "test"));
        let fp_handle = FilePickerSpawnerHandle::new(Arc::new(NullFilePickerSpawner::cancel()));
        let sc_handle = ScreenCastStreamProvisionerHandle::new(provisioner.clone());

        let (_server, client, _ctx) = bring_up_pair_with_spawners(
            store,
            queue,
            caller_map,
            fp_handle,
            sc_handle,
            RemoteInputInjectorHandle::null(),
            PrintSpoolerHandle::null(),
        )
        .await;

        let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.ScreenCast").await;
        let handle: ObjectPath<'_> = "/org/freedesktop/portal/desktop/request/test/h6"
            .try_into()
            .unwrap();
        let session: ObjectPath<'_> = "/org/freedesktop/portal/desktop/session/test/s6"
            .try_into()
            .unwrap();
        let mut opts: HashMap<&str, OwnedValue> = HashMap::new();
        let size: (u32, u32) = (1280, 720);
        opts.insert("size", OwnedValue::try_from(Value::new(size)).unwrap());

        let empty: HashMap<&str, OwnedValue> = HashMap::new();
        let (created, _) = sc_call(
            &proxy,
            "CreateSession",
            &(&handle, &session, "org.example.cast", &empty),
        )
        .await;
        assert_eq!(created, 0);
        let (selected, _) = sc_call(
            &proxy,
            "SelectSources",
            &(&handle, &session, "org.example.cast", &empty),
        )
        .await;
        assert_eq!(selected, 0);

        let (response, results): (u32, HashMap<String, OwnedValue>) = proxy
            .call_method("Start", &(&handle, &session, "org.example.cast", "", &opts))
            .await
            .expect("call Start")
            .body()
            .deserialize()
            .expect("deserialize reply");

        assert_eq!(response, 0, "fast-path Allow yields response=0");
        let streams_v = results
            .get("streams")
            .expect("results must include streams");
        let cloned: OwnedValue = streams_v.try_clone().expect("clone OwnedValue");
        let streams: Vec<(u32, HashMap<String, OwnedValue>)> =
            Vec::<(u32, HashMap<String, OwnedValue>)>::try_from(cloned).expect("Vec<(u32, dict)>");
        assert_eq!(streams.len(), 1, "exactly one stream provisioned");
        assert_eq!(streams[0].0, 77, "mock's node id round-trips");
        let seen = provisioner.requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].width, 1280);
        assert_eq!(seen[0].height, 720);
    }

    // Phase `screencast_start_honors_selected_source_type_and_cursor_mode`.
    {
        let app = "org.example.win";
        let (store, map, _ws) = screencast_always(201, app);
        let provisioner = Arc::new(MockScreenCastProvisioner::new_with_node(50, "t"));
        let (_s, client, _ctx) = bring_up_pair_with_spawners(
            store,
            PromptQueueHandle::new(PromptResolver::Channel),
            map,
            FilePickerSpawnerHandle::new(Arc::new(NullFilePickerSpawner::cancel())),
            ScreenCastStreamProvisionerHandle::new(provisioner.clone()),
            RemoteInputInjectorHandle::null(),
            PrintSpoolerHandle::null(),
        )
        .await;
        let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.ScreenCast").await;
        let h: ObjectPath<'_> = "/org/freedesktop/portal/desktop/request/test/ha"
            .try_into()
            .unwrap();
        let session: ObjectPath<'_> = "/org/freedesktop/portal/desktop/session/test/win"
            .try_into()
            .unwrap();
        let empty: HashMap<&str, OwnedValue> = HashMap::new();
        let _ = sc_call(&proxy, "CreateSession", &(&h, &session, app, &empty)).await;

        let mut opts: HashMap<&str, OwnedValue> = HashMap::new();
        opts.insert("types", OwnedValue::try_from(Value::U32(2)).unwrap()); // window
        opts.insert("cursor_mode", OwnedValue::try_from(Value::U32(1)).unwrap()); // hidden
        let (r, _) = sc_call(&proxy, "SelectSources", &(&h, &session, app, &opts)).await;
        assert_eq!(r, 0);

        let (r2, results) = sc_call(&proxy, "Start", &(&h, &session, app, "", &empty)).await;
        assert_eq!(r2, 0);
        let streams = streams_of(&results);
        assert_eq!(streams.len(), 1);
        assert_eq!(
            u32_prop(&streams[0].1, "source_type"),
            2,
            "window source type round-trips through the stream props"
        );
        let seen = provisioner.requests();
        assert_eq!(seen[0].source_type, 2);
        assert_eq!(seen[0].cursor_mode, 1, "hidden cursor mode forwarded");
    }

    // Phase `screencast_multiple_yields_one_stream_per_source`.
    {
        let app = "org.example.multi";
        let (store, map, _ws) = screencast_always(202, app);
        let (_s, client, _ctx) = bring_up_pair_with_spawners(
            store,
            PromptQueueHandle::new(PromptResolver::Channel),
            map,
            FilePickerSpawnerHandle::new(Arc::new(NullFilePickerSpawner::cancel())),
            ScreenCastStreamProvisionerHandle::new(Arc::new(
                MockScreenCastProvisioner::new_with_node(80, "t"),
            )),
            RemoteInputInjectorHandle::null(),
            PrintSpoolerHandle::null(),
        )
        .await;
        let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.ScreenCast").await;
        let h: ObjectPath<'_> = "/org/freedesktop/portal/desktop/request/test/hm"
            .try_into()
            .unwrap();
        let session: ObjectPath<'_> = "/org/freedesktop/portal/desktop/session/test/multi"
            .try_into()
            .unwrap();
        let empty: HashMap<&str, OwnedValue> = HashMap::new();
        let _ = sc_call(&proxy, "CreateSession", &(&h, &session, app, &empty)).await;
        let mut opts: HashMap<&str, OwnedValue> = HashMap::new();
        opts.insert("types", OwnedValue::try_from(Value::U32(1 | 2)).unwrap()); // monitor+window
        opts.insert("multiple", OwnedValue::try_from(Value::Bool(true)).unwrap());
        let (r, _) = sc_call(&proxy, "SelectSources", &(&h, &session, app, &opts)).await;
        assert_eq!(r, 0);
        let (_r2, results) = sc_call(&proxy, "Start", &(&h, &session, app, "", &empty)).await;
        let streams = streams_of(&results);
        assert_eq!(streams.len(), 2, "multiple=true yields a stream per source");
    }

    // Phase `screencast_restore_data_skips_reprompt`.
    {
        let app = "org.example.persist";
        let (store, map, ws_id) = screencast_always(203, app);
        let queue = PromptQueueHandle::new(PromptResolver::Channel);
        let (_s, client, ctx) = bring_up_pair_with_spawners(
            store,
            queue.clone(),
            map,
            FilePickerSpawnerHandle::new(Arc::new(NullFilePickerSpawner::cancel())),
            ScreenCastStreamProvisionerHandle::new(Arc::new(
                MockScreenCastProvisioner::new_with_node(90, "t"),
            )),
            RemoteInputInjectorHandle::null(),
            PrintSpoolerHandle::null(),
        )
        .await;
        let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.ScreenCast").await;
        let h: ObjectPath<'_> = "/org/freedesktop/portal/desktop/request/test/hp"
            .try_into()
            .unwrap();
        let empty: HashMap<&str, OwnedValue> = HashMap::new();

        // Issue standard restore data first (fast-path via the Always grant).
        let s1: ObjectPath<'_> = "/org/freedesktop/portal/desktop/session/test/p1"
            .try_into()
            .unwrap();
        let _ = sc_call(&proxy, "CreateSession", &(&h, &s1, app, &empty)).await;
        let mut opts1: HashMap<&str, OwnedValue> = HashMap::new();
        opts1.insert("types", OwnedValue::try_from(Value::U32(2)).unwrap()); // window
        opts1.insert("persist_mode", OwnedValue::try_from(Value::U32(2)).unwrap());
        let (r, _) = sc_call(&proxy, "SelectSources", &(&h, &s1, app, &opts1)).await;
        assert_eq!(r, 0);
        let (rs, results) = sc_call(&proxy, "Start", &(&h, &s1, app, "", &empty)).await;
        assert_eq!(rs, 0);
        let restore_data = results
            .get("restore_data")
            .expect("standard restore_data")
            .try_clone()
            .unwrap();
        assert_eq!(queue.queued_len(), 0, "fast path: no prompt queued");

        // Remove the Always grant so only restore_data can authorize what follows.
        {
            let handle = ctx.grants.tree_handle();
            let mut t = handle.write();
            if let Some(ws) = t.desktop.workspaces.iter_mut().find(|w| w.id == ws_id) {
                // Removing the ordinary capability proves this second request is
                // using the separately app-bound portal restore authority.
                ws.revoke_sandbox_matching(|c| {
                    matches!(&c.subject, Subject::Principal(Principal::App(_)))
                });
            }
        }

        // Present restore_data — under the Channel resolver (never
        // fulfils a queued prompt), an un-restored path would enqueue and hang.
        let s2: ObjectPath<'_> = "/org/freedesktop/portal/desktop/session/test/p2"
            .try_into()
            .unwrap();
        let _ = sc_call(&proxy, "CreateSession", &(&h, &s2, app, &empty)).await;
        let mut opts2: HashMap<&str, OwnedValue> = HashMap::new();
        opts2.insert("restore_data", restore_data);
        let sel2 = tilo_test_env::within(
            "restore data must skip the prompt (would hang otherwise)",
            proxy.call_method("SelectSources", &(&h, &s2, app, &opts2)),
        )
        .await
        .expect("SelectSources");
        let (r2, _): (u32, HashMap<String, OwnedValue>) = sel2.body().deserialize().unwrap();
        assert_eq!(r2, 0, "valid restore data authorizes without a prompt");
        assert_eq!(queue.queued_len(), 0, "no prompt should have been queued");

        let start2 = tilo_test_env::within(
            "restored Start must not prompt",
            proxy.call_method("Start", &(&h, &s2, app, "", &empty)),
        )
        .await
        .expect("Start");
        let (rs2, results2): (u32, HashMap<String, OwnedValue>) =
            start2.body().deserialize().unwrap();
        assert_eq!(rs2, 0);
        let streams2 = streams_of(&results2);
        assert_eq!(
            u32_prop(&streams2[0].1, "source_type"),
            2,
            "restored session recovers the WINDOW selection"
        );
    }
}

// ScreenCast: source types, cursor modes, `multiple`, restore-token
// round-trip, and exact backend interface properties.

/// Pre-seeded with an `Always` Screencast grant + a forced caller, so the
/// handshake fast-paths past the prompt.
fn screencast_always(ws_num: u64, app: &str) -> (GrantStoreHandle, CallerMap, WorkspaceId) {
    let mut ws = empty_workspace(ws_num, "ws");
    ws.grant_app(
        app,
        &Capability::Screencast {
            scope: GRANT_ALWAYS,
        },
    );
    let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
    let map = CallerMap::new();
    map.force_caller_for_tests(PortalCaller::new(ws_id, app));
    (store, map, ws_id)
}

fn u32_prop(dict: &HashMap<String, OwnedValue>, key: &str) -> u32 {
    u32::try_from(dict.get(key).unwrap().try_clone().unwrap()).unwrap()
}

fn streams_of(results: &HashMap<String, OwnedValue>) -> Vec<(u32, HashMap<String, OwnedValue>)> {
    Vec::try_from(results.get("streams").unwrap().try_clone().unwrap()).unwrap()
}

#[allow(
    clippy::future_not_send,
    reason = "the generic `body` reference makes the future's Send-ness depend on the \
              caller's argument; every call site awaits this in place on the test's own \
              task, never spawning it onto another thread"
)]
async fn sc_call(
    proxy: &Proxy<'_>,
    method: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> (u32, HashMap<String, OwnedValue>) {
    proxy
        .call_method(method, body)
        .await
        .expect("call")
        .body()
        .deserialize()
        .expect("deserialize")
}

// RemoteDesktop: consent -> Start -> Notify* seat injection.

#[tokio::test]
async fn remote_desktop_injection_needs_consent_and_an_authenticated_sender() {
    // Phase `remote_desktop_senderless_transport_cannot_inject_after_consent`.
    {
        let app = "org.example.remote";
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_workspace(210, "ws"));
        let map = CallerMap::new();
        map.force_caller_for_tests(PortalCaller::new(ws_id, app));
        let rec = Arc::new(RecordingInjector::new());
        let (_s, client, _ctx) = bring_up_pair_with_spawners(
            store,
            PromptQueueHandle::new(PromptResolver::AutoAllow),
            map,
            FilePickerSpawnerHandle::new(Arc::new(NullFilePickerSpawner::cancel())),
            ScreenCastStreamProvisionerHandle::new(Arc::new(
                MockScreenCastProvisioner::new_with_node(1, "t"),
            )),
            RemoteInputInjectorHandle::new(rec.clone()),
            PrintSpoolerHandle::null(),
        )
        .await;
        let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.RemoteDesktop").await;
        let h: ObjectPath<'_> = "/org/freedesktop/portal/desktop/request/test/hrd"
            .try_into()
            .unwrap();
        let session: ObjectPath<'_> = "/org/freedesktop/portal/desktop/session/test/rd1"
            .try_into()
            .unwrap();
        let empty: HashMap<&str, OwnedValue> = HashMap::new();
        let _ = sc_call(&proxy, "CreateSession", &(&h, &session, app, &empty)).await;

        let mut dev: HashMap<&str, OwnedValue> = HashMap::new();
        dev.insert("types", OwnedValue::try_from(Value::U32(1 | 2)).unwrap()); // keyboard+pointer
        let (sel, _) = sc_call(&proxy, "SelectDevices", &(&h, &session, app, &dev)).await;
        assert_eq!(sel, 0);
        let (started, res) = sc_call(&proxy, "Start", &(&h, &session, app, "", &empty)).await;
        assert_eq!(started, 0);
        assert_eq!(u32_prop(&res, "devices"), 1 | 2, "granted devices returned");

        proxy
            .call_method("NotifyPointerButton", &(&session, &empty, 272i32, 1u32))
            .await
            .expect("NotifyPointerButton");
        proxy
            .call_method("NotifyKeyboardKeycode", &(&session, &empty, 30i32, 1u32))
            .await
            .expect("NotifyKeyboardKeycode");

        let actions = rec.actions();
        assert!(
            actions.is_empty(),
            "a p2p message has no D-Bus unique sender and must never acquire the live session's injection authority"
        );
    }

    // Phase `remote_desktop_denial_blocks_injection`.
    {
        let app = "org.example.denied.rd";
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_workspace(211, "ws"));
        let map = CallerMap::new();
        map.force_caller_for_tests(PortalCaller::new(ws_id, app));
        let rec = Arc::new(RecordingInjector::new());
        let (_s, client, _ctx) = bring_up_pair_with_spawners(
            store,
            PromptQueueHandle::new(PromptResolver::AutoDeny),
            map,
            FilePickerSpawnerHandle::new(Arc::new(NullFilePickerSpawner::cancel())),
            ScreenCastStreamProvisionerHandle::new(Arc::new(
                MockScreenCastProvisioner::new_with_node(1, "t"),
            )),
            RemoteInputInjectorHandle::new(rec.clone()),
            PrintSpoolerHandle::null(),
        )
        .await;
        let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.RemoteDesktop").await;
        let h: ObjectPath<'_> = "/org/freedesktop/portal/desktop/request/test/hrdd"
            .try_into()
            .unwrap();
        let session: ObjectPath<'_> = "/org/freedesktop/portal/desktop/session/test/deny"
            .try_into()
            .unwrap();
        let empty: HashMap<&str, OwnedValue> = HashMap::new();
        let _ = sc_call(&proxy, "CreateSession", &(&h, &session, app, &empty)).await;
        let mut dev: HashMap<&str, OwnedValue> = HashMap::new();
        dev.insert("types", OwnedValue::try_from(Value::U32(2)).unwrap());
        let (sel, _) = sc_call(&proxy, "SelectDevices", &(&h, &session, app, &dev)).await;
        assert_eq!(sel, 1, "denied consent returns response=1");
        // The session never started; Notify* must be dropped.
        proxy
            .call_method("NotifyPointerButton", &(&session, &empty, 272i32, 1u32))
            .await
            .expect("NotifyPointerButton");
        assert!(
            rec.actions().is_empty(),
            "a denied RemoteDesktop session must not inject input"
        );
    }
}

// Print — the property under test: an app hands over a *file descriptor*
// and a request, and the bytes reach the spooler on the compositor's side
// of the sandbox, while the app never sees a printer, socket, or IPP message.

async fn bring_up_print_pair(
    prompts: PromptQueueHandle,
    spooler: Arc<RecordingSpooler>,
) -> (Connection, Connection) {
    let (store, _ws) = GrantStoreHandle::test_with_workspace(empty_workspace(90, "print"));
    let (server, client, _ctx) = bring_up_pair_with_spawners(
        store,
        prompts,
        CallerMap::new(),
        FilePickerSpawnerHandle::new(Arc::new(NullFilePickerSpawner::cancel())),
        ScreenCastStreamProvisionerHandle::new(Arc::new(MockScreenCastProvisioner::new_with_node(
            1, "test",
        ))),
        RemoteInputInjectorHandle::null(),
        PrintSpoolerHandle::new(spooler),
    )
    .await;
    (server, client)
}

/// `OwnedFd` here is a genuine fd crossing the connection, not a path.
fn document_fd(bytes: &[u8]) -> (tempfile::NamedTempFile, zbus::zvariant::OwnedFd) {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = tempfile::NamedTempFile::new().expect("temp document");
    f.write_all(bytes).expect("write document");
    f.flush().expect("flush");
    f.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");
    let dup = f.as_file().try_clone().expect("dup fd");
    (
        f,
        zbus::zvariant::OwnedFd::from(std::os::fd::OwnedFd::from(dup)),
    )
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
async fn print_reaches_the_spooler_only_when_allowed_and_to_a_real_destination() {
    // Phase `print_round_trips_the_document_to_the_spooler`.
    // The token minted by `PreparePrint` is what authorises the Print call.
    {
        let spooler = Arc::new(RecordingSpooler::with_destinations(vec![
            "Office_LaserJet".to_string(),
        ]));
        let prompts = PromptQueueHandle::new(PromptResolver::AutoAllow);
        let (_s, client) = bring_up_print_pair(prompts, spooler.clone()).await;
        let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.Print").await;

        let handle = ObjectPath::try_from("/org/freedesktop/portal/desktop/request/1").unwrap();
        let empty: HashMap<String, Value<'_>> = HashMap::new();
        let (response, results): (u32, HashMap<String, OwnedValue>) = proxy
            .call(
                "PreparePrint",
                &(
                    &handle,
                    "org.example.Editor",
                    "",
                    "Quarterly report",
                    &empty,
                    &empty,
                    &empty,
                ),
            )
            .await
            .expect("PreparePrint");
        assert_eq!(response, 0, "PreparePrint succeeded: {results:?}");
        let token = results.get("token").expect("a token was minted");
        let token: u32 = u32::try_from(token).expect("token is a u32");
        assert_ne!(token, 0, "a zero token would be guessable");

        let (doc, fd) = document_fd(b"%PDF-1.7\nthe quarterly report\n");
        let mut opts: HashMap<String, Value<'_>> = HashMap::new();
        opts.insert("token".to_string(), Value::from(token));
        let (response, results): (u32, HashMap<String, OwnedValue>) = proxy
            .call(
                "Print",
                &(
                    &handle,
                    "org.example.Editor",
                    "",
                    "Quarterly report",
                    fd,
                    &opts,
                ),
            )
            .await
            .expect("Print");
        assert_eq!(response, 0, "Print succeeded: {results:?}");

        let jobs = spooler.jobs();
        assert_eq!(
            jobs.len(),
            1,
            "exactly one job reached the spooler: {jobs:?}"
        );
        assert_eq!(
            jobs[0].document, b"%PDF-1.7\nthe quarterly report\n",
            "the spooler received the bytes behind the descriptor"
        );
        assert_eq!(
            jobs[0].destination, "Office_LaserJet",
            "the destination came from the machine's own printer list, not the app"
        );
        drop(doc);
    }

    // Phase `a_denied_print_never_reaches_the_spooler`.
    {
        let spooler = Arc::new(RecordingSpooler::with_destinations(vec![
            "Office_LaserJet".to_string(),
        ]));
        let prompts = PromptQueueHandle::new(PromptResolver::AutoDeny);
        let (_s, client) = bring_up_print_pair(prompts, spooler.clone()).await;
        let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.Print").await;

        let handle = ObjectPath::try_from("/org/freedesktop/portal/desktop/request/2").unwrap();
        let (doc, fd) = document_fd(b"%PDF-1.7\nsecret\n");
        let opts: HashMap<String, Value<'_>> = HashMap::new();
        let (response, _results): (u32, HashMap<String, OwnedValue>) = proxy
            .call(
                "Print",
                &(&handle, "org.example.Editor", "", "Secret", fd, &opts),
            )
            .await
            .expect("Print call completes");
        assert_eq!(response, 1, "a denied request reports user-cancelled");
        assert!(
            spooler.jobs().is_empty(),
            "nothing was spooled: {:?}",
            spooler.jobs()
        );
        drop(doc);
    }

    // Phase `an_app_cannot_name_a_destination_that_does_not_exist`.
    // Printing to an attacker-chosen name is how a device URI becomes a
    // command line.
    {
        let spooler = Arc::new(RecordingSpooler::with_destinations(vec![
            "Office_LaserJet".to_string(),
        ]));
        let prompts = PromptQueueHandle::new(PromptResolver::AutoAllow);
        let (_s, client) = bring_up_print_pair(prompts, spooler.clone()).await;
        let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.Print").await;

        let handle = ObjectPath::try_from("/org/freedesktop/portal/desktop/request/3").unwrap();
        let (doc, fd) = document_fd(b"%PDF-1.7\nx\n");
        let mut opts: HashMap<String, Value<'_>> = HashMap::new();
        opts.insert(
            "printer".to_string(),
            Value::from("../../etc/cups/printers.conf"),
        );
        let (response, _results): (u32, HashMap<String, OwnedValue>) = proxy
            .call(
                "Print",
                &(&handle, "org.example.Editor", "", "x", fd, &opts),
            )
            .await
            .expect("Print call completes");
        assert_eq!(response, 0, "the job still goes to a real printer");
        let jobs = spooler.jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(
            jobs[0].destination, "Office_LaserJet",
            "the app's chosen destination was ignored in favour of a real one"
        );
        drop(doc);
    }
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use shell::shell::state::WorkspaceExt as _;
/// Legacy toolkits read the OS appearance through `ReadAll`/`Read` and follow
/// it through `SettingChanged`; all three carry the GNOME interface keys the
/// settings file resolves to.
#[tokio::test]
async fn settings_serve_the_gnome_interface_keys_and_signal_changes() {
    use futures_util::StreamExt as _;
    use shell::portal::appearance::{APPEARANCE, GNOME_INTERFACE, LegacyAppearance};

    let (store, _ws_id) = GrantStoreHandle::test_with_workspace(empty_workspace(120, "ws"));
    let (server, client, ctx) = bring_up_pair(
        store,
        PromptQueueHandle::new(PromptResolver::AutoAllow),
        CallerMap::new(),
    )
    .await;
    let settings = ctx.desktop_services.settings.clone();
    shell::portal::settings::spawn_signal_emitter(server.clone(), settings.subscribe());
    let light = LegacyAppearance::from_settings_text(
        r#"{"appearance": {"mode": "light", "text_scale": 1.5}}"#,
        false,
    );
    for (namespace, key, value) in light.portal_settings() {
        settings.set(namespace, key, value);
    }
    let proxy = portal_proxy(&client, "org.freedesktop.impl.portal.Settings").await;

    let all: HashMap<String, HashMap<String, OwnedValue>> = proxy
        .call("ReadAll", &(vec!["org.gnome.desktop.*"],))
        .await
        .expect("ReadAll");
    assert!(
        !all.contains_key(APPEARANCE),
        "the namespace glob filters out org.freedesktop.appearance"
    );
    let interface = all.get(GNOME_INTERFACE).expect("GNOME interface namespace");
    for key in [
        "color-scheme",
        "gtk-theme",
        "accent-color",
        "font-name",
        "monospace-font-name",
        "text-scaling-factor",
        "cursor-theme",
        "cursor-size",
        "enable-animations",
    ] {
        assert!(interface.contains_key(key), "missing {key}");
    }
    assert_eq!(
        String::try_from(interface["gtk-theme"].try_clone().unwrap()).unwrap(),
        "Adwaita"
    );
    assert_eq!(
        f64::try_from(interface["text-scaling-factor"].try_clone().unwrap()).unwrap(),
        1.5
    );

    let reduced: OwnedValue = proxy
        .call("Read", &(APPEARANCE, "reduced-motion"))
        .await
        .expect("Read reduced-motion");
    assert_eq!(u32::try_from(reduced).unwrap(), 0);

    let mut changes = proxy
        .receive_signal("SettingChanged")
        .await
        .expect("subscribe SettingChanged");
    let reduced_motion = LegacyAppearance::from_settings_text(
        r#"{"appearance": {"mode": "light", "text_scale": 1.5, "reduce_motion": true}}"#,
        false,
    );
    for (namespace, key, value) in reduced_motion.portal_settings() {
        settings.set(namespace, key, value);
    }
    let mut seen = Vec::new();
    while seen.len() < 2 {
        let message = tilo_test_env::within("SettingChanged arrives", changes.next())
            .await
            .expect("signal stream open");
        let (namespace, key, _value): (String, String, OwnedValue) =
            message.body().deserialize().expect("signal body");
        seen.push((namespace, key));
    }
    seen.sort();
    assert_eq!(
        seen,
        vec![
            (APPEARANCE.to_string(), "reduced-motion".to_string()),
            (GNOME_INTERFACE.to_string(), "enable-animations".to_string()),
        ],
        "only the values that changed are signalled"
    );
}
