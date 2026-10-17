//! Text the shell commits into the focused field on the user's behalf — a
//! picked emoji, a clipboard history item chosen to paste — delivered the
//! way an input method delivers it.
//!
//! There is exactly one way text reaches a field without being typed:
//! `zwp_input_method_v2.commit_string`, which the compositor turns into the
//! field's `zwp_text_input_v3.commit_string` + `done`. Nothing here
//! synthesises key events. Who performs the commit follows the seat's
//! input-method arbitration ([`super::input_method`]):
//!
//! * **A trusted input method owns the seat** (the on-screen keyboard, or
//!   the composer). The commit is delivered inside that owner's session: on
//!   the text input its activation enabled, with the session's own serial.
//!   The owner keeps the seat and its connection; nothing about its state is
//!   disturbed, and it reads the result back from the field's next
//!   surrounding-text update like any other edit.
//! * **Nobody owns the seat.** The arbiter lends the idle seat to the
//!   compositor for one commit: an in-process connection with the
//!   [`TrustedInputMethod::Compositor`] credential binds the input method,
//!   waits for the field to activate it, commits the text, and destroys the
//!   input method again. Only then does a field that has no input method
//!   hear `enter` at all, which is why this cannot be a direct write.
//!
//! The shell's picker takes keyboard focus while it is open, so the field
//! gets it back — and re-enables its text input — only after the pick. A
//! request therefore waits, bounded by [`DELIVERY_WINDOW`], for the focused
//! field to be enabled again.
//!
//! The shell also follows where each field's caret is
//! (`set_cursor_rectangle`), so the picker opens beside it
//! ([`State::focused_text_caret`]).
//!
//! Every request reports whether the field it reached was sensitive
//! (a password, a PIN, or hidden/sensitive content hints), from the content
//! type the field declared. Emoji are content and are committed into such a
//! field as well; the caller only uses the flag to keep them out of recents.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    zwp_text_input_v3::{self, ZwpTextInputV3},
};
use smithay::reexports::wayland_server::backend::{ClientId, DisconnectReason, ObjectId};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, Resource, WEnum, delegate_dispatch,
    delegate_global_dispatch,
};
use smithay::wayland::text_input::{TextInputManagerState, TextInputSeat, TextInputUserData};
use wayland_client::protocol::{wl_callback, wl_registry, wl_seat};
use wayland_client::{Connection, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_manager_v2, zwp_input_method_v2,
};

use super::input_method::TrustedInputMethod;
use super::state::{ClientState, State};

/// How long a request waits for the focused field to take text.
pub const DELIVERY_WINDOW: Duration = Duration::from_secs(2);

/// `zwp_text_input_v3.content_purpose` values whose text is a secret. Digits
/// count: the on-screen keyboard treats a digits field as a PIN pad too.
const SECRET_PURPOSES: [u32; 3] = [2, 8, 9];
/// `zwp_text_input_v3.content_hint` bits `hidden_text` and `sensitive_data`.
const SECRET_HINTS: u32 = 0x40 | 0x80;

/// A field's declared content type, as raw protocol values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContentType {
    pub hint: u32,
    pub purpose: u32,
}

impl ContentType {
    /// Whether text in this field must not be remembered anywhere else.
    pub fn is_sensitive(self) -> bool {
        SECRET_PURPOSES.contains(&self.purpose) || self.hint & SECRET_HINTS != 0
    }
}

/// A caret in the field's surface coordinates: `x`, `y`, width, height.
type CursorRectangle = (i32, i32, i32, i32);

#[derive(Clone, Copy, Debug, Default)]
struct FieldState {
    pending_enable: bool,
    protocol_activation: Option<bool>,
    pending: Option<ContentType>,
    committed: ContentType,
    pending_cursor: Option<CursorRectangle>,
    cursor: Option<CursorRectangle>,
    revision: u64,
    pending_edit: bool,
}

/// The content type and caret every text input has committed, tracked beside
/// Smithay's own text-input state (which forwards them to an input method but
/// does not keep them).
#[derive(Debug, Default)]
pub struct TextFields {
    fields: HashMap<ObjectId, FieldState>,
    active_protocol: Option<ObjectId>,
}

impl TextFields {
    pub(crate) fn protocol_focus_left(&mut self) {
        self.active_protocol = None;
    }

