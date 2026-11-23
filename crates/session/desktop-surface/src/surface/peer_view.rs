//! Another device's shared window, as the tile this desktop shows it in.
//!
//! A peer shares a window ([`PeerStreamStart`], what its keyframes say of the
//! pictures, or a page's announcement); this desktop decodes it into that
//! window's tab and shows it:
//!
//! - **One decoder per shared surface.** [`PeerSurface`] owns the surface's
//!   [`VideoDecoder`], replaced when the sender restarts the stream, and feeds
//!   it only the units that continue its reference chain ([`UnitChain`]). A
//!   gap asks the sender for a keyframe, again every
//!   [`KEYFRAME_REQUEST_RETRY`] until it answers. Decoding never delays the
//!   tile: a picture still coming is collected on a later unit
//!   ([`PeerSurface::collect_late_picture`]), or when the decoder's
//!   [`PictureWake`] wakes the host. At most [`PENDING_CEILING`] pictures wait
//!   for a paint the tile has not had; a third drops the oldest.
//! - **The state is a resource.** Whose window it is, whether this viewer may
//!   type into it, how the stream stands and, once it is here, the newest
//!   picture: [`PeerSurface::read`], published as one resource per tile
//!   ([`PeerSurface::resource`]).
//! - **The behaviour is actions on that resource**: pointer motion, buttons
//!   and scrolling, keys and composed text, a keyframe request, and letting
//!   the window go. Every input
//!   goes through the share's own rule ([`ShareRegistry`]), and leaves as the
//!   [`NeighbourMsg::RemoteInput`] the owner replays.
//!
//! The tile ([`PeerView`]) binds that state to widgets the toolkit already has:
//! the framework's arriving card while nothing of the window is here, the
//! image widget once a picture is, and the page view once a page's whole frame
//! is. It decides nothing: a person's click and keystroke ask
//! the same actions anything else driving the tile asks.
//!
//! A tab whose window runs on another device never stands empty. While no share stands in it, and
//! once the share in it ended, it shows the toolkit's notice saying where the window runs and why
//! it is not here ([`Elsewhere`], [`WhyEnded`]). Every word the tile says, its actions' names and
//! refusals included, is the catalogue's, resolved when it is read.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use neighbour_wire::colorimetry::Colorimetry;
use neighbour_wire::hdr_metadata::Hdr10Metadata;
use neighbour_wire::unit_chain::{Admission, UnitChain};
pub use neighbour_wire::{NeighbourMsg, PeerId, SurfaceId};
use tilo_callstream::surface_decoder::{DecodedFrame, DecodedPixels, VideoDecoder};
use tilo_color_pipeline::color_pipeline::{OutputColorState, SurfaceColorOp, plan_surface_op};
pub use tilo_color_pipeline::hdr_display::DisplayCapabilities;
use tilo_color_pipeline::hdr_tonemap::{HdrTonemapConfig, tonemap_p010_to_rgba8};
use tilo_desktop_model::{Layout, Tab, TabId, TileContent as ModelTile, Workspace, WorkspaceId};
pub use tilo_media::realtime::PictureWake;
use tilo_window_share::streaming::{InputEvent, ShareRegistry, StreamingShare};
use ui::host_operation::{ResourceOperation, ResourceOperationStart, ResourceOperations};
use ui::kurbo::{Point, Rect};
use ui::taffy::{Dimension, NodeId, Position, Size, Style, TaffyTree};
use ui::time::{Duration, Instant};
use ui::widget::{
    AppKind, AppModel, AppWidget, EmptyState, EventCtx, IconDescriptor, Image, ImageFit,
    MediaAspect, MediaGrid, ModelPath, TaffyTreeExt, TileContent, TileHost, Widget,
};
use ui::{TextRenderer, Theme};

use super::elsewhere;
pub use super::elsewhere::Elsewhere;
use crate::page_view::{PageStream, PageView};
use tilo_actions::{ActionOutput, ActionSpec, ResourcePresentation, ResourceSpec};
use tilo_resource::Effect as _;
use tilo_session_core::desktop::i18n::{t, t_args};
use ui::widget::DrawChild;

/// The kind of resource a shared surface's tile publishes.
pub const KIND: &str = "desktop.peer-surface";

/// A shared surface's tile, as a declared type: whose window it is and how
/// its stream stands.
#[derive(tilo_resource::Row)]
#[row(kind = "desktop.peer-surface", label = "Shared window")]
pub struct PeerSurfaceRow;
/// Move the viewer's pointer over the shared window, in the sender's pixels.
pub const POINTER_MOTION: &str = "pointer-motion";
/// Press or release one of the viewer's pointer buttons.
pub const POINTER_BUTTON: &str = "pointer-button";
/// Press or release one key on the viewer's keyboard.
pub const KEY: &str = "key";
/// Scroll the shared window under the viewer's pointer.
pub const SCROLL: &str = "scroll";
/// Type text the viewer composed into the shared window's focused field.
pub const TEXT: &str = "text";
/// Ask the sender for a keyframe.
pub const KEYFRAME: &str = "keyframe";
/// Let the shared window's tile go.
pub const END: &str = "end";

