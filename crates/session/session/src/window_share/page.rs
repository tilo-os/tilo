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

use iroh::EndpointId;
use tilo_desktop_model::TabId;
use tilo_desktop_surface::{DesktopHandle, Standing};
use tilo_live::page::PageObject;

use super::WindowShares;

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
}
