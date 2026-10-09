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

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Instant;

use shell::compositor::input_method::TrustedInputMethod;
use shell::compositor::state::{ClientState, State};
use shell::compositor::text_commit::DeliveredText;
use shell::shell::overlay_host::ShellOverlayHost;
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use taffy::{NodeId, TaffyTree};
use wayland_client::protocol::{wl_compositor, wl_keyboard, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3, zwp_text_input_v3,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_manager_v2, zwp_input_method_v2,
};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::wayland::private_text_input_protocol::{
    tilo_text_input_manager_v1, tilo_text_input_v1,
};

use crate::support::host::{Layout, lay_out, press_key};
use crate::support::wayland::{Globals as _, foreign_id};

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
    private_operations: Vec<(u32, u32)>,
}

impl Dispatch<tilo_text_input_v1::TiloTextInputV1, ()> for Peer {
    fn event(
        peer: &mut Self,
        _: &tilo_text_input_v1::TiloTextInputV1,
        event: tilo_text_input_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let tilo_text_input_v1::Event::Operation { serial, kind, .. } = event else {
            panic!("unexpected private text input event");
        };
        peer.private_operations.push((serial, u32::from(kind)));
    }
}
delegate_noop!(Peer: ignore tilo_text_input_manager_v1::TiloTextInputManagerV1);

crate::support::wayland::test_client!(Peer);

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
        let (name, version) = self
            .peer
            .global(interface, version)
            .unwrap_or_else(|| panic!("{interface} is not advertised"));
        let registry = self.conn.display().get_registry(&self.queue.handle(), ());
        registry.bind(name, version, &self.queue.handle(), ())
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
        let Layout { taffy, child_node } = lay_out(&mut host, W, H);
        Self {
            display,
            state,
            clients: Vec::new(),
            host,
            taffy,
            child: child_node,
            _event_loop: event_loop,
        }
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
        press_key(&mut self.host, &self.taffy, self.child, logical, mods);
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
        let app_id = format!("org.example.Editor.{client}");
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
            top.set_app_id(app_id.clone());
            (surface, text_input, xdg, top, keyboard)
        };
        self.pump();
        let surface_id = foreign_id(&self.state, &app_id);
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

#[test]
fn dictation_streams_and_corrects_without_editing_until_one_utf8_commit() {
    assert_dictation_foreign_commit(&["wrong hypothesis".into()], "Bonjour, 世界 👋");
}

#[test]
fn dictation_paragraphs_reach_an_explicit_multiline_field_once_without_keys() {
    let mut h = Harness::new();
    let (_osk, _input_method) = h.own_seat_with_on_screen_keyboard();
    let field = h.field(Some((
        zwp_text_input_v3::ContentHint::Multiline,
        zwp_text_input_v3::ContentPurpose::Normal,
    )));
    let now = Instant::now();
    let id = h.state.begin_dictation(now).unwrap();
    h.state
        .update_dictation(id, "First draft.\nAnother draft.".into(), now)
        .unwrap();
    h.pump();
    assert!(h.peer(&field).committed.is_empty());
    let final_text = "First paragraph.\n\nDeuxième paragraphe. 世界 👋";
    h.state
        .update_dictation(id, final_text.into(), now)
        .unwrap();
    h.state.finish_dictation(id, now).unwrap();
    h.pump();
    assert_eq!(h.peer(&field).committed, [Some(final_text.into())]);
    assert!(h.peer(&field).keys.is_empty());
    assert!(h.state.finish_dictation(id, now).is_err());
    drop(h);
}

#[test]
fn dictation_paragraphs_cannot_survive_multiline_policy_changes_or_enter_a_terminal() {
    use shell::compositor::dictation::DictationError;
    let mut h = Harness::new();
    let (_osk, _input_method) = h.own_seat_with_on_screen_keyboard();
    let now = Instant::now();
    let field = h.field(Some((
        zwp_text_input_v3::ContentHint::Multiline,
        zwp_text_input_v3::ContentPurpose::Normal,
    )));
    let id = h.state.begin_dictation(now).unwrap();
    h.state
        .update_dictation(id, "First.\nSecond.".into(), now)
        .unwrap();
    field.text_input().set_content_type(
        zwp_text_input_v3::ContentHint::empty(),
        zwp_text_input_v3::ContentPurpose::Normal,
    );
    field.text_input().commit();
    h.pump();
    assert_eq!(
        h.state.finish_dictation(id, now),
        Err(DictationError::TargetChanged)
    );
    assert!(h.peer(&field).committed.is_empty());
    let terminal = h.field(Some((
        zwp_text_input_v3::ContentHint::Multiline,
        zwp_text_input_v3::ContentPurpose::Terminal,
    )));
    assert_eq!(
        h.state.begin_dictation(now),
        Err(DictationError::UnsupportedField)
    );
    assert!(h.peer(&terminal).committed.is_empty());
    assert!(h.peer(&terminal).keys.is_empty());
    drop(h);
}

