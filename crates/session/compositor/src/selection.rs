//! Clipboard and primary selection across Wayland clients, X11 clients and
//! the session's surface apps.
//!
//! | protocol | who may bind | what it is for |
//! |---|---|---|
//! | `wl_data_device_manager` | every client | Ctrl+C / Ctrl+V and drag-and-drop |
//! | `zwp_primary_selection_device_manager_v1` | every client | select-then-middle-click |
//! | `zwlr_data_control_manager_v1` | trusted clients only | clipboard managers |
//! | `ext_data_control_manager_v1` | trusted clients only | ditto, the successor |
//!
//! `wl_data_device` is focus-guarded by the protocol itself, so every client may
//! bind it. Data control sees every selection unconditionally, which would
//! let a sandboxed app harvest one-time codes and passwords off the
//! clipboard, so only clients the session trusts see it (a clipboard
//! history service is one).
//!
//! Xwayland's selections are bridged both ways: an X11 copy becomes the
//! seat's selection (served by the X11 window manager), a Wayland copy is
//! offered to X11 clients, and an X11 client reads only while an X11 window
//! has keyboard focus. Surface apps are no Wayland clients: they set
//! a selection the compositor serves itself
//! ([`crate::Compositor::set_selection`]) and read the current one through a
//! pipe ([`crate::Compositor::receive_selection`]), never blocking on a
//! client.

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::sync::Arc;

use smithay::input::Seat;
use smithay::reexports::wayland_server::Client;
use smithay::utils::Serial;
use smithay::wayland::selection::data_device::{
    clear_data_device_selection, current_data_device_selection_userdata,
    request_data_device_client_selection, set_data_device_selection,
};
use smithay::wayland::selection::ext_data_control::{
    DataControlHandler as ExtDataControlHandler, DataControlState as ExtDataControlState,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionState, clear_primary_selection, current_primary_selection_userdata,
    request_primary_client_selection, set_primary_selection,
};
use smithay::wayland::selection::wlr_data_control::{DataControlHandler, DataControlState};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};
use smithay::xwayland::xwm::XwmId;

use crate::state::{ClientState, State};

pub use tilo_x11_transfer::MAX_SELECTION_BYTES;
pub use tilo_x11_transfer::TEXT_MIME;
use tilo_x11_transfer::{MAX_MIMES, TEXT_MIMES, is_text_mime, valid_mime};
pub(crate) use tilo_x11_transfer::{normalized_mimes, serve};

/// How long after a connection arrives the session's keeper may re-assert the selection its app
/// owned: long enough for the new desktop to hand out its globals, short enough that the person
/// cannot have copied into anything else first.
const KEEPER_CLAIM_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

/// Which selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    /// Ctrl+C / Ctrl+V.
    Clipboard,
    /// Select, then middle-click.
    Primary,
}

impl From<SelectionTarget> for Selection {
    fn from(target: SelectionTarget) -> Self {
        match target {
            SelectionTarget::Clipboard => Self::Clipboard,
            SelectionTarget::Primary => Self::Primary,
        }
    }
}

impl From<Selection> for SelectionTarget {
    fn from(selection: Selection) -> Self {
        match selection {
            Selection::Clipboard => Self::Clipboard,
            Selection::Primary => Self::Primary,
        }
    }
}

impl Selection {
    /// Its place in the two-entry tables this module keeps.
    fn index(self) -> usize {
        match self {
            Self::Clipboard => 0,
            Self::Primary => 1,
        }
    }
}

/// A selection a session-handed connection re-asserted: the time its app set it, which orders
/// two keepers' claims.
struct Claim {
    at: u32,
}

/// Who serves a selection the compositor set on the seat.
#[derive(Clone, Debug)]
pub enum Served {
    /// An X11 client of `xwm` 's display, through its window manager.
    X11(XwmId),
    /// A hosted surface app: these bytes per format.
    Surface(Arc<BTreeMap<String, Vec<u8>>>),
}

