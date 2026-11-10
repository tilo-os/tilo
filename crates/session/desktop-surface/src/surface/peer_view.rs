//! Another device's shared window, as the tile this desktop shows it in.
//!
//! A peer announces a surface it streams ([`PeerStreamStart`]); this desktop
//! decodes it into that window's tab and shows it:
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
//! - **The behaviour is actions on that resource**: pointer motion and
//!   buttons, keys, a keyframe request, and letting the window go. Every input
//!   goes through the share's own rule ([`ShareRegistry`]), and leaves as the
//!   [`NeighbourMsg::RemoteInput`] the owner replays.
//!
//! The tile ([`PeerView`]) binds that state to widgets the toolkit already has:
//! the framework's arriving card while no picture is here, and the image
//! widget once one is. It decides nothing: a person's click and keystroke ask
//! the same actions anything else driving the tile asks.

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
use tilo_desktop_model::{
    DesktopTree, Tab, TabId, TileContent as ModelTile, Workspace, WorkspaceSlot,
};
pub use tilo_media::realtime::PictureWake;
use tilo_window_share::streaming::{InputEvent, ShareRegistry, StreamingShare};
use ui::host_operation::{ResourceOperation, ResourceOperationStart, ResourceOperations};
use ui::kurbo::{Point, Rect};
use ui::taffy::{Dimension, NodeId, Position, Size, Style, TaffyTree};
use ui::time::{Duration, Instant};
use ui::widget::{
    AppKind, AppModel, AppWidget, EventCtx, IconDescriptor, Image, ImageFit, MediaAspect,
    MediaGrid, ModelPath, TaffyTreeExt, TileContent, TileHost, Widget,
};
use ui::{TextRenderer, Theme};

use crate::page_view::PageStream;
use tilo_actions::{
    ActionOutput, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType,
    ResourcePresentation, ResourceSpec,
};

/// The kind of resource a shared surface's tile publishes.
pub const KIND: &str = "desktop.peer-surface";
/// Move the viewer's pointer over the shared window, in the sender's pixels.
pub const POINTER_MOTION: &str = "pointer-motion";
/// Press or release one of the viewer's pointer buttons.
pub const POINTER_BUTTON: &str = "pointer-button";
/// Press or release one key on the viewer's keyboard.
pub const KEY: &str = "key";
/// Ask the sender for a keyframe.
pub const KEYFRAME: &str = "keyframe";
/// Let the shared window's tile go.
pub const END: &str = "end";

const POINTER_MOTION_OPERATION: &str = "desktop.peer-surface.pointer-motion";
const POINTER_BUTTON_OPERATION: &str = "desktop.peer-surface.pointer-button";
const KEY_OPERATION: &str = "desktop.peer-surface.key";
const KEYFRAME_OPERATION: &str = "desktop.peer-surface.keyframe";
const END_OPERATION: &str = "desktop.peer-surface.end";

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
pub enum Standing {
    /// The sender announced the stream; no picture has arrived yet.
    Started,
    /// A picture is in the tile.
    Pictured,
    /// The share ended, and why.
    Ended {
        /// Why the share ended, in the words a person reads.
        why: String,
    },
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
    pub standing: Standing,
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
    /// Ask the sender for a keyframe.
    Keyframe,
    /// Let the tile go.
    End,
}

