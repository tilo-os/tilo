//! Exercise the installed identity resolver with real Wayland client identities.
use super::*;
use smithay::reexports::wayland_server::Display;
use std::os::unix::net::UnixStream;

struct Fixture {
    display: Display<State>,
    state: State,
    client: Client,
    _peer: UnixStream,
    room: crate::shell::state::RoomId,
}

impl Fixture {
    fn new() -> Self {
        let display = Display::<State>::new().expect("display");
        let event_loop = smithay::reexports::calloop::EventLoop::try_new().expect("event loop");
        let state = State::new(event_loop.handle(), display.handle());
        let room = crate::shell::state::RoomId::new();
        let (client, peer) = Self::insert_client(&display, "so.tilo.chat", room);
        Self {
            display,
            state,
            client,
            _peer: peer,
            room,
        }
    }

    fn insert_client(
        display: &Display<State>,
        package: &str,
        room: crate::shell::state::RoomId,
    ) -> (Client, UnixStream) {
        let (server, peer) = UnixStream::pair().expect("pair");
        let client = display
            .handle()
            .insert_client(
                server,
                ClientState::attributed_arc(
                    crate::shell::launcher::VerifiedLaunchIdentity {
                        package_id: package.into(),
                        workspace_id: Some(room.to_string()),
                        confined: true,
                    },
                    None,
                    42,
                ),
            )
            .expect("actual client");
        (client, peer)
    }

    fn pending(&mut self) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.state
            .authorize_floating_toplevel(PendingFloatingToplevel {
                owner: Some(Arc::new(FloatingToplevelOwner {
                    client: self.client.id(),
                    room: Some(self.room),
                })),
                token: token.clone(),
                connection: 7,
                public_id: "conversation".into(),
                app_id: "so.tilo.chat".into(),
                content_rect: Rect::new(0.0, 0.0, 400.0, 400.0),
                input_rect: None,
                rect: Rect::new(0.0, 0.0, 400.0, 400.0),
                transfer_frame: None,
                transfer_source: None,
                suspended: false,
            });
        format!(
            "{}{token}",
            ui::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX
        )
    }
}

#[test]
fn confined_floating_identity_survives_pending_promotion_and_preserves_enforcement() {
    let mut fixture = Fixture::new();
    let announced = fixture.pending();
    assert!(
        authenticated_toplevel_identity(fixture.client.get_data(), Some(announced.clone()))
            .is_err()
    );
    assert_eq!(
        fixture
            .state
            .resolve_toplevel_identity(9, Some(&fixture.client), Some(announced.clone())),
        Ok((Some(announced.clone()), Some(fixture.room)))
    );
    assert!(fixture.state.promote_floating_toplevel(9, &announced));
    assert!(fixture.state.pending_floating_toplevels.is_empty());
    assert_eq!(fixture.state.floating_toplevels.len(), 1);
    assert!(
        fixture
            .state
            .resolve_toplevel_identity(10, Some(&fixture.client), Some(announced.clone()))
            .is_err()
    );
    fixture.state.mark_floating_toplevel_presented(9);
    assert!(matches!(
        fixture.state.take_floating_toplevel_events().as_slice(),
        [FloatingToplevelEvent::Mapped(_)]
    ));
    assert_eq!(
        fixture
            .state
            .resolve_toplevel_identity(9, Some(&fixture.client), Some(announced.clone())),
        Ok((Some(announced.clone()), Some(fixture.room)))
    );
    assert!(
        fixture
            .state
            .resolve_toplevel_identity(9, Some(&fixture.client), Some("so.tilo.notes".into()))
            .is_err()
    );
    assert!(fixture.state.dismiss_floating_toplevel(7, "conversation"));
    assert!(
        fixture
            .state
            .resolve_toplevel_identity(9, Some(&fixture.client), Some(announced))
            .is_err()
    );
}

#[test]
fn floating_capability_refuses_other_client_package_room_and_unminted_token() {
    let mut fixture = Fixture::new();
    let announced = fixture.pending();
    for (package, room) in [
        ("so.tilo.chat", fixture.room),
        ("so.tilo.notes", fixture.room),
        ("so.tilo.chat", crate::shell::state::RoomId::new()),
    ] {
        let (other, _peer) = Fixture::insert_client(&fixture.display, package, room);
        assert!(
            fixture
                .state
                .resolve_toplevel_identity(9, Some(&other), Some(announced.clone()))
                .is_err()
        );
        let other_owner = FloatingToplevelOwner {
            client: other.id(),
            room: Some(room),
        };
        assert!(
            fixture
                .state
                .validate_floating_resume_owner(7, "conversation", Some(&other_owner))
                .is_err()
        );
    }
    assert_eq!(fixture.state.pending_floating_toplevels.len(), 1);
    assert!(
        fixture
            .state
            .resolve_toplevel_identity(9, None, Some(announced))
            .is_err()
    );
    let unminted = format!(
        "{}{}",
        ui::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX,
        uuid::Uuid::new_v4().simple()
    );
    assert!(
        fixture
            .state
            .resolve_toplevel_identity(9, Some(&fixture.client), Some(unminted))
            .is_err()
    );
    Arc::make_mut(
        fixture.state.pending_floating_toplevels[0]
            .owner
            .as_mut()
            .unwrap(),
    )
    .room = Some(crate::shell::state::RoomId::new());
    let announced = format!(
        "{}{}",
        ui::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX,
        fixture.state.pending_floating_toplevels[0].token
    );
    assert!(
        fixture
            .state
            .resolve_toplevel_identity(9, Some(&fixture.client), Some(announced.clone()))
            .is_err()
    );
    Arc::make_mut(
        fixture.state.pending_floating_toplevels[0]
            .owner
            .as_mut()
            .unwrap(),
    )
    .room = Some(fixture.room);
    fixture.state.pending_floating_toplevels[0].app_id = "so.tilo.notes".into();
    assert!(
        fixture
            .state
            .resolve_toplevel_identity(9, Some(&fixture.client), Some(announced))
            .is_err()
    );
}