/// The compositor's side of the selections: the data-control globals and what each selection offers
/// now.
pub(crate) struct Selections {
    wlr: DataControlState,
    ext: ExtDataControlState,
    /// The formats each selection offers, whoever owns it.
    offered: [Vec<String>; 2],
    /// How often each selection changed hands: a snapshot of `offered` and this number tells a new
    /// selection from none.
    changes: [u64; 2],
    /// The keeper claim on each selection, when one is there.
    claims: [Option<Claim>; 2],
    /// The claim accepted for the request being handled, which `new_selection` keeps.
    pending: [Option<Claim>; 2],
}

impl Selections {
    pub(crate) fn new(
        display: &smithay::reexports::wayland_server::DisplayHandle,
        primary: &PrimarySelectionState,
    ) -> Self {
        let trusted = |client: &smithay::reexports::wayland_server::Client| {
            client
                .get_data::<ClientState>()
                .is_some_and(ClientState::trusted)
        };
        Self {
            wlr: DataControlState::new::<State, _>(display, Some(primary), trusted),
            ext: ExtDataControlState::new::<State, _>(display, Some(primary), trusted),
            offered: [Vec::new(), Vec::new()],
            changes: [0; 2],
            claims: [None, None],
            pending: [None, None],
        }
    }

    /// Who offers `selection` now, as a number that changes with every selection that is set or
    /// dropped.
    pub(crate) fn changes(&self, selection: Selection) -> u64 {
        let [clipboard, primary] = self.changes;
        match selection {
            Selection::Clipboard => clipboard,
            Selection::Primary => primary,
        }
    }

    /// `selection` now offers `mimes` .
    fn set_offered(&mut self, selection: Selection, mimes: Vec<String>) {
        let [clipboard, primary] = &mut self.offered;
        let [changed_clipboard, changed_primary] = &mut self.changes;
        match selection {
            Selection::Clipboard => {
                *clipboard = mimes;
                *changed_clipboard = changed_clipboard.wrapping_add(1);
            }
            Selection::Primary => {
                *primary = mimes;
                *changed_primary = changed_primary.wrapping_add(1);
            }
        }
    }

    /// Whether `client` may set `selection` without holding the keyboard: a session-handed
    /// connection's one claim, at the time its app's copy was made, while nothing else owns the
    /// selection — or a keeper's own earlier claim, which a later copy replaces.
    fn may_claim(&mut self, selection: Selection, client: &Client, at: u32) -> bool {
        let Some(data) = client.get_data::<ClientState>() else {
            return false;
        };
        if !data.handed() || data.attached().elapsed() > KEEPER_CLAIM_WINDOW {
            return false;
        }
        // One claim per connection and selection: an attempt that loses is spent too.
        if data.spend_claim(selection) {
            return false;
        }
        let index = selection.index();
        let accepted = match self.claims.get(index).and_then(Option::as_ref) {
            Some(claim) => self.changes(selection) > 0 && Serial::from(claim.at) < Serial::from(at),
            None => self.changes(selection) == 0,
        };
        if accepted && let Some(pending) = self.pending.get_mut(index) {
            *pending = Some(Claim { at });
        }
        accepted
    }

    /// The claim the request being handled was accepted for, if any.
    fn take_pending(&mut self, selection: Selection) -> Option<Claim> {
        self.pending
            .get_mut(selection.index())
            .and_then(Option::take)
    }

    /// A client set `selection`: a keeper claim holds only when this was its own request.
    fn keep_claim(&mut self, selection: Selection, claim: Option<Claim>) {
        if let Some(kept) = self.claims.get_mut(selection.index()) {
            *kept = claim;
        }
    }

    /// `selection` is set by something other than a keeper's claim.
    fn clear_claim(&mut self, selection: Selection) {
        self.keep_claim(selection, None);
    }

    /// The formats `selection` offers now.
    pub(crate) fn offered(&self, selection: Selection) -> &[String] {
        let [clipboard, primary] = &self.offered;
        match selection {
            Selection::Clipboard => clipboard,
            Selection::Primary => primary,
        }
    }
}