impl PeerAction {
    /// The action an operation type carries, if it is one of this tile's.
    #[must_use]
    pub fn of_type(type_id: &str) -> Option<Self> {
        match type_id {
            POINTER_MOTION_OPERATION => Some(Self::PointerMotion),
            POINTER_BUTTON_OPERATION => Some(Self::PointerButton),
            KEY_OPERATION => Some(Self::Key),
            KEYFRAME_OPERATION => Some(Self::Keyframe),
            END_OPERATION => Some(Self::End),
            _ => None,
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
    gate: ShareRegistry,
    standing: Standing,
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
        let mut gate = ShareRegistry::new();
        gate.insert(
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
            gate,
            standing: Standing::Started,
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

    /// Changes whenever anything the tile shows changes: a picture, the stream's standing, a
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
            standing: self.standing.clone(),
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
        ResourceSpec::new(self.resource_id(), KIND, self.title.to_string())
            .with_presentation(ResourcePresentation {
                detail: Some(bounded(
                    &format!("{}'s window", self.sharer),
                    ResourcePresentation::MAX_DETAIL_BYTES,
                )),
                badge: Some(
                    if self.writable {
                        "Can type"
                    } else {
                        "View only"
                    }
                    .to_string(),
                ),
                preview: Some(bounded(
                    &self.standing_line(),
                    ResourcePresentation::MAX_PREVIEW_BYTES,
                )),
            })
            .with_action(motion_action())
            .with_action(button_action())
            .with_action(key_action())
            .with_action(keyframe_action())
            .with_action(end_action())
    }

    /// This tile's resource id.
    #[must_use]
    pub fn resource_id(&self) -> String {
        format!("{KIND}:{}", self.surface)
    }

    /// A sentence for the resource's preview: how the stream stands.
    fn standing_line(&self) -> String {
        match &self.standing {
            Standing::Started if self.awaiting_keyframe => {
                "A frame went missing; waiting for the sender's keyframe".to_string()
            }
            Standing::Started => "Waiting for its first picture".to_string(),
            Standing::Pictured => "Live".to_string(),
            Standing::Ended { why } => format!("Ended: {why}"),
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
            self.standing = Standing::Pictured;
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
        self.standing = Standing::Pictured;
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
            return Err("This window's share has ended".into());
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
                let button = u32::try_from(button)
                    .map_err(|_| "`button` is not a pointer button".to_string())?;
                self.send(&InputEvent::PointerButton { button, pressed })
            }
            PeerAction::Key => {
                let keysym = whole(arguments, "keysym")?;
                let pressed = flag(arguments, "pressed")?;
                let modifiers = whole(arguments, "modifiers").unwrap_or(0);
                let keysym =
                    u32::try_from(keysym).map_err(|_| "`keysym` is not a key code".to_string())?;
                let modifiers = u32::try_from(modifiers).unwrap_or(0);
                self.send(&InputEvent::Key {
                    keysym,
                    pressed,
                    modifiers,
                })
            }
            PeerAction::Keyframe => {
                self.request_keyframe(self.last_seq, true);
                Ok(ActionOutput::default())
            }
            PeerAction::End => {
                self.end("this device let the window go");
                Ok(ActionOutput::default())
            }
        }
    }

    /// Ship one input event to the sender, through the share's own rule.
    fn send(&mut self, event: &InputEvent) -> Result<ActionOutput, String> {
        if !self.gate.accept_remote_input(self.local_peer, self.surface) {
            return Err("This window is shared for watching only".into());
        }
        let encoded = postcard::to_allocvec(event).map_err(|error| error.to_string())?;
        self.outgoing.push(NeighbourMsg::RemoteInput {
            peer: self.local_peer,
            surface: self.surface,
            event: encoded,
        });
        Ok(ActionOutput::default())
    }

    /// End the tile's share: nothing is decoded after this, and what the tile publishes says why.
    pub fn end(&mut self, why: &str) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.standing = Standing::Ended {
            why: why.to_owned(),
        };
        self.revision = self.revision.wrapping_add(1);
    }

    /// What this tile asks its sender to do, in order.
    pub fn take_outgoing(&mut self) -> Vec<NeighbourMsg> {
        std::mem::take(&mut self.outgoing)
    }

