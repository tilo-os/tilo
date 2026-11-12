//! Another device's window, drawn in this page's desktop from its pictures.
//!
//! A device hosting a window shares it with the workspace as video: one tilo-video/1 object per
//! picture on the window's track of its live plane (`window/<tab>/high`), `tab` being the tab the
//! window stands in on every device of the workspace. This page decodes what that track brings
//! with the browser's own decoder (`WebCodecs`, `tilo_media`'s page decoder) and draws the newest
//! picture in the window's tile, whole and keeping its shape, the same way the native desk places
//! a shared window's picture ([`tilo_desktop_surface::window_input::picture_rect`]). A reader who
//! cannot see the picture finds the tile by the window's title.
//!
//! A picture continues the ones before it: an object that does not follow the last one decoded
//! waits for the next keyframe, which the plane hands a reader that joins and the publisher sends
//! at every group.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use tilo_live::video::{Codec, VideoObject};
use ui::a11y::{AccessNode, Role};
use ui::kurbo::{Affine, Point, Rect};
use ui::peniko::{Blob, Fill, ImageBrush, ImageData};
use ui::taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::time::{Duration, Instant};
use ui::widget::{AppKind, AppWidget, IconDescriptor, TaffyTreeExt, TileContent, TileHost, Widget};
use ui::{TextRenderer, Theme};

/// How soon a tile looks again for a picture the decoder still owes: about a frame of a page that
/// draws at 120 Hz.
const LATE_PICTURE: Duration = Duration::from_millis(8);

/// One picture a decoder made: RGBA, `width` by `height`.
pub(crate) struct Picture {
    pub rgba: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

/// What decodes one window's pictures: the browser's decoder in a page.
pub(crate) trait Decoder {
    /// Hand one access unit over; the newest picture decoded since the last call, if any.
    ///
    /// # Errors
    ///
    /// Why the decoder can decode nothing more.
    fn decode(&mut self, unit: &[u8], keyframe: bool) -> Result<Option<Picture>, String>;
    /// A picture decoded since the last call that no new unit collected.
    ///
    /// # Errors
    ///
    /// Why the decoder can decode nothing more.
    fn take(&mut self) -> Result<Option<Picture>, String>;
    /// Whether a unit handed over is still being decoded.
    fn pending(&self) -> bool;
}

/// Opens a decoder for a codec, or says why this page cannot decode it.
pub(crate) type OpenDecoder = fn(Codec) -> Result<Box<dyn Decoder>, String>;

/// One window this page draws.
pub(crate) struct Window {
    /// The window's size in its own pixels, as its publisher said with the last picture.
    size: (u32, u32),
    codec: Option<Codec>,
    decoder: Option<Box<dyn Decoder>>,
    /// The sequence the next object continues the decoded pictures at; `None` while the window
    /// waits for a keyframe.
    next: Option<u64>,
    /// The newest picture, as the renderer draws it.
    image: Option<ImageData>,
    /// Why this page cannot show the window's pictures.
    failed: Option<String>,
}

/// What an object did to the window's tile.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Heard {
    /// Nothing the tile shows changed.
    Nothing,
    /// A newer picture is in.
    Picture,
    /// The tile shows something else than before: its first picture, or why it has none.
    Tile,
}

impl Window {
    const fn new() -> Self {
        Self {
            size: (0, 0),
            codec: None,
            decoder: None,
            next: None,
            image: None,
            failed: None,
        }
    }

    /// Take one tilo-video/1 object of the window's track, decoding it with a decoder `open` makes
    /// for its codec the first time it comes.
    pub(crate) fn heard(&mut self, body: &[u8], open: OpenDecoder) -> Heard {
        let Ok(object) = VideoObject::decode(body) else {
            return Heard::Nothing;
        };
        if self.failed.is_some() {
            return Heard::Nothing;
        }
        if !object.keyframe && self.next != Some(object.sequence) {
            // A picture that does not continue the decoded ones: the next group starts afresh.
            self.next = None;
            return Heard::Nothing;
        }
        if self.decoder.is_none() || self.codec != Some(object.codec) {
            match open(object.codec) {
                Ok(decoder) => {
                    self.decoder = Some(decoder);
                    self.codec = Some(object.codec);
                }
                Err(why) => {
                    self.failed = Some(why);
                    return Heard::Tile;
                }
            }
        }
        self.size = (u32::from(object.width), u32::from(object.height));
        let Some(decoder) = self.decoder.as_mut() else {
            return Heard::Nothing;
        };
        let Ok(picture) = decoder.decode(&object.payload, object.keyframe) else {
            // The decoder broke on this stream: the next keyframe opens another.
            self.decoder = None;
            self.next = None;
            return Heard::Nothing;
        };
        self.next = object.sequence.checked_add(1);
        self.show(picture)
    }

