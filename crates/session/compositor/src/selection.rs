//! Clipboard and primary selection across Wayland clients, X11 clients and
//! the session's hosted surface apps.
//!
//! | protocol | who may bind | what it is for |
//! |---|---|---|
//! | `wl_data_device_manager` | every client | Ctrl+C / Ctrl+V and drag-and-drop |
//! | `zwp_primary_selection_device_manager_v1` | every client | select-then-middle-click |
//! | `zwlr_data_control_manager_v1` | trusted clients only | clipboard managers |
//! | `ext_data_control_manager_v1` | trusted clients only | ditto, the successor |
//!
//! `wl_data_device` is focus-gated by the protocol itself, so every client may
//! bind it. Data control sees every selection unconditionally, which would
//! let a sandboxed app harvest one-time codes and passwords off the
//! clipboard, so only clients the session trusts see it (a clipboard
//! history service is one).
//!
//! Xwayland's selections are bridged both ways: an X11 copy becomes the
//! seat's selection (served by the X11 window manager), a Wayland copy is
//! offered to X11 clients, and an X11 client reads only while an X11 window
//! has keyboard focus. Hosted surface apps are no Wayland clients: they set
//! a selection the compositor serves itself
//! ([`crate::Compositor::set_selection`]) and read the current one through a
//! pipe ([`crate::Compositor::receive_selection`]), never blocking on a
//! client.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::sync::Arc;

use smithay::input::Seat;
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

/// UTF-8 text, the form every text offer is also given in.
pub const TEXT_MIME: &str = "text/plain;charset=utf-8";

/// The names text goes by, most specific first: X11 clients ask for the
/// older ones through Xwayland.
const TEXT_MIMES: [&str; 5] = [TEXT_MIME, "text/plain", "UTF8_STRING", "STRING", "TEXT"];

/// Cap on the formats one offer advertises.
const MAX_MIMES: usize = 16;
/// Cap on one format's name.
const MAX_MIME_BYTES: usize = 128;
/// Cap on what a hosted app's offer holds in all.
pub const MAX_SELECTION_BYTES: usize = 8 * 1024 * 1024;

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

/// Who serves a selection the compositor set on the seat.
#[derive(Clone, Debug)]
pub enum Served {
    /// An X11 client, through the window manager.
    X11,
    /// A hosted surface app: these bytes per format.
    Host(Arc<BTreeMap<String, Vec<u8>>>),
}

/// The compositor's side of the selections: the data-control globals and
/// what each selection offers now.
pub(crate) struct Selections {
    wlr: DataControlState,
    ext: ExtDataControlState,
    /// The formats each selection offers, whoever owns it.
    offered: [Vec<String>; 2],
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
        }
    }

    fn offered_mut(&mut self, selection: Selection) -> &mut Vec<String> {
        let [clipboard, primary] = &mut self.offered;
        match selection {
            Selection::Clipboard => clipboard,
            Selection::Primary => primary,
        }
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

fn is_text_mime(mime: &str) -> bool {
    TEXT_MIMES.contains(&mime)
}

fn valid_mime(mime: &str) -> bool {
    !mime.is_empty()
        && mime.len() <= MAX_MIME_BYTES
        && !mime.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// The formats an offer advertises: valid names only, bounded, and text
/// under every name text goes by, UTF-8 first so old clients do not pick an
/// ambiguous X11 alias.
pub(crate) fn normalized_mimes(mimes: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut mimes: Vec<String> = mimes
        .into_iter()
        .filter(|mime| valid_mime(mime))
        .take(MAX_MIMES)
        .collect();
    if mimes.iter().any(|mime| is_text_mime(mime)) {
        for alias in TEXT_MIMES {
            if !mimes.iter().any(|mime| mime == alias) && mimes.len() < MAX_MIMES {
                mimes.push(alias.to_owned());
            }
        }
    }
    mimes.sort();
    mimes.dedup();
    mimes.sort_by_key(|mime| (mime != TEXT_MIME, mime.clone()));
    mimes
}

/// The bytes an offer holds for `mime`, text under any of its names.
pub(crate) fn value_for<'a>(values: &'a BTreeMap<String, Vec<u8>>, mime: &str) -> Option<&'a [u8]> {
    values.get(mime).map(Vec::as_slice).or_else(|| {
        is_text_mime(mime)
            .then(|| TEXT_MIMES.iter().find_map(|alias| values.get(*alias)))
            .flatten()
            .map(Vec::as_slice)
    })
}

