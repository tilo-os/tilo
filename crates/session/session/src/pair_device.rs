//! "Add a device": the link that makes a new device of the person one of theirs, kept current while
//! its sheet is up.
//!
//! The link is the workspace link to the workspace in front, at the host that keeps it, carrying
//! the person's root and a fresh invite ([`crate::pairing_link`]). One invite is open at a time:
//! asking while it works answers the same link, so a sheet left on screen keeps showing the code
//! that admits the person's device, and asking once it ran out makes a new one, so the code on
//! screen is never one that admits nothing. While a link works, this device waits at its host for
//! the device that opens it ([`crate::certification::watch_host`]). A workspace no host on the web
//! keeps has no link: the sheet says to add the device nearby.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use tilo_sheets::Seat as _;
use tilo_sheets::pair_device::{AddDevice, AddDeviceSeat, PairDeviceEffect};

use crate::pairing_link::Invites;
use crate::platform::clipboard::Clipboard;

/// How long a link the sheet copied stays on the clipboard: long enough to open it on the new
/// device or paste it into a message to yourself, and no longer, because the link is what admits
/// the device.
const ON_CLIPBOARD_FOR: Duration = Duration::from_secs(300);

/// Where a pairing link goes: the page it opens on, the host that keeps the workspace in front, that
/// workspace, and the person whose device a newcomer becomes.
pub(crate) struct Place {
    pub site: String,
    pub origin: String,
    pub room: String,
    pub person: [u8; 32],
}

/// Why there is no link to show: the device is added nearby instead, or why not even that.
pub(crate) enum NoLink {
    /// No host on the web keeps the workspace in front, so a link would lead nowhere.
    Nearby,
    /// The catalogue key saying why.
    Because(&'static str),
}

/// The host a link shown now sends a new device to, and until when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Waiting {
    pub origin: String,
    pub person: [u8; 32],
    pub until_ms: u64,
}

/// The link the sheet shows, and what its presses do.
pub(crate) struct Adding {
    invites: RefCell<Invites>,
    clipboard: Rc<dyn Clipboard>,
    /// The link last put on the clipboard, while it is still the one shown: what the copy button
    /// says it did.
    copied: RefCell<Option<String>>,
    /// Where the link last shown sends a new device.
    waiting: RefCell<Option<Waiting>>,
}

impl Adding {
    #[must_use]
    pub(crate) fn new(clipboard: Rc<dyn Clipboard>) -> Self {
        Self {
            invites: RefCell::new(Invites::default()),
            clipboard,
            copied: RefCell::default(),
            waiting: RefCell::default(),
        }
    }

    /// Where the link last shown sends a new device, and until when.
    pub(crate) fn waiting(&self) -> Option<Waiting> {
        self.waiting.borrow().clone()
    }

    /// The sheet at `now_ms`: the link that admits a device, or a fresh one once the one shown ran
    /// out, or why no link can be made here.
    #[must_use]
    pub(crate) fn device(&self, place: Result<Place, NoLink>, now_ms: u64) -> AddDevice {
        let place = match place {
            Ok(place) => place,
            Err(NoLink::Nearby) => return AddDevice::nearby(),
            Err(NoLink::Because(key)) => return AddDevice::refusing(&tilo_sheets::word(key)),
        };
        match self.invites.borrow_mut().at(
            &place.site,
            &place.origin,
            &place.room,
            &place.person,
            now_ms,
        ) {
            Ok(shown) => {
                let left = shown.expires_at_ms.saturating_sub(now_ms) / 1_000;
                let left = i64::try_from(left).unwrap_or(i64::MAX);
                let mut device = AddDevice::of(&shown.link, &ui::format::relative_ahead(left));
                if self.copied.borrow().as_deref() == Some(shown.link.as_str()) {
                    device.copy = device.copied.clone();
                }
                *self.waiting.borrow_mut() = Some(Waiting {
                    origin: place.origin,
                    person: place.person,
                    until_ms: shown.expires_at_ms,
                });
                device
            }
            Err(reason) => {
                tracing::warn!(%reason, "no pairing link could be made");
                AddDevice::refusing(&tilo_sheets::word("pair-device-no-link"))
            }
        }
    }