#[test]
fn public_aot_transcripts_reach_foreign_fields_without_committing_hallucinated_drafts() {
    for row in super::input_method_wire::public_aot_transcripts() {
        assert_dictation_foreign_commit(&row.drafts(), &row.raw_final);
    }
}

#[test]
fn public_image_transcripts_reach_foreign_fields_without_committing_drafts() {
    for row in super::input_method_wire::public_image_transcripts() {
        assert_dictation_foreign_commit(&row.drafts(), &row.raw_final);
    }
}

#[test]
fn public_image_transcripts_full_session_reaches_foreign_fields_once() {
    for row in super::input_method_wire::public_long_image_transcripts() {
        assert_dictation_foreign_commit(&row.drafts(), &row.raw_final);
    }
}

#[test]
fn public_image_transcripts_cannot_cross_focus_changes_or_canceled_session_boundaries() {
    for row in super::input_method_wire::public_image_transcripts() {
        let mut h = Harness::new();
        let (_osk, _input_method) = h.own_seat_with_on_screen_keyboard();
        let first = h.field(None);
        let now = Instant::now();
        let original = h.state.begin_dictation(now).unwrap();
        for draft in row.drafts() {
            h.state.update_dictation(original, draft, now).unwrap();
        }
        let second = h.field(None);
        h.state.set_client_keyboard_focus(Some(first.surface_id));
        h.pump();
        assert!(
            h.state
                .update_dictation(original, row.raw_final.clone(), now)
                .is_err()
        );
        assert!(h.state.finish_dictation(original, now).is_err());
        let canceled = h.state.begin_dictation(now).unwrap();
        h.state
            .update_dictation(canceled, row.raw_final.clone(), now)
            .unwrap();
        assert!(h.state.cancel_dictation(canceled));
        let current = h.state.begin_dictation(now).unwrap();
        assert!(
            h.state
                .update_dictation(canceled, row.raw_final.clone(), now)
                .is_err()
        );
        assert!(!h.state.cancel_dictation(canceled));
        h.state
            .update_dictation(current, row.raw_final.clone(), now)
            .unwrap();
        h.state.finish_dictation(current, now).unwrap();
        h.pump();
        assert_eq!(h.peer(&first).committed, [Some(row.raw_final)]);
        assert!(h.peer(&second).committed.is_empty());
        assert!(h.peer(&first).keys.is_empty() && h.peer(&second).keys.is_empty());
        drop(h);
    }
}

fn assert_dictation_foreign_commit(drafts: &[String], final_text: &str) {
    let mut h = Harness::new();
    let (_osk, _input_method) = h.own_seat_with_on_screen_keyboard();
    let field = h.field(None);
    let now = Instant::now();
    let id = h
        .state
        .begin_dictation(now)
        .expect("ordinary editable field");
    for draft in drafts {
        h.state.update_dictation(id, draft.clone(), now).unwrap();
        h.pump();
        assert!(h.peer(&field).committed.is_empty(), "draft is not an edit");
    }
    h.state
        .update_dictation(id, final_text.into(), now)
        .unwrap();
    h.state.finish_dictation(id, now).unwrap();
    h.pump();
    assert_eq!(h.peer(&field).committed, [Some(final_text.into())]);
    assert!(h.peer(&field).keys.is_empty(), "no synthetic key events");
    assert!(h.state.take_delivered_text().is_empty(), "no emoji recents");
    assert!(
        h.state.finish_dictation(id, now).is_err(),
        "no double insertion"
    );
    assert_eq!(
        h.state.input_method_owner_kind(),
        Some(TrustedInputMethod::OnScreenKeyboard)
    );
    drop(h);
}

