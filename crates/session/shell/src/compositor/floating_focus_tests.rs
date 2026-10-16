use super::super::{
    ClientState, FloatingTransferSource, InputMethodPreeditState, PendingFloatingToplevel, Rect,
};
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
        self.float_with_input(None)
    }

    fn float_with_input(&mut self, input_rect: Option<Rect>) -> FrameBuilder {
        let [source, child, _] = self.ids;
        let owner = self
            .state
            .floating_owner_for_surface(source)
            .expect("source owner");
        let rect = Rect::new(0.0, 0.0, 400.0, 300.0);
        assert_eq!(
            self.state.resume_floating_request_with_focus(
                7,
                "conversation",
                (rect, rect),
                (None, None),
                Some((source, owner.clone())),
            ),
            Ok(None)
        );
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.state
            .authorize_floating_toplevel(PendingFloatingToplevel {
                owner: Some(owner),
                token: token.clone(),
                connection: 7,
                public_id: "conversation".into(),
                app_id: "so.tilo.chat".into(),
                content_rect: Rect::new(0.0, 0.0, 400.0, 300.0),
                input_rect,
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
        fb.set_floating_surfaces_with_input(&[(
            self.ids[1],
            "so.tilo.chat".into(),
            Rect::new(0.0, 0.0, 400.0, 300.0),
            None,
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

fn route_float_pointer(fixture: &mut Fixture, fb: &mut FrameBuilder, point: (f64, f64)) {
    let moved = ui::synthetic_input::cursor_moved(point.0, point.1);
    assert!(crate::compositor::client_input::route_input(
        &mut fixture.state,
        fb,
        &moved,
        &winit::event::Modifiers::default()
    ));
}

fn assert_floating_input_routes(
    fixture: &mut Fixture,
    fb: &mut FrameBuilder,
    point: (f64, f64),
    expected: (u64, (f64, f64)),
) {
    use crate::compositor::backend::{
        BackendEvent, TabletDevice, TabletInput, TabletPhase, TabletTool, TabletToolKind,
        TouchEvent,
    };
    use crate::compositor::client_input::{
        publish_remote_input_view, route_tablet_event, touch_target,
    };
    route_float_pointer(fixture, fb, point);
    assert_eq!(fixture.state.client_input.pointer_focus(), Some(expected.0));
    assert_eq!(
        fixture.state.client_input.pointer_focus_origin(),
        Some(expected.1)
    );
    assert_eq!(
        touch_target(
            &fixture.state,
            fb,
            &TouchEvent::Down {
                slot: 0,
                x: point.0,
                y: point.1
            }
        ),
        Some(expected)
    );
    let pen = TabletInput {
        device: TabletDevice {
            name: "test pen".into(),
            usb_id: None,
            syspath: None,
        },
        tool: TabletTool {
            kind: TabletToolKind::Pen,
            serial: 1,
            wacom_id: 1,
            pressure: true,
            distance: false,
            tilt: false,
            rotation: false,
            slider: false,
            wheel: false,
        },
        phase: TabletPhase::Motion,
        position: point,
        pressure: 0.0,
        distance: 0.0,
        tilt: (0.0, 0.0),
        rotation: 0.0,
        slider: 0.0,
        wheel: (0.0, 0),
        time_msec: 7,
    };
    // Ordinary native apps take the production tablet-to-pointer fallback.
    let events = route_tablet_event(&mut fixture.state, fb, &pen, false);
    assert!(!events.is_empty());
    for event in events {
        let BackendEvent::Input(event) = event else {
            panic!("tablet fallback must route input");
        };
        assert!(crate::compositor::client_input::route_input(
            &mut fixture.state,
            fb,
            &event,
            &winit::event::Modifiers::default()
        ));
    }
    assert_eq!(fixture.state.client_input.pointer_focus(), Some(expected.0));
    assert_eq!(
        fixture.state.client_input.pointer_focus_origin(),
        Some(expected.1)
    );
    publish_remote_input_view(&mut fixture.state, fb, false);
    assert_eq!(
        fixture.state.remote_input_firewall.view().topmost_at(point),
        Some(expected.0)
    );
}

#[test]
fn floating_input_gutter_routes_pointer_touch_and_remote_to_lower_float() {
    use crate::network::input_firewall::{Denial, RemoteInputFirewall};
    use crate::network::streaming::InputEvent;

    let mut fixture = Fixture::new();
    let [source, child, lower] = fixture.ids;
    let mut fb = fixture.float();
    fixture.ready(&mut fb);
    let window = Rect::new(60.0, 60.0, 560.0, 460.0);
    let body = Rect::new(120.0, 120.0, 500.0, 380.0);
    let video = Rect::new(150.0, 160.0, 480.0, 300.0);
    assert!(fixture.state.update_floating_toplevel_rect(
        7,
        "conversation",
        window,
        video,
        Some(body)
    ));
    let token = uuid::Uuid::new_v4().simple().to_string();
    let lower_window = Rect::new(80.0, 80.0, 650.0, 450.0);
    fixture
        .state
        .authorize_floating_toplevel(PendingFloatingToplevel {
            owner: Some(fixture.state.floating_owner_for_surface(source).unwrap()),
            token: token.clone(),
            connection: 7,
            public_id: "lower".into(),
            app_id: "so.tilo.chat".into(),
            rect: lower_window,
            content_rect: lower_window,
            input_rect: None,
            transfer_frame: None,
            transfer_source: None,
            suspended: false,
        });
    assert!(fixture.state.promote_floating_toplevel(
        lower,
        &format!(
            "{}{token}",
            ui::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX
        )
    ));
    let store = crate::compositor::foreign::store();
    // Each native buffer answers its current allocation; otherwise input is
    // correctly scaled from the displayed window to the old buffer's size.
    store.publish_rgba(child, &vec![255; 500 * 400 * 4], 500, 400);
    assert!(store.mark_presented(child, window));
    store.publish_rgba(lower, &vec![255; 570 * 370 * 4], 570, 370);
    assert!(store.mark_presented(lower, lower_window));
    fixture.state.mark_floating_toplevel_presented(lower);
    // Rebuild from the production back-to-front order, rather than preserving
    // the child overlay inserted by ready() ahead of a newly inserted lower.
    fb.set_floating_surfaces_with_input(&[]);
    fb.set_floating_surfaces_with_input(&[
        (lower, "so.tilo.chat".into(), lower_window, None),
        (child, "so.tilo.chat".into(), window, Some(body)),
    ]);
    for (point, expected, origin) in [
        ((90.0, 90.0), lower, (80.0, 80.0)),
        ((130.0, 130.0), child, (60.0, 60.0)),
    ] {
        assert_floating_input_routes(&mut fixture, &mut fb, point, (expected, origin));
    }
    drop(fb);
    let mut firewall = RemoteInputFirewall::new();
    firewall.publish_view(fixture.state.remote_input_firewall.view().clone());
    assert_eq!(
        firewall
            .review(
                9,
                lower,
                &InputEvent::PointerMotion { x: 10.0, y: 10.0 },
                std::time::Instant::now()
            )
            .unwrap(),
        InputEvent::PointerMotion { x: 90.0, y: 90.0 }
    );
    assert_eq!(
        firewall.review(
            9,
            child,
            &InputEvent::PointerMotion { x: 30.0, y: 30.0 },
            std::time::Instant::now()
        ),
        Err(Denial::Occluded)
    );
    assert_eq!(
        firewall
            .review(
                9,
                child,
                &InputEvent::PointerMotion { x: 70.0, y: 70.0 },
                std::time::Instant::now()
            )
            .unwrap(),
        InputEvent::PointerMotion { x: 130.0, y: 130.0 }
    );
}

#[test]
fn floating_input_gutter_falls_through_to_the_workspace_tile() {
    use crate::shell::input_policy::{foreign_tiles, insert_foreign_group};
    let mut fixture = Fixture::new();
    let [source, child, _] = fixture.ids;
    let mut fb = fixture.float();
    fixture.ready(&mut fb);
    fb.with_chrome(|chrome| {
        insert_foreign_group(&mut chrome.tree_mut(), source, Some("so.tilo.chat"))
            .expect("source tile");
        chrome.bump();
    });
    let _ = fb.paint_settled();
    let tile = {
        let chrome = fb.chrome();
        foreign_tiles(&chrome.tree(), fb.host())
            .into_iter()
            .find(|tile| tile.surface_id == source)
            .expect("laid out source tile")
    };
    let point = (tile.rect.center().x, tile.rect.center().y);
    let window = Rect::from_center_size(tile.rect.center(), (160.0, 120.0));
    let store = crate::compositor::foreign::store();
    let width = ui::num::u32_of_f64(tile.rect.width().round());
    let height = ui::num::u32_of_f64(tile.rect.height().round());
    store.publish_rgba(
        source,
        &vec![255; (width * height * 4) as usize],
        width,
        height,
    );
    assert!(store.mark_presented(source, tile.rect));
    store.publish_rgba(child, &vec![255; 160 * 120 * 4], 160, 120);
    assert!(store.mark_presented(child, window));
    let body = ui::floating_surface::translate_input_rect(
        Some(Rect::new(8.0, 8.0, 48.0, 112.0)),
        vello::kurbo::Point::ZERO,
        window.origin(),
    )
    .expect("body geometry");
    assert!(fixture.state.update_floating_toplevel_rect(
        7,
        "conversation",
        window,
        body,
        Some(body)
    ));
    fb.set_floating_surfaces_with_input(&[(child, "so.tilo.chat".into(), window, Some(body))]);
    assert!(
        fb.floating_surface_hit(point).is_none(),
        "transparent gutter is not an overlay hit"
    );
    assert_floating_input_routes(
        &mut fixture,
        &mut fb,
        point,
        (source, (tile.rect.x0, tile.rect.y0)),
    );
    // An absent declaration retains the old full-window behavior.
    assert!(
        fixture
            .state
            .update_floating_toplevel_rect(7, "conversation", window, body, None)
    );
    fb.set_floating_surfaces_with_input(&[(child, "so.tilo.chat".into(), window, None)]);
    assert_floating_input_routes(
        &mut fixture,
        &mut fb,
        point,
        (child, (window.x0, window.y0)),
    );
    drop(fb);
}

#[test]
fn floating_input_validation_and_resume_preserve_last_valid_region() {
    let mut fixture = Fixture::new();
    let body = Rect::new(40.0, 40.0, 360.0, 260.0);
    let _fb = fixture.float_with_input(Some(body));
    assert_eq!(
        fixture.state.floating_toplevels[0].input_rect,
        Some(body),
        "promotion copies the declared region"
    );
    let window = Rect::new(0.0, 0.0, 400.0, 300.0);
    for invalid in [
        Rect::new(-1.0, 40.0, 360.0, 260.0),
        Rect::new(40.0, 40.0, f64::INFINITY, 260.0),
        Rect::new(40.0, 40.0, 40.0, 260.0),
    ] {
        assert!(!fixture.state.update_floating_toplevel_rect(
            7,
            "conversation",
            window,
            window,
            Some(invalid)
        ));
        assert_eq!(fixture.state.floating_toplevels[0].input_rect, Some(body));
        assert_eq!(fixture.state.floating_toplevels[0].rect, window);
    }
    assert!(
        !fixture
            .state
            .update_floating_toplevel_rect(99, "conversation", window, window, None)
    );
    assert_eq!(fixture.state.floating_toplevels[0].input_rect, Some(body));
    fixture
        .state
        .suspend_floating_toplevel(7, "conversation", None);
    let tiny = Rect::new(0.0, 0.0, 8.0, 8.0);
    assert!(
        fixture
            .state
            .resume_floating_request(7, "conversation", (tiny, tiny), None, None)
            .is_err()
    );
    assert!(
        fixture.state.pending_floating_toplevels.is_empty(),
        "refused retained geometry cannot authorize a replacement"
    );
    assert!(
        fixture.state.floating_toplevels[0].suspended,
        "invalid resume remains suspended rather than widening input"
    );
    assert_eq!(fixture.state.floating_toplevels[0].input_rect, Some(body));
    assert_eq!(fixture.state.floating_toplevels[0].rect, window);
    assert!(
        !fixture
            .state
            .update_retained_floating_geometry(7, "conversation", tiny, tiny)
    );
    assert_eq!(fixture.state.floating_toplevels[0].input_rect, Some(body));
    assert!(
        fixture
            .state
            .resume_floating_toplevel(
                7,
                "conversation",
                Rect::new(100.0, 80.0, 500.0, 380.0),
                Rect::new(100.0, 80.0, 500.0, 380.0),
                None,
                None
            )
            .is_some()
    );
    assert_eq!(
        fixture.state.floating_toplevels[0].input_rect,
        Some(Rect::new(140.0, 120.0, 460.0, 340.0))
    );
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
            let rect = Rect::new(0.0, 0.0, 400.0, 300.0);
            assert!(
                fixture
                    .state
                    .resume_floating_request_with_focus(
                        7,
                        "conversation",
                        (rect, rect),
                        (None, None),
                        Some((source, owner)),
                    )
                    .expect("valid live geometry")
                    .is_some()
            );
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
fn validated_resume_rearms_focus_after_return_on_the_same_child() {
    let mut fixture = Fixture::new();
    let [source, child, _] = fixture.ids;
    fixture.state.set_client_keyboard_focus(Some(source));
    let mut fb = fixture.float();
    fixture.ready(&mut fb);
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(child));
    fixture
        .state
        .suspend_floating_toplevel(7, "conversation", None);
    fixture
        .state
        .mark_floating_destination_presented(7, "conversation");
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(source));
    let token = fixture.state.floating_toplevels[0].token.clone();
    let owner = fixture
        .state
        .floating_owner_for_surface(source)
        .expect("source owner");
    let rect = Rect::new(0.0, 0.0, 400.0, 300.0);
    assert_eq!(
        fixture.state.resume_floating_request_with_focus(
            7,
            "conversation",
            (rect, rect),
            (None, None),
            Some((source, owner)),
        ),
        Ok(Some(token)),
    );
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(source));
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(source));
    fixture.ready(&mut fb);
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    drop(fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(child));
    assert_eq!(fixture.state.floating_toplevels.len(), 1);
    assert_eq!(fixture.state.floating_toplevels[0].surface_id, child);
}

#[test]
fn rejected_resume_preserves_the_existing_return_focus_intent() {
    let mut fixture = Fixture::new();
    let [source, child, _] = fixture.ids;
    fixture.state.set_client_keyboard_focus(Some(source));
    let body = Rect::new(40.0, 40.0, 360.0, 260.0);
    let mut fb = fixture.float_with_input(Some(body));
    fixture.ready(&mut fb);
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    fixture
        .state
        .suspend_floating_toplevel(7, "conversation", None);
    assert_eq!(fixture.state.floating_return_focus(), Some(child));
    let owner = fixture
        .state
        .floating_owner_for_surface(source)
        .expect("source owner");
    let tiny = Rect::new(0.0, 0.0, 8.0, 8.0);
    assert!(
        fixture
            .state
            .resume_floating_request_with_focus(
                7,
                "conversation",
                (tiny, tiny),
                (None, None),
                Some((source, owner)),
            )
            .is_err()
    );
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(child));
    assert_eq!(fixture.state.floating_return_focus(), Some(child));
    assert!(fixture.state.floating_toplevels[0].suspended);
    assert_eq!(fixture.state.floating_toplevels[0].input_rect, Some(body));
    fixture
        .state
        .mark_floating_destination_presented(7, "conversation");
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    drop(fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(source));
}

#[test]
fn duplicate_live_resume_cannot_resurrect_cancelled_float_focus() {
    let mut fixture = Fixture::new();
    let [source, _, other] = fixture.ids;
    fixture.state.set_client_keyboard_focus(Some(source));
    let mut fb = fixture.float();
    fixture.state.set_client_keyboard_focus(Some(other));
    fixture.state.set_client_keyboard_focus(Some(source));
    let owner = fixture
        .state
        .floating_owner_for_surface(source)
        .expect("source owner");
    let rect = Rect::new(0.0, 0.0, 400.0, 300.0);
    assert!(
        fixture
            .state
            .resume_floating_request_with_focus(
                7,
                "conversation",
                (rect, rect),
                (None, None),
                Some((source, owner)),
            )
            .expect("valid live geometry")
            .is_some()
    );
    fixture.ready(&mut fb);
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    drop(fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(source));
}

#[test]
fn duplicate_live_resume_from_replacement_source_cannot_create_focus_intent() {
    let mut fixture = Fixture::new();
    let [source, child, replacement] = fixture.ids;
    fixture.state.set_client_keyboard_focus(Some(source));
    let mut fb = fixture.float();
    fixture.ready(&mut fb);
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(child));
    fixture.state.retire_floating_focus_source(source);
    assert!(fixture.state.floating_focus_handoffs.is_empty());
    fixture.state.set_client_keyboard_focus(Some(replacement));
    let owner = fixture
        .state
        .floating_owner_for_surface(replacement)
        .expect("authenticated replacement source owner");
    assert_eq!(
        fixture.state.floating_toplevels[0].owner.as_deref(),
        Some(owner.as_ref()),
        "replacement source has the same authenticated app and room owner"
    );
    let rect = Rect::new(0.0, 0.0, 400.0, 300.0);
    assert!(
        fixture
            .state
            .resume_floating_request_with_focus(
                7,
                "conversation",
                (rect, rect),
                (None, None),
                Some((replacement, owner)),
            )
            .expect("valid live geometry")
            .is_some()
    );
    assert!(fixture.state.floating_focus_handoffs.is_empty());
    fixture.ready(&mut fb);
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    drop(fb);
    assert_eq!(
        fixture.state.client_input.keyboard_focus(),
        Some(replacement)
    );
    assert_eq!(fixture.state.floating_toplevels[0].surface_id, child);
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
    fixture.state.input_method_preedit = InputMethodPreeditState::Active;
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
    assert_eq!(fixture.state.floating_preedit_source_for(child), None);

    let mut committed = Fixture::new();
    let [committed_source, committed_child, _] = committed.ids;
    committed
        .state
        .set_client_keyboard_focus(Some(committed_source));
    committed.state.input_method_preedit = InputMethodPreeditState::Active;
    let mut fb = committed.float();
    committed.ready(&mut fb);
    committed.state.input_method_preedit = InputMethodPreeditState::Inactive;
    assert_eq!(
        committed.state.floating_preedit_source_for(committed_child),
        None,
        "commit/disable retires the observation before the seat transfers"
    );
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
    wrong_alias.state.input_method_preedit = InputMethodPreeditState::Active;
    let mut fb = wrong_alias.float();
    wrong_alias.ready(&mut fb);
    wrong_alias.state.floating_toplevels[0].public_id = "other-conversation".into();
    assert_eq!(
        wrong_alias.state.floating_preedit_source_for(wrong_child),
        None,
        "same-process ownership with the wrong public id is not observed"
    );
    wrong_alias
        .state
        .set_client_keyboard_focus(Some(wrong_child));
    assert_eq!(
        wrong_alias.state.client_input.keyboard_focus(),
        Some(wrong_child),
        "same-process ownership cannot substitute for the exact floating id"
    );

    let mut retired = Fixture::new();
    let [retired_source, retired_child, _] = retired.ids;
    retired
        .state
        .set_client_keyboard_focus(Some(retired_source));
    retired.state.input_method_preedit = InputMethodPreeditState::Active;
    let mut fb = retired.float();
    retired.ready(&mut fb);
    assert_eq!(
        retired.state.floating_preedit_source_for(retired_child),
        Some(retired_source)
    );
    let owner = retired.state.floating_toplevels[0].owner.take();
    assert_eq!(
        retired.state.floating_preedit_source_for(retired_child),
        None,
        "a child without the exact authenticated owner is not observed"
    );
    retired.state.floating_toplevels[0].owner = owner;
    retired.state.floating_toplevels[0].suspended = true;
    assert_eq!(
        retired.state.floating_preedit_source_for(retired_child),
        None
    );
    retired.state.floating_toplevels[0].suspended = false;
    retired.state.retire_floating_focus_source(retired_source);
    assert_eq!(
        retired.state.floating_preedit_source_for(retired_child),
        None,
        "destroying the source retires the exact observation"
    );
    retired.state.floating_toplevels.clear();
    assert_eq!(
        retired.state.floating_preedit_source_for(retired_child),
        None,
        "destroying the child cannot leave an observed proxy"
    );
}

#[test]
fn returned_primary_observes_only_its_exact_active_child_preedit() {
    let mut fixture = Fixture::new();
    let [source, child, other] = fixture.ids;
    fixture.state.set_client_keyboard_focus(Some(source));
    let mut fb = fixture.float();
    fixture.ready(&mut fb);
    crate::compositor::client_input::sync_keyboard_focus(&mut fixture.state, &fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(child));
    fixture.state.input_method_preedit = InputMethodPreeditState::Active;
    fixture
        .state
        .suspend_floating_toplevel(7, "conversation", None);
    fixture
        .state
        .mark_floating_destination_presented(7, "conversation");
    assert_eq!(
        fixture.state.floating_preedit_child_for(source),
        Some(child)
    );
    assert_eq!(fixture.state.floating_preedit_child_for(other), None);

    fixture.state.input_method_preedit = InputMethodPreeditState::Inactive;
    assert_eq!(
        fixture.state.floating_preedit_child_for(source),
        None,
        "commit retires the reverse observation immediately"
    );
    fixture.state.input_method_preedit = InputMethodPreeditState::Active;
    fixture.state.retire_floating_focus_source(source);
    assert_eq!(
        fixture.state.floating_preedit_child_for(source),
        None,
        "destroyed or replaced source ownership cannot be observed"
    );
}

#[test]
fn return_during_live_preedit_needs_no_reverse_protocol_focus() {
    let mut fixture = Fixture::new();
    let [source, child, _] = fixture.ids;
    fixture.state.set_client_keyboard_focus(Some(source));
    fixture.state.input_method_preedit = InputMethodPreeditState::Active;
    let mut fb = fixture.float();
    fixture.ready(&mut fb);
    crate::compositor::client_input::sync_keyboard_focus(&mut fixture.state, &fb);
    fixture
        .state
        .suspend_floating_toplevel(7, "conversation", None);
    assert_eq!(
        fixture.state.floating_preedit_source_for(child),
        None,
        "Return/suspension closes the detached continuation observation"
    );
    fixture
        .state
        .mark_floating_destination_presented(7, "conversation");
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(source));
}

#[test]
fn child_preedit_withholds_return_focus_only_until_commit() {
    let mut fixture = Fixture::new();
    let [source, child, _] = fixture.ids;
    fixture.state.set_client_keyboard_focus(Some(source));
    let mut fb = fixture.float();
    fixture.ready(&mut fb);
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    assert_eq!(fixture.state.client_input.keyboard_focus(), Some(child));

    fixture.state.input_method_preedit = InputMethodPreeditState::Active;
    fixture
        .state
        .suspend_floating_toplevel(7, "conversation", None);
    fixture
        .state
        .mark_floating_destination_presented(7, "conversation");
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    assert_eq!(
        fixture.state.client_input.keyboard_focus(),
        Some(child),
        "Return cannot deactivate the child's live composition"
    );

    fixture.state.input_method_preedit = InputMethodPreeditState::Inactive;
    let _ = fixture.state.apply_floating_focus_handoffs(&fb);
    assert_eq!(
        fixture.state.client_input.keyboard_focus(),
        Some(source),
        "the ordinary reverse handoff resumes immediately after commit"
    );
}