    /// Collect a picture the decoder made since it was last asked.
    fn collect(&mut self) -> Heard {
        let Some(decoder) = self.decoder.as_mut() else {
            return Heard::Nothing;
        };
        let Ok(picture) = decoder.take() else {
            self.decoder = None;
            self.next = None;
            return Heard::Nothing;
        };
        self.show(picture)
    }

    fn show(&mut self, picture: Option<Picture>) -> Heard {
        let Some(picture) = picture else {
            return Heard::Nothing;
        };
        if picture.width == 0 || picture.height == 0 {
            return Heard::Nothing;
        }
        let first = self.image.is_none();
        self.image = Some(ui::raster::rgba8(
            Blob::new(picture.rgba),
            picture.width,
            picture.height,
        ));
        if first { Heard::Tile } else { Heard::Picture }
    }

    /// Whether a picture of the window is here to draw.
    pub(crate) const fn pictured(&self) -> bool {
        self.image.is_some()
    }
}

thread_local! {
    static WINDOWS: RefCell<HashMap<u64, Rc<RefCell<Window>>>> = RefCell::new(HashMap::new());
}

/// Take one object of the pictures of window `tab`, which `from` publishes on `workspace_id`'s
/// plane for the page's peer `id`: the mounts drawing that workspace show what it changed.
#[cfg(target_arch = "wasm32")]
pub(crate) fn heard(id: u32, workspace_id: u64, tab: u64, body: &[u8]) {
    let window = WINDOWS.with_borrow_mut(|windows| {
        Rc::clone(
            windows
                .entry(tab)
                .or_insert_with(|| Rc::new(RefCell::new(Window::new()))),
        )
    });
    let heard = window.borrow_mut().heard(body, page_decoder);
    match heard {
        Heard::Nothing => {}
        Heard::Picture => crate::browser::presence::heard(id, workspace_id),
        // What the tile is changes: the desk builds it again.
        Heard::Tile => crate::browser::presence::page_arrived(id, workspace_id, tab),
    }
}

/// The tile of window `tab` of app `app_id` while this page draws its pictures, named `title` for a
/// reader: `None` while no picture of it is here, so the desk's own tile answers that tab.
pub(crate) fn tile(tab: u64, title: &str, app_id: &str) -> Option<TileHost> {
    let window = WINDOWS.with_borrow(|windows| windows.get(&tab).cloned())?;
    if let Some(why) = window.borrow().failed.clone() {
        return Some(TileHost::new(TileContent::Notice {
            title: title.to_owned(),
            detail: why,
        }));
    }
    if !window.borrow().pictured() {
        return None;
    }
    Some(TileHost::new(TileContent::App(Box::new(WindowView::new(
        window, title, app_id,
    )))))
}

/// The browser's decoder for `codec`.
#[cfg(target_arch = "wasm32")]
fn page_decoder(codec: Codec) -> Result<Box<dyn Decoder>, String> {
    use tilo_media::realtime::{DecoderConfig, RealtimeDecoder, StreamCodec};
    let codec = match codec {
        Codec::H264 => StreamCodec::H264,
        Codec::Vp9 => StreamCodec::Vp9,
        Codec::Av1 => StreamCodec::Av1,
        Codec::Vp8 => return Err("This browser cannot show the window's VP8 pictures".into()),
    };
    let decoder = RealtimeDecoder::new(DecoderConfig::new(codec))
        .map_err(|_| "This browser cannot decode the window's pictures".to_owned())?;
    Ok(Box::new(PageDecoder(decoder)))
}

#[cfg(target_arch = "wasm32")]
struct PageDecoder(tilo_media::realtime::RealtimeDecoder);

#[cfg(target_arch = "wasm32")]
fn page_picture(picture: Option<tilo_media::realtime::DecodedPicture>) -> Option<Picture> {
    let frame = picture?.frame;
    let tilo_media::FramePayload::Rgba(rgba) = frame.payload else {
        return None;
    };
    Some(Picture {
        rgba,
        width: frame.width,
        height: frame.height,
    })
}