    /// Whether Smithay will emit an input-method `done` for this request.
    /// This mirrors its focused-client and single-active-text-input gates;
    /// it carries no text and only advances the serial used to reject stale
    /// input-method commits.
    pub(crate) fn input_method_done(
        &mut self,
        resource: &ZwpTextInputV3,
        request: &zwp_text_input_v3::Request,
        admitted: bool,
    ) -> Option<bool> {
        if !admitted {
            return None;
        }
        let id = resource.id();
        let field = self.fields.entry(id.clone()).or_default();
        match request {
            zwp_text_input_v3::Request::Enable => {
                field.protocol_activation = Some(true);
                None
            }
            zwp_text_input_v3::Request::Disable => {
                field.protocol_activation = Some(false);
                None
            }
            zwp_text_input_v3::Request::Commit => {
                // `pending_enable` represents true. `pending_edit` alone is
                // not enough to distinguish Disable, so use the dedicated
                // protocol field maintained alongside visual metadata.
                let activation = field.protocol_activation.take();
                match activation {
                    Some(true)
                        if self
                            .active_protocol
                            .as_ref()
                            .is_none_or(|active| active == &id) =>
                    {
                        self.active_protocol = Some(id);
                        Some(true)
                    }
                    Some(false)
                        if self
                            .active_protocol
                            .as_ref()
                            .is_none_or(|active| active == &id) =>
                    {
                        self.active_protocol = None;
                        Some(false)
                    }
                    None if self.active_protocol.as_ref() == Some(&id) => Some(true),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Follow one text-input request. State is double-buffered: it applies
    /// on `commit`, and enabling resets it to the protocol's defaults.
    pub fn observe(&mut self, resource: &ZwpTextInputV3, request: &zwp_text_input_v3::Request) {
        let field = self.fields.entry(resource.id()).or_default();
        match request {
            zwp_text_input_v3::Request::Enable => {
                field.pending_enable = true;
                field.pending_edit = true;
            }
            zwp_text_input_v3::Request::Disable
            | zwp_text_input_v3::Request::SetSurroundingText { .. }
            | zwp_text_input_v3::Request::SetTextChangeCause { .. } => field.pending_edit = true,
            zwp_text_input_v3::Request::SetContentType { hint, purpose } => {
                field.pending = Some(ContentType {
                    hint: match hint {
                        WEnum::Value(hint) => hint.bits(),
                        WEnum::Unknown(raw) => *raw,
                    },
                    purpose: match purpose {
                        WEnum::Value(purpose) => u32::from(*purpose),
                        WEnum::Unknown(raw) => *raw,
                    },
                });
            }
            zwp_text_input_v3::Request::SetCursorRectangle {
                x,
                y,
                width,
                height,
            } => field.pending_cursor = Some((*x, *y, *width, *height)),
            zwp_text_input_v3::Request::Commit => {
                if std::mem::take(&mut field.pending_edit)
                    || field
                        .pending
                        .is_some_and(|content| content != field.committed)
                    || field
                        .pending_cursor
                        .is_some_and(|cursor| Some(cursor) != field.cursor)
                {
                    field.revision = field.revision.saturating_add(1);
                }
                let pending = field.pending.take();
                let pending_cursor = field.pending_cursor.take();
                if std::mem::take(&mut field.pending_enable) {
                    field.committed = pending.unwrap_or_default();
                    field.cursor = pending_cursor;
                } else {
                    if let Some(pending) = pending {
                        field.committed = pending;
                    }
                    if pending_cursor.is_some() {
                        field.cursor = pending_cursor;
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn revision(&self, resource: &ZwpTextInputV3) -> Option<u64> {
        self.fields.get(&resource.id()).map(|field| field.revision)
    }

    pub fn forget(&mut self, resource: &ZwpTextInputV3, may_deactivate: bool) -> bool {
        let id = resource.id();
        self.fields.remove(&id);
        let deactivates = may_deactivate
            && !self
                .fields
                .keys()
                .any(|remaining| remaining.same_client_as(&id));
        if deactivates {
            self.active_protocol = None;
        }
        deactivates
    }

    /// The caret `resource` last committed, in its surface's coordinates.
    pub fn cursor_rectangle(&self, resource: &ZwpTextInputV3) -> Option<CursorRectangle> {
        self.fields
            .get(&resource.id())
            .and_then(|field| field.cursor)
    }

    /// The content type `resource` last committed.
    pub fn content_type(&self, resource: &ZwpTextInputV3) -> ContentType {
        self.fields
            .get(&resource.id())
            .map(|field| field.committed)
            .unwrap_or_default()
    }
}

delegate_global_dispatch!(State: [ZwpTextInputManagerV3: ()] => TextInputManagerState);
delegate_dispatch!(State: [ZwpTextInputManagerV3: ()] => TextInputManagerState);

impl Dispatch<ZwpTextInputV3, TextInputUserData> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ZwpTextInputV3,
        request: zwp_text_input_v3::Request,
        data: &TextInputUserData,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        // Consult Smithay's authoritative text-input focus. The shell's
        // independently tracked keyboard target can legitimately differ
        // while chrome or a floating preedit continuation owns the seat.
        let admitted = state.input_method_resource_client.is_some()
            && state
                .seat
                .text_input()
                .focus()
                .is_some_and(|surface| surface.id().same_client_as(&resource.id()));
        let input_method_done = state
            .text_fields
            .input_method_done(resource, &request, admitted);
        state.text_fields.observe(resource, &request);
        <TextInputManagerState as Dispatch<ZwpTextInputV3, TextInputUserData, Self>>::request(
            state, client, resource, request, data, dhandle, data_init,
        );
        if let Some(active) = input_method_done {
            state.input_method_serial = state.input_method_serial.saturating_add(1);
            if !active {
                state.input_method_preedit = super::state::InputMethodPreeditState::Inactive;
            }
        }
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &ZwpTextInputV3,
        data: &TextInputUserData,
    ) {
        // This is the same public handle Smithay's own destruction path uses.
        let destroyed_focused = state
            .seat
            .text_input()
            .focus()
            .is_none_or(|surface| surface.id().same_client_as(&resource.id()));
        if state.text_fields.forget(
            resource,
            state.input_method_resource_client.is_some() && destroyed_focused,
        ) {
            state.input_method_serial = state.input_method_serial.saturating_add(1);
        }
        <TextInputManagerState as Dispatch<ZwpTextInputV3, TextInputUserData, Self>>::destroyed(
            state, client, resource, data,
        );
    }
}

/// Text that reached a field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveredText {
    pub text: String,
    /// The field declared its content secret.
    pub sensitive: bool,
    /// Whose input-method session carried the commit.
    pub through: TrustedInputMethod,
}

struct Pending {
    text: String,
    deadline: Instant,
    lent: Option<LentInputMethod>,
    /// Set once the lent input method has committed; the request finishes
    /// when the compositor has processed the commit.
    committed_sensitive: Option<bool>,
    /// Whether the shell picked the text here and keeps it among its
    /// recents; text another device typed is delivered and forgotten.
    picked_here: bool,
}

/// The request in flight (at most one: a new pick replaces an undelivered
/// one) and what has been delivered since the caller last asked.
#[derive(Default)]
pub struct TextCommit {
    pending: Option<Pending>,
    delivered: Vec<DeliveredText>,
    pub(crate) dictation: super::dictation::Dictation,
}

impl State {
    /// Commit `text` into the focused field as soon as it takes text.
    pub fn request_text_commit(&mut self, text: String, now: Instant) {
        self.queue_text_commit(text, now, true);
    }

    /// Commit text into the focused field without reporting it back as
    /// something picked here: what another device's input method composed,
    /// or a clipboard history item chosen to paste.
    pub fn request_remote_text_commit(&mut self, text: String, now: Instant) {
        self.queue_text_commit(text, now, false);
    }

    fn queue_text_commit(&mut self, text: String, now: Instant, picked_here: bool) {
        self.text_commit.pending = Some(Pending {
            text,
            deadline: now.checked_add(DELIVERY_WINDOW).unwrap_or(now),
            lent: None,
            committed_sensitive: None,
            picked_here,
        });
        self.advance_text_commit(now);
    }

    /// When an undelivered request gives up.
    pub fn text_commit_deadline(&self) -> Option<Instant> {
        self.text_commit
            .pending
            .as_ref()
            .map(|pending| pending.deadline)
    }

    /// What has reached a field since the last call.
    pub fn take_delivered_text(&mut self) -> Vec<DeliveredText> {
        std::mem::take(&mut self.text_commit.delivered)
    }

    /// Move the request in flight forward: deliver it through the seat's
    /// owner, or through the compositor's lent input method, or give up
    /// once its window has passed.
    pub fn advance_text_commit(&mut self, now: Instant) {
        let Some(mut pending) = self.text_commit.pending.take() else {
            return;
        };
        if let Some(sensitive) = pending.committed_sensitive {
            let finished = pending
                .lent
                .as_mut()
                .is_none_or(|lent| !matches!(lent.pump(), Progress::Waiting | Progress::Active));
            if finished || now >= pending.deadline {
                if pending.picked_here {
                    self.text_commit.delivered.push(DeliveredText {
                        text: pending.text,
                        sensitive,
                        through: TrustedInputMethod::Compositor,
                    });
                }
            } else {
                self.text_commit.pending = Some(pending);
            }
            return;
        }
        if now >= pending.deadline {
            tracing::debug!(target: "tilo::input_method", "no field took the text in time");
            return;
        }
        if let Some(owner) = self.input_method_owner_kind() {
            // The seat moved to an owner while the compositor held it: that
            // owner's session carries the commit instead.
            pending.lent = None;
            if let Some(sensitive) = self.commit_through_owner(&pending.text) {
                if pending.picked_here {
                    self.text_commit.delivered.push(DeliveredText {
                        text: pending.text,
                        sensitive,
                        through: owner,
                    });
                }
                return;
            }
            self.text_commit.pending = Some(pending);
            return;
        }
        if pending.lent.is_none() {
            match LentInputMethod::connect(self) {
                Ok(lent) => pending.lent = Some(lent),
                Err(error) => {
                    tracing::warn!(target: "tilo::input_method", %error, "could not lend the seat to the compositor");
                    return;
                }
            }
        }
        let Some(lent) = pending.lent.as_mut() else {
            return;
        };
        match lent.pump() {
            Progress::Waiting => {}
            Progress::Active => {
                let sensitive = self.active_field_content_type().is_sensitive();
                lent.commit(&pending.text);
                pending.committed_sensitive = Some(sensitive);
            }
            Progress::Finished | Progress::Failed => {
                tracing::debug!(target: "tilo::input_method", "the lent input method ended before committing");
                return;
            }
        }
        self.text_commit.pending = Some(pending);
    }

    /// The focused field's active text input, enabled in the owner's
    /// session: commit there, with that session's serial.
    fn commit_through_owner(&self, text: &str) -> Option<bool> {
        let text_input = self.seat.text_input().clone();
        let mut target = None;
        text_input.with_active_text_input(|field, _| target = Some(field.clone()));
        let field = target?;
        let sensitive = self.text_fields.content_type(&field).is_sensitive();
        field.commit_string(Some(text.to_owned()));
        text_input.done(false);
        Some(sensitive)
    }

    /// Where the focused field's caret is, in desktop coordinates: the
    /// cursor rectangle its enabled text input last committed, placed on the
    /// surface it belongs to. `None` when no field takes text, it has not
    /// said, or its surface is not on screen.
    pub fn focused_text_caret(&self) -> Option<vello::kurbo::Rect> {
        let mut target = None;
        self.seat
            .text_input()
            .with_active_text_input(|field, surface| {
                target = Some((field.clone(), super::state::surface_id(surface)));
            });
        let (field, surface) = target?;
        let (x, y, width, height) = self.text_fields.cursor_rectangle(&field)?;
        let origin = self.surface_desktop_rect(surface)?;
        let x0 = origin.x0 + f64::from(x);
        let y0 = origin.y0 + f64::from(y);
        Some(vello::kurbo::Rect::new(
            x0,
            y0,
            x0 + f64::from(width.max(0)),
            y0 + f64::from(height.max(0)),
        ))
    }

    /// The content type of the focused field's enabled text input; the
    /// default (nothing secret) when no field takes text.
    pub(crate) fn active_field_content_type(&self) -> ContentType {
        let mut target = None;
        self.seat
            .text_input()
            .with_active_text_input(|field, _| target = Some(field.clone()));
        target.map_or_else(ContentType::default, |field| {
            self.text_fields.content_type(&field)
        })
    }
}

/// Hand the text the shell picked to the seat, and report what reached a
/// field back to the shell, which keeps the recents. Returns what arrived.
pub fn relay(
    state: &mut State,
    host: &mut crate::shell::overlay_host::ShellOverlayHost,
    now: Instant,
) -> Vec<DeliveredText> {
    for text in host.take_emoji_commits() {
        state.request_text_commit(text, now);
    }
    // A clipboard history item chosen to paste goes in the same way, and is
    // not an emoji to remember.
    for id in host.take_clipboard_pastes() {
        if let Some(text) = super::selection::history_item_text(&state.selection_store(), id) {
            state.request_remote_text_commit(text, now);
        }
    }
    state.advance_text_commit(now);
    let delivered = state.take_delivered_text();
    for text in &delivered {
        host.note_emoji_delivered(&text.text, text.sensitive);
    }
    // Where the next Super+. opens. The picker takes the keyboard while it
    // is open, so it keeps the caret it opened against.
    host.set_text_caret(state.focused_text_caret());
    // Typing into the focused window sends desk widgets away and holds
    // back banners not addressed to the person.
    host.note_client_typing(state.last_client_typing_at, now);
    delivered
}

pub(super) enum Progress {
    /// Still binding, or waiting for a field to activate the input method.
    Waiting,
    /// A field activated it: commit now.
    Active,
    /// The commit was processed.
    Finished,
    /// The seat refused the input method, or the connection broke.
    Failed,
}

/// What the compositor's lent input method has heard.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag records one independent input-method-v2 event"
)]
#[derive(Default)]
struct LentState {
    seat: Option<(u32, u32)>,
    manager: Option<(u32, u32)>,
    pending_active: bool,
    active: bool,
    done: u32,
    unavailable: bool,
    finished: bool,
}

/// The compositor's in-process `input-method-v2` connection, alive for one
/// commit. Dropping it closes the connection and releases the seat.
pub(super) struct LentInputMethod {
    /// Declared first so the server side disconnects before `conn` closes.
    _link: LoopbackClient,
    conn: Connection,
    queue: EventQueue<LentState>,
    state: LentState,
    input_method: Option<zwp_input_method_v2::ZwpInputMethodV2>,
    committed: bool,
}

/// The compositor's own end of an in-process Wayland client: the server-side
/// client and the event-loop wake on the client's socket. Dropping it
/// disconnects the client.
pub(super) struct LoopbackClient {
    pub(super) server: Client,
    pub(super) display: DisplayHandle,
    loop_handle: LoopHandle<'static, State>,
    wake: Option<RegistrationToken>,
}

impl LoopbackClient {
    /// Insert a client carrying `data` into the compositor's own display and
    /// return the client side's connection. The event loop only wakes when
    /// something arrives on it; the loop's tick reads it, so the source is
    /// never removed from inside its own callback.
    pub(super) fn connect(
        state: &mut State,
        data: std::sync::Arc<ClientState>,
    ) -> Result<(Self, Connection), String> {
        let (server, client) = UnixStream::pair().map_err(|e| e.to_string())?;
        client.set_nonblocking(true).map_err(|e| e.to_string())?;
        let server = state
            .display_handle
            .insert_client(server, data)
            .map_err(|e| e.to_string())?;
        let conn = Connection::from_socket(client).map_err(|e| e.to_string())?;
        let wake = conn
            .backend()
            .poll_fd()
            .try_clone_to_owned()
            .ok()
            .and_then(|fd| {
                state
                    .loop_handle
                    .insert_source(
                        Generic::new(fd, Interest::READ, Mode::Level),
                        |_, _, _: &mut State| Ok(PostAction::Continue),
                    )
                    .ok()
            });
        let link = Self {
            server,
            display: state.display_handle.clone(),
            loop_handle: state.loop_handle.clone(),
            wake,
        };
        Ok((link, conn))
    }
}

impl Drop for LoopbackClient {
    fn drop(&mut self) {
        if let Some(wake) = self.wake.take() {
            self.loop_handle.remove(wake);
        }
        let id = self.server.id();
        if self
            .display
            .backend_handle()
            .get_client_data(id.clone())
            .is_ok()
        {
            self.display
                .backend_handle()
                .kill_client(id, DisconnectReason::ConnectionClosed);
        }
    }
}

impl LentInputMethod {
    pub(super) fn connect(state: &mut State) -> Result<Self, String> {
        let (link, conn) = LoopbackClient::connect(
            state,
            ClientState::input_method_arc(TrustedInputMethod::Compositor),
        )?;
        state.register_trusted_input_method_client(&link.server, TrustedInputMethod::Compositor);
        let queue = conn.new_event_queue::<LentState>();
        let _registry = conn.display().get_registry(&queue.handle(), ());
        let _ = conn.flush();
        Ok(Self {
            _link: link,
            conn,
            queue,
            state: LentState::default(),
            input_method: None,
            committed: false,
        })
    }

    pub(super) fn pump(&mut self) -> Progress {
        if self.queue.dispatch_pending(&mut self.state).is_err() {
            return Progress::Failed;
        }
        if let Some(guard) = self.conn.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(error))
                    if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return Progress::Failed,
            }
        }
        if self.queue.dispatch_pending(&mut self.state).is_err() || self.state.unavailable {
            return Progress::Failed;
        }
        if self.input_method.is_none()
            && let (Some((seat, seat_version)), Some((manager, manager_version))) =
                (self.state.seat, self.state.manager)
        {
            let qh = self.queue.handle();
            let registry = self.conn.display().get_registry(&qh, ());
            let seat: wl_seat::WlSeat = registry.bind(seat, seat_version.min(1), &qh, ());
            let manager: zwp_input_method_manager_v2::ZwpInputMethodManagerV2 =
                registry.bind(manager, manager_version.min(1), &qh, ());
            self.input_method = Some(manager.get_input_method(&seat, &qh, ()));
        }
        let _ = self.conn.flush();
        if self.state.finished {
            Progress::Finished
        } else if self.state.active && !self.committed {
            Progress::Active
        } else {
            Progress::Waiting
        }
    }

    /// Commit `text` into the activated field, then give the input method
    /// back; the sync callback reports when the compositor processed both.
    fn commit(&mut self, text: &str) {
        if let Some(input_method) = self.input_method.take() {
            input_method.commit_string(text.to_owned());
            input_method.commit(self.state.done);
            input_method.destroy();
        }
        let _ = self.conn.display().sync(&self.queue.handle(), ());
        self.committed = true;
        let _ = self.conn.flush();
    }
}

impl wayland_client::Dispatch<wl_registry::WlRegistry, ()> for LentState {
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
            match interface.as_str() {
                "wl_seat" if state.seat.is_none() => state.seat = Some((name, version)),
                "zwp_input_method_manager_v2" => state.manager = Some((name, version)),
                _ => {}
            }
        }
    }
}