#[test]
fn dictation_cancellation_and_late_callbacks_cannot_touch_a_new_session() {
    use shell::compositor::dictation::DictationError;
    let mut h = Harness::new();
    let (_osk, _input_method) = h.own_seat_with_on_screen_keyboard();
    let field = h.field(None);
    let now = Instant::now();
    let old = h.state.begin_dictation(now).unwrap();
    h.state
        .update_dictation(old, "discard me".into(), now)
        .unwrap();
    assert_eq!(h.state.begin_dictation(now), Err(DictationError::Busy));
    assert!(h.state.cancel_dictation(old));
    let new = h.state.begin_dictation(now).unwrap();
    assert_ne!(old, new);
    assert!(!h.state.cancel_dictation(old));
    assert_eq!(
        h.state.update_dictation(old, "late".into(), now),
        Err(DictationError::StaleSession)
    );
    h.state
        .update_dictation(new, "keep me".into(), now)
        .unwrap();
    h.state.finish_dictation(new, now).unwrap();
    h.pump();
    assert_eq!(h.peer(&field).committed, [Some("keep me".into())]);
    drop(h);
}

#[test]
fn dictation_focus_away_and_back_never_revives_the_old_target() {
    let mut h = Harness::new();
    let (_osk, _input_method) = h.own_seat_with_on_screen_keyboard();
    let first = h.field(None);
    let now = Instant::now();
    let id = h.state.begin_dictation(now).unwrap();
    h.state
        .update_dictation(id, "private draft".into(), now)
        .unwrap();
    let second = h.field(None);
    assert_ne!(first.surface_id, second.surface_id);
    h.state.set_client_keyboard_focus(Some(first.surface_id));
    h.pump();
    assert!(h.state.finish_dictation(id, now).is_err());
    h.pump();
    assert!(h.peer(&first).committed.is_empty());
    assert!(h.peer(&second).committed.is_empty());
    drop(h);
}

#[test]
fn dictation_rejects_field_and_caret_changes_within_the_same_surface() {
    use shell::compositor::dictation::DictationError;
    let mut h = Harness::new();
    let (_osk, _input_method) = h.own_seat_with_on_screen_keyboard();
    let field = h.field(None);
    let now = Instant::now();
    let id = h.state.begin_dictation(now).unwrap();
    h.state
        .update_dictation(id, "stale edit position".into(), now)
        .unwrap();
    field
        .text_input()
        .set_surrounding_text("typed while recognizing".into(), 5, 5);
    field.text_input().commit();
    h.pump();
    assert_eq!(
        h.state.finish_dictation(id, now),
        Err(DictationError::TargetChanged)
    );
    let id = h.state.begin_dictation(now).unwrap();
    field.text_input().disable();
    field.text_input().commit();
    field.text_input().enable();
    field.text_input().commit();
    h.pump();
    assert_eq!(
        h.state.finish_dictation(id, now),
        Err(DictationError::TargetChanged)
    );
    assert!(h.peer(&field).committed.is_empty());
    drop(h);
}

#[test]
fn dictation_rejects_secret_fields_and_an_unavailable_input_method() {
    use shell::compositor::dictation::DictationError;
    let mut h = Harness::new();
    let now = Instant::now();
    assert_eq!(
        h.state.begin_dictation(now),
        Err(DictationError::InputMethodUnavailable)
    );
    let (_osk, _input_method) = h.own_seat_with_on_screen_keyboard();
    for (hint, purpose) in [
        (
            zwp_text_input_v3::ContentHint::empty(),
            zwp_text_input_v3::ContentPurpose::Password,
        ),
        (
            zwp_text_input_v3::ContentHint::SensitiveData,
            zwp_text_input_v3::ContentPurpose::Normal,
        ),
    ] {
        let field = h.field(Some((hint, purpose)));
        assert_eq!(
            h.state.begin_dictation(now),
            Err(DictationError::SensitiveField)
        );
        assert!(h.peer(&field).committed.is_empty());
    }
    drop(h);
}