/// The bytes an offer holds for `mime` , text under any of its names.
pub(crate) fn value_for<'a>(values: &'a BTreeMap<String, Vec<u8>>, mime: &str) -> Option<&'a [u8]> {
    values.get(mime).map(Vec::as_slice).or_else(|| {
        is_text_mime(mime)
            .then(|| TEXT_MIMES.iter().find_map(|alias| values.get(*alias)))
            .flatten()
            .map(Vec::as_slice)
    })
}

/// A surface app's offer as the compositor keeps it: valid formats, within [`MAX_SELECTION_BYTES`];
/// `None` when it holds nothing to offer.
pub(crate) fn surface_offer(offer: Vec<(String, Vec<u8>)>) -> Option<BTreeMap<String, Vec<u8>>> {
    let mut total = 0usize;
    let mut values = BTreeMap::new();
    for (mime, bytes) in offer.into_iter().take(MAX_MIMES) {
        if !valid_mime(&mime) {
            continue;
        }
        total = total.saturating_add(bytes.len());
        if total > MAX_SELECTION_BYTES {
            break;
        }
        values.insert(mime, bytes);
    }
    (!values.is_empty()).then_some(values)
}

impl State {
    /// Put a surface app's offer on the seat (an empty one clears the selection when a surface app
    /// owns it).
    pub(crate) fn set_surface_selection(
        &mut self,
        selection: Selection,
        offer: Vec<(String, Vec<u8>)>,
    ) {
        let display = self.display.clone();
        let Some(values) = surface_offer(offer) else {
            if matches!(self.served(selection), Some(Served::Surface(_))) {
                self.selections.clear_claim(selection);
                match selection {
                    Selection::Clipboard => clear_data_device_selection(&display, &self.seat),
                    Selection::Primary => clear_primary_selection(&display, &self.seat),
                }
                self.offer_to_x11(selection, None);
                // A cleared selection offers nothing: whoever watches the formats sees the bytes
                // are gone, as they do when a client drops its own.
                self.selections.set_offered(selection, Vec::new());
            }
            return;
        };
        let mimes = normalized_mimes(values.keys().cloned());
        self.selections.clear_claim(selection);
        let served = Served::Surface(Arc::new(values));
        match selection {
            Selection::Clipboard => {
                set_data_device_selection(&display, &self.seat, mimes.clone(), served);
            }
            Selection::Primary => {
                set_primary_selection(&display, &self.seat, mimes.clone(), served);
            }
        }
        self.offer_to_x11(selection, Some(mimes.as_slice()));
        self.selections.set_offered(selection, mimes);
    }

    /// Who serves `selection` , when the compositor set it.
    fn served(&self, selection: Selection) -> Option<Served> {
        match selection {
            Selection::Clipboard => {
                current_data_device_selection_userdata(&self.seat).map(|data| data.clone())
            }
            Selection::Primary => {
                current_primary_selection_userdata(&self.seat).map(|data| data.clone())
            }
        }
    }

    /// Read `selection` as `mime` into `fd` , whoever owns it.
    pub(crate) fn receive_selection(
        &mut self,
        selection: Selection,
        mime: &str,
        fd: OwnedFd,
    ) -> Result<(), String> {
        match self.served(selection) {
            Some(Served::Surface(values)) => {
                let bytes = value_for(&values, mime)
                    .ok_or_else(|| format!("the selection offers no {mime}"))?;
                serve(fd, bytes.to_vec());
                Ok(())
            }
            Some(Served::X11(xwm)) => self.send_to_x11_reader(xwm, selection, mime.to_owned(), fd),
            None => match selection {
                Selection::Clipboard => {
                    request_data_device_client_selection(&self.seat, mime.to_owned(), fd)
                        .map_err(|error| format!("{error:?}"))
                }
                Selection::Primary => {
                    request_primary_client_selection(&self.seat, mime.to_owned(), fd)
                        .map_err(|error| format!("{error:?}"))
                }
            },
        }
    }

