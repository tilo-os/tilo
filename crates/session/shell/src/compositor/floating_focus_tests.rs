use super::super::{ClientState, FloatingTransferSource, PendingFloatingToplevel, Rect};
use super::*;
use crate::compositor::frame::FrameBuilder;
use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::{calloop::EventLoop, wayland_server::Display};
use std::os::unix::net::UnixStream;
use wayland_client::protocol::{wl_compositor, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};

#[derive(Default)]
struct ClientView {
    compositor: Option<wl_compositor::WlCompositor>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for ClientView {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
            && interface == "wl_compositor"
        {
            state.compositor = Some(registry.bind(name, version.min(6), qh, ()));
        }
    }
}
delegate_noop!(ClientView: ignore wl_compositor::WlCompositor);
delegate_noop!(ClientView: ignore wl_surface::WlSurface);

struct Fixture {
    state: State,
    display: Display<State>,
    connection: Connection,
    queue: wayland_client::EventQueue<ClientView>,
    view: ClientView,
    _loop: EventLoop<'static, State>,
    ids: [u64; 3],
}

impl Fixture {
    fn new() -> Self {
        let event_loop = EventLoop::try_new().expect("event loop");
        let display = Display::<State>::new().expect("display");
        let state = State::new(event_loop.handle(), display.handle());
        let (server, peer) = UnixStream::pair().expect("socket pair");
        display
            .handle()
            .insert_client(
                server,
                ClientState::attributed_arc(
                    crate::shell::launcher::VerifiedLaunchIdentity {
                        package_id: "so.tilo.chat".into(),
                        workspace_id: Some(crate::shell::state::RoomId::new().to_string()),
                        confined: true,
                    },
                    None,
                    42,
                ),
            )
            .expect("actual confined client");
        peer.set_nonblocking(true).expect("nonblocking socket");
        let connection = Connection::from_socket(peer).expect("client connection");
        let queue = connection.new_event_queue::<ClientView>();
        connection.display().get_registry(&queue.handle(), ());
        let mut fixture = Self {
            state,
            display,
            connection,
            queue,
            view: ClientView::default(),
            _loop: event_loop,
            ids: [0; 3],
        };
        fixture.pump();
        let compositor = fixture.view.compositor.as_ref().expect("compositor");
        let surfaces = std::array::from_fn::<_, 3, _>(|_| {
            compositor.create_surface(&fixture.queue.handle(), ())
        });
        fixture.pump();
        for (index, surface) in surfaces.iter().enumerate() {
            let (id, handle) = fixture
                .state
                .surface_handles
                .iter()
                .find(|(_, handle)| handle.id().protocol_id() == surface.id().protocol_id())
                .expect("actual server surface");
            fixture.ids[index] = *id;
            fixture
                .state
                .client_input
                .register_transient(*id, handle.clone());
        }
        fixture
    }

    fn pump(&mut self) {
        for _ in 0..8 {
            self.queue.flush().expect("flush client");
            self.display
                .dispatch_clients(&mut self.state)
                .expect("dispatch server");
            self.display.flush_clients().expect("flush server");
            if let Some(read) = self.connection.prepare_read() {
                let _ = read.read();
            }
            self.queue
                .dispatch_pending(&mut self.view)
                .expect("dispatch client");
        }
    }

    fn float(&mut self) -> FrameBuilder {
        let [source, child, _] = self.ids;
        let owner = self
            .state
            .floating_owner_for_surface(source)
            .expect("source owner");
        self.state
            .arm_floating_focus(7, "conversation", source, owner.clone());
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.state
            .authorize_floating_toplevel(PendingFloatingToplevel {
                owner: Some(owner),
                token: token.clone(),
                connection: 7,
                public_id: "conversation".into(),
                app_id: "so.tilo.chat".into(),
                content_rect: Rect::new(0.0, 0.0, 400.0, 300.0),
                rect: Rect::new(0.0, 0.0, 400.0, 300.0),
                transfer_frame: None,
                transfer_source: Some(FloatingTransferSource { rect: Rect::ZERO }),
                suspended: false,
            });
        assert!(self.state.promote_floating_toplevel(
            child,
            &format!(
                "{}{token}",
                ui::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX
            )
        ));
        FrameBuilder::new((800, 600))
    }

    fn ready(&mut self, fb: &mut FrameBuilder) {
        let store = crate::compositor::foreign::store();
        store.publish_rgba(self.ids[1], &vec![255; 400 * 300 * 4], 400, 300);
        assert!(store.mark_presented(self.ids[1], Rect::new(0.0, 0.0, 400.0, 300.0)));
        self.state.mark_floating_toplevel_presented(self.ids[1]);
        self.state.mark_floating_source_cleared(7, "conversation");
        fb.set_floating_surfaces(&[(
            self.ids[1],
            "so.tilo.chat".into(),
            Rect::new(0.0, 0.0, 400.0, 300.0),
        )]);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for id in self.ids {
            crate::compositor::foreign::store().forget(id);
        }
    }
}

#[test]
fn floating_focus_waits_for_visible_source_clear_and_returns_after_destination() {
    let mut fixture = Fixture::new();
    let [source, child, _] = fixture.ids;
    fixture.state.set_client_keyboard_focus(Some(source));
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(source));
    let mut fb = fixture.float();
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(source));
    fixture.ready(&mut fb);
    crate::compositor::client_input::sync_keyboard_focus(&mut fixture.state, &fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(child));
    fixture
        .state
        .suspend_floating_toplevel(7, "conversation", None);
    assert_eq!(fixture.state.floating_return_focus(), Some(child));
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(child));
    fixture
        .state
        .mark_floating_destination_presented(7, "conversation");
    crate::compositor::client_input::sync_keyboard_focus(&mut fixture.state, &fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(source));
}

