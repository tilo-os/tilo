//! Music-app icon glyphs, drawn from the shared `ui::icons::Icon` set.
//!
//! The toolkit has no icon font, so the chrome (nav rail, transport
//! controls, badges) draws its glyphs directly into the Vello scene. Every
//! icon is centred in a logical `size`×`size` box at `center` and tinted
//! with the caller's (theme-derived) color, so icons inherit the OS theme
//! exactly like text does.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

/// Identifier for each glyph the app needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Home,
    Search,
    Songs,
    Albums,
    Artists,
    Playlist,
    Queue,
    Download,
    Downloaded,
    Play,
    Pause,
    Prev,
    Next,
    Shuffle,
    Repeat,
    RepeatOne,
    Heart,
    HeartFilled,
    VolumeHigh,
    VolumeMute,
    Plus,
    ChevronLeft,
    Note,
    Disc,
}

/// Draw `icon` centred at `center` within a `size`×`size` box, in `color`.
///
/// Every glyph comes from the shared [`ui::icons::Icon`] set — the one
/// themable, Lucide-based icon set the whole OS draws from — so the transport
/// bar, nav rail and badges match every other app. The local [`Icon`] enum is
/// just a music-domain alias mapped onto that shared set here.
pub fn draw(scene: &mut Scene, t: Affine, icon: Icon, center: Point, size: f64, color: Color) {
    use ui::icons::Icon as U;

    let rect = Rect::from_center_size((center.x, center.y), (size, size));

    // Marks whose glyph is a single closed silhouette read best solid-filled.
    // (Skip glyphs keep their open end-bar, so they must stay stroked.)
    let filled = matches!(icon, Icon::Play | Icon::Pause | Icon::HeartFilled);

    let glyph = match icon {
        Icon::Home => U::Home,
        Icon::Search => U::Search,
        Icon::Songs => U::Music,
        Icon::Albums => U::Disc3,
        Icon::Artists => U::Users,
        Icon::Playlist => U::ListMusic,
        Icon::Queue => U::ListOrdered,
        Icon::Download => U::Download,
        Icon::Downloaded => U::Check,
        Icon::Play => U::Play,
        Icon::Pause => U::Pause,
        Icon::Prev => U::SkipBack,
        Icon::Next => U::SkipForward,
        Icon::Shuffle => U::Shuffle,
        Icon::Repeat => U::Repeat,
        Icon::RepeatOne => U::Repeat1,
        Icon::Heart => U::Heart,
        Icon::HeartFilled => U::Heart,
        Icon::VolumeHigh => U::Volume2,
        Icon::VolumeMute => U::VolumeX,
        Icon::Plus => U::Plus,
        Icon::ChevronLeft => U::ChevronLeft,
        Icon::Note => U::Music,
        Icon::Disc => U::Disc3,
    };

    if filled {
        glyph.draw_filled(scene, t, rect, color);
    } else {
        // Lucide's 2/24 stroke ratio, floored so small badges stay legible.
        let stroke_px = (size * 0.083).max(1.5);
        glyph.draw_with_stroke(scene, t, rect, color, stroke_px);
    }
}
