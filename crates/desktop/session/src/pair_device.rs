//! "Add a device": the link that makes a new device of the person one of theirs, kept current while
//! its sheet is up.
//!
//! The link is the workspace link to the workspace in front, at the host that keeps it, carrying
//! the person's root and a fresh invite ([`crate::pairing_link`]). One invite is open at a time:
//! asking while it works answers the same link, so a sheet left on screen keeps showing the code
//! that admits the person's device, and asking once it ran out makes a new one, so the code on
//! screen is never one that admits nothing. While a link works, this device waits at its host for
//! the device that opens it ([`crate::certification::watch_host`]). A workspace no host on the web
//! keeps has no link: the sheet shows a ticket this device serves instead
//! ([`crate::device_tickets`]), which the new computer spends with `tilo login`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_lock::MutexRecover as _;

use tilo_sheets::SheetState as _;
use tilo_sheets::pair_device::{AddDevice, AddDeviceState, PairDeviceEffect};

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
    pub workspace: String,
    /// Where the host that keeps the workspace in front is dialled, as its site publishes it.
    pub host: tilo_server_client::ServerAddress,
    pub person: [u8; 32],
}

/// When the site link gives way to a ticket, or pairing is unavailable.
pub(crate) enum NoLink {
    /// No web host keeps the workspace in front: a ticket from this device takes the link's place.
    Nearby,
    /// The catalogue key saying why.
    Because(&'static str),
}

/// The host a link shown now sends a new device to, and until when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Waiting {
    pub origin: String,
    /// Where the host the link names is dialled.
    pub host: tilo_server_client::ServerAddress,
    pub person: [u8; 32],
    pub until_ms: u64,
}

/// The ticket the sheet shows where there is no link: asked of this device's ticket service, then
/// shown until it runs out, or why it could not be had.
enum TicketOffer {
    Asking(Arc<Mutex<Option<Result<String, String>>>>),
    Shown { ticket: String, expires_at_ms: u64 },
    Refused(String),
}

/// How long a ticket admits a computer, as the sheet counts it down.
const TICKET_LIFETIME_MS: u64 = tilo_device_identity::login::TICKET_LIFETIME_NS / 1_000_000;

/// The link the sheet shows, and what its presses do.
pub(crate) struct Adding {
    invites: RefCell<Invites>,
    /// Where a ticket comes from: this device's own ticket service, on a session that takes part
    /// as someone.
    tickets: Option<crate::device_tickets::DeviceTickets>,
    ticket: RefCell<Option<TicketOffer>>,
    clipboard: Rc<dyn Clipboard>,
    /// The link last put on the clipboard, while it is still the one shown: what the copy button
    /// says it did.
    copied: RefCell<Option<String>>,
    /// Where the link last shown sends a new device.
    waiting: RefCell<Option<Waiting>>,
}