    /// What stands in the tile's place: the newest picture as the image widget takes it, or -
    /// before one is here - the framework's arriving card naming the device the window comes from.
    fn content(&self) -> Box<dyn Widget> {
        if let Some(page) = &self.page {
            // The page is drawn here, not decoded: the display list its publisher sends is what
            // this toolkit's own Vello and text stack draws, the same code the page's own window
            // draws it with.
            return PageStream::view(page, format!("{} on {}", self.title, self.sharer));
        }
        match &self.picture {
            Some(picture) => Box::new(
                Image::from_rgba(Arc::clone(&picture.rgba), picture.width, picture.height)
                    .filling()
                    .with_fit(ImageFit::Contain)
                    .with_alt(format!("{} on {}", self.title, self.sharer)),
            ),
            None => Box::new(MediaGrid::arriving(
                1,
                self.aspect(),
                &ui::arriving::Source::peer(Arc::clone(&self.sharer)),
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

/// The argument-encoding operation of one effect action.
fn operation(
    id: &'static str,
    label: &'static str,
    placeholder: &'static str,
    type_id: &'static str,
    params: Vec<ActionParam>,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        placeholder,
        OperationKind::Effect,
        type_id,
        params,
        move |arguments| {
            EncodedAction::encode_operation(type_id, &arguments.to_json()).unwrap_or_else(|_| {
                EncodedAction {
                    type_id: type_id.to_string(),
                    payload: Vec::new(),
                }
            })
        },
    )
}

fn motion_action() -> ActionSpec {
    operation(
        POINTER_MOTION,
        "Move the pointer",
        "Where the pointer goes",
        POINTER_MOTION_OPERATION,
        vec![
            ActionParam::required("x", ParamType::Number, "x in the window's pixels"),
            ActionParam::required("y", ParamType::Number, "y in the window's pixels"),
        ],
    )
}

fn button_action() -> ActionSpec {
    operation(
        POINTER_BUTTON,
        "Press a pointer button",
        "Which button",
        POINTER_BUTTON_OPERATION,
        vec![
            ActionParam::required("button", ParamType::Integer, "evdev button code"),
            ActionParam::required("pressed", ParamType::Boolean, "down or up"),
        ],
    )
}

fn key_action() -> ActionSpec {
    operation(
        KEY,
        "Press a key",
        "Which key",
        KEY_OPERATION,
        vec![
            ActionParam::required("keysym", ParamType::Integer, "evdev key code"),
            ActionParam::required("pressed", ParamType::Boolean, "down or up"),
            ActionParam::required("modifiers", ParamType::Integer, "held modifiers"),
        ],
    )
}

fn keyframe_action() -> ActionSpec {
    operation(
        KEYFRAME,
        "Ask for a keyframe",
        "Ask the sender",
        KEYFRAME_OPERATION,
        Vec::new(),
    )
}

fn end_action() -> ActionSpec {
    operation(
        END,
        "Let the window go",
        "Stop showing it",
        END_OPERATION,
        Vec::new(),
    )
}

fn number(
    arguments: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<f64, String> {
    arguments
        .get(name)
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| format!("`{name}` is not a number"))
}

fn whole(
    arguments: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<i64, String> {
    arguments
        .get(name)
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| format!("`{name}` is not a whole number"))
}

fn flag(
    arguments: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<bool, String> {
    arguments
        .get(name)
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| format!("`{name}` is not true or false"))
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
    pub fn start(&mut self, tree: &DesktopTree, start: &PeerStreamStart) -> Option<TabId> {
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

    /// The tile standing in `tab` .
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

    /// End the share of `surface` : its tile goes, so the tab shows what this desktop holds for it
    /// again. The tab it stood in, when it stood in one.
    pub fn end(&mut self, peer: PeerId, surface: SurfaceId, why: &str) -> Option<TabId> {
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
/// tab shows another device's window, else the first such tab of its app in the active workspace
/// that `unclaimed` says holds no other stream.
fn stream_tab_in(
    tree: &DesktopTree,
    app_id: &str,
    tab: TabId,
    unclaimed: impl Fn(TabId) -> bool,
) -> Option<TabId> {
    let elsewhere = |candidate: &Tab| {
        matches!(
            candidate.tile,
            ModelTile::PeerWindow { .. } | ModelTile::HostedWindow { .. }
        )
    };
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
        ModelTile::PeerWindow { app_id: window, .. }
        | ModelTile::HostedWindow { app_id: window, .. } => window == app_id,
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

    /// Where a point in the tile is in the sender's own pixels, so a click on a scaled picture
    /// lands where the person pointed.
    fn stream_point(&self, point: Point) -> (f64, f64) {
        let state = self.state.borrow();
        if self.rect.width() <= 0.0
            || self.rect.height() <= 0.0
            || state.width == 0
            || state.height == 0
        {
            return (point.x, point.y);
        }
        (
            (point.x - self.rect.x0) / self.rect.width() * f64::from(state.width),
            (point.y - self.rect.y0) / self.rect.height() * f64::from(state.height),
        )
    }
}

/// One key of the keyboard as the wire names it: a Linux evdev key code, which the owner translates
/// into its own keyboard.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per key: the wire's code for each key of the keyboard"
)]
fn evdev_keycode(code: winit::keyboard::KeyCode) -> Option<u32> {
    use winit::keyboard::KeyCode as K;
    let code = match code {
        K::KeyA => 30,
        K::KeyB => 48,
        K::KeyC => 46,
        K::KeyD => 32,
        K::KeyE => 18,
        K::KeyF => 33,
        K::KeyG => 34,
        K::KeyH => 35,
        K::KeyI => 23,
        K::KeyJ => 36,
        K::KeyK => 37,
        K::KeyL => 38,
        K::KeyM => 50,
        K::KeyN => 49,
        K::KeyO => 24,
        K::KeyP => 25,
        K::KeyQ => 16,
        K::KeyR => 19,
        K::KeyS => 31,
        K::KeyT => 20,
        K::KeyU => 22,
        K::KeyV => 47,
        K::KeyW => 17,
        K::KeyX => 45,
        K::KeyY => 21,
        K::KeyZ => 44,
        K::Digit0 => 11,
        K::Digit1 => 2,
        K::Digit2 => 3,
        K::Digit3 => 4,
        K::Digit4 => 5,
        K::Digit5 => 6,
        K::Digit6 => 7,
        K::Digit7 => 8,
        K::Digit8 => 9,
        K::Digit9 => 10,
        K::Escape => 1,
        K::Minus => 12,
        K::Equal => 13,
        K::Backspace => 14,
        K::Tab => 15,
        K::BracketLeft => 26,
        K::BracketRight => 27,
        K::Enter => 28,
        K::ControlLeft => 29,
        K::Semicolon => 39,
        K::Quote => 40,
        K::Backquote => 41,
        K::ShiftLeft => 42,
        K::Backslash => 43,
        K::Comma => 51,
        K::Period => 52,
        K::Slash => 53,
        K::ShiftRight => 54,
        K::NumpadMultiply => 55,
        K::AltLeft => 56,
        K::Space => 57,
        K::CapsLock => 58,
        K::F1 => 59,
        K::F2 => 60,
        K::F3 => 61,
        K::F4 => 62,
        K::F5 => 63,
        K::F6 => 64,
        K::F7 => 65,
        K::F8 => 66,
        K::F9 => 67,
        K::F10 => 68,
        K::NumLock => 69,
        K::ScrollLock => 70,
        K::Numpad7 => 71,
        K::Numpad8 => 72,
        K::Numpad9 => 73,
        K::NumpadSubtract => 74,
        K::Numpad4 => 75,
        K::Numpad5 => 76,
        K::Numpad6 => 77,
        K::NumpadAdd => 78,
        K::Numpad1 => 79,
        K::Numpad2 => 80,
        K::Numpad3 => 81,
        K::Numpad0 => 82,
        K::NumpadDecimal => 83,
        K::F11 => 87,
        K::F12 => 88,
        K::NumpadEnter => 96,
        K::ControlRight => 97,
        K::NumpadDivide => 98,
        K::PrintScreen => 99,
        K::AltRight => 100,
        K::Home => 102,
        K::ArrowUp => 103,
        K::PageUp => 104,
        K::ArrowLeft => 105,
        K::ArrowRight => 106,
        K::End => 107,
        K::ArrowDown => 108,
        K::PageDown => 109,
        K::Insert => 110,
        K::Delete => 111,
        K::SuperLeft => 125,
        K::SuperRight => 126,
        _ => return None,
    };
    Some(code)
}

/// The wire's name for a pointer button: the libinput code the owner's compositor replays.
fn evdev_button(button: winit::event::MouseButton) -> u32 {
    use winit::event::MouseButton;
    match button {
        MouseButton::Left => 272,
        MouseButton::Right => 273,
        MouseButton::Middle => 274,
        MouseButton::Back => 275,
        MouseButton::Forward => 276,
        MouseButton::Other(other) => u32::from(other),
    }
}

/// What the held modifiers are, in the xkb bitfield the wire carries: shift,
/// control, alt and super, at the bits the owner reads.
fn modifier_bits(modifiers: winit::keyboard::ModifiersState) -> u32 {
    u32::from(modifiers.shift_key())
        | (u32::from(modifiers.control_key()) << 2)
        | (u32::from(modifiers.alt_key()) << 3)
        | (u32::from(modifiers.super_key()) << 6)
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

    fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: ui::kurbo::Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.child
            .draw(scene, transform, text_renderer, theme, presence);
    }

    /// A person's pointer and keys are the tile's own actions, asked exactly as anything else
    /// driving the tile asks them: the share's rule refuses them the same way for both.
    fn handle_event(&mut self, event: &winit::event::WindowEvent, ctx: &mut EventCtx<'_>) {
        use winit::event::{ElementState, WindowEvent};
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if !self.rect.contains(point) {
                    return;
                }
                let (x, y) = self.stream_point(point);
                let mut arguments = serde_json::Map::new();
                arguments.insert("x".into(), serde_json::Value::from(x));
                arguments.insert("y".into(), serde_json::Value::from(y));
                if self.ask(PeerAction::PointerMotion, &arguments) {
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let mut arguments = serde_json::Map::new();
                arguments.insert(
                    "button".into(),
                    serde_json::Value::from(evdev_button(*button)),
                );
                arguments.insert(
                    "pressed".into(),
                    serde_json::Value::from(*state == ElementState::Pressed),
                );
                if self.ask(PeerAction::PointerButton, &arguments) {
                    *ctx.handled = true;
                    if *state == ElementState::Pressed {
                        // Clicking a window puts the keyboard on it: the keys that follow are meant
                        // for the shared one.
                        *ctx.focus_target = Some(ctx.node_id);
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let ui::winit::keyboard::PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                let Some(keysym) = evdev_keycode(code) else {
                    return;
                };
                let mut arguments = serde_json::Map::new();
                arguments.insert("keysym".into(), serde_json::Value::from(keysym));
                arguments.insert(
                    "pressed".into(),
                    serde_json::Value::from(event.state == ElementState::Pressed),
                );
                arguments.insert(
                    "modifiers".into(),
                    serde_json::Value::from(modifier_bits(ctx.modifiers.state())),
                );
                if self.ask(PeerAction::Key, &arguments) {
                    *ctx.handled = true;
                }
            }
            _ => {}
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
        &[
            POINTER_MOTION_OPERATION,
            POINTER_BUTTON_OPERATION,
            KEY_OPERATION,
            KEYFRAME_OPERATION,
            END_OPERATION,
        ]
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
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        let Some(action) = PeerAction::of_type(request.type_id) else {
            return Ok(None);
        };
        let arguments: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(request.payload).map_err(|error| error.to_string())?;
        self.state
            .borrow_mut()
            .act(action, &arguments)
            .map(|output| Some(ResourceOperationStart::Completed(output)))
    }
}

/// What a host's [`TileSource`](crate::TileSource) answers with while a peer streams into a tab:
/// the share's own tile there, and everything else from the host.
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
        let shared = self.peers.borrow().tile_host(tab.id);
        if let Some(host) = shared {
            return host;
        }
        self.inner.tile(workspace, tab)
    }

    fn launch(&mut self, workspace: &Workspace, id: TabId, app: &str) -> Option<Tab> {
        self.inner.launch(workspace, id, app)
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

    fn window_record(&self, tab: TabId) -> Option<(String, String)> {
        self.inner.window_record(tab)
    }

    fn carry(&self, tab: TabId, to: WorkspaceSlot) -> bool {
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

    /// One encoded unit of a shared stream ([`NeighbourMsg::SurfaceFrame`]) goes to the decoder of
    /// the tile showing that surface. `None` for a stream this desktop shows nowhere.
    pub fn peer_surface_frame(&self, message: &NeighbourMsg) -> Option<PeerOutcome> {
        let NeighbourMsg::SurfaceFrame {
            surface,
            source_peer,
            encoded_h264,
            seq,
            ..
        } = message
        else {
            return None;
        };
        let peers = Rc::clone(&self.shared.borrow().peers);
        peers
            .borrow_mut()
            .push(*source_peer, *surface, *seq, encoded_h264)
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

    /// The share of `surface` ended ([`NeighbourMsg::SurfaceStreamEnd`]), and why: its tile goes,
    /// and the tab shows what this desktop holds for it again.
    pub fn end_peer_surface(&self, peer: PeerId, surface: SurfaceId, why: &str) -> Option<TabId> {
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

    /// The monitor of the display the tile in `tab` stands on, while the desk spreads over several;
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
        let (collected, changed) = {
            let mut peers = peers.borrow_mut();
            let collected = peers.collect_pictures();
            let changed = peers.take_changed_tabs();
            (collected, changed)
        };
        let mut shared = self.shared.borrow_mut();
        for tab in &changed {
            if !shared.rebuild.contains(tab) {
                shared.rebuild.push(*tab);
            }
        }
        drop(shared);
        collected || !changed.is_empty()
    }
}