/// What a viewer does to a shared surface's window: the input a tile ships
/// to the sender, one effect each.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "desktop.peer-surface")]
pub enum PeerEffect {
    /// Move the viewer's pointer over the shared window, in the sender's pixels.
    #[effect(kind = "outside", label = "peer-window-action-pointer-motion")]
    PointerMotion {
        #[param(label = "peer-window-param-x")]
        x: f64,
        #[param(label = "peer-window-param-y")]
        y: f64,
    },
    /// Press or release one of the viewer's pointer buttons.
    #[effect(kind = "outside", label = "peer-window-action-pointer-button")]
    PointerButton {
        #[param(label = "peer-window-param-button")]
        button: i64,
        #[param(label = "peer-window-param-pressed")]
        pressed: bool,
    },
    /// Press or release one key on the viewer's keyboard.
    #[effect(kind = "outside", label = "peer-window-action-key")]
    Key {
        #[param(label = "peer-window-param-keysym")]
        keysym: i64,
        #[param(label = "peer-window-param-pressed")]
        pressed: bool,
        #[param(label = "peer-window-param-modifiers")]
        modifiers: i64,
    },
    /// Scroll the shared window under the viewer's pointer.
    #[effect(kind = "outside", label = "peer-window-action-scroll")]
    Scroll {
        #[param(label = "peer-window-param-dx")]
        dx: f64,
        #[param(label = "peer-window-param-dy")]
        dy: f64,
    },
    /// Type text the viewer composed into the shared window's focused field.
    #[effect(kind = "outside", label = "peer-window-action-text")]
    Text {
        #[param(label = "peer-window-param-text")]
        text: String,
    },
    /// Ask the sender for a keyframe.
    #[effect(kind = "outside", label = "peer-window-action-keyframe")]
    Keyframe,
    /// Let the shared window's tile go.
    #[effect(kind = "outside", label = "peer-window-action-end")]
    End,
}

/// How many decoded pictures a tile holds for a paint it has not had yet: up to two, and the oldest
/// goes once a third arrives.
const PENDING_CEILING: u32 = 2;
/// Decoding never delays the tile: a unit's picture comes out on a later one.
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
    /// Decoded, but the tile already held [`PENDING_CEILING`] pictures for a paint it has not had:
    /// the oldest one is gone.
    DroppedOldest,
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

impl WhyEnded {
    /// The catalogue's words for the reason, as the rest of the sentence after "Ended:".
    const fn key(self) -> &'static str {
        match self {
            Self::TabGone => "peer-window-ended-tab-gone",
            Self::LeftWorkspace => "peer-window-ended-left-workspace",
            Self::SenderStopped => "peer-window-ended-sender-stopped",
            Self::Restarted => "peer-window-ended-restarted",
            Self::LetGo => "peer-window-ended-let-go",
        }
    }
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
    /// Decoded pictures held for a paint the tile has not had yet.
    pub frames_held: u32,
    /// Pictures dropped because a paint was held.
    pub frames_dropped: u64,
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

/// One action on a shared surface's tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerAction {
    /// Pointer motion, in the sender's pixels.
    PointerMotion,
    /// A pointer button.
    PointerButton,
    /// One key.
    Key,
    /// Scrolling, in the sender's pixels.
    Scroll,
    /// Composed text.
    Text,
    /// Ask the sender for a keyframe.
    Keyframe,
    /// Let the tile go.
    End,
}

impl PeerAction {
    /// The action `effect` ships: what a tile does with one input event.
    #[must_use]
    pub fn of(effect: &PeerEffect) -> Self {
        match effect {
            PeerEffect::PointerMotion { .. } => Self::PointerMotion,
            PeerEffect::PointerButton { .. } => Self::PointerButton,
            PeerEffect::Key { .. } => Self::Key,
            PeerEffect::Scroll { .. } => Self::Scroll,
            PeerEffect::Text { .. } => Self::Text,
            PeerEffect::Keyframe => Self::Keyframe,
            PeerEffect::End => Self::End,
        }
    }
}

/// One picture, as the toolkit's image widget takes it.
#[derive(Debug, Clone)]
struct Picture {
    rgba: Arc<[u8]>,
    width: u32,
    height: u32,
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
    width: u32,
    height: u32,
    writable: bool,
    /// The share's own rule for this viewer's input, asked before every event rather than copied
    /// into a second flag.
    check: ShareRegistry,
    state: StreamState,
    awaiting_keyframe: bool,
    picture: Option<Picture>,
    picture_seq: u64,
    frames_held: u32,
    frames_dropped: u64,
    since: Instant,
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
    /// What the tile asks its sender to do: input and keyframe requests.
    outgoing: Vec<NeighbourMsg>,
    /// The window's page, when its sender publishes one (`tilo-paint`): a display list drawn here
    /// instead of a picture decoded here.
    page: Option<Rc<RefCell<PageStream>>>,
}

