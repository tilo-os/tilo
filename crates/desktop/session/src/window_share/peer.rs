//! Another device's shared window, as the session decodes it.
//!
//! A peer shares a window ([`PeerStreamStart`], what its keyframes say of the pictures, or a
//! page's announcement); this session decodes it into that window's tab:
//!
//! - **One decoder per shared surface.** [`PeerSurface`] owns the surface's [`VideoDecoder`],
//!   replaced when the sender restarts the stream, and feeds it only the units that continue its
//!   reference chain ([`UnitChain`]). A gap asks the sender for a keyframe, again every
//!   [`KEYFRAME_REQUEST_RETRY`] until it answers. Decoding never delays the stream: a picture
//!   still coming is collected on a later unit ([`PeerSurface::collect_late_picture`]), or when
//!   the decoder's [`PictureWake`] wakes the host.
//! - **The state is one value per tile.** Whose window it is, whether this viewer may type into
//!   it, how the stream stands and, once it is here, the newest picture: [`PeerSurface::read`],
//!   what the host reads to show the window.
//! - **What a viewer asks of the sender leaves as messages**: the keyframe a broken chain needs,
//!   kept for whoever sends to the owner ([`PeerSurfaces::take_outgoing`]).
//!
//! A tab whose window runs on another device never stands empty. While no share stands in it, and
//! once the share in it ended, it says where the window runs and why it is not here
//! ([`Elsewhere`], [`WhyEnded`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use media::realtime::PictureWake;
use neighbour_wire::colorimetry::Colorimetry;
use neighbour_wire::hdr_metadata::Hdr10Metadata;
use neighbour_wire::unit_chain::{Admission, UnitChain};
use neighbour_wire::{NeighbourMsg, PeerId, SurfaceId};
use tilo_callstream::surface_decoder::{DecodedFrame, DecodedPixels, VideoDecoder};
use tilo_color_pipeline::color_pipeline::{OutputColorState, SurfaceColorOp, plan_surface_op};
use tilo_color_pipeline::hdr_display::DisplayCapabilities;
use tilo_color_pipeline::hdr_tonemap::{HdrTonemapConfig, tonemap_p010_to_rgba8};
use tilo_desktop_model::{Layout, Tab, TabId, TileContent as ModelTile, Workspace};
use tilo_session_core::desktop::state::{DesktopHandle, Shared, route_of};
use ui::time::{Duration, Instant};
use ui::webview::paint::stream::wire::{CODEC, Payload, Receiver};

use super::elsewhere::Elsewhere;

/// Decoding never delays the stream: a unit's picture comes out on a later one.
const PICTURE_WAIT: Duration = Duration::ZERO;
/// How long a picture still coming is awaited after its unit was pushed. Past it the stream is not
/// merely slow, and the next unit is the one to wait on.
const LATE_PICTURE_WINDOW: Duration = Duration::from_secs(1);
/// How long an unanswered keyframe request waits before it is asked again.
const KEYFRAME_REQUEST_RETRY: Duration = Duration::from_millis(300);

/// What became of one encoded unit pushed at a tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerOutcome {
    /// Decoded, and now the tile's newest picture.
    Decoded,
    /// The decoder took the unit and its picture is still coming. Nothing is recorded yet: the
    /// picture arrives with a later unit, or through [`PeerSurface::collect_late_picture`].
    Pending,
    /// The decoder rejected the unit. The deltas after it wait for a keyframe, as for
    /// [`Self::AwaitingKeyframe`].
    DecodeFailed,
    /// A delta whose reference chain is broken: a unit before it never arrived, or the stream has
    /// not reached its first keyframe.
    AwaitingKeyframe,
    /// A delta older than one already decoded, overtaken on the way or repeated.
    Stale,
    /// The share ended; nothing is decoded any more.
    Ended,
}

/// How a stream stands, as its tile publishes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamState {
    /// The sender announced the stream; no picture has arrived yet.
    Started,
    /// A picture is in the tile.
    Pictured,
    /// The share ended, and why.
    Ended {
        /// Why the share ended.
        why: WhyEnded,
    },
}

/// Why a shared window's tile ended. The tile keeps the reason and not a sentence, so whoever
/// reads it later reads it in their language now, whatever language it ended in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhyEnded {
    /// The tab it stood in is no longer on the shell, or shows something else.
    TabGone,
    /// This device left the workspace the window is shared with.
    LeftWorkspace,
    /// The device sharing the window stopped.
    SenderStopped,
    /// The sender started the stream over at another size or in another codec.
    Restarted,
    /// This device let the window go.
    LetGo,
}