/// A hosted app's offer as the compositor keeps it: valid formats, within
/// [`MAX_SELECTION_BYTES`]; `None` when it holds nothing to offer.
pub(crate) fn host_offer(offer: Vec<(String, Vec<u8>)>) -> Option<BTreeMap<String, Vec<u8>>> {
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

/// Write `bytes` to a reader's pipe off the loop: a reader that is slow or
/// never reads must not stall the compositor. Closing the pipe is the end.
fn serve(fd: OwnedFd, bytes: Vec<u8>) {
    std::thread::spawn(move || {
        if let Err(error) = std::fs::File::from(fd).write_all(&bytes) {
            tracing::debug!(%error, "selection reader went away mid-transfer");
        }
    });
}

impl State {
    /// Put a hosted app's offer on the seat (an empty one clears the
    /// selection when a hosted app owns it).
    pub(crate) fn set_host_selection(
        &mut self,
        selection: Selection,
        offer: Vec<(String, Vec<u8>)>,
    ) {
        let display = self.display.clone();
        let Some(values) = host_offer(offer) else {
            if matches!(self.served(selection), Some(Served::Host(_))) {
                match selection {
                    Selection::Clipboard => clear_data_device_selection(&display, &self.seat),
                    Selection::Primary => clear_primary_selection(&display, &self.seat),
                }
                self.offer_to_x11(selection, None);
            }
            return;
        };
        let mimes = normalized_mimes(values.keys().cloned());
        let served = Served::Host(Arc::new(values));
        match selection {
            Selection::Clipboard => {
                set_data_device_selection(&display, &self.seat, mimes.clone(), served);
            }
            Selection::Primary => {
                set_primary_selection(&display, &self.seat, mimes.clone(), served);
            }
        }
        self.offer_to_x11(selection, Some(mimes.clone()));
        *self.selections.offered_mut(selection) = mimes;
    }

    /// Who serves `selection`, when the compositor set it.
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

    /// Read `selection` as `mime` into `fd`, whoever owns it.
    pub(crate) fn receive_selection(
        &mut self,
        selection: Selection,
        mime: &str,
        fd: OwnedFd,
    ) -> Result<(), String> {
        match self.served(selection) {
            Some(Served::Host(values)) => {
                let bytes = value_for(&values, mime)
                    .ok_or_else(|| format!("the selection offers no {mime}"))?;
                serve(fd, bytes.to_vec());
                Ok(())
            }
            Some(Served::X11) => self.send_to_x11_reader(selection, mime.to_owned(), fd),
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

    /// Ask the X11 owner of `selection` for `mime` into `fd`.
    fn send_to_x11_reader(
        &mut self,
        selection: Selection,
        mime: String,
        fd: OwnedFd,
    ) -> Result<(), String> {
        let handle = self
            .loop_handle
            .clone()
            .ok_or_else(|| "no event loop".to_owned())?;
        let xwm = self
            .xwm
            .as_mut()
            .ok_or_else(|| "Xwayland is not running".to_owned())?;
        xwm.send_selection(selection.into(), mime, fd, handle)
            .map_err(|error| format!("{error:?}"))
    }

    /// Tell X11 clients what `selection` offers now (`None`: nothing).
    fn offer_to_x11(&mut self, selection: Selection, mimes: Option<Vec<String>>) {
        if let Some(xwm) = self.xwm.as_mut()
            && let Err(error) = xwm.new_selection(selection.into(), mimes)
        {
            tracing::warn!(?error, "X11 clients not told of the selection");
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

    /// An X11 client reads `selection` as `mime` into `fd`.
    pub(crate) fn x11_reads_selection(&mut self, selection: Selection, mime: &str, fd: OwnedFd) {
        if let Err(error) = self.receive_selection(selection, mime, fd) {
            tracing::debug!(%error, "X11 selection read refused");
        }
    }

    /// An X11 client set `selection`.
    pub(crate) fn x11_set_selection(&mut self, selection: Selection, mimes: Vec<String>) {
        let display = self.display.clone();
        match selection {
            Selection::Clipboard => {
                set_data_device_selection(&display, &self.seat, mimes.clone(), Served::X11);
            }
            Selection::Primary => {
                set_primary_selection(&display, &self.seat, mimes.clone(), Served::X11)
            }
        }
        *self.selections.offered_mut(selection) = mimes;
    }

    /// An X11 client dropped `selection`; cleared if it still owns it.
    pub(crate) fn x11_cleared_selection(&mut self, selection: Selection) {
        if !matches!(self.served(selection), Some(Served::X11)) {
            return;
        }
        let display = self.display.clone();
        match selection {
            Selection::Clipboard => clear_data_device_selection(&display, &self.seat),
            Selection::Primary => clear_primary_selection(&display, &self.seat),
        }
        self.selections.offered_mut(selection).clear();
    }

    /// Send keyboard focus's client the selections (`wl_data_device` and
    /// primary selection follow focus).
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
        let mimes = source.map(|source| source.mime_types());
        *self.selections.offered_mut(ty.into()) = mimes.clone().unwrap_or_default();
        self.offer_to_x11(ty.into(), mimes);
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
            Served::Host(values) => {
                if let Some(bytes) = value_for(values, &mime_type) {
                    serve(fd, bytes.to_vec());
                } else {
                    tracing::debug!(mime_type, "selection asked for a format it does not offer");
                }
            }
            Served::X11 => {
                if let Err(error) = self.send_to_x11_reader(ty.into(), mime_type, fd) {
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

    #[test]
    fn text_offers_go_by_every_text_name_utf8_first_and_bad_names_are_dropped() {
        let mimes = normalized_mimes(strings(&["text/plain", "image/png", "bad mime", ""]));
        assert_eq!(mimes.first().map(String::as_str), Some(TEXT_MIME));
        for alias in TEXT_MIMES {
            assert!(
                mimes.iter().any(|mime| mime == alias),
                "{alias} missing: {mimes:?}"
            );
        }
        assert!(mimes.iter().any(|mime| mime == "image/png"));
        assert!(
            !mimes
                .iter()
                .any(|mime| mime.contains(' ') || mime.is_empty())
        );
        assert_eq!(
            normalized_mimes(strings(&["image/png"])),
            strings(&["image/png"])
        );
    }

    #[test]
    fn text_is_read_under_any_of_its_names() {
        let values = BTreeMap::from([("text/plain".to_owned(), b"hello".to_vec())]);
        assert_eq!(value_for(&values, "UTF8_STRING"), Some(&b"hello"[..]));
        assert_eq!(value_for(&values, TEXT_MIME), Some(&b"hello"[..]));
        assert_eq!(value_for(&values, "image/png"), None);
    }

    #[test]
    fn hosted_offers_are_bounded_and_empty_ones_offer_nothing() {
        assert_eq!(host_offer(Vec::new()), None);
        assert_eq!(host_offer(vec![("bad mime".into(), b"x".to_vec())]), None);
        let huge = vec![
            ("text/plain".to_owned(), vec![b'a'; MAX_SELECTION_BYTES]),
            ("text/html".to_owned(), vec![b'b'; 16]),
        ];
        let kept = host_offer(huge).expect("the first format fits");
        assert_eq!(
            kept.keys().cloned().collect::<Vec<_>>(),
            strings(&["text/plain"])
        );
    }
}