#[test]
fn dictation_terminal_purpose_requires_a_qualified_inner_editing_target() {
    use shell::compositor::dictation::DictationError;
    let mut h = Harness::new();
    let (_osk, _input_method) = h.own_seat_with_on_screen_keyboard();
    let now = Instant::now();
    for hint in [
        zwp_text_input_v3::ContentHint::empty(),
        zwp_text_input_v3::ContentHint::HiddenText,
    ] {
        let field = h.field(Some((hint, zwp_text_input_v3::ContentPurpose::Terminal)));
        let expected = if hint.is_empty() {
            DictationError::UnsupportedField
        } else {
            DictationError::SensitiveField
        };
        for _ in 0..2 {
            assert_eq!(h.state.prepare_dictation(now), Err(expected));
            assert_eq!(h.state.begin_dictation(now), Err(expected));
        }
        h.pump();
        assert!(h.peer(&field).committed.is_empty());
    }
    let editable = h.field(None);
    let id = h.state.begin_dictation(now).unwrap();
    h.state
        .update_dictation(id, "editable successor".into(), now)
        .unwrap();
    h.state.finish_dictation(id, now).unwrap();
    h.pump();
    assert_eq!(
        h.peer(&editable).committed,
        [Some("editable successor".into())]
    );
    drop(h);
}

#[test]
fn dictation_editable_to_terminal_transition_cancels_and_does_not_revive() {
    use shell::compositor::dictation::DictationError;
    let mut h = Harness::new();
    let (_osk, _input_method) = h.own_seat_with_on_screen_keyboard();
    let field = h.field(None);
    let now = Instant::now();
    let old = h.state.begin_dictation(now).unwrap();
    h.state
        .update_dictation(old, "must not reach process input".into(), now)
        .unwrap();
    field.text_input().set_content_type(
        zwp_text_input_v3::ContentHint::empty(),
        zwp_text_input_v3::ContentPurpose::Terminal,
    );
    field.text_input().commit();
    h.pump();
    assert_eq!(
        h.state.check_dictation(old, now),
        Err(DictationError::UnsupportedField)
    );
    assert_eq!(
        h.state.finish_dictation(old, now),
        Err(DictationError::StaleSession)
    );
    field.text_input().set_content_type(
        zwp_text_input_v3::ContentHint::empty(),
        zwp_text_input_v3::ContentPurpose::Normal,
    );
    field.text_input().commit();
    h.pump();
    let next = h.state.begin_dictation(now).unwrap();
    assert_ne!(old, next);
    assert!(!h.state.cancel_dictation(old));
    assert_eq!(
        h.state.update_dictation(old, "late callback".into(), now),
        Err(DictationError::StaleSession)
    );
    assert!(h.state.cancel_dictation(next));
    let final_id = h.state.begin_dictation(now).unwrap();
    h.state
        .update_dictation(final_id, "fresh edit".into(), now)
        .unwrap();
    h.state.finish_dictation(final_id, now).unwrap();
    h.pump();
    assert_eq!(h.peer(&field).committed, [Some("fresh edit".into())]);
    drop(h);
}

#[test]
fn dictation_idle_terminal_activation_refuses_before_recording() {
    use shell::compositor::dictation::DictationError;
    let mut h = Harness::new();
    let field = h.field(Some((
        zwp_text_input_v3::ContentHint::empty(),
        zwp_text_input_v3::ContentPurpose::Terminal,
    )));
    let id = h.state.prepare_dictation(Instant::now()).unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        match h.state.advance_dictation_preparation(id, Instant::now()) {
            Ok(false) => h.pump(),
            Err(DictationError::UnsupportedField) => break,
            other => panic!("unexpected terminal activation result: {other:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "terminal purpose was not identified"
        );
    }
    assert_eq!(
        h.state.finish_dictation(id, Instant::now()),
        Err(DictationError::StaleSession)
    );
    h.pump();
    assert!(h.peer(&field).committed.is_empty());
    drop(h);
}