/// A surface another device announced it streams, and what this desktop knows of it beyond the
/// announcement itself.
#[derive(Debug, Clone)]
pub struct PeerStreamStart {
    /// The shared surface, as the sender names it.
    pub surface: SurfaceId,
    /// The device sharing the window.
    pub peer: PeerId,
    /// This device, as the sender knows whoever sends input back.
    pub local_peer: PeerId,
    /// The picture size the sender announced.
    pub width: u32,
    /// The picture size the sender announced.
    pub height: u32,
    /// The codec label the sender chose (`"h264"`, `"vp9"`, `"av1"`,
    /// `"hevc-main10"`).
    pub codec: String,
    /// Codec target bit depth; `0` when the sender does not say.
    pub bit_depth: u8,
    /// What the sender announced its pictures to be; `None` means SDR BT.709.
    pub colorimetry: Option<Colorimetry>,
    /// The HDR10 static metadata of an HDR stream.
    #[expect(
        dead_code,
        reason = "carried with the announce for a display that reads HDR metadata; the decoder tone-maps from the colourimetry alone"
    )]
    pub hdr: Option<Hdr10Metadata>,
    /// The app whose window is shared.
    pub app_id: String,
    /// The tab the sender shows the window in, the same id on every device of the workspace; `0`
    /// when it names none.
    pub tab: TabId,
    /// The sharing device's name, as the person knows it.
    pub sharer: String,
    /// What the tile calls the window.
    pub title: String,
    /// Whether this viewer may type into and click the window.
    pub writable: bool,
}

impl PeerStreamStart {
    /// The start a [`NeighbourMsg::SurfaceStreamStart`] announces, with what
    /// only this desktop knows: who it is to the sender, whose device it is,
    /// what the share lets it do, and what the tile calls the window. `None`
    /// for any other message.
    pub fn announced(
        message: &NeighbourMsg,
        local_peer: PeerId,
        sharer: impl Into<String>,
        title: impl Into<String>,
        writable: bool,
    ) -> Option<Self> {
        let NeighbourMsg::SurfaceStreamStart {
            surface,
            source_peer,
            width,
            height,
            codec,
            colorimetry,
            hdr_static_metadata,
            bit_depth,
            app_id,
            tab,
            ..
        } = message
        else {
            return None;
        };
        let app_id = app_id.clone();
        let title = title.into();
        Some(Self {
            surface: *surface,
            peer: *source_peer,
            local_peer,
            width: *width,
            height: *height,
            codec: codec.clone(),
            bit_depth: *bit_depth,
            colorimetry: *colorimetry,
            hdr: *hdr_static_metadata,
            app_id: app_id.clone(),
            tab: *tab,
            sharer: sharer.into(),
            title: if title.trim().is_empty() {
                app_id
            } else {
                title
            },
            writable,
        })
    }
}

/// What a tile publishes as its state, read by whoever shows it: a view, a test, or a host that
/// names the window.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerRead {
    /// The tab the tile stands in.
    pub tab: TabId,
    /// The device sharing the window.
    pub peer: PeerId,
    /// The shared surface.
    pub surface: SurfaceId,
    /// The sharing device's name.
    pub sharer: String,
    /// The app whose window is shared.
    pub app_id: String,
    /// What the tile calls the window.
    pub title: String,
    /// Whether this viewer may type into and click the window.
    pub writable: bool,
    /// How the stream stands.
    pub state: StreamState,
    /// A unit went missing and the sender has been asked for a keyframe.
    pub awaiting_keyframe: bool,
    /// Whether the window's page is what this tile draws, rather than a picture of it.
    pub page: bool,
    /// The newest picture's width and height, and the RGBA bytes it carries.
    pub picture: Option<(u32, u32, usize)>,
    /// The unit the newest picture came from.
    pub picture_seq: u64,
    /// The colour of the display the tile stands on, which its pictures are prepared for.
    pub display: DisplayCapabilities,
}

impl PeerRead {
    /// Whether the tile has a picture to show.
    #[must_use]
    pub fn pictured(&self) -> bool {
        self.picture.is_some()
    }
}

/// One picture, as the tile holds it.
#[derive(Debug, Clone)]
struct Picture {
    rgba: Arc<[u8]>,
    width: u32,
    height: u32,
}

/// The page of a shared window as a viewer holds it: the frame the publisher holds, and the
/// number of the last whole frame taken, so a payload that filled a gap is told from one that
/// changed what the tile shows.
struct PageStream {
    receiver: Receiver,
    seq: Option<u64>,
}

