//! The emoji picker, from Super+. to the emoji in a real text field.
//!
//! The shell's overlay host takes the keys, exactly as the compositor
//! routes them; a real `zwp_text_input_v3` client (a toplevel with a text
//! field) receives the result on the wire. Between them runs the same relay
//! the compositor's loop runs every tick, and the same keyboard-focus rule:
//! while the picker is open it owns the keyboard, and the field gets it back
//! when the picker closes. With nobody owning the seat the compositor lends
//! it to itself for one commit; with the on-screen keyboard owning it the
//! commit is delivered in the keyboard's session, which keeps the seat.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard; the counters and a 5 s deadline are far from overflow"
)]

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Instant;

use shell::compositor::input_method::TrustedInputMethod;
use shell::compositor::state::{ClientState, State};
use shell::compositor::text_commit::DeliveredText;
use shell::shell::overlay_host::ShellOverlayHost;
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use wayland_client::protocol::{wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3, zwp_text_input_v3,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_manager_v2, zwp_input_method_v2,
};
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

const W: f32 = 1280.0;
const H: f32 = 800.0;

/// Everything a connection observed.
#[derive(Default)]
struct Peer {
    globals: Vec<(u32, String, u32)>,
    /// What the text field declares whenever it (re-)enables.
    content: Option<(
        zwp_text_input_v3::ContentHint,
        zwp_text_input_v3::ContentPurpose,
    )>,
    entered: u32,
    committed: Vec<Option<String>>,
    done: u32,
    keys: Vec<u32>,
    activated: u32,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Peer {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            state.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Peer {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key { key, .. } = event {
            state.keys.push(key);
        }
    }
}

/// A text field behaves as GTK, Qt and winit do: it enables its text input
/// (declaring its content type) whenever it gains text focus, and disables
/// it when it loses it.
impl Dispatch<zwp_text_input_v3::ZwpTextInputV3, ()> for Peer {
    fn event(
        state: &mut Self,
        text_input: &zwp_text_input_v3::ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_text_input_v3::Event::Enter { .. } => {
                state.entered += 1;
                text_input.enable();
                if let Some((hint, purpose)) = state.content {
                    text_input.set_content_type(hint, purpose);
                }
                text_input.commit();
            }
            zwp_text_input_v3::Event::Leave { .. } => {
                text_input.disable();
                text_input.commit();
            }
            zwp_text_input_v3::Event::CommitString { text } => state.committed.push(text),
            zwp_text_input_v3::Event::Done { .. } => state.done += 1,
            _ => {}
        }
    }
}