    /// Carry out what the person pressed in the sheet, and say the link is on the clipboard when it
    /// is.
    pub(crate) fn take(&self, seat: &AddDeviceSeat) {
        for effect in seat.take_asked() {
            match effect {
                PairDeviceEffect::Copy { link } => {
                    self.clipboard.copy(link.clone(), ON_CLIPBOARD_FOR);
                    *self.copied.borrow_mut() = Some(link);
                }
                // Its own close: the sheet took itself down as it asked.
                PairDeviceEffect::Done => {}
            }
        }
    }
}

/// Serve the sheet for one pass: what it shows at `now_ms`, while it is up.
pub(crate) fn follow(
    seat: Option<&AddDeviceSeat>,
    adding: &Adding,
    place: impl FnOnce() -> Result<Place, NoLink>,
    now_ms: u64,
) {
    let Some(seat) = seat else {
        return;
    };
    adding.take(seat);
    if !seat.presentation().is_open() {
        return;
    }
    seat.show(adding.device(place(), now_ms));
}

impl crate::Shared {
    /// Add a device: the sheet showing the link a new device of this person's becomes one of
    /// theirs, up over the desk.
    pub(crate) fn open_pair_device(&self) {
        let Some(seat) = self.pair_device.as_ref() else {
            return;
        };
        let now = ui::time::unix_now_ms();
        seat.show(self.adding.device(self.pairing_place(), now));
        seat.presentation().open(false);
        self.wait_for_new_device();
    }

    /// Keep the sheet current while it is up: the link that admits a device, renewed once the one
    /// shown ran out, and what its presses do.
    pub(crate) fn follow_pair_device(&self) {
        crate::pair_device::follow(
            self.pair_device.as_ref(),
            &self.adding,
            || self.pairing_place(),
            ui::time::unix_now_ms(),
        );
        self.wait_for_new_device();
    }

    /// Wait at the host the link shown names for the device that opens it, until the link runs
    /// out: what asks the person here.
    fn wait_for_new_device(&self) {
        if let (Some(fabric), Some(waiting)) = (self.fabric.as_ref(), self.adding.waiting()) {
            crate::certification::watch_host(
                fabric.as_ref(),
                &waiting.origin,
                waiting.person,
                waiting.until_ms,
            );
        }
    }

    /// Where the link goes: the workspace in front, at the host that keeps it as the person's
    /// directory lists it, else at the community this computer is set up for, and the person whose
    /// device a newcomer becomes. Without either, the device is added nearby.
    fn pairing_place(&self) -> Result<Place, NoLink> {
        // Only a device of the person's answers the new device, on their own devices' fabric.
        let engine = self
            .engine
            .as_ref()
            .filter(|_| self.fabric.is_some())
            .ok_or(NoLink::Because("pair-device-not-yours"))?;
        let room = self
            .handle
            .read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.id().to_string())
            })
            .ok_or(NoLink::Because("pair-device-no-workspace"))?;
        let keeper = self
            .directory_store
            .listing(&room)
            .and_then(|listed| listed.door)
            .map(|door| door.origin().ascii_serialization());
        let (site, origin) =
            place_of(crate::platform::directory::community_site(), keeper).ok_or(NoLink::Nearby)?;
        Ok(Place {
            site,
            origin,
            room,
            person: engine.identity().device_cert.user_public_key,
        })
    }
}