impl PageStream {
    fn new() -> Self {
        Self {
            receiver: Receiver::new(),
            seq: None,
        }
    }

    /// Take one object of the page: the payload as its publisher encoded it. Whether the frame
    /// the tile holds changed.
    fn apply(&mut self, payload: &[u8]) -> bool {
        let Ok(payload) = Payload::decode(payload) else {
            return false;
        };
        self.receiver.apply(payload);
        let seq = self.receiver.frame().map(|frame| frame.seq);
        let changed = seq.is_some() && seq != self.seq;
        self.seq = seq;
        changed
    }
}

/// One shared surface this desktop shows: its stream, and the state its tile publishes.
pub struct PeerSurface {
    tab: TabId,
    peer: PeerId,
    surface: SurfaceId,
    local_peer: PeerId,
    sharer: Arc<str>,
    app_id: Arc<str>,
    title: Arc<str>,
    writable: bool,
    state: StreamState,
    awaiting_keyframe: bool,
    picture: Option<Picture>,
    picture_seq: u64,
    revision: u64,
    decoder: VideoDecoder,
    /// What the stream's pictures are, so a change of display re-plans the tone-map.
    colorimetry: Colorimetry,
    /// The colour of the display this tile stands on.
    display: DisplayCapabilities,
    /// The tone-map an HDR stream runs through before it becomes a picture;
    /// `None` when its pixels are already what this display shows.
    tonemap: Option<HdrTonemapConfig>,
    /// Which units continue the decoder's reference chain.
    chain: UnitChain,
    /// The newest unit the chain admitted, named when a keyframe is asked for.
    last_seq: u64,
    /// Until when the picture of the latest unit is still awaited, while one is still coming.
    late_until: Option<Instant>,
    keyframe_asked: Option<Instant>,
    ended: bool,
    /// What the tile asks its sender to do: keyframe requests.
    outgoing: Vec<NeighbourMsg>,
    /// The window's page, when its sender publishes one (`tilo-paint`): the display list drawn
    /// here instead of a picture decoded here.
    page: Option<PageStream>,
}

impl PeerSurface {
    /// The tile for `start` . Its pictures are prepared for SDR until the host says which
    /// display the tab stands on, which it does in the pass the tile is made in.
    fn new(start: &PeerStreamStart, tab: TabId) -> Self {
        let colorimetry = start.colorimetry.unwrap_or_default();
        let display = DisplayCapabilities::sdr_default();
        let tonemap = tonemap_for(colorimetry, display);
        // A codec this build cannot decode still shows the window and its name: the stream stays in
        // `Started` , and its units decode nothing.
        let mut decoder = VideoDecoder::for_stream(&start.codec, start.bit_depth)
            .unwrap_or_else(|_| VideoDecoder::h264());
        decoder.set_picture_wait(PICTURE_WAIT);
        decoder.set_stream_colorimetry(Some(colorimetry));
        decoder.set_display_size(Some((start.width, start.height)));
        Self {
            tab,
            peer: start.peer,
            surface: start.surface,
            local_peer: start.local_peer,
            sharer: Arc::from(start.sharer.as_str()),
            app_id: Arc::from(start.app_id.as_str()),
            title: Arc::from(start.title.as_str()),
            writable: start.writable,
            state: StreamState::Started,
            awaiting_keyframe: false,
            picture: None,
            picture_seq: 0,
            revision: 1,
            decoder,
            colorimetry,
            display,
            tonemap,
            chain: UnitChain::default(),
            last_seq: 0,
            late_until: None,
            keyframe_asked: None,
            ended: false,
            outgoing: Vec::new(),
            page: (start.codec == CODEC).then(PageStream::new),
        }
    }

    /// The tile moved to a display showing `display` , or that display's colour changed: pictures
    /// decoded from now on are prepared for it.
    fn set_display(&mut self, display: DisplayCapabilities) {
        if self.display == display {
            return;
        }
        self.display = display;
        self.tonemap = tonemap_for(self.colorimetry, display);
    }