impl Dispatch<zwp_input_method_v2::ZwpInputMethodV2, ()> for Peer {
    fn event(
        state: &mut Self,
        _: &zwp_input_method_v2::ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_input_method_v2::Event::Activate = event {
            state.activated += 1;
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Peer {
    fn event(
        _: &mut Self,
        base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for Peer {
    fn event(
        _: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

delegate_noop!(Peer: ignore wl_compositor::WlCompositor);
delegate_noop!(Peer: ignore wl_surface::WlSurface);
delegate_noop!(Peer: ignore wl_seat::WlSeat);
delegate_noop!(Peer: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Peer: ignore zwp_text_input_manager_v3::ZwpTextInputManagerV3);
delegate_noop!(Peer: ignore zwp_input_method_manager_v2::ZwpInputMethodManagerV2);

struct Connected {
    server: smithay::reexports::wayland_server::Client,
    conn: Connection,
    queue: EventQueue<Peer>,
    peer: Peer,
}

impl Connected {
    fn bind<I: Proxy + 'static>(&self, interface: &str, version: u32) -> I
    where
        Peer: Dispatch<I, ()>,
    {
        let (name, _, advertised) = self
            .peer
            .globals
            .iter()
            .find(|(_, name, _)| name == interface)
            .unwrap_or_else(|| panic!("{interface} is not advertised"))
            .clone();
        let registry = self.conn.display().get_registry(&self.queue.handle(), ());
        registry.bind(name, version.min(advertised), &self.queue.handle(), ())
    }
}

/// The objects a text field keeps alive.
pub(crate) struct Field {
    client: usize,
    pub(crate) surface_id: u64,
    objects: (
        wl_surface::WlSurface,
        zwp_text_input_v3::ZwpTextInputV3,
        xdg_surface::XdgSurface,
        xdg_toplevel::XdgToplevel,
        wl_keyboard::WlKeyboard,
    ),
}

impl Field {
    /// The field's text input, for changing what it declares.
    pub(crate) fn text_input(&self) -> &zwp_text_input_v3::ZwpTextInputV3 {
        &self.objects.1
    }
}

/// A compositor with real Wayland clients and the shell's overlay host,
/// driven as the native loop drives them.
pub(crate) struct Harness {
    display: Display<State>,
    pub(crate) state: State,
    clients: Vec<Connected>,
    host: ShellOverlayHost,
    taffy: TaffyTree<()>,
    child: NodeId,
    _event_loop: EventLoop<'static, State>,
}

impl Harness {
    pub(crate) fn new() -> Self {
        let event_loop = EventLoop::try_new().expect("calloop");
        let display = Display::new().expect("display");
        let state = State::new(event_loop.handle(), display.handle());
        let tree = shell::shell::state::WorkspaceTree::bootstrap_default();
        let (mut host, _chrome) = shell::shell::build_tilo_overlay_host_with_chrome(&tree);
        host.set_shortcut_config(settings_store::hardware::ShortcutConfig::default())
            .expect("the built-in shortcut table is valid");
        let mut h = Self {
            display,
            state,
            clients: Vec::new(),
            host,
            taffy: TaffyTree::new(),
            child: NodeId::new(0),
            _event_loop: event_loop,
        };
        h.lay_out();
        h
    }

    fn lay_out(&mut self) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = self.host.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(W),
                        height: Dimension::length(H),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        let mut tr = ui::text_render::TextRenderer::new();
        taffy
            .compute_layout_with_measure(
                root,
                Size {
                    width: AvailableSpace::Definite(W),
                    height: AvailableSpace::Definite(H),
                },
                |known, available, node_id, _, _| {
                    node_map.get(&node_id).map_or(taffy::Size::ZERO, |&ptr| {
                        // SAFETY: the pointers were collected from the live host
                        // tree during this very layout pass.
                        unsafe { (*ptr).measure(known, available, &mut tr) }
                    })
                },
            )
            .expect("compute_layout");
        self.host.apply_layout(&taffy, child, Point::ORIGIN);
        self.taffy = taffy;
        self.child = child;
    }

    fn connect(&mut self, credential: Option<TrustedInputMethod>) -> usize {
        let (server, client) = UnixStream::pair().expect("socket pair");
        let data = credential.map_or_else(
            || Arc::new(ClientState::default()),
            ClientState::input_method_arc,
        );
        let inserted = self
            .display
            .handle()
            .insert_client(server, data)
            .expect("client insert");
        if let Some(kind) = credential {
            self.state
                .register_trusted_input_method_client(&inserted, kind);
        }
        client.set_nonblocking(true).expect("nonblocking");
        let conn = Connection::from_socket(client).expect("client connection");
        let queue = conn.new_event_queue::<Peer>();
        let _registry = conn.display().get_registry(&queue.handle(), ());
        self.clients.push(Connected {
            server: inserted,
            conn,
            queue,
            peer: Peer::default(),
        });
        self.pump();
        self.clients.len() - 1
    }

    pub(crate) fn pump(&mut self) {
        for _ in 0..8 {
            for client in &mut self.clients {
                let _ = client.queue.flush();
            }
            let _ = self.display.dispatch_clients(&mut self.state);
            let _ = self.display.flush_clients();
            for client in &mut self.clients {
                if let Some(guard) = client.conn.prepare_read() {
                    let _ = guard.read();
                }
                let _ = client.queue.dispatch_pending(&mut client.peer);
            }
        }
    }

    /// One compositor tick as the native loop runs it: the keyboard goes to
    /// the field unless the picker holds it, then picks are relayed.
    fn tick(&mut self, field: &Field) -> Vec<DeliveredText> {
        let focus = (!self.host.emoji_picker_open()).then_some(field.surface_id);
        self.state.set_client_keyboard_focus(focus);
        let delivered =
            shell::compositor::text_commit::relay(&mut self.state, &mut self.host, Instant::now());
        self.pump();
        delivered
    }

    /// Tick until something reaches a field, failing after a generous bound.
    fn tick_until_delivered(&mut self, field: &Field) -> Vec<DeliveredText> {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let delivered = self.tick(field);
            if !delivered.is_empty() {
                // Let the field read what the commit sent.
                self.pump();
                return delivered;
            }
            assert!(Instant::now() < deadline, "nothing reached the field");
            std::thread::yield_now();
        }
    }

    fn key(&mut self, logical: Key, mods: ModifiersState) {
        // winit's `KeyEvent` carries a private platform field, so the struct is
        // zero-initialised and every public field overwritten.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical;
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        let event = WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        };
        let modifiers = Modifiers::from(mods);
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT.clone();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        self.host.handle_event(
            &event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: self.child,
                taffy: &self.taffy,
            },
        );
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let key = if c == ' ' {
                Key::Named(NamedKey::Space)
            } else {
                Key::Character(c.to_string().into())
            };
            self.key(key, ModifiersState::empty());
        }
    }

    /// Super+. → `query` → Enter, with a compositor tick while the picker
    /// is open, so it holds the keyboard as it does on a live desktop.
    fn pick(&mut self, field: &Field, query: &str) {
        self.key(Key::Character(".".into()), ModifiersState::SUPER);
        assert!(self.host.emoji_picker_open(), "Super+. opens the picker");
        assert!(self.tick(field).is_empty());
        self.type_text(query);
        self.key(Key::Named(NamedKey::Enter), ModifiersState::empty());
        assert!(!self.host.emoji_picker_open(), "Enter closes it");
    }

    /// Give the seat to the on-screen keyboard: a trusted connection that
    /// binds the input method, which the focused field then activates.
    pub(crate) fn own_seat_with_on_screen_keyboard(
        &mut self,
    ) -> (usize, zwp_input_method_v2::ZwpInputMethodV2) {
        self.state
            .set_input_method_owner(Some(TrustedInputMethod::OnScreenKeyboard));
        let osk = self.connect(Some(TrustedInputMethod::OnScreenKeyboard));
        let input_method = {
            let c = &self.clients[osk];
            let seat = c.bind::<wl_seat::WlSeat>("wl_seat", 9);
            let manager = c.bind::<zwp_input_method_manager_v2::ZwpInputMethodManagerV2>(
                "zwp_input_method_manager_v2",
                1,
            );
            manager.get_input_method(&seat, &c.queue.handle(), ())
        };
        self.pump();
        (osk, input_method)
    }

    /// A focused toplevel with a text field declaring `content`.
    pub(crate) fn field(
        &mut self,
        content: Option<(
            zwp_text_input_v3::ContentHint,
            zwp_text_input_v3::ContentPurpose,
        )>,
    ) -> Field {
        let client = self.connect(None);
        self.clients[client].peer.content = content;
        let objects = {
            let c = &self.clients[client];
            let qh = c.queue.handle();
            let compositor = c.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
            let wm = c.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 3);
            let seat = c.bind::<wl_seat::WlSeat>("wl_seat", 9);
            let keyboard = seat.get_keyboard(&qh, ());
            let manager = c.bind::<zwp_text_input_manager_v3::ZwpTextInputManagerV3>(
                "zwp_text_input_manager_v3",
                1,
            );
            let text_input = manager.get_text_input(&seat, &qh, ());
            let surface = compositor.create_surface(&qh, ());
            let xdg = wm.get_xdg_surface(&surface, &qh, ());
            let top = xdg.get_toplevel(&qh, ());
            top.set_app_id("org.example.Editor".into());
            (surface, text_input, xdg, top, keyboard)
        };
        self.pump();
        let surface_id = foreign_id(&self.state, "org.example.Editor");
        self.state.set_client_keyboard_focus(Some(surface_id));
        self.pump();
        Field {
            client,
            surface_id,
            objects,
        }
    }

    fn peer(&self, field: &Field) -> &Peer {
        &self.clients[field.client].peer
    }
}

fn foreign_id(state: &State, app_id: &str) -> u64 {
    state
        .workspace_tree
        .read()
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find_map(|tab| match &tab.tile {
            shell::shell::state::TileContent::Foreign {
                app_id: actual,
                surface_id,
            } if actual == app_id => Some(*surface_id),
            _ => None,
        })
        .expect("foreign surface was adopted")
}

#[test]
fn super_period_thumbs_enter_commits_through_the_compositor_when_no_input_method_owns_the_seat() {
    let mut h = Harness::new();
    assert_eq!(h.state.input_method_owner_kind(), None);
    let field = h.field(None);
    assert_eq!(
        h.peer(&field).entered,
        0,
        "with no input method the field never hears enter"
    );

    h.pick(&field, "thumbs");
    let delivered = h.tick_until_delivered(&field);
    assert_eq!(
        delivered,
        [DeliveredText {
            text: "👍".into(),
            sensitive: false,
            through: TrustedInputMethod::Compositor,
        }]
    );
    let peer = h.peer(&field);
    assert_eq!(peer.committed, [Some("👍".to_string())]);
    assert!(peer.done >= 1, "the commit is applied by a done");
    assert!(
        peer.keys.is_empty(),
        "no key events are synthesised: {:?}",
        peer.keys
    );
    assert_eq!(h.host.emoji_picker().recents(), ["👍".to_string()]);

    // The lent seat is given back: the compositor's connection is gone and
    // nobody owns the seat.
    for _ in 0..4 {
        h.tick(&field);
    }
    assert_eq!(h.state.input_method_owner_kind(), None);
    let later = h.connect(Some(TrustedInputMethod::OnScreenKeyboard));
    let offers_input_method_manager = h.clients[later]
        .peer
        .globals
        .iter()
        .any(|(_, name, _)| name == "zwp_input_method_manager_v2");
    drop(h);
    assert!(
        !offers_input_method_manager,
        "an idle seat is only ever lent to the compositor"
    );
}

#[test]
fn a_pick_is_committed_in_the_on_screen_keyboards_session_when_it_owns_the_seat() {
    let mut h = Harness::new();
    let (osk, _input_method) = h.own_seat_with_on_screen_keyboard();
    let field = h.field(None);
    assert!(h.peer(&field).entered >= 1);
    assert!(h.clients[osk].peer.activated >= 1, "the field activated it");

    h.pick(&field, "thumbs");
    let delivered = h.tick_until_delivered(&field);
    assert_eq!(
        delivered,
        [DeliveredText {
            text: "👍".into(),
            sensitive: false,
            through: TrustedInputMethod::OnScreenKeyboard,
        }]
    );
    assert_eq!(h.peer(&field).committed, [Some("👍".to_string())]);
    assert!(h.peer(&field).keys.is_empty());
    assert_eq!(
        h.state.input_method_owner_kind(),
        Some(TrustedInputMethod::OnScreenKeyboard),
        "the owner keeps the seat"
    );
    let owner_connected = h
        .display
        .handle()
        .backend_handle()
        .get_client_data(h.clients[osk].server.id())
        .is_ok();
    let owner_activations = h.clients[osk].peer.activated;
    drop(h);
    assert!(owner_connected, "the owner stays connected");
    assert!(
        owner_activations >= 2,
        "the field re-activated the owner when it got the keyboard back"
    );
}

#[test]
fn escape_cancels_and_nothing_is_committed() {
    let mut h = Harness::new();
    let field = h.field(None);
    h.key(Key::Character(".".into()), ModifiersState::SUPER);
    assert!(h.host.emoji_picker_open());
    h.type_text("thumbs");
    h.key(Key::Named(NamedKey::Escape), ModifiersState::empty());
    assert!(!h.host.emoji_picker_open());
    for _ in 0..16 {
        assert!(h.tick(&field).is_empty());
    }
    assert!(h.peer(&field).committed.is_empty());
    assert!(h.host.emoji_picker().recents().is_empty());

    // The chord closes an open picker too, again committing nothing.
    h.key(Key::Character(".".into()), ModifiersState::SUPER);
    h.type_text("pizza");
    h.key(Key::Character(".".into()), ModifiersState::SUPER);
    assert!(!h.host.emoji_picker_open());
    for _ in 0..16 {
        assert!(h.tick(&field).is_empty());
    }
    let committed_after_chord = h.peer(&field).committed.clone();
    drop(h);
    assert!(committed_after_chord.is_empty());
}

#[test]
fn recents_list_the_latest_pick_first() {
    let mut h = Harness::new();
    let field = h.field(None);
    h.pick(&field, "thumbs");
    h.tick_until_delivered(&field);
    h.pick(&field, "pizza");
    h.tick_until_delivered(&field);
    assert_eq!(
        h.peer(&field).committed,
        [Some("👍".to_string()), Some("🍕".to_string())]
    );
    assert_eq!(
        h.host.emoji_picker().recents(),
        ["🍕".to_string(), "👍".to_string()]
    );
    h.key(Key::Character(".".into()), ModifiersState::SUPER);
    let items = h.host.emoji_picker().items().to_vec();
    let selected = h.host.emoji_picker().selected().map(str::to_owned);
    drop(h);
    assert_eq!(&items[..2], ["🍕".to_string(), "👍".to_string()]);
    assert_eq!(selected.as_deref(), Some("🍕"));
}

#[test]
fn a_password_field_gets_the_emoji_but_it_is_not_remembered() {
    let mut h = Harness::new();
    let field = h.field(Some((
        zwp_text_input_v3::ContentHint::HiddenText | zwp_text_input_v3::ContentHint::SensitiveData,
        zwp_text_input_v3::ContentPurpose::Password,
    )));
    h.pick(&field, "thumbs");
    let delivered = h.tick_until_delivered(&field);
    assert_eq!(delivered.len(), 1);
    assert!(delivered[0].sensitive);
    let committed = h.peer(&field).committed.clone();
    let recents = h.host.emoji_picker().recents().to_vec();
    drop(h);
    assert_eq!(committed, [Some("👍".to_string())]);
    assert!(
        recents.is_empty(),
        "nothing typed into a secret field is remembered"
    );
}

#[test]
fn the_picker_opens_at_the_caret_and_centred_when_the_field_reports_none() {
    use shell::compositor::foreign::store;
    use vello::kurbo::Rect;

    let mut h = Harness::new();
    // A field enables its text input, and reports its caret, only while an
    // input method is there to hear it.
    let _keyboard_session = h.own_seat_with_on_screen_keyboard();
    let field = h.field(None);
    assert!(h.state.text_input_active(), "the field takes text");
    // Where the window is laid out on the desktop.
    let window = Rect::new(200.0, 150.0, 1000.0, 650.0);
    store().note_placement(field.surface_id, window);
    let screen = Rect::new(0.0, 0.0, f64::from(W), f64::from(H));

    // The field reports its caret in surface coordinates, as toolkits do on
    // every enable and whenever the caret moves.
    let open_with_caret = |h: &mut Harness, caret: Option<(i32, i32)>| -> Rect {
        let text_input = field.text_input();
        if let Some((x, y)) = caret {
            text_input.set_cursor_rectangle(x, y, 2, 18);
        } else {
            // Re-enabling resets what the field declared.
            text_input.disable();
            text_input.commit();
            text_input.enable();
        }
        text_input.commit();
        h.pump();
        assert!(h.tick(&field).is_empty());
        h.key(Key::Character(".".into()), ModifiersState::SUPER);
        assert!(h.host.emoji_picker_open());
        let card = h.host.emoji_picker().card_rect();
        assert!(card.area() > 0.0);
        assert!(
            card.x0 >= screen.x0
                && card.y0 >= screen.y0
                && card.x1 <= screen.x1
                && card.y1 <= screen.y1,
            "the card stays on the display: {card:?}"
        );
        h.key(Key::Named(NamedKey::Escape), ModifiersState::empty());
        assert!(!h.host.emoji_picker_open());
        card
    };

    // Phase `below_the_caret`.
    let caret = Rect::new(320.0, 190.0, 322.0, 208.0);
    let card = open_with_caret(&mut h, Some((120, 40)));
    assert!(
        (card.x0 - caret.x0).abs() < 0.5,
        "the card starts at the caret: {card:?}"
    );
    assert!(
        card.y0 >= caret.y1 && card.y0 <= caret.y1 + 24.0,
        "the card opens just below the caret: {card:?}"
    );

    // Phase `above_a_caret_near_the_bottom`.
    let caret = Rect::new(320.0, 630.0, 322.0, 648.0);
    let card = open_with_caret(&mut h, Some((120, 480)));
    assert!(
        card.y1 <= caret.y0 && card.y1 >= caret.y0 - 24.0,
        "the card flips above the caret when the display ends first: {card:?}"
    );

    // Phase `clamped_at_the_right_edge`.
    let caret = Rect::new(990.0, 190.0, 992.0, 208.0);
    let card = open_with_caret(&mut h, Some((790, 40)));
    assert!(
        card.x0 < caret.x0 && card.x1 < screen.x1,
        "the card is pulled back onto the display: {card:?}"
    );
    assert!(card.y0 >= caret.y1);

    // Phase `centred_without_a_caret`.
    let card = open_with_caret(&mut h, None);
    drop(h);
    assert!(
        (card.center().x - screen.center().x).abs() < 0.5,
        "a field that reports no caret gets the card centred: {card:?}"
    );

    store().forget(field.surface_id);
}