#[cfg(target_arch = "wasm32")]
impl Decoder for PageDecoder {
    fn decode(&mut self, unit: &[u8], keyframe: bool) -> Result<Option<Picture>, String> {
        self.0
            .decode(unit, Some(keyframe))
            .map(page_picture)
            .map_err(|error| error.to_string())
    }

    fn take(&mut self) -> Result<Option<Picture>, String> {
        self.0
            .take_late_picture()
            .map(page_picture)
            .map_err(|error| error.to_string())
    }

    fn pending(&self) -> bool {
        self.0.picture_pending()
    }
}

/// The tile showing a window's newest picture.
pub(crate) struct WindowView {
    window: Rc<RefCell<Window>>,
    rect: Rect,
    /// What the window is called: its tile's name for a reader and in the window bar.
    title: String,
    /// The app the window is of, as the hosting device names it: the mark the window wears.
    app_id: String,
}

impl WindowView {
    fn new(window: Rc<RefCell<Window>>, title: &str, app_id: &str) -> Self {
        Self {
            window,
            rect: Rect::ZERO,
            title: title.to_owned(),
            app_id: app_id.to_owned(),
        }
    }
}

impl AppWidget for WindowView {
    fn app_kind(&self) -> AppKind {
        AppKind::App(ui::icons::app_glyph(&self.app_id, None))
    }

    fn app_title(&self) -> &str {
        &self.title
    }

    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("\u{f0ac}")
    }
}