#[test]
fn dictation_private_warm_ack_does_not_mask_a_changed_targets_refusal() {
    use shell::compositor::dictation::DictationError;
    let mut h = Harness::new();
    let (_osk, _input_method) = h.own_seat_with_on_screen_keyboard();
    for (hint, purpose, expected) in [
        (
            zwp_text_input_v3::ContentHint::empty(),
            zwp_text_input_v3::ContentPurpose::Terminal,
            DictationError::UnsupportedField,
        ),
        (
            zwp_text_input_v3::ContentHint::HiddenText,
            zwp_text_input_v3::ContentPurpose::Password,
            DictationError::SensitiveField,
        ),
    ] {
        let field = h.field(None);
        let client = &h.clients[field.client];
        let manager = client.bind::<tilo_text_input_manager_v1::TiloTextInputManagerV1>(
            "tilo_text_input_manager_v1",
            1,
        );
        let private = manager.get_field(&field.objects.0, &client.queue.handle(), ());
        private.update(vec![7; 32]);
        h.pump();
        let now = Instant::now();
        let id = h.state.prepare_dictation(now).unwrap();
        h.pump();
        let (serial, kind) = *h.peer(&field).private_operations.last().unwrap();
        assert_eq!(kind, u32::from(tilo_text_input_v1::Kind::Prepare));
        field.text_input().set_content_type(hint, purpose);
        field.text_input().commit();
        private.acknowledge(serial, tilo_text_input_v1::Outcome::Accepted);
        h.pump();
        assert_eq!(
            h.state.advance_dictation_preparation(id, now),
            Err(expected)
        );
        assert_eq!(
            h.state.finish_dictation(id, now),
            Err(DictationError::StaleSession)
        );
        h.pump();
        assert!(h.peer(&field).committed.is_empty());
        assert!(h.peer(&field).private_operations.iter().all(|(_, kind)| {
            *kind != u32::from(tilo_text_input_v1::Kind::Start)
                && *kind != u32::from(tilo_text_input_v1::Kind::Final)
        }));
    }
    drop(h);
}

#[test]
fn dictation_bounds_silence_model_output_and_terminal_control_characters() {
    use shell::compositor::dictation::{DictationError, MAX_DRAFT_BYTES, SESSION_WINDOW};
    let mut h = Harness::new();
    let (_osk, _input_method) = h.own_seat_with_on_screen_keyboard();
    let field = h.field(None);
    let now = Instant::now();
    let id = h.state.begin_dictation(now).unwrap();
    h.state.finish_dictation(id, now).unwrap();
    for draft in ["command\n", "command\r", "escape\u{1b}", "nul\0"] {
        let id = h.state.begin_dictation(now).unwrap();
        assert_eq!(
            h.state.update_dictation(id, draft.into(), now),
            Err(DictationError::ControlCharacter)
        );
        assert!(h.state.finish_dictation(id, now).is_err());
    }
    let id = h.state.begin_dictation(now).unwrap();
    assert_eq!(
        h.state
            .update_dictation(id, "x".repeat(MAX_DRAFT_BYTES + 1), now),
        Err(DictationError::DraftTooLarge)
    );
    let id = h.state.begin_dictation(now).unwrap();
    h.state
        .update_dictation(id, "too late".into(), now)
        .unwrap();
    assert_eq!(
        h.state.check_dictation(id, now + SESSION_WINDOW),
        Err(DictationError::TimedOut)
    );
    assert!(h.state.finish_dictation(id, now).is_err());
    h.pump();
    assert!(h.peer(&field).committed.is_empty());
    drop(h);
}

#[test]
fn dictation_rechecks_secret_content_and_input_method_ownership() {
    use shell::compositor::dictation::DictationError;
    let mut h = Harness::new();
    let (_osk, _input_method) = h.own_seat_with_on_screen_keyboard();
    let field = h.field(None);
    let now = Instant::now();
    let id = h.state.begin_dictation(now).unwrap();
    h.state
        .update_dictation(id, "ordinary draft".into(), now)
        .unwrap();
    field.text_input().set_content_type(
        zwp_text_input_v3::ContentHint::HiddenText,
        zwp_text_input_v3::ContentPurpose::Password,
    );
    field.text_input().commit();
    h.pump();
    assert_eq!(
        h.state.check_dictation(id, now),
        Err(DictationError::SensitiveField)
    );
    assert!(h.state.finish_dictation(id, now).is_err());
    field.text_input().set_content_type(
        zwp_text_input_v3::ContentHint::empty(),
        zwp_text_input_v3::ContentPurpose::Normal,
    );
    field.text_input().commit();
    h.pump();
    let id = h.state.begin_dictation(now).unwrap();
    h.state
        .update_dictation(id, "owner went away".into(), now)
        .unwrap();
    h.state.set_input_method_owner(None);
    h.state
        .set_input_method_owner(Some(TrustedInputMethod::OnScreenKeyboard));
    assert_eq!(
        h.state.finish_dictation(id, now),
        Err(DictationError::StaleSession)
    );
    h.pump();
    assert!(h.peer(&field).committed.is_empty());
    drop(h);
}

