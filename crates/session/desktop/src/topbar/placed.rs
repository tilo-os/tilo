//! What the bar's layout decided beyond where each part stands: the words and faces each part
//! shows, as [`TopBarChrome::lay_out_bar`](super::TopBarChrome) chose them for the room it had.
//! Painting and the page's HTML both read it; neither measures or places anything again.

use ui::icons::Icon;
use vello::kurbo::Rect;
use vello::peniko::Color;

use super::AppTrayItem;

/// The bar's laid-out content. Where each part stands is kept beside it, in the bar's own rects
/// (`ws_pill_rect`, `tray_pill_rect`, ...), which its targets read.
#[derive(Default)]
pub(super) struct BarPlaced {
    /// The bar's middle line.
    pub center_y: f64,
    /// The workspace's name, elided to the room it had.
    pub workspace_label: String,
    /// The strip's name and the room its label had.
    pub strip: Option<(String, f64)>,
    pub media: Option<MediaPlaced>,
    /// The application tray's items, in the order they stand left to right.
    pub app_tray: Vec<AppTrayItem>,
    /// Where the person's own face ends (its right edge), when it stands on the bar.
    pub own_face: Option<f64>,
    /// The sharing disclosure's words and the room its label had.
    pub sharing: Option<(String, f64)>,
    /// The watching count's words and where it stands.
    pub watching: Option<(String, Rect)>,
    /// The others' faces shown, by roster index, the badge counting the rest, and where the stack
    /// ends.
    pub faces: Option<FacesPlaced>,
    /// The people button stands for who is here: whether the pointer lights it.
    pub people_button: Option<bool>,
    /// The "+N" the people button wears.
    pub people_count: Option<String>,
    pub arrival: Option<ArrivalPlaced>,
    /// The wave notice's words and where it stands.
    pub wave: Option<(String, Rect)>,
    /// The continuity offer's words.
    pub continuity: Option<String>,
    /// How far the centre moved to stand clear of the right side.
    pub center_dx: f64,
    /// Where the people cluster begins, before the notices left of it.
    pub people_x: f64,
    /// The ask about the person stands beside the right side (the full bar).
    pub ask_beside: bool,
}

/// The media indicator: what is in use and by whom.
pub(super) struct MediaPlaced {
    pub name: String,
    pub glyphs: Vec<Icon>,
    pub label: String,
    pub tone: Option<Color>,
}

impl MediaPlaced {
    /// The chip the indicator is.
    pub fn chip_of<'a>(
        name: &'a str,
        glyphs: &'a [Icon],
        label: &'a str,
        tone: Option<Color>,
    ) -> ui::widget::BarChipFace<'a> {
        let mut chip = ui::widget::BarChipFace::new(name, &[])
            .icons(glyphs)
            .at_height(super::BAR_CONTROL_PX);
        if !label.is_empty() {
            chip = chip.label(label);
        }
        if let Some(tone) = tone {
            chip = chip.tone(tone);
        }
        chip
    }
}

/// The others' faces on the bar.
pub(super) struct FacesPlaced {
    /// Roster indices, in the order they stand.
    pub shown: Vec<usize>,
    pub more: usize,
    /// Where the stack ends, and its middle line.
    pub anchor: (f64, f64),
}

/// The arrival status, part way out of the faces.
pub(super) struct ArrivalPlaced {
    pub text: String,
    /// Where it stands once whole.
    pub rect: Rect,
    /// How much of it is out, 0–1.
    pub reveal: f64,
    pub opacity: f32,
    pub hovered: bool,
}