impl Widget for WindowView {
    fn name(&self) -> &'static str {
        "WindowView"
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                min_size: Size {
                    width: Dimension::length(0.0),
                    height: Dimension::length(0.0),
                },
                ..Style::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        // What the picture leaves of the tile is the tile's own ground.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        let mut window = self.window.borrow_mut();
        // A picture the decoder finished since the last one came is the newest.
        let _ = window.collect();
        let Some(image) = window.image.clone() else {
            return;
        };
        let Some(shown) = tilo_desktop_surface::window_input::picture_rect(
            self.rect,
            (image.width, image.height),
        ) else {
            return;
        };
        let scale = shown.width() / f64::from(image.width);
        let placed = Affine::scale(scale).then_translate(shown.origin().to_vec2());
        scene.fill(
            Fill::NonZero,
            transform,
            &ImageBrush::new(image),
            Some(placed),
            &shown,
        );
    }

    /// While the decoder still owes a picture, the tile looks for it again soon.
    fn next_wake_deadline(&self) -> Option<Instant> {
        let window = self.window.borrow();
        let owed = window
            .decoder
            .as_ref()
            .is_some_and(|decoder| decoder.pending());
        owed.then(|| Instant::now().checked_add(LATE_PICTURE))
            .flatten()
    }

    /// The picture is the window: a reader finds it by the window's title.
    fn accessibility(&self) -> Option<AccessNode> {
        Some(
            AccessNode::new(Role::Image)
                .with_name(self.title.clone())
                .with_bounds(self.rect),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A decoder that makes a 4 x 2 picture of each unit it decodes, the picture a unit's first
    /// byte asks for, and counts the decoders made.
    struct Counted;

    thread_local! {
        static OPENED: RefCell<Vec<Codec>> = const { RefCell::new(Vec::new()) };
    }

    impl Decoder for Counted {
        fn decode(&mut self, unit: &[u8], _keyframe: bool) -> Result<Option<Picture>, String> {
            if unit == b"broken" {
                return Err("broken".into());
            }
            Ok(Some(Picture {
                rgba: Arc::new(vec![unit[0]; 4 * 2 * 4]),
                width: 4,
                height: 2,
            }))
        }

        fn take(&mut self) -> Result<Option<Picture>, String> {
            Ok(None)
        }

        fn pending(&self) -> bool {
            false
        }
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "it opens decoders as the page's opener does"
    )]
    fn counted(codec: Codec) -> Result<Box<dyn Decoder>, String> {
        OPENED.with_borrow_mut(|opened| opened.push(codec));
        Ok(Box::new(Counted))
    }

    fn unsupported(_: Codec) -> Result<Box<dyn Decoder>, String> {
        Err("This browser cannot decode the window's pictures".into())
    }

    fn object(sequence: u64, keyframe: bool, payload: &[u8]) -> Vec<u8> {
        VideoObject {
            keyframe,
            codec: Codec::H264,
            layer: tilo_live::video::Layer::High,
            sequence,
            timestamp_us: sequence.saturating_mul(33_333),
            width: 1280,
            height: 720,
            payload: payload.to_vec(),
        }
        .encode()
        .expect("a valid object")
    }

    fn first_byte(window: &Window) -> Option<u8> {
        window.image.as_ref().map(|image| image.data.data()[0])
    }

    #[test]
    fn pictures_from_a_keyframe_on_are_drawn_in_order() {
        OPENED.with_borrow_mut(Vec::clear);
        let mut window = Window::new();
        // A reader that joins mid-group waits for the keyframe.
        assert_eq!(
            window.heard(&object(4, false, &[4]), counted),
            Heard::Nothing
        );
        assert!(!window.pictured());
        assert_eq!(window.heard(&object(5, true, &[5]), counted), Heard::Tile);
        assert_eq!(window.size, (1280, 720));
        assert_eq!(
            window.heard(&object(6, false, &[6]), counted),
            Heard::Picture
        );
        assert_eq!(first_byte(&window), Some(6));
        // One decoder serves the whole stream.
        assert_eq!(OPENED.with_borrow(Vec::len), 1);
    }

    #[test]
    fn a_lost_picture_waits_for_the_next_keyframe() {
        let mut window = Window::new();
        window.heard(&object(1, true, &[1]), counted);
        // 2 was lost: 3 cannot be decoded, nor anything after it until a keyframe.
        assert_eq!(
            window.heard(&object(3, false, &[3]), counted),
            Heard::Nothing
        );
        assert_eq!(
            window.heard(&object(4, false, &[4]), counted),
            Heard::Nothing
        );
        assert_eq!(first_byte(&window), Some(1));
        assert_eq!(
            window.heard(&object(9, true, &[9]), counted),
            Heard::Picture
        );
        assert_eq!(
            window.heard(&object(10, false, &[10]), counted),
            Heard::Picture
        );
        assert_eq!(first_byte(&window), Some(10));
    }

    #[test]
    fn a_broken_decoder_is_replaced_at_the_next_keyframe() {
        OPENED.with_borrow_mut(Vec::clear);
        let mut window = Window::new();
        window.heard(&object(1, true, &[1]), counted);
        assert_eq!(
            window.heard(&object(2, false, b"broken"), counted),
            Heard::Nothing
        );
        assert_eq!(
            window.heard(&object(3, false, &[3]), counted),
            Heard::Nothing
        );
        assert_eq!(
            window.heard(&object(4, true, &[4]), counted),
            Heard::Picture
        );
        assert_eq!(OPENED.with_borrow(Vec::len), 2);
    }

    #[test]
    fn a_window_this_page_cannot_decode_says_so_and_bytes_that_are_no_picture_are_dropped() {
        let mut window = Window::new();
        assert_eq!(window.heard(b"not a picture", counted), Heard::Nothing);
        assert_eq!(
            window.heard(&object(1, true, &[1]), unsupported),
            Heard::Tile
        );
        assert_eq!(
            window.failed.as_deref(),
            Some("This browser cannot decode the window's pictures")
        );
        assert_eq!(
            window.heard(&object(2, true, &[2]), counted),
            Heard::Nothing
        );
    }

    #[test]
    fn the_tile_is_the_picture_named_by_the_windows_title() {
        const TAB: u64 = 0x5eed;
        assert!(
            tile(TAB, "Calculator", "org.gnome.Calculator").is_none(),
            "no picture yet"
        );
        let window = Rc::new(RefCell::new(Window::new()));
        window.borrow_mut().heard(&object(1, true, &[1]), counted);
        WINDOWS.with_borrow_mut(|windows| windows.insert(TAB, Rc::clone(&window)));
        assert!(tile(TAB, "Calculator", "org.gnome.Calculator").is_some());
        let mut view = WindowView::new(window, "Calculator", "org.gnome.Calculator");
        view.rect = Rect::new(0.0, 0.0, 640.0, 480.0);
        let node = view.accessibility().expect("a node");
        assert_eq!(node.role, Role::Image);
        assert_eq!(node.name.as_deref(), Some("Calculator"));
    }
}