    /// Changes whenever anything the tile shows changes: a picture, the stream's state, a
    /// keyframe asked for.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// What the tile publishes, as one value.
    #[must_use]
    pub fn read(&self) -> PeerRead {
        PeerRead {
            tab: self.tab,
            peer: self.peer,
            surface: self.surface,
            sharer: self.sharer.to_string(),
            app_id: self.app_id.to_string(),
            title: self.title.to_string(),
            writable: self.writable,
            state: self.state.clone(),
            awaiting_keyframe: self.awaiting_keyframe,
            page: self.page.is_some(),
            picture: self
                .picture
                .as_ref()
                .map(|picture| (picture.width, picture.height, picture.rgba.len())),
            picture_seq: self.picture_seq,
            display: self.display,
        }
    }

    /// Take one encoded unit of the stream: decode it into the tile's newest picture, or hold it
    /// back when it does not continue the decoder's reference chain.
    pub fn push(&mut self, seq: u64, unit: &[u8]) -> PeerOutcome {
        if self.ended {
            return PeerOutcome::Ended;
        }
        let keyframe = tilo_callstream::is_keyframe(self.decoder.codec(), unit);
        match self.chain.admit(seq, keyframe) {
            Admission::Decode => {}
            Admission::Stale => return PeerOutcome::Stale,
            Admission::AwaitingKeyframe { broke } => {
                self.request_keyframe(seq, broke);
                return PeerOutcome::AwaitingKeyframe;
            }
        }
        if keyframe {
            self.keyframe_asked = None;
            self.awaiting_keyframe = false;
        }
        self.last_seq = seq;
        let decoded = self.decoder.decode(unit);
        let pending = self.decoder.picture_pending();
        self.late_until = if pending {
            ui::time::now().checked_add(LATE_PICTURE_WINDOW)
        } else {
            None
        };
        let Some(frame) = decoded else {
            if pending {
                return PeerOutcome::Pending;
            }
            // What the decoder kept of a rejected unit is no base for the deltas after it.
            let broke = self.chain.intact();
            self.chain.lose();
            self.request_keyframe(seq, broke);
            return PeerOutcome::DecodeFailed;
        };
        if self.accept(frame) {
            PeerOutcome::Decoded
        } else {
            PeerOutcome::DecodeFailed
        }
    }