/// The page a link opens on and the host it sends a new device to: the host that keeps the
/// workspace (`keeper`), else the community `community`; the page is the community's, or the
/// host's own. `None` when neither is known.
fn place_of(community: Option<url::Url>, keeper: Option<String>) -> Option<(String, String)> {
    let origin = keeper.or_else(|| {
        community
            .as_ref()
            .map(|site| site.origin().ascii_serialization())
    })?;
    let site = community.map_or_else(|| format!("{origin}/"), |site| site.to_string());
    Some((site, origin))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a failed step in a test is the assertion"
    )]

    use std::cell::RefCell;

    use super::*;

    const SITE: &str = "https://tilo.coffee/";
    const ORIGIN: &str = "https://eu1.tilo.coffee";
    const ROOM: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
    const START: u64 = 1_700_000_000_000;

    /// A clipboard that keeps what it was given.
    #[derive(Default)]
    struct Sheets(RefCell<Vec<String>>);

    impl Clipboard for Sheets {
        fn copy(&self, text: String, _kept_for: Duration) {
            self.0.borrow_mut().push(text);
        }
    }

    fn place() -> Place {
        Place {
            site: SITE.to_owned(),
            origin: ORIGIN.to_owned(),
            room: ROOM.to_owned(),
            person: [0xab; 32],
        }
    }

    fn adding() -> (Adding, Rc<Sheets>) {
        let clipboard = Rc::new(Sheets::default());
        let adding = Adding::new(Rc::clone(&clipboard) as Rc<dyn Clipboard>);
        (adding, clipboard)
    }

    /// The sheet shows the link the session made, and the same one on every pass until it runs
    /// out: a sheet that redrew a fresh invite each pass would hand out a code that never works.
    #[test]
    fn the_sheet_shows_one_link_until_it_runs_out() {
        let (adding, _) = adding();
        let first = adding.device(Ok(place()), START);
        assert!(
            first
                .link
                .starts_with("https://tilo.coffee/#/@eu1.tilo.coffee/w/")
        );
        assert!(first.link.contains("?pair=abab"), "{}", first.link);
        let again = adding.device(Ok(place()), START + 60_000);
        assert_eq!(again.link, first.link);
        let renewed = adding.device(Ok(place()), START + 600_000);
        assert_ne!(renewed.link, first.link);
    }

    /// A workspace no host on the web keeps: no link, and the sheet says to add the device nearby
    /// rather than showing a code that admits nobody.
    #[test]
    fn without_a_host_on_the_web_the_sheet_offers_nearby() {
        let (adding, _) = adding();
        let device = adding.device(Err(NoLink::Nearby), START);
        assert_eq!(device.link, "");
        assert_eq!(device.refusal, AddDevice::nearby().refusal);
        assert!(adding.waiting().is_none(), "nothing to wait for");
    }

    /// The host that keeps the workspace is the one the link names; the community this computer
    /// is set up for only when none is known.
    #[test]
    fn the_link_names_the_host_that_keeps_the_workspace() {
        let community = url::Url::parse(SITE).ok();
        assert_eq!(
            place_of(community.clone(), Some("https://door.example".to_owned())),
            Some((SITE.to_owned(), "https://door.example".to_owned()))
        );
        assert_eq!(
            place_of(community, None),
            Some((SITE.to_owned(), "https://tilo.coffee".to_owned()))
        );
        assert_eq!(
            place_of(None, Some("https://door.example".to_owned())),
            Some((
                "https://door.example/".to_owned(),
                "https://door.example".to_owned()
            ))
        );
        assert_eq!(place_of(None, None), None);
    }

    /// A link shown is one this device waits at its host for, until it runs out.
    #[test]
    fn a_shown_link_is_waited_for_until_it_runs_out() {
        let (adding, _) = adding();
        let shown = adding.device(Ok(place()), START);
        assert!(shown.refusal.is_none());
        assert_eq!(
            adding.waiting(),
            Some(Waiting {
                origin: ORIGIN.to_owned(),
                person: [0xab; 32],
                until_ms: START + 600_000,
            })
        );
    }

    /// The sheet the session hands the seat is the one it shows: a press copies exactly the link
    /// on screen, never one that was replaced under it.
    #[test]
    fn the_seat_shows_the_link_the_session_made() {
        let (adding, _) = adding();
        let seat = AddDeviceSeat::default();
        seat.presentation().open(false);
        follow(Some(&seat), &adding, || Ok(place()), START);
        let shown = seat.device();
        assert_eq!(shown, adding.device(Ok(place()), START));
        assert!(shown.link.contains("?pair="));
    }
}