    /// Ask the X11 owner of `selection` on `xwm` 's display for `mime` into `fd` .
    fn send_to_x11_reader(
        &mut self,
        xwm: XwmId,
        selection: Selection,
        mime: String,
        fd: OwnedFd,
    ) -> Result<(), String> {
        let handle = self
            .loop_handle
            .clone()
            .ok_or_else(|| "no event loop".to_owned())?;
        let wm = self
            .x11_server_mut(xwm)
            .map(|server| &mut server.wm)
            .ok_or_else(|| "the display's Xwayland is not running".to_owned())?;
        wm.send_selection(selection.into(), mime, fd, handle)
            .map_err(|error| format!("{error:?}"))
    }

    /// Tell the displays what `selection` offers now (`None`: nothing).
    /// An X11 client's copy stays on its display: the apps of its workspace read it there, and no
    /// other display is told of it. What a Wayland client or a surface app copied goes to every
    /// display, as it does to every Wayland client.
    fn offer_to_x11(&mut self, selection: Selection, mimes: Option<&[String]>) {
        let owner = match self.served(selection) {
            Some(Served::X11(xwm)) => Some(xwm),
            _ => None,
        };
        for server in &mut self.xwms {
            let offered = if owner.is_none_or(|owner| owner == server.id()) {
                mimes.map(<[String]>::to_vec)
            } else {
                None
            };
            if let Err(error) = server.wm.new_selection(selection.into(), offered) {
                tracing::warn!(?error, "X11 clients not told of the selection");
            }
        }
    }

    /// Whether an X11 window of `xwm` has keyboard focus.
    fn x11_focused(&self, xwm: XwmId) -> bool {
        let Some(focus) = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus())
        else {
            return false;
        };
        self.space.elements().any(|window| {
            window.x11_surface().is_some_and(|surface| {
                surface.xwm_id() == Some(xwm) && surface.wl_surface().as_ref() == Some(&focus)
            })
        })
    }

    /// An X11 client may read a selection while an X11 window has focus.
    pub(crate) fn x11_selection_access(&self, xwm: XwmId) -> bool {
        self.x11_focused(xwm)
    }

    /// An X11 client reads `selection` as `mime` into `fd` , from the display that serves it.
    pub(crate) fn x11_reads_selection(&mut self, selection: Selection, mime: &str, fd: OwnedFd) {
        if let Err(error) = self.receive_selection(selection, mime, fd) {
            tracing::debug!(%error, "X11 selection read refused");
        }
    }

    /// An X11 client of `xwm` 's display set `selection` .
    pub(crate) fn x11_set_selection(
        &mut self,
        xwm: XwmId,
        selection: Selection,
        mimes: Vec<String>,
    ) {
        let display = self.display.clone();
        self.selections.clear_claim(selection);
        match selection {
            Selection::Clipboard => {
                set_data_device_selection(&display, &self.seat, mimes.clone(), Served::X11(xwm));
            }
            Selection::Primary => {
                set_primary_selection(&display, &self.seat, mimes.clone(), Served::X11(xwm));
            }
        }
        self.selections.set_offered(selection, mimes);
    }

    /// An X11 client of `xwm` 's display dropped `selection` ; cleared if it still owns it.
    pub(crate) fn x11_cleared_selection(&mut self, xwm: XwmId, selection: Selection) {
        if !matches!(self.served(selection), Some(Served::X11(owner)) if owner == xwm) {
            return;
        }
        let display = self.display.clone();
        self.selections.clear_claim(selection);
        match selection {
            Selection::Clipboard => clear_data_device_selection(&display, &self.seat),
            Selection::Primary => clear_primary_selection(&display, &self.seat),
        }
        self.selections.set_offered(selection, Vec::new());
    }

    /// The server of `xwm` 's display is gone: a selection its clients owned is gone with it.
    pub(crate) fn forget_x11_selection(&mut self, xwm: XwmId) {
        for selection in [Selection::Clipboard, Selection::Primary] {
            if !matches!(self.served(selection), Some(Served::X11(owner)) if owner == xwm) {
                continue;
            }
            let display = self.display.clone();
            self.selections.clear_claim(selection);
            match selection {
                Selection::Clipboard => clear_data_device_selection(&display, &self.seat),
                Selection::Primary => clear_primary_selection(&display, &self.seat),
            }
            self.selections.set_offered(selection, Vec::new());
        }
    }

    /// Send keyboard focus's client the selections (`wl_data_device` and primary selection follow
    /// focus).
    pub(crate) fn selection_focus(
        &self,
        client: Option<&smithay::reexports::wayland_server::Client>,
    ) {
        smithay::wayland::selection::data_device::set_data_device_focus(
            &self.display,
            &self.seat,
            client.cloned(),
        );
        smithay::wayland::selection::primary_selection::set_primary_focus(
            &self.display,
            &self.seat,
            client.cloned(),
        );
    }
}