impl Adding {
    #[must_use]
    pub(crate) fn new(
        clipboard: Rc<dyn Clipboard>,
        tickets: Option<crate::device_tickets::DeviceTickets>,
    ) -> Self {
        Self {
            invites: RefCell::new(Invites::default()),
            tickets,
            ticket: RefCell::default(),
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
            Err(NoLink::Nearby) => return self.ticket(now_ms),
            Err(NoLink::Because(key)) => return AddDevice::refusing(&tilo_sheets::word(key)),
        };
        match self.invites.borrow_mut().at(
            &place.site,
            &place.origin,
            &place.workspace,
            &place.person,
            now_ms,
        ) {
            Ok(shown) => {
                let mut device =
                    AddDevice::of(&shown.link, minutes_left(shown.expires_at_ms, now_ms));
                if self.copied.borrow().as_deref() == Some(shown.link.as_str()) {
                    device.copy = device.copied.clone();
                }
                *self.waiting.borrow_mut() = Some(Waiting {
                    origin: place.origin,
                    host: place.host,
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

    /// The sheet with this device's ticket at `now_ms`: one is asked for once none is shown or the
    /// one shown ran out, and shown once this device served it.
    fn ticket(&self, now_ms: u64) -> AddDevice {
        let mut offer = self.ticket.borrow_mut();
        let answered = match offer.as_ref() {
            Some(TicketOffer::Asking(answer)) => answer.lock_recover().take(),
            _ => None,
        };
        if let Some(answer) = answered {
            *offer = Some(match answer {
                Ok(ticket) => TicketOffer::Shown {
                    ticket,
                    expires_at_ms: now_ms.saturating_add(TICKET_LIFETIME_MS),
                },
                Err(reason) => TicketOffer::Refused(reason),
            });
        }
        let ran_out = matches!(offer.as_ref(), Some(TicketOffer::Shown { expires_at_ms, .. }) if *expires_at_ms <= now_ms);
        if offer.is_none() || ran_out {
            *offer = Some(self.ask_ticket());
        }
        match offer.as_ref() {
            Some(TicketOffer::Shown {
                ticket,
                expires_at_ms,
            }) => {
                let mut device = AddDevice::ticket(ticket, minutes_left(*expires_at_ms, now_ms));
                if self.copied.borrow().as_deref() == Some(ticket.as_str()) {
                    device.copy = device.copied.clone();
                }
                device
            }
            Some(TicketOffer::Refused(reason)) => AddDevice::refusing(reason),
            Some(TicketOffer::Asking(_)) | None => AddDevice::asking(),
        }
    }

    /// Ask this device's ticket service for a ticket, off the shell's thread.
    fn ask_ticket(&self) -> TicketOffer {
        let Some(tickets) = self.tickets.clone() else {
            return TicketOffer::Refused(tilo_sheets::word("pair-device-not-yours"));
        };
        let runtime = match tilo_rt::try_app() {
            Ok(runtime) => runtime,
            Err(error) => return TicketOffer::Refused(error.to_string()),
        };
        let answer = Arc::new(Mutex::new(None));
        let answered = Arc::clone(&answer);
        drop(runtime.spawn(async move {
            let issued = tickets.issue().await;
            *answered.lock_recover() = Some(issued);
        }));
        TicketOffer::Asking(answer)
    }

    /// Forget a ticket that could not be had, so opening the sheet again asks anew.
    fn forget_refusal(&self) {
        let mut offer = self.ticket.borrow_mut();
        if matches!(offer.as_ref(), Some(TicketOffer::Refused(_))) {
            *offer = None;
        }
    }

    /// When the ticket shown next changes on its own: its minutes count down, or the answer to the
    /// ask for one is looked for again.
    fn ticket_next_in(&self, now_ms: u64) -> Option<Duration> {
        match self.ticket.borrow().as_ref()? {
            TicketOffer::Asking(_) => Some(Duration::from_millis(100)),
            TicketOffer::Shown { expires_at_ms, .. } => Some(next_count_in(*expires_at_ms, now_ms)),
            TicketOffer::Refused(_) => None,
        }
    }

    /// Carry out what the person pressed in the sheet, and say the link is on the clipboard when it
    /// is.
    pub(crate) fn take(&self, state: &AddDeviceState) {
        for effect in state.take_asked() {
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

/// The minutes a link that stops working at `until_ms` still works at `now_ms`, as the sheet
/// counts them: a part of a minute counts whole, so the last minute reads "the next minute".
fn minutes_left(until_ms: u64, now_ms: u64) -> u32 {
    u32::try_from(until_ms.saturating_sub(now_ms).div_ceil(MINUTE_MS)).unwrap_or(u32::MAX)
}

/// How long after `now_ms` the count of a link stopping at `until_ms` goes down by one: at its
/// last minute, the instant it stops working and a new link takes its place.
fn next_count_in(until_ms: u64, now_ms: u64) -> Duration {
    let into_minute = until_ms.saturating_sub(now_ms).saturating_sub(1) % MINUTE_MS;
    Duration::from_millis(into_minute.saturating_add(1))
}

const MINUTE_MS: u64 = 60_000;

impl crate::Shared {
    /// Add a device: the sheet showing the link a new device of this person's becomes one of
    /// theirs, up over the shell.
    pub(crate) fn open_pair_device(&self) {
        let Some(state) = self.pair_device.as_ref() else {
            return;
        };
        self.adding.forget_refusal();
        state.presentation().open(false);
        self.follow_pair_device();
    }

    /// Keep the sheet current while it is up: the link that admits a device, renewed once the one
    /// shown ran out, and what its presses do.
    pub(crate) fn follow_pair_device(&self) {
        if let Some(state) = self.pair_device.as_ref() {
            self.adding.take(state);
        }
        if let Some(state) = self.pair_device.as_ref()
            && state.presentation().is_open()
        {
            state.show(self.pairing_device(ui::time::unix_now_ms()));
        }
        self.wait_for_new_device();
    }

    /// When the sheet next changes on its own while it is up: its minutes count down, and the link
    /// that ran out gives way to a new one.
    pub(crate) fn pair_device_deadline(&self) -> Option<std::time::Instant> {
        let state = self.pair_device.as_ref()?;
        if !state.presentation().is_open() {
            return None;
        }
        let now_ms = ui::time::unix_now_ms();
        let after = if matches!(self.pairing_place(), Err(NoLink::Nearby)) {
            self.adding.ticket_next_in(now_ms)?
        } else {
            next_count_in(self.adding.waiting()?.until_ms, now_ms)
        };
        std::time::Instant::now().checked_add(after)
    }

    /// Wait at the host the link shown names for the device that opens it, until the link runs
    /// out: what asks the person here.
    fn wait_for_new_device(&self) {
        if let (Some(fabric), Some(waiting)) = (self.fabric.as_ref(), self.adding.waiting()) {
            crate::certification::watch_host(
                fabric.as_ref(),
                &waiting.origin,
                waiting.host,
                waiting.person,
                waiting.until_ms,
            );
        }
    }

    fn pairing_device(&self, now_ms: u64) -> AddDevice {
        self.adding.device(self.pairing_place(), now_ms)
    }

    /// Where the link goes: the workspace in front at the web host that keeps it,
    /// and the person whose device a newcomer becomes. Without a web keeper,
    /// the sheet shows a ticket instead.
    fn pairing_place(&self) -> Result<Place, NoLink> {
        // Only a device of the person's answers the new device, on their own devices' fabric.
        let view = self
            .view
            .as_ref()
            .filter(|_| self.fabric.is_some())
            .ok_or(NoLink::Because("pair-device-not-yours"))?;
        let workspace = self
            .handle
            .read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.id().to_string())
            })
            .ok_or(NoLink::Because("pair-device-no-workspace"))?;
        let listed = self.directory_store.listing(&workspace);
        let keeper = listed
            .as_ref()
            .and_then(|listed| listed.entry_url.as_ref())
            .map(|place| place.origin().ascii_serialization());
        let (site, origin) =
            place_of(crate::platform::directory::community_site(), keeper).ok_or(NoLink::Nearby)?;
        // Where that host is reached, as the site or directory that names the entry published it.
        let host = listed
            .and_then(|listed| listed.entry_host)
            .and_then(|host| host.address)
            .map(|address| tilo_server_client::ServerAddress::published(&address.into()))
            .ok_or(NoLink::Because("pair-device-no-host"))?;
        Ok(Place {
            site,
            origin,
            workspace,
            host,
            person: view.person(),
        })
    }
}

/// The page opens at the community site or at the workspace's own web host.
/// A community can provide the page when it is known, while the keeper
/// alone determines the host that holds the workspace.
fn place_of(community: Option<url::Url>, keeper: Option<String>) -> Option<(String, String)> {
    let origin = keeper?;
    let site = community.map_or_else(|| format!("{origin}/"), |site| site.to_string());
    Some((site, origin))
}

#[cfg(test)]
mod tests {

    use std::cell::RefCell;

    use super::*;

    const SITE: &str = "https://tilo.coffee/";
    const ORIGIN: &str = "https://eu1.tilo.coffee";
    const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
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
            workspace: WORKSPACE.to_owned(),
            host: host(),
            person: [0xab; 32],
        }
    }

    fn host() -> tilo_server_client::ServerAddress {
        tilo_server_client::ServerAddress::at(iroh::EndpointAddr::new(
            iroh::SecretKey::from_bytes(&[9; 32]).public(),
        ))
    }

    fn adding() -> (Adding, Rc<Sheets>) {
        let clipboard = Rc::new(Sheets::default());
        let adding = Adding::new(Rc::clone(&clipboard) as Rc<dyn Clipboard>, None);
        (adding, clipboard)
    }

    tilo_test_env::scenario! {
        fn the_pairing_sheet_shows_one_link_and_counts_it_down;

        phase the_sheet_shows_one_link_until_it_runs_out;
        // The sheet shows the link the session made, and the same one on every pass until it runs
        // out: a sheet that redrew a fresh invite each pass would hand out a code that never works.
        {
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

        phase without_a_host_on_the_web_the_sheet_asks_for_a_ticket;
        // A workspace no host on the web keeps: no link, and a session that takes part as nobody
        // has no ticket to show either, and says so rather than show a code that admits nobody.
        {
            let (adding, _) = adding();
            let device = adding.device(Err(NoLink::Nearby), START);
            assert_eq!(device.link, "");
            assert_eq!(
                device.refusal,
                AddDevice::refusing(&tilo_sheets::word("pair-device-not-yours")).refusal
            );
            assert!(adding.waiting().is_none(), "nothing to wait for");
        }

        phase the_link_names_the_host_that_keeps_the_workspace;
        // The web link needs the workspace's actual keeper. A configured
        // community may provide the page even when another host keeps the workspace.
        {
            let community = url::Url::parse(SITE).ok();
            assert_eq!(
                place_of(community.clone(), Some("https://site.example".to_owned())),
                Some((SITE.to_owned(), "https://site.example".to_owned()))
            );
            assert_eq!(
                place_of(community, None),
                None
            );
            assert_eq!(
                place_of(None, Some("https://site.example".to_owned())),
                Some((
                    "https://site.example/".to_owned(),
                    "https://site.example".to_owned()
                ))
            );
            assert_eq!(place_of(None, None), None);
        }

        phase a_shown_link_is_waited_for_until_it_runs_out;
        // A link shown is one this device waits at its host for, until it runs out.
        {
            let (adding, _) = adding();
            let shown = adding.device(Ok(place()), START);
            assert!(shown.refusal.is_none());
            assert_eq!(
                adding.waiting(),
                Some(Waiting {
                    origin: ORIGIN.to_owned(),
                    host: host(),
                    person: [0xab; 32],
                    until_ms: START + 600_000,
                })
            );
        }

        phase the_sheet_counts_down_to_a_new_link;
        // The minutes on the sheet count down while it is up: a pass is due the instant the count
        // drops, and the last minute ends when the link does, where a new one takes its place.
        {
            let until = START + 600_000;
            for (now, minutes, next) in [(0, 10, 60), (30, 10, 30), (60, 9, 60), (590, 1, 10)] {
                assert_eq!(
                    minutes_left(until, START + now * 1_000),
                    minutes,
                    "at {now} s"
                );
                let next_in = next_count_in(until, START + now * 1_000);
                assert_eq!(next_in, Duration::from_secs(next), "at {now} s");
            }
            let (adding, _) = adding();
            let first = adding.device(Ok(place()), START);
            assert_eq!(first.lasting, AddDevice::of(&first.link, 10).lasting);
            let later = adding.device(Ok(place()), START + 60_000);
            assert_eq!(later.lasting, AddDevice::of(&first.link, 9).lasting);
        }
    }
}