#[test]
fn dictation_prepares_an_idle_seat_and_releases_it_after_insertion() {
    let mut h = Harness::new();
    let field = h.field(None);
    let now = Instant::now();
    let id = h.state.prepare_dictation(now).unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while !h
        .state
        .advance_dictation_preparation(id, Instant::now())
        .unwrap()
    {
        h.pump();
        assert!(Instant::now() < deadline, "idle field did not activate");
    }
    h.state
        .update_dictation(id, "Native dictation 👋".into(), Instant::now())
        .unwrap();
    h.state.finish_dictation(id, Instant::now()).unwrap();
    h.pump();
    assert_eq!(
        h.peer(&field).committed,
        [Some("Native dictation 👋".into())]
    );
    assert!(h.peer(&field).keys.is_empty());
    assert_eq!(h.state.input_method_owner_kind(), None);
    // The next request must establish its own trusted connection.
    let next = h.state.prepare_dictation(Instant::now()).unwrap();
    assert_ne!(id, next);
    assert!(h.state.cancel_dictation(next));
    drop(h);
}

#[test]
fn dictation_preparation_is_cancellable_and_late_ticks_do_not_retarget() {
    use shell::compositor::dictation::DictationError;
    let mut h = Harness::new();
    let first = h.field(None);
    let now = Instant::now();
    let id = h.state.prepare_dictation(now).unwrap();
    let second = h.field(None);
    h.state.set_client_keyboard_focus(Some(first.surface_id));
    h.pump();
    assert_eq!(
        h.state.advance_dictation_preparation(id, now),
        Err(DictationError::StaleSession)
    );
    let next = h.state.prepare_dictation(now).unwrap();
    assert!(!h.state.cancel_dictation(id));
    assert!(h.state.cancel_dictation(next));
    assert_eq!(
        h.state.advance_dictation_preparation(next, now),
        Err(DictationError::StaleSession)
    );
    assert!(h.peer(&first).committed.is_empty());
    assert!(h.peer(&second).committed.is_empty());
    drop(h);
}

#[test]
fn dictation_keeps_its_target_across_unchanged_field_updates() {
    let mut h = Harness::new();
    let (_osk, _input_method) = h.own_seat_with_on_screen_keyboard();
    let field = h.field(None);
    field.text_input().set_cursor_rectangle(5, 10, 1, 20);
    field.text_input().commit();
    h.pump();
    let now = Instant::now();
    let id = h.state.begin_dictation(now).unwrap();
    field.text_input().set_cursor_rectangle(5, 10, 1, 20);
    field.text_input().commit();
    field.text_input().commit();
    h.pump();
    h.state
        .update_dictation(id, "Still the same field.".into(), now)
        .unwrap();
    h.state.finish_dictation(id, now).unwrap();
    h.pump();
    assert_eq!(
        h.peer(&field).committed,
        [Some("Still the same field.".into())]
    );
    drop(h);
}

#[test]
fn dictation_idle_activation_rejects_secrets_and_bounds_preparation_time() {
    use shell::compositor::dictation::DictationError;
    use shell::compositor::text_commit::DELIVERY_WINDOW;
    let mut h = Harness::new();
    let field = h.field(Some((
        zwp_text_input_v3::ContentHint::HiddenText,
        zwp_text_input_v3::ContentPurpose::Password,
    )));
    let id = h.state.prepare_dictation(Instant::now()).unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        match h.state.advance_dictation_preparation(id, Instant::now()) {
            Ok(false) => h.pump(),
            Err(DictationError::SensitiveField) => break,
            other => panic!("unexpected secret-field activation result: {other:?}"),
        }
        assert!(Instant::now() < deadline, "secret field was not identified");
    }
    assert!(h.state.finish_dictation(id, Instant::now()).is_err());
    let now = Instant::now();
    let id = h.state.prepare_dictation(now).unwrap();
    assert_eq!(
        h.state
            .advance_dictation_preparation(id, now + DELIVERY_WINDOW),
        Err(DictationError::TimedOut)
    );
    assert!(h.state.finish_dictation(id, now).is_err());
    assert!(h.peer(&field).committed.is_empty());
    drop(h);
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