impl PeerSurface {
    /// The tile for `start` . Its pictures are prepared for SDR until the desktop says which
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
        let mut check = ShareRegistry::new();
        check.insert(
            start.local_peer,
            start.surface,
            StreamingShare {
                read_only: !start.writable,
            },
        );
        Self {
            tab,
            peer: start.peer,
            surface: start.surface,
            local_peer: start.local_peer,
            sharer: Arc::from(start.sharer.as_str()),
            app_id: Arc::from(start.app_id.as_str()),
            title: Arc::from(start.title.as_str()),
            width: start.width,
            height: start.height,
            writable: start.writable,
            check,
            state: StreamState::Started,
            awaiting_keyframe: false,
            picture: None,
            picture_seq: 0,
            frames_held: 0,
            frames_dropped: 0,
            since: ui::time::now(),
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
            page: (start.codec == ui::webview::paint::stream::wire::CODEC)
                .then(|| Rc::new(RefCell::new(PageStream::new()))),
        }
    }

    /// The tab this surface's tile stands in.
    #[must_use]
    pub fn tab(&self) -> TabId {
        self.tab
    }

    /// The surface this tile shows.
    #[must_use]
    pub fn surface(&self) -> SurfaceId {
        self.surface
    }

    /// The colour of the display this tile stands on: the pictures it decodes from now on are
    /// prepared for it.
    #[must_use]
    pub fn display(&self) -> DisplayCapabilities {
        self.display
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

    /// The device sharing it.
    #[must_use]
    pub fn peer(&self) -> PeerId {
        self.peer
    }

    /// Whether the share ended.
    #[must_use]
    pub fn ended(&self) -> bool {
        self.ended
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
            frames_held: self.frames_held,
            frames_dropped: self.frames_dropped,
            display: self.display,
        }
    }

    /// The resource this tile publishes: whose window it is, whether viewers may type into it, how
    /// its stream stands, and the actions that drive it.
    #[must_use]
    pub fn resource(&self) -> ResourceSpec {
        let mut resource = tilo_resource::typed_resource::<PeerSurfaceRow>(
            self.resource_id(),
            self.title.to_string(),
        )
        .with_presentation(ResourcePresentation {
            detail: Some(bounded(
                &t_args("peer-window-of", &[("name", (&*self.sharer).into())]),
                ResourcePresentation::MAX_DETAIL_BYTES,
            )),
            badge: Some(t(if self.writable {
                "peer-window-can-type"
            } else {
                "peer-window-view-only"
            })),
            preview: Some(bounded(
                &self.state_line(),
                ResourcePresentation::MAX_PREVIEW_BYTES,
            )),
        });
        for action in peer_actions().into_iter().flatten() {
            resource = resource.with_action(action);
        }
        resource
    }

    /// This tile's resource id.
    #[must_use]
    pub fn resource_id(&self) -> String {
        format!("{KIND}:{}", self.surface)
    }

    /// A sentence for the resource's preview: how the stream stands, in the language of the
    /// person reading it.
    fn state_line(&self) -> String {
        match &self.state {
            StreamState::Started if self.awaiting_keyframe => t("peer-window-picture-lost"),
            StreamState::Started => t("peer-window-first-picture"),
            StreamState::Pictured => t("peer-window-live"),
            StreamState::Ended { why } => {
                t_args("peer-window-ended", &[("why", t(why.key()).into())])
            }
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
        self.accept(frame).unwrap_or(PeerOutcome::DecodeFailed)
    }

    /// Take one object of the window's page: the payload its publisher's engine encoded. Whether
    /// the frame the tile draws changed.
    pub fn push_page(&mut self, payload: &[u8]) -> bool {
        if self.ended {
            return false;
        }
        let Some(page) = &self.page else {
            return false;
        };
        let changed = page.borrow_mut().apply(payload);
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
        self.accept(frame?)
    }

    /// Whether a pushed unit's picture is still coming.
    #[must_use]
    pub fn picture_pending(&self) -> bool {
        self.decoder.picture_pending()
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

    /// The picture has been shown. A paint after it lets the tile hold more.
    pub fn mark_drawn(&mut self) {
        self.frames_held = 0;
    }

    /// Accept a decoded picture as the tile's newest, dropping the one held for a paint that never
    /// came once a third arrives.
    fn accept(&mut self, frame: DecodedFrame) -> Option<PeerOutcome> {
        let dropped = self.frames_held >= PENDING_CEILING && self.picture.is_some();
        if dropped {
            self.frames_dropped = self.frames_dropped.saturating_add(1);
        } else {
            self.frames_held = self.frames_held.saturating_add(1);
        }
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
            // A picture the image widget would refuse to paint is no picture: the tile keeps the
            // one it has and the stream waits for a keyframe, as for any unit the decoder could not
            // use.
            let broke = self.chain.intact();
            self.chain.lose();
            self.request_keyframe(self.last_seq, broke);
            return None;
        }
        self.picture = Some(Picture {
            rgba: Arc::from(rgba),
            width: frame.width,
            height: frame.height,
        });
        self.picture_seq = self.last_seq;
        self.state = StreamState::Pictured;
        self.revision = self.revision.wrapping_add(1);
        Some(if dropped {
            PeerOutcome::DroppedOldest
        } else {
            PeerOutcome::Decoded
        })
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

    /// Do what an action on this tile's resource asks.
    ///
    /// # Errors
    ///
    /// The share's refusal for a viewer that may only watch, a share that has
    /// ended, and arguments the action's contract does not allow.
    pub fn act(
        &mut self,
        action: PeerAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionOutput, String> {
        if self.ended {
            return Err(t("peer-window-refused-ended"));
        }
        match action {
            PeerAction::PointerMotion => {
                let x = number(arguments, "x")?;
                let y = number(arguments, "y")?;
                self.send(&InputEvent::PointerMotion { x, y })
            }
            PeerAction::PointerButton => {
                let button = whole(arguments, "button")?;
                let pressed = flag(arguments, "pressed")?;
                let button = u32::try_from(button).map_err(|_| t("peer-window-refused-button"))?;
                self.send(&InputEvent::PointerButton { button, pressed })
            }
            PeerAction::Key => {
                let keysym = whole(arguments, "keysym")?;
                let pressed = flag(arguments, "pressed")?;
                let modifiers = whole(arguments, "modifiers").unwrap_or(0);
                let keysym = u32::try_from(keysym).map_err(|_| t("peer-window-refused-key"))?;
                let modifiers = u32::try_from(modifiers).unwrap_or(0);
                self.send(&InputEvent::Key {
                    keysym,
                    pressed,
                    modifiers,
                })
            }
            PeerAction::Scroll => {
                let dx = number(arguments, "dx")?;
                let dy = number(arguments, "dy")?;
                self.send(&InputEvent::PointerAxis { dx, dy })
            }
            PeerAction::Text => {
                let text = arguments
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| t("peer-window-refused-text"))?;
                self.send(&InputEvent::Text {
                    text: text.to_owned(),
                })
            }
            PeerAction::Keyframe => {
                self.request_keyframe(self.last_seq, true);
                Ok(ActionOutput::default())
            }
            PeerAction::End => {
                self.end(WhyEnded::LetGo);
                Ok(ActionOutput::default())
            }
        }
    }

    /// Ship one input event to the sender, through the share's own rule.
    fn send(&mut self, event: &InputEvent) -> Result<ActionOutput, String> {
        if !self
            .check
            .accept_remote_input(self.local_peer, self.surface)
        {
            return Err(t("peer-window-refused-view-only"));
        }
        let encoded = postcard::to_allocvec(event).map_err(|error| error.to_string())?;
        self.outgoing.push(NeighbourMsg::RemoteInput {
            peer: self.local_peer,
            surface: self.surface,
            event: encoded,
        });
        Ok(ActionOutput::default())
    }

    /// End the tile's share: nothing is decoded after this, and what the tile publishes and shows
    /// says why.
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

    /// What stands in the tile's place: the window's page or its newest picture, or - before
    /// either is here - the framework's arriving card naming the device the window comes from.
    /// A share that ended follows the window no longer, so its tile shows no picture of it: it
    /// says where the window runs and why the share ended, in the toolkit's notice.
    fn content(&self) -> Box<dyn Widget> {
        if self.ended {
            return Box::new(
                EmptyState::new(elsewhere::title(&self.app_id, &self.sharer))
                    .with_message(self.state_line()),
            );
        }
        let alt = t_args(
            "peer-window-name",
            &[
                ("window", (&*self.title).into()),
                ("device", (&*self.sharer).into()),
            ],
        );
        if let Some(page) = &self.page
            && page.borrow().frame().is_some()
        {
            // The page is drawn here, not decoded: the display list its publisher sends is what
            // this toolkit's own Vello and text stack draws, the same code the page's own window
            // draws it with.
            return Box::new(PageView::new(Rc::clone(page), alt));
        }
        match &self.picture {
            Some(picture) => Box::new(
                Image::from_rgba(Arc::clone(&picture.rgba), picture.width, picture.height)
                    .filling()
                    .with_fit(ImageFit::Contain)
                    .with_alt(alt),
            ),
            // A page of which no whole frame came waits as a picture does: in the shape its
            // device announced.
            None => Box::new(MediaGrid::arriving(
                1,
                self.aspect(),
                &ui::arriving::Source::Remote,
                self.since,
            )),
        }
    }

    /// The shape the placeholder reserves for the window: what the sender announced, sixteen by
    /// nine until it announces anything.
    fn aspect(&self) -> MediaAspect {
        if self.width == 0 || self.height == 0 {
            return MediaAspect::Wide;
        }
        MediaAspect::Ratio(f64::from(self.width) / f64::from(self.height))
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

/// `text` , cut to `max` bytes at a character boundary.
fn bounded(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut cut = max;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    text.get(..cut).unwrap_or_default().to_owned()
}

/// What a tile offers: the declared effects of [`PeerEffect`], each with its
/// arguments asked from the viewer.
fn peer_actions() -> [Option<ActionSpec>; 7] {
    [
        PeerEffect::spec(PeerEffect::POINTER_MOTION.id),
        PeerEffect::spec(PeerEffect::POINTER_BUTTON.id),
        PeerEffect::spec(PeerEffect::KEY.id),
        PeerEffect::spec(PeerEffect::SCROLL.id),
        PeerEffect::spec(PeerEffect::TEXT.id),
        PeerEffect::spec(PeerEffect::KEYFRAME.id),
        PeerEffect::spec(PeerEffect::END.id),
    ]
}

/// The refusal of an argument that is not what its action takes, naming the argument.
fn refused(words: &str, name: &str) -> String {
    t_args(words, &[("argument", name.into())])
}

fn number(
    arguments: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<f64, String> {
    arguments
        .get(name)
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| refused("peer-window-refused-number", name))
}

fn whole(
    arguments: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<i64, String> {
    arguments
        .get(name)
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| refused("peer-window-refused-whole", name))
}

fn flag(
    arguments: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<bool, String> {
    arguments
        .get(name)
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| refused("peer-window-refused-flag", name))
}

/// The shared surfaces this desktop shows, by tab, and which tab each stream's units go to.
#[derive(Default)]
pub struct PeerSurfaces {
    tiles: HashMap<TabId, Rc<RefCell<PeerSurface>>>,
    /// Which tile a stream's units belong to.
    streams: HashMap<(PeerId, SurfaceId), TabId>,
    /// The revision of each tile the desktop last built, so a picture that arrived since is built
    /// into the tile again.
    built: HashMap<TabId, u64>,
    /// Where a picture that comes out late wakes the host.
    wake: Option<PictureWake>,
    /// Why each window that runs on another device is not shown in its tab, as the host's
    /// session last read it: what the tab says while no share stands in it.
    elsewhere: HashMap<TabId, Elsewhere>,
    /// The catalogue's revision the tiles were last built under: what a tile says is resolved
    /// when it is built, so a change of language builds them again.
    spoken: Option<u64>,
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

    /// The tabs whose tile changed since the desktop last built it, and each one's revision now.
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

    /// The tile in `tab` .
    #[must_use]
    pub fn tile(&self, tab: TabId) -> Option<&Rc<RefCell<PeerSurface>>> {
        self.tiles.get(&tab)
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

    /// `tab` 's tile as its window shows it, for a host asking what to build there.
    #[must_use]
    pub fn tile_host(&self, tab: TabId) -> Option<TileHost> {
        let tile = Rc::clone(self.tiles.get(&tab)?);
        Some(TileHost::new(TileContent::App(Box::new(PeerView::new(
            tile,
        )))))
    }

    /// The host's session read why each window that runs on another device is not shown in its
    /// tab. A tab `states` leaves out is one nobody said anything of. The tabs whose notice
    /// says something else now.
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
    #[must_use]
    pub fn elsewhere(&self, tab: TabId) -> Elsewhere {
        self.elsewhere.get(&tab).cloned().unwrap_or_default()
    }

    /// What `tab` shows of a window that runs on another device while no share stands in it: the
    /// toolkit's notice saying where it runs and why it is not here. `None` for any other tab.
    #[must_use]
    pub fn stand_in(&self, tab: &Tab) -> Option<TileHost> {
        elsewhere::notice(tab, &self.elsewhere(tab.id)).map(TileHost::new)
    }

    /// Whether the person reads another language than the tiles were last built in.
    fn respoken(&mut self) -> bool {
        let now = ui::i18n::I18nManager::revision();
        self.spoken.replace(now).is_some_and(|built| built != now)
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

/// One shared surface's tile: the state another device's window publishes,
/// bound to the widgets that show it.
pub struct PeerView {
    state: Rc<RefCell<PeerSurface>>,
    child: Box<dyn Widget>,
    child_node: Option<NodeId>,
    rect: Rect,
    /// What the window bar calls the tile, owned because the bar asks for it by reference.
    title: String,
}

impl PeerView {
    fn new(state: Rc<RefCell<PeerSurface>>) -> Self {
        let (child, title) = {
            let state = state.borrow();
            (state.content(), state.title.to_string())
        };
        Self {
            state,
            child,
            child_node: None,
            rect: Rect::ZERO,
            title,
        }
    }

    /// Ask the tile to do `action` , as its resource publishes it: the same request anything else
    /// driving the tile makes. Whether the tile took it.
    fn ask(
        &self,
        action: PeerAction,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> bool {
        self.state.borrow_mut().act(action, arguments).is_ok()
    }
}

impl Widget for PeerView {
    ui::provides!(
        dyn ui::resources::PublishesResources,
        dyn ResourceOperations
    );

    fn name(&self) -> &'static str {
        "PeerView"
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.child.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.child.build_layout(taffy, theme, node_map);
        // The picture fills the window however the window was sized: a share that changes
        // resolution keeps its tile's box.
        let mut style = taffy.style(child).cloned().unwrap_or_default();
        style.position = Position::Absolute;
        style.size = Size {
            width: Dimension::percent(1.0),
            height: Dimension::percent(1.0),
        };
        let _ = taffy.set_style(child, style);
        self.child_node = Some(child);
        // The tile's own box is the window's. With the picture absolute it
        // sets no size of its own, so the box it fills has to say it fills;
        // its floor is zero so a stream never pushes the window's bar out
        // of the strip.
        let fill = Size {
            width: Dimension::percent(1.0),
            height: Dimension::percent(1.0),
        };
        taffy.widget_node(
            Style {
                size: fill,
                min_size: Size {
                    width: Dimension::length(0.0),
                    height: Dimension::length(0.0),
                },
                ..Style::default()
            },
            &[child],
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        if let Some(child) = self.child_node {
            self.child.apply_layout(taffy, child, origin);
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw(&mut self, ctx: &mut tilo_ui::DrawCtx<'_>) {
        #[allow(unused_variables, unused_mut, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.child
            .draw_child(scene, transform, text_renderer, theme,&env);
    }

    /// A person's pointer and keys are the tile's own actions, asked exactly as anything else
    /// driving the tile asks them: the share's rule refuses them the same way for both.
    fn handle_event(&mut self, event: &winit::event::WindowEvent, ctx: &mut EventCtx<'_>) {
        let size = {
            let state = self.state.borrow();
            (state.width, state.height)
        };
        let Some(input) = crate::window_input::translate(
            event,
            self.rect,
            size,
            ctx.scale_factor,
            ctx.modifiers.state(),
        ) else {
            return;
        };
        let (action, arguments) = match input {
            InputEvent::PointerMotion { x, y } => (
                PeerAction::PointerMotion,
                serde_json::json!({ "x": x, "y": y }),
            ),
            InputEvent::PointerButton { button, pressed } => (
                PeerAction::PointerButton,
                serde_json::json!({ "button": button, "pressed": pressed }),
            ),
            InputEvent::Key {
                keysym,
                pressed,
                modifiers,
            } => (
                PeerAction::Key,
                serde_json::json!({ "keysym": keysym, "pressed": pressed, "modifiers": modifiers }),
            ),
            InputEvent::PointerAxis { dx, dy } => (
                PeerAction::Scroll,
                serde_json::json!({ "dx": dx, "dy": dy }),
            ),
            InputEvent::Text { ref text } => {
                (PeerAction::Text, serde_json::json!({ "text": text }))
            }
            _ => return,
        };
        let serde_json::Value::Object(arguments) = arguments else {
            return;
        };
        if self.ask(action, &arguments) {
            *ctx.handled = true;
            if matches!(input, InputEvent::PointerButton { pressed: true, .. }) {
                // Clicking a window puts the keyboard on it: the keys that follow are meant for the
                // shared one.
                *ctx.focus_target = Some(ctx.node_id);
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(self.child.as_ref());
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }

    /// A paint after the tile's newest picture lets it hold more.
    fn mark_drawn(&mut self) {
        self.state.borrow_mut().mark_drawn();
        self.child.mark_drawn();
    }

    /// The tile's own deadlines besides its child's: a picture still owed, and a decoder still
    /// starting up.
    fn next_wake_deadline(&self) -> Option<Instant> {
        let own = self.state.borrow().next_wake();
        let child = self.child.next_wake_deadline();
        match (own, child) {
            (Some(own), Some(child)) => Some(own.min(child)),
            (own, child) => own.or(child),
        }
    }
}

impl AppWidget for PeerView {
    fn app_kind(&self) -> AppKind {
        AppKind::App(ui::icons::app_glyph(&self.state.borrow().app_id, None))
    }

    fn app_title(&self) -> &str {
        &self.title
    }

    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("\u{f0ac}")
    }
}

impl ui::resources::PublishesResources for PeerView {
    fn resources(&self, _doc: &ui::automerge::AutoCommit) -> Vec<ResourceSpec> {
        vec![self.state.borrow().resource()]
    }

    fn resource(&self, _doc: &ui::automerge::AutoCommit, id: &str) -> Option<ResourceSpec> {
        let resource = self.state.borrow().resource();
        (resource.id.as_str() == id).then_some(resource)
    }
}

impl ResourceOperations for PeerView {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[PeerEffect::OPERATION]
    }

    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut ui::widget::Edits<'_>,
        path: &ui::automerge::ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if let Some(started) = ui::host_operation::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let request = op.request();
        if request.type_id != PeerEffect::OPERATION {
            return Ok(None);
        }
        let effect =
            PeerEffect::from_payload(request.payload).map_err(|error| error.to_string())?;
        let action = PeerAction::of(&effect);
        let arguments = effect.arguments();
        self.state
            .borrow_mut()
            .act(action, &arguments)
            .map(|output| Some(ResourceOperationStart::Completed(output)))
    }
}

/// What a host's [`TileSource`](crate::TileSource) answers with for a window that runs on another
/// device: the share's own tile while a peer streams into its tab, and until then the notice
/// saying where the window runs and why it is not here. The host has nothing to draw for such a
/// window, so it is not asked; everything else is the host's.
pub struct PeerTiles {
    inner: Box<dyn crate::TileSource>,
    peers: Rc<RefCell<PeerSurfaces>>,
}

impl PeerTiles {
    /// The host's own tab contents, with a shared surface winning in its tab.
    #[must_use]
    pub fn new(inner: Box<dyn crate::TileSource>, peers: Rc<RefCell<PeerSurfaces>>) -> Self {
        Self { inner, peers }
    }
}

impl crate::TileSource for PeerTiles {
    fn tile(&mut self, workspace: &Workspace, tab: &Tab) -> TileHost {
        let elsewhere = {
            let peers = self.peers.borrow();
            peers.tile_host(tab.id).or_else(|| peers.stand_in(tab))
        };
        if let Some(host) = elsewhere {
            return host;
        }
        self.inner.tile(workspace, tab)
    }

    fn launch(
        &mut self,
        workspace: &Workspace,
        id: TabId,
        app: &str,
        beside: Option<TabId>,
    ) -> Option<Tab> {
        self.inner.launch(workspace, id, app, beside)
    }

    fn reopened(&mut self, workspace: &Workspace, tab: &Tab) {
        self.inner.reopened(workspace, tab);
    }

    fn search_applications(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        self.inner.search_applications()
    }

    fn showing(&self, app: &str) -> Option<TabId> {
        self.inner.showing(app)
    }

    fn window_resource(&self, tab: TabId) -> Option<(String, String)> {
        self.inner.window_resource(tab)
    }

    fn carry(&self, tab: TabId, to: WorkspaceId) -> bool {
        self.inner.carry(tab, to)
    }

    fn workspace_id(&self) -> Option<String> {
        self.inner.workspace_id()
    }

    fn draws_embeds(&self) -> bool {
        self.inner.draws_embeds()
    }

    fn embed(&mut self, view: &ui::window_share::ResourceView) -> Result<TileHost, String> {
        self.inner.embed(view)
    }
}

impl super::DesktopHandle {
    /// Show the shared window `start` announces in the tab that holds it,
    /// built into that tab on the next update pass. `None` when this desktop
    /// has no tab showing such a window.
    pub fn show_peer_surface(&self, start: &PeerStreamStart) -> Option<TabId> {
        let peers = Rc::clone(&self.shared.borrow().peers);
        let tab = {
            let shared = self.shared.borrow();
            peers.borrow_mut().start(&shared.tree, start)
        };
        if let Some(tab) = tab {
            self.rebuild(tab);
        }
        tab
    }

    /// One encoded picture of `peer`'s shared `surface`, `seq` in its stream, goes to the decoder
    /// of the tile showing that surface. `None` for a stream this desktop shows nowhere.
    pub fn peer_surface_picture(
        &self,
        peer: PeerId,
        surface: SurfaceId,
        seq: u64,
        unit: &[u8],
    ) -> Option<PeerOutcome> {
        let peers = Rc::clone(&self.shared.borrow().peers);
        peers.borrow_mut().push(peer, surface, seq, unit)
    }

    /// One object of the page of `surface`'s window: the display list its publisher's engine sent,
    /// drawn in that window's tile. `None` for a stream this desktop shows nowhere.
    pub fn peer_surface_page(
        &self,
        peer: PeerId,
        surface: SurfaceId,
        payload: &[u8],
    ) -> Option<bool> {
        let peers = Rc::clone(&self.shared.borrow().peers);
        peers.borrow_mut().push_page(peer, surface, payload)
    }

    /// The host's session read why each window that runs on another device is not shown in its
    /// tab: not shared, shared and still on its way, or on a device that is away or left. Each
    /// tab says so from its next pass on; a tab `states` leaves out says only where its window
    /// runs.
    pub fn set_elsewhere(&self, states: Vec<(TabId, Elsewhere)>) {
        let peers = Rc::clone(&self.shared.borrow().peers);
        let changed = peers.borrow_mut().set_elsewhere(states);
        for tab in changed {
            self.rebuild(tab);
        }
    }

    /// Why the window in `tab` is not shown there, as the host's session last read it.
    #[must_use]
    pub fn elsewhere(&self, tab: TabId) -> Elsewhere {
        let peers = Rc::clone(&self.shared.borrow().peers);
        peers.borrow().elsewhere(tab)
    }

    /// The share of `surface` ended, and why: its tile goes,
    /// and the tab says again where its window runs.
    pub fn end_peer_surface(
        &self,
        peer: PeerId,
        surface: SurfaceId,
        why: WhyEnded,
    ) -> Option<TabId> {
        let peers = Rc::clone(&self.shared.borrow().peers);
        let tab = peers.borrow_mut().end(peer, surface, why);
        if let Some(tab) = tab {
            self.rebuild(tab);
        }
        tab
    }

    /// What is left for the owner of each shared window in order: remote input and keyframe
    /// requests, to be sent as they are.
    pub fn take_peer_outgoing(&self) -> Vec<NeighbourMsg> {
        let peers = Rc::clone(&self.shared.borrow().peers);
        peers.borrow().take_outgoing()
    }

    /// What `tab` 's shared window publishes now: whose it is, how its stream stands, and the
    /// picture it shows.
    pub fn peer_surface_read(&self, tab: TabId) -> Option<PeerRead> {
        let peers = Rc::clone(&self.shared.borrow().peers);
        peers.borrow().read(tab)
    }

    /// What the tile showing `peer`'s shared `surface` publishes now, whichever tab it stands in.
    /// `None` for a stream this desktop shows nowhere.
    pub fn peer_stream_read(&self, peer: PeerId, surface: SurfaceId) -> Option<PeerRead> {
        let peers = Rc::clone(&self.shared.borrow().peers);
        let peers = peers.borrow();
        peers.read(peers.tab_of(peer, surface)?)
    }

    /// Wake `wake` when a pushed picture comes out, so a window that stopped changing still shows
    /// the last one: this desktop's event loop.
    pub fn set_peer_picture_wake(&self, wake: Option<PictureWake>) {
        let peers = Rc::clone(&self.shared.borrow().peers);
        peers.borrow_mut().set_picture_wake(wake);
    }

    /// Every tab a shared window's tile stands in.
    #[must_use]
    pub fn peer_surface_tabs(&self) -> Vec<TabId> {
        let peers = Rc::clone(&self.shared.borrow().peers);
        peers.borrow().tabs()
    }

    /// The monitor of the display the tile in `tab` stands on, while the shell spreads over several;
    /// `None` when one picture is shown on every output, or the tile stands nowhere.
    #[must_use]
    pub fn peer_surface_display(&self, tab: TabId) -> Option<String> {
        self.read(|tree| {
            if !tree.is_partitioned() {
                return None;
            }
            let group = super::route_of(tree, tab)?.group;
            let display = tree.outputs.display_of(group)?;
            tree.outputs.monitor_of(display).map(str::to_owned)
        })
    }

    /// The tile in `tab` stands on a display showing `display` 's colour: the pictures it decodes
    /// from now on are prepared for it.
    pub fn set_peer_display(&self, tab: TabId, display: DisplayCapabilities) {
        let peers = Rc::clone(&self.shared.borrow().peers);
        peers.borrow_mut().set_tile_display(tab, display);
    }
}

impl super::DesktopSurface {
    /// Collect the pictures a shared window's decoder finished after its unit was pushed, and ask
    /// for the tile of every shared window whose stream changed to be built again, so the picture
    /// that arrived lands in it. Whether anything changed.
    pub(super) fn follow_peer_streams(&self) -> bool {
        let peers = Rc::clone(&self.shared.borrow().peers);
        let (collected, mut changed, respoken) = {
            let mut peers = peers.borrow_mut();
            let collected = peers.collect_pictures();
            let changed = peers.take_changed_tabs();
            (collected, changed, peers.respoken())
        };
        let mut shared = self.shared.borrow_mut();
        if respoken {
            // The person reads another language now: what each of these tabs says was resolved
            // when its tile was built, so each is built again.
            changed.extend(
                shared
                    .tree
                    .workspaces
                    .iter()
                    .flat_map(workspace_tabs)
                    .filter(|tab| matches!(tab.tile, ModelTile::HostedWindow { .. }))
                    .map(|tab| tab.id),
            );
        }
        for tab in &changed {
            if !shared.rebuild.contains(tab) {
                shared.rebuild.push(*tab);
            }
        }
        drop(shared);
        collected || !changed.is_empty()
    }
}