    /// Take one object of the window's page: the payload its publisher's engine encoded. Whether
    /// the frame the tile holds changed.
    pub fn push_page(&mut self, payload: &[u8]) -> bool {
        if self.ended {
            return false;
        }
        let Some(page) = self.page.as_mut() else {
            return false;
        };
        let changed = page.apply(payload);
        if changed {
            self.state = StreamState::Pictured;
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    /// Show the picture of an earlier unit that finished after its push returned, without waiting
    /// for another: a window that stopped changing would otherwise keep its last picture inside the
    /// decoder. `None` when none has finished.
    pub fn collect_late_picture(&mut self) -> Option<PeerOutcome> {
        if self.ended {
            return None;
        }
        let frame = self.decoder.take_late_frame();
        if !self.decoder.picture_pending() {
            self.late_until = None;
        }
        self.accept(frame?).then_some(PeerOutcome::Decoded)
    }

    /// The deadline at which the tile must be looked at again without a unit of its own: a picture
    /// still owed past [`LATE_PICTURE_WINDOW`], and a decoder still starting up, which announces
    /// nothing when it gives up. A running stream's picture wakes the host instead
    /// ([`PeerSurfaces::set_picture_wake`]).
    #[must_use]
    pub fn next_wake(&self) -> Option<Instant> {
        if !self.decoder.picture_pending() {
            return None;
        }
        let startup = self.decoder.startup_deadline();
        let late = self.late_until;
        match (startup, late) {
            (Some(startup), Some(late)) => Some(startup.min(late)),
            (startup, late) => startup.or(late),
        }
    }

    /// Accept a decoded picture as the tile's newest. Whether it is one a picture can be made of.
    fn accept(&mut self, frame: DecodedFrame) -> bool {
        let rgba = match frame.pixels {
            DecodedPixels::Rgba8(bytes) => bytes,
            DecodedPixels::P010 {
                y,
                uv,
                y_stride,
                uv_stride,
            } => tonemap_p010_to_rgba8(
                &y,
                &uv,
                y_stride,
                uv_stride,
                frame.width,
                frame.height,
                frame.colorimetry,
                self.tonemap.unwrap_or_default(),
            ),
        };
        if rgba.len() != frame_pixels(frame.width, frame.height) {
            // A picture of no size a tile could hold is no picture: the tile keeps the one it has
            // and the stream waits for a keyframe, as for any unit the decoder could not use.
            let broke = self.chain.intact();
            self.chain.lose();
            self.request_keyframe(self.last_seq, broke);
            return false;
        }
        self.picture = Some(Picture {
            rgba: Arc::from(rgba),
            width: frame.width,
            height: frame.height,
        });
        self.picture_seq = self.last_seq;
        self.state = StreamState::Pictured;
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// Ask the sender for a keyframe: at once when the chain just broke, and again once the last
    /// request has gone unanswered for [`KEYFRAME_REQUEST_RETRY`].
    fn request_keyframe(&mut self, seq: u64, broke: bool) {
        let due = broke
            || self
                .keyframe_asked
                .is_none_or(|asked| asked.elapsed() >= KEYFRAME_REQUEST_RETRY);
        if !due {
            return;
        }
        self.keyframe_asked = Some(ui::time::now());
        self.awaiting_keyframe = true;
        self.revision = self.revision.wrapping_add(1);
        self.outgoing.push(NeighbourMsg::SurfaceKeyframeRequest {
            peer: self.local_peer,
            surface: self.surface,
            seq,
        });
    }

    /// End the tile's share: nothing is decoded after this, and what the tile publishes says why.
    pub fn end(&mut self, why: WhyEnded) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.state = StreamState::Ended { why };
        self.revision = self.revision.wrapping_add(1);
    }

    /// What this tile asks its sender to do, in order.
    pub fn take_outgoing(&mut self) -> Vec<NeighbourMsg> {
        std::mem::take(&mut self.outgoing)
    }
}

/// The tone-map a stream needs to become a picture on `display` : only an HDR stream on an SDR
/// output needs one.
fn tonemap_for(colorimetry: Colorimetry, display: DisplayCapabilities) -> Option<HdrTonemapConfig> {
    let output = OutputColorState::from_display(&display);
    match plan_surface_op(colorimetry, &output, None) {
        SurfaceColorOp::TonemapHdrToSdr { config } => Some(config),
        SurfaceColorOp::Passthrough
        | SurfaceColorOp::MapSdrToHdr
        | SurfaceColorOp::TranscodeHdrTransfer => None,
    }
}

/// The pixels a `width` × `height` RGBA picture has.
fn frame_pixels(width: u32, height: u32) -> usize {
    (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4)
}

/// The shared surfaces this desktop shows, by tab, and which tab each stream's units go to.
#[derive(Default)]
pub struct PeerSurfaces {
    tiles: HashMap<TabId, Rc<RefCell<PeerSurface>>>,
    /// Which tile a stream's units belong to.
    streams: HashMap<(PeerId, SurfaceId), TabId>,
    /// The revision of each tile the host last read, so a picture that arrived since is read
    /// again.
    built: HashMap<TabId, u64>,
    /// Where a picture that comes out late wakes the host.
    wake: Option<PictureWake>,
    /// Why each window that runs on another device is not shown in its tab, as the host's
    /// session last read it: what the tab says while no share stands in it.
    elsewhere: HashMap<TabId, Elsewhere>,
}

impl PeerSurfaces {
    /// The tile in `tab` stands on a display showing `display` : its pictures are prepared for that
    /// display from now on. The tab the desktop gives no such tile has nothing to change.
    pub fn set_tile_display(&mut self, tab: TabId, display: DisplayCapabilities) {
        if let Some(tile) = self.tiles.get(&tab) {
            tile.borrow_mut().set_display(display);
        }
    }

    /// Wake `wake` when a pushed picture comes out, so it is collected without waiting for another
    /// unit: every tile open now, and every one opened after.
    pub fn set_picture_wake(&mut self, wake: Option<PictureWake>) {
        for tile in self.tiles.values() {
            tile.borrow_mut().decoder.set_picture_wake(wake.clone());
        }
        self.wake = wake;
    }

    /// Show the surface `start` announces in the tab that holds its window: the tab the sender
    /// names, else the first tab showing another device's window of that app. `None` when this
    /// desktop has no such tab.
    pub fn start(&mut self, tree: &Layout, start: &PeerStreamStart) -> Option<TabId> {
        let tab = {
            let tiles = &self.tiles;
            stream_tab_in(tree, &start.app_id, start.tab, |tab| {
                !tiles.contains_key(&tab)
            })?
        };
        let mut tile = PeerSurface::new(start, tab);
        tile.decoder.set_picture_wake(self.wake.clone());
        let revision = tile.revision();
        self.streams.insert((start.peer, start.surface), tab);
        self.built.insert(tab, revision);
        self.tiles.insert(tab, Rc::new(RefCell::new(tile)));
        Some(tab)
    }

    /// Take one encoded unit of the stream `peer` shares through `surface` . `None` for a stream
    /// this desktop shows nowhere.
    pub fn push(
        &mut self,
        peer: PeerId,
        surface: SurfaceId,
        seq: u64,
        unit: &[u8],
    ) -> Option<PeerOutcome> {
        let tab = *self.streams.get(&(peer, surface))?;
        let tile = Rc::clone(self.tiles.get(&tab)?);
        Some(tile.borrow_mut().push(seq, unit))
    }

    /// Take one object of the page of the window `surface` shows. `None` for a stream this desktop
    /// shows nowhere, and whether the frame changed for one it does.
    pub fn push_page(&mut self, peer: PeerId, surface: SurfaceId, payload: &[u8]) -> Option<bool> {
        let tab = *self.streams.get(&(peer, surface))?;
        let tile = Rc::clone(self.tiles.get(&tab)?);
        Some(tile.borrow_mut().push_page(payload))
    }

    /// Show every picture that came out after its unit was pushed. Whether any tile changed.
    pub fn collect_pictures(&mut self) -> bool {
        let mut changed = false;
        for tile in self.tiles.values() {
            if tile.borrow_mut().collect_late_picture().is_some() {
                changed = true;
            }
        }
        changed
    }

    /// The tabs whose tile changed since the host last read it, and each one's revision now.
    pub fn take_changed_tabs(&mut self) -> Vec<TabId> {
        let mut changed = Vec::new();
        for (tab, tile) in &self.tiles {
            let revision = tile.borrow().revision();
            let stale = self.built.get(tab).is_none_or(|built| *built != revision);
            if stale {
                changed.push(*tab);
            }
        }
        changed.sort_unstable();
        for tab in &changed {
            if let Some(tile) = self.tiles.get(tab) {
                let revision = tile.borrow().revision();
                self.built.insert(*tab, revision);
            }
        }
        changed
    }

    /// The tab holding `surface` 's tile, if this desktop still shows it.
    #[must_use]
    pub fn tab_of(&self, peer: PeerId, surface: SurfaceId) -> Option<TabId> {
        self.streams.get(&(peer, surface)).copied()
    }

    /// What `tab` 's tile publishes, as of now.
    #[must_use]
    pub fn read(&self, tab: TabId) -> Option<PeerRead> {
        self.tiles.get(&tab).map(|tile| tile.borrow().read())
    }

    /// Every tab a share currently stands in.
    #[must_use]
    pub fn tabs(&self) -> Vec<TabId> {
        let mut tabs: Vec<TabId> = self.tiles.keys().copied().collect();
        tabs.sort_unstable();
        tabs
    }

    /// When the host must look at the tiles again without a unit of its own: a picture still owed,
    /// and a decoder still starting up. `None` when every tile waits for nothing.
    #[must_use]
    pub fn next_wake(&self) -> Option<Instant> {
        self.tiles
            .values()
            .filter_map(|tile| tile.borrow().next_wake())
            .min()
    }

    /// The host's session read why each window that runs on another device is not shown in its
    /// tab. A tab `states` leaves out is one nobody said anything of. The tabs whose state says
    /// something else now.
    pub fn set_elsewhere(
        &mut self,
        states: impl IntoIterator<Item = (TabId, Elsewhere)>,
    ) -> Vec<TabId> {
        let read: HashMap<TabId, Elsewhere> = states.into_iter().collect();
        let mut changed: Vec<TabId> = read
            .iter()
            .filter(|(tab, state)| self.elsewhere.get(tab) != Some(state))
            .map(|(tab, _)| *tab)
            .chain(
                self.elsewhere
                    .keys()
                    .filter(|tab| !read.contains_key(tab))
                    .copied(),
            )
            .collect();
        changed.sort_unstable();
        self.elsewhere = read;
        changed
    }

    /// Why the window in `tab` is not shown there, as the host's session last read it.
    #[cfg(test)]
    #[must_use]
    pub fn elsewhere(&self, tab: TabId) -> Elsewhere {
        self.elsewhere.get(&tab).cloned().unwrap_or_default()
    }

    /// End the share of `surface` : its tile goes, so the tab says again where its window runs.
    /// The tab it stood in, when it stood in one.
    pub fn end(&mut self, peer: PeerId, surface: SurfaceId, why: WhyEnded) -> Option<TabId> {
        let tab = self.streams.remove(&(peer, surface))?;
        if let Some(tile) = self.tiles.remove(&tab) {
            tile.borrow_mut().end(why);
        }
        self.built.remove(&tab);
        Some(tab)
    }

    /// Take what the tiles ask their senders to do, in order.
    pub fn take_outgoing(&self) -> Vec<NeighbourMsg> {
        self.tiles
            .values()
            .flat_map(|tile| tile.borrow_mut().take_outgoing())
            .collect()
    }
}

/// The tab of `tree` a peer's stream of `app_id` mounts into: exactly the tab it names when that
/// tab shows a window that runs on another device, else the first such tab of its app in the active workspace
/// that `unclaimed` says holds no other stream.
fn stream_tab_in(
    tree: &Layout,
    app_id: &str,
    tab: TabId,
    unclaimed: impl Fn(TabId) -> bool,
) -> Option<TabId> {
    let elsewhere = |candidate: &Tab| matches!(candidate.tile, ModelTile::HostedWindow { .. });
    if tab != 0
        && let Some(named) = tree
            .workspaces
            .iter()
            .flat_map(workspace_tabs)
            .find(|candidate| candidate.id == tab && elsewhere(candidate))
    {
        return Some(named.id);
    }
    let of_app = |candidate: &Tab| match &candidate.tile {
        ModelTile::HostedWindow { app_id: window, .. } => window == app_id,
        _ => false,
    };
    tree.active_workspace()
        .into_iter()
        .flat_map(workspace_tabs)
        .find(|candidate| of_app(candidate) && unclaimed(candidate.id))
        .map(|candidate| candidate.id)
}

/// Every tab of `workspace` , window by window.
fn workspace_tabs(workspace: &Workspace) -> impl Iterator<Item = &Tab> {
    workspace
        .strips
        .iter()
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
}

/// The windows other devices share with this desktop, as the session that decodes them shows
/// them.
pub(crate) trait PeerViews {
    /// The desktop whose tabs show the windows.
    fn desktop(&self) -> &DesktopHandle;
    /// Show the shared window `start` announces in the tab that holds it, read into that tab on
    /// the next pass. `None` when this desktop has no tab showing such a window.
    fn show_peer_surface(&self, start: &PeerStreamStart) -> Option<TabId> {
        let peers = peers_of(self.desktop());
        let tab = {
            let shared = self.desktop().shared.borrow();
            peers.borrow_mut().start(&shared.tree, start)
        };
        if let Some(tab) = tab {
            self.desktop().rebuild(tab);
        }
        tab
    }

    /// One encoded picture of `peer`'s shared `surface`, `seq` in its stream, goes to the decoder
    /// of the tile showing that surface. `None` for a stream this desktop shows nowhere.
    fn peer_surface_picture(
        &self,
        peer: PeerId,
        surface: SurfaceId,
        seq: u64,
        unit: &[u8],
    ) -> Option<PeerOutcome> {
        let peers = peers_of(self.desktop());
        peers.borrow_mut().push(peer, surface, seq, unit)
    }

    /// One object of the page of `surface`'s window: the display list its publisher's engine sent,
    /// held by that window's tile. `None` for a stream this desktop shows nowhere.
    fn peer_surface_page(&self, peer: PeerId, surface: SurfaceId, payload: &[u8]) -> Option<bool> {
        let peers = peers_of(self.desktop());
        peers.borrow_mut().push_page(peer, surface, payload)
    }

    /// The host's session read why each window that runs on another device is not shown in its
    /// tab: not shared, shared and still on its way, or on a device that is away or left. Each
    /// tab says so from its next pass on; a tab `states` leaves out says only where its window
    /// runs.
    fn set_elsewhere(&self, states: Vec<(TabId, Elsewhere)>) {
        let peers = peers_of(self.desktop());
        let changed = peers.borrow_mut().set_elsewhere(states);
        for tab in changed {
            self.desktop().rebuild(tab);
        }
    }

    /// Why the window in `tab` is not shown there, as the host's session last read it.
    #[cfg(test)]
    #[must_use]
    fn elsewhere(&self, tab: TabId) -> Elsewhere {
        let peers = peers_of(self.desktop());
        peers.borrow().elsewhere(tab)
    }

    /// The share of `surface` ended, and why: its tile goes,
    /// and the tab says again where its window runs.
    fn end_peer_surface(&self, peer: PeerId, surface: SurfaceId, why: WhyEnded) -> Option<TabId> {
        let peers = peers_of(self.desktop());
        let tab = peers.borrow_mut().end(peer, surface, why);
        if let Some(tab) = tab {
            self.desktop().rebuild(tab);
        }
        tab
    }

    /// What is left for the owner of each shared window in order: the keyframe requests of the
    /// streams that lost a unit, to be sent as they are.
    fn take_peer_outgoing(&self) -> Vec<NeighbourMsg> {
        let peers = peers_of(self.desktop());
        peers.borrow().take_outgoing()
    }

    /// What `tab` 's shared window publishes now: whose it is, how its stream stands, and the
    /// picture it holds.
    #[cfg(test)]
    fn peer_surface_read(&self, tab: TabId) -> Option<PeerRead> {
        let peers = peers_of(self.desktop());
        peers.borrow().read(tab)
    }

    /// What the tile showing `peer`'s shared `surface` publishes now, whichever tab it stands in.
    /// `None` for a stream this desktop shows nowhere.
    fn peer_stream_read(&self, peer: PeerId, surface: SurfaceId) -> Option<PeerRead> {
        let peers = peers_of(self.desktop());
        let peers = peers.borrow();
        peers.read(peers.tab_of(peer, surface)?)
    }

    /// Wake `wake` when a pushed picture comes out, so a window that stopped changing still shows
    /// the last one: this desktop's event loop.
    fn set_peer_picture_wake(&self, wake: Option<PictureWake>) {
        let peers = peers_of(self.desktop());
        peers.borrow_mut().set_picture_wake(wake);
    }

    /// Every tab a shared window's tile stands in.
    #[must_use]
    fn peer_surface_tabs(&self) -> Vec<TabId> {
        let peers = peers_of(self.desktop());
        peers.borrow().tabs()
    }

    /// The monitor of the display the tile in `tab` stands on, while the shell spreads over several;
    /// `None` when one picture is shown on every output, or the tile stands nowhere.
    #[must_use]
    fn peer_surface_display(&self, tab: TabId) -> Option<String> {
        self.desktop().read(|tree| {
            if !tree.is_partitioned() {
                return None;
            }
            let group = route_of(tree, tab)?.group;
            let display = tree.outputs.display_of(group)?;
            tree.outputs.monitor_of(display).map(str::to_owned)
        })
    }

    /// The tile in `tab` stands on a display showing `display` 's colour: the pictures it decodes
    /// from now on are prepared for it.
    fn set_peer_display(&self, tab: TabId, display: DisplayCapabilities) {
        let peers = peers_of(self.desktop());
        peers.borrow_mut().set_tile_display(tab, display);
    }

    /// Collect the pictures a shared window's decoder finished after its unit was pushed, and name
    /// the tabs whose tile changed since the host last read them, so the picture that arrived is
    /// read and shown. Whether anything changed.
    fn follow_peer_streams(&self) -> bool {
        let peers = peers_of(self.desktop());
        let (collected, changed) = {
            let mut peers = peers.borrow_mut();
            (peers.collect_pictures(), peers.take_changed_tabs())
        };
        for tab in &changed {
            self.desktop().rebuild(*tab);
        }
        collected || !changed.is_empty()
    }

    /// When the shared windows' tiles must be looked at again without a unit of their own: a
    /// picture still owed, and a decoder still starting up.
    #[must_use]
    fn next_peer_wake(&self) -> Option<Instant> {
        let peers = peers_of(self.desktop());
        peers.borrow().next_wake()
    }
}

impl PeerViews for DesktopHandle {
    fn desktop(&self) -> &DesktopHandle {
        self
    }
}

/// Install the shared windows this desktop decodes in `handle`: the slot the desktop's state
/// leaves for what only a host that shows other devices' windows knows. A session calls it once,
/// on the desktop it builds, before any share arrives.
pub(crate) fn install(handle: &DesktopHandle) {
    let peers: Rc<RefCell<PeerSurfaces>> = Rc::default();
    handle.shared.borrow_mut().peers = Some(peers as Rc<dyn std::any::Any>);
}

/// The shared windows the desktop `handle` decodes.
fn peers_of(handle: &DesktopHandle) -> Rc<RefCell<PeerSurfaces>> {
    peers_in(&handle.shared.borrow())
}

/// The other devices' windows the desktop over `shared` decodes.
#[expect(
    clippy::expect_used,
    reason = "only a session that shows other devices' windows installs its peers, and only its own handle reads them"
)]
fn peers_in(shared: &Shared) -> Rc<RefCell<PeerSurfaces>> {
    shared
        .peers
        .clone()
        .and_then(|peers| peers.downcast::<RefCell<PeerSurfaces>>().ok())
        .expect("a desktop that shows other devices' windows installed its peers")
}