impl SelectionHandler for State {
    type SelectionUserData = Served;

    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        let selection: Selection = ty.into();
        let mimes = source.as_ref().map(SelectionSource::mime_types);
        // A claim stands only for the request the compositor accepted it for; every other
        // selection takes it over, as a fresh desktop's first copy does.
        let pending = self.selections.take_pending(selection);
        self.selections
            .keep_claim(selection, pending.filter(|_| source.is_some()));
        self.selections
            .set_offered(selection, mimes.clone().unwrap_or_default());
        self.offer_to_x11(selection, mimes.as_deref());
    }

    /// A session-handed connection asks to set a selection without holding the keyboard, as an
    /// app keeper does once after its desktop restarts. See [`Selections::may_claim`].
    fn may_set_selection_without_focus(
        &mut self,
        ty: SelectionTarget,
        client: &Client,
        serial: Serial,
    ) -> bool {
        self.selections
            .may_claim(ty.into(), client, u32::from(serial))
    }

    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        user_data: &Served,
    ) {
        match user_data {
            Served::Surface(values) => {
                if let Some(bytes) = value_for(values, &mime_type) {
                    serve(fd, bytes.to_vec());
                } else {
                    tracing::debug!(mime_type, "selection asked for a format it does not offer");
                }
            }
            Served::X11(xwm) => {
                if let Err(error) = self.send_to_x11_reader(*xwm, ty.into(), mime_type, fd) {
                    tracing::debug!(%error, "X11 selection not read");
                }
            }
        }
    }
}

impl DataControlHandler for State {
    fn data_control_state(&self) -> &DataControlState {
        &self.selections.wlr
    }
}

impl ExtDataControlHandler for State {
    fn data_control_state(&self) -> &ExtDataControlState {
        &self.selections.ext
    }
}

smithay::delegate_data_control!(State);
smithay::delegate_ext_data_control!(State);

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|mime| (*mime).to_owned()).collect()
    }

    tilo_test_env::scenario! {
        /// The formats a selection is offered and read under.
        fn text_selections_go_by_every_text_name;

        phase text_is_read_under_any_of_its_names;
        {
            let values = BTreeMap::from([("text/plain".to_owned(), b"hello".to_vec())]);
            assert_eq!(value_for(&values, "UTF8_STRING"), Some(&b"hello"[..]));
            assert_eq!(value_for(&values, TEXT_MIME), Some(&b"hello"[..]));
            assert_eq!(value_for(&values, "image/png"), None);
        }

        phase surface_offers_are_bounded_and_empty_ones_offer_nothing;
        {
            assert_eq!(surface_offer(Vec::new()), None);
            assert_eq!(surface_offer(vec![("bad mime".into(), b"x".to_vec())]), None);
            let huge = vec![
                ("text/plain".to_owned(), vec![b'a'; MAX_SELECTION_BYTES]),
                ("text/html".to_owned(), vec![b'b'; 16]),
            ];
            let kept = surface_offer(huge).expect("the first format fits");
            assert_eq!(
                kept.keys().cloned().collect::<Vec<_>>(),
                strings(&["text/plain"])
            );
        }
    }
}