#[test]
fn floating_focus_never_rearms_after_intervening_focus_or_steals_unfocused_source() {
    for initially_focused in [true, false] {
        let mut fixture = Fixture::new();
        let [source, _, other] = fixture.ids;
        fixture
            .state
            .set_client_keyboard_focus(Some(if initially_focused { source } else { other }));
        let mut fb = fixture.float();
        if initially_focused {
            fixture.state.set_client_keyboard_focus(Some(other));
            fixture.state.set_client_keyboard_focus(Some(source));
            let owner = fixture
                .state
                .floating_owner_for_surface(source)
                .expect("source owner");
            fixture
                .state
                .arm_floating_focus(7, "conversation", source, owner);
        }
        fixture.ready(&mut fb);
        let _ = fixture.state.apply_floating_focus_handoffs(&fb);
        assert_eq!(
            fixture.state.client_input.keyboard_focus(),
            Some(if initially_focused { source } else { other })
        );
    }
}

#[test]
fn floating_focus_refuses_wrong_owner_and_cancelled_reverse_transfer() {
    let mut fixture = Fixture::new();
    let [source, child, other] = fixture.ids;
    fixture.state.set_client_keyboard_focus(Some(source));
    let mut fb = fixture.float();
    fixture.ready(&mut fb);
    let real_owner = fixture.state.floating_toplevels[0].owner.clone();
    let mut wrong_owner = real_owner.as_deref().expect("owner").clone();
    wrong_owner.room = Some(crate::shell::state::RoomId::new());
    fixture.state.floating_toplevels[0].owner = Some(Arc::new(wrong_owner));
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(source));
    fixture.state.floating_toplevels[0].owner = real_owner;
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(child));
    fixture
        .state
        .suspend_floating_toplevel(7, "conversation", None);
    fixture.state.set_client_keyboard_focus(Some(other));
    fixture.state.set_client_keyboard_focus(Some(child));
    fixture
        .state
        .suspend_floating_toplevel(7, "conversation", None);
    fixture
        .state
        .mark_floating_destination_presented(7, "conversation");
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(child));
    assert_eq!(fixture.state.floating_return_focus(), None);
}

#[test]
fn live_preedit_keeps_exact_source_seat_until_commit_or_same_owner_return() {
    let mut fixture = Fixture::new();
    let [source, child, other] = fixture.ids;
    fixture.state.set_client_keyboard_focus(Some(source));
    fixture
        .state
        .set_input_method_preedit_active_for_tests(true);
    let mut fb = fixture.float();
    fixture.ready(&mut fb);

    crate::compositor::client_input::sync_keyboard_focus(&mut fixture.state, &fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(source));
    fixture.state.set_client_keyboard_focus(Some(child));
    assert_eq!(
        fixture.state.client_input.keyboard_focus(),
        Some(source),
        "the exact floating child cannot deactivate a live composition"
    );

    fixture.state.set_client_keyboard_focus(Some(other));
    assert_eq!(
        fixture.state.client_input.keyboard_focus(),
        Some(other),
        "an unrelated focus transition is never proxied"
    );

    let mut committed = Fixture::new();
    let [committed_source, committed_child, _] = committed.ids;
    committed
        .state
        .set_client_keyboard_focus(Some(committed_source));
    committed
        .state
        .set_input_method_preedit_active_for_tests(true);
    let mut fb = committed.float();
    committed.ready(&mut fb);
    committed
        .state
        .set_input_method_preedit_active_for_tests(false);
    crate::compositor::client_input::sync_keyboard_focus(&mut committed.state, &fb);
    assert_eq!(
        committed.state.client_input.keyboard_focus(),
        Some(committed_child)
    );

    let mut wrong_alias = Fixture::new();
    let [wrong_source, wrong_child, _] = wrong_alias.ids;
    wrong_alias
        .state
        .set_client_keyboard_focus(Some(wrong_source));
    wrong_alias
        .state
        .set_input_method_preedit_active_for_tests(true);
    let mut fb = wrong_alias.float();
    wrong_alias.ready(&mut fb);
    wrong_alias.state.floating_toplevels[0].public_id = "other-conversation".into();
    wrong_alias
        .state
        .set_client_keyboard_focus(Some(wrong_child));
    assert_eq!(
        wrong_alias.state.client_input.keyboard_focus(),
        Some(wrong_child),
        "same-process ownership cannot substitute for the exact floating id"
    );
}

#[test]
fn return_during_live_preedit_needs_no_reverse_protocol_focus() {
    let mut fixture = Fixture::new();
    let [source, _, _] = fixture.ids;
    fixture.state.set_client_keyboard_focus(Some(source));
    fixture
        .state
        .set_input_method_preedit_active_for_tests(true);
    let mut fb = fixture.float();
    fixture.ready(&mut fb);
    crate::compositor::client_input::sync_keyboard_focus(&mut fixture.state, &fb);
    fixture
        .state
        .suspend_floating_toplevel(7, "conversation", None);
    fixture
        .state
        .mark_floating_destination_presented(7, "conversation");
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(source));
}