impl wayland_client::Dispatch<zwp_input_method_v2::ZwpInputMethodV2, ()> for LentState {
    fn event(
        state: &mut Self,
        _: &zwp_input_method_v2::ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => state.pending_active = true,
            zwp_input_method_v2::Event::Deactivate => state.pending_active = false,
            zwp_input_method_v2::Event::Done => {
                state.done = state.done.wrapping_add(1);
                state.active = state.pending_active;
            }
            zwp_input_method_v2::Event::Unavailable => state.unavailable = true,
            _ => {}
        }
    }
}

impl wayland_client::Dispatch<wl_callback::WlCallback, ()> for LentState {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.finished = true;
        }
    }
}

delegate_noop!(LentState: ignore wl_seat::WlSeat);
delegate_noop!(LentState: ignore zwp_input_method_manager_v2::ZwpInputMethodManagerV2);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_the_protocol_secret_purposes_and_hints() {
        let normal = ContentType::default();
        assert!(!normal.is_sensitive());
        for purpose in [2, 8, 9] {
            assert!(
                ContentType { hint: 0, purpose }.is_sensitive(),
                "purpose {purpose}"
            );
        }
        for hint in [0x40, 0x80, 0x40 | 0x200] {
            assert!(
                ContentType { hint, purpose: 0 }.is_sensitive(),
                "hint {hint:#x}"
            );
        }
        // Spellcheck, multiline, e-mail and terminal fields are ordinary.
        assert!(
            !ContentType {
                hint: 0x2 | 0x200,
                purpose: 6
            }
            .is_sensitive()
        );
        assert!(
            !ContentType {
                hint: 0,
                purpose: 13
            }
            .is_sensitive()
        );
    }
}
