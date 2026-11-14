//! A shared window's page on the device that shows it, and what the tile of a shared window holds.
//!
//! A window whose app draws its page travels as that page ([`crate::page_track`]): its host
//! announces a stream whose codec is the display list, and the page's objects come over the
//! workspace's live plane on a track of their own. The plane opens what it carried and hands each
//! object over whole, its header on it; the tile takes the payload, as the publisher's engine
//! encoded it.
//!
//! A stream's start says nothing of what is on the screen. The tile it makes shows an arriving
//! card until a picture is decoded or a page's whole frame came, so whether a window is pictured
//! is asked of its tile, every pass, and never read off the start.

use std::time::{Duration, Instant};

use iroh::EndpointId;
use tilo_desktop_model::TabId;
use tilo_desktop_surface::{DesktopHandle, Standing};
use tilo_live::live::Track;
use tilo_live::page::PageObject;

use super::WindowShares;

/// How long a page this device asked the plane for may take to arrive before the reading says the
/// device that runs its window sent none: what that device gives its app to hand the page over
/// ([`crate::page_track::HAND_OVER_BOUND`]), and five seconds for the ask and the page to cross
/// the plane, so this side never says it before the other one can.
pub(super) const ARRIVAL_BOUND: Duration =
    crate::page_track::HAND_OVER_BOUND.saturating_add(Duration::from_secs(5));

/// How this device stands with a window's page it asks the plane for and holds nothing of.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Awaited {
    /// It asks for none, or its tile holds the page.
    #[default]
    Nothing,
    /// It asks since then, and the page has [`ARRIVAL_BOUND`] from then to come.
    Since(Instant),
    /// The page did not come in that time: the device that runs the window was asked and sent
    /// none.
    Overdue,
}

impl WindowShares {
    /// One object of the page of the window `tab` the device `from` shares, as the plane opened
    /// it: its payload goes to the tile that draws that window.
    pub(super) fn page_heard(
        &self,
        desk: &DesktopHandle,
        from: EndpointId,
        tab: TabId,
        body: &[u8],
    ) {
        let Some(stream) = self
            .received
            .values()
            .find(|stream| stream.page && stream.tab == tab && stream.host == from)
        else {
            return;
        };
        let Ok(object) = tilo_failure::ignored!(
            "the plane opened only what its relay validated as a page object",
            PageObject::decode(body)
        ) else {
            return;
        };
        desk.peer_surface_page(
            neighbour_wire::peer_handle(from.as_bytes()),
            stream.start.surface,
            &object.payload,
        );
    }

    /// Ask the tile of each window this device shows whether it holds the window now, so the
    /// reading says a window is pictured only while its tile draws it.
    pub(super) fn follow_pictured(&mut self, desk: &DesktopHandle) {
        let mut changed = false;
        for ((peer, surface), stream) in &mut self.received {
            let pictured = stream.shown
                && desk
                    .peer_stream_read(*peer, *surface)
                    .is_some_and(|tile| tile.standing == Standing::Pictured);
            changed |= pictured != stream.pictured;
            stream.pictured = pictured;
        }
        if changed {
            self.changed();
        }
    }

    /// Note which pages this device asks the plane for and holds nothing of for longer than a
    /// page takes to come ([`ARRIVAL_BOUND`]), so the reading says which side such a window waits
    /// on: this device asked, and the one that runs the window sent no page.
    pub(super) fn follow_overdue(&mut self, now: Instant) {
        let mut changed = false;
        let mut due: Option<Instant> = None;
        for stream in self.received.values_mut() {
            let asks = stream.page
                && !stream.pictured
                && self.shown_tracks.values().any(|hosts| {
                    hosts
                        .get(&stream.host)
                        .is_some_and(|tracks| tracks.contains(&Track::Page(stream.tab)))
                });
            let awaited = match stream.awaited {
                _ if !asks => Awaited::Nothing,
                Awaited::Nothing => Awaited::Since(now),
                Awaited::Since(since) if now.saturating_duration_since(since) >= ARRIVAL_BOUND => {
                    Awaited::Overdue
                }
                kept => kept,
            };
            changed |= (awaited == Awaited::Overdue) != (stream.awaited == Awaited::Overdue);
            stream.awaited = awaited;
            if let Awaited::Since(since) = awaited {
                due = match (due, since.checked_add(ARRIVAL_BOUND)) {
                    (Some(first), Some(this)) => Some(first.min(this)),
                    (first, this) => first.or(this),
                };
            }
        }
        self.arrival_due = due;
        if changed {
            self.changed();
        }
    }
}
