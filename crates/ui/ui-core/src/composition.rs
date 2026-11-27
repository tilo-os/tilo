//! A window's composition: its laid-out widgets, each with the semantics the window already
//! produces for assistive technology (role, name, value, state, actions, bounds), and the CSS of
//! the layout node it stands in. A page renders it as semantic HTML in one place
//! ([`page`]); no widget writes HTML of its own.

use serde::Serialize;

pub mod page;

/// One widget of a composition.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Composed {
    /// The widget's path, as semantic actions name it: also its identity from one composition to the
    /// next.
    pub path: String,
    pub widget: String,
    /// `[x, y, width, height]`, logical pixels in the window.
    pub rect: [f64; 4],
    /// Where its children are clipped to, in the window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip: Option<[f64; 4]>,
    /// Its layout node's CSS, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    /// It scrolls vertically: `[offset, content, viewport]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll: Option<[f64; 3]>,
    /// What it is, as the window tells assistive technology: its role, words, state and the
    /// action targets of what it holds, with the nodes it publishes that have no widget of their
    /// own (a list's rows, a document's blocks) and where they stand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantics: Option<tilo_ui_base::a11y::AccessNode>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub focusable: bool,
    /// Its children stand where its layout node's style puts them, so a
    /// host layout engine lays them out from `layout`; otherwise the
    /// widget places them itself and a host places them at their `rect`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub flows: bool,
    /// A surface over the window appears from this point (`[x, y]`, absolute): the control that
    /// summoned it, the caret, a context menu's pointer ([`tilo_ui_base::widget::Widget::appears_from`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub appears_from: Option<[f64; 2]>,
    /// It shows as a picture of its paint ([`page::shows_paint`]). The host that draws it hands
    /// the picture over, and takes back the claim it made only because the part says nothing else
    /// where that paint is nothing: a page never shows an image of nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<Picture>,
    /// The live picture it shows, by its address in the window's live pictures
    /// ([`crate::live_pictures`]): a host that decodes it draws it in the widget's place, and
    /// no picture of its paint is made for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<String>,
    /// The vector glyph it draws in place of words, by its name in the shared icon set
    /// (`clock`): a host with no paint draws it from the same sources a native host strokes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glyph: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Composed>,
}

/// The picture of a widget's paint a page shows.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Picture {
    /// What it was drawn from: the picture is drawn again when this changes.
    pub version: u64,
    /// The picture, as a `data:` address of a PNG
    /// (the headless driver's `paint_pictures`); none until a host draws it.
    #[serde(skip)]
    pub data: Option<std::sync::Arc<str>>,
}

impl Composed {
    /// A widget at `path` at `rect` in the window, showing nothing yet: what a host that
    /// composes its parts fills in.
    #[must_use]
    pub fn new(path: String, widget: &str, rect: [f64; 4]) -> Self {
        Self {
            path,
            widget: widget.to_owned(),
            rect,
            clip: None,
            layout: None,
            scroll: None,
            semantics: None,
            focusable: false,
            flows: false,
            appears_from: None,
            picture: None,
            live: None,
            glyph: None,
            children: Vec::new(),
        }
    }
}

/// A window's whole composition.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Composition {
    pub viewport: [u32; 2],
    /// The path of the widget keyboard focus is in.
    pub focus: Option<String>,
    pub root: Composed,
    /// What sits over the window (an open menu, the palette), in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<Composed>,
}

impl Composition {
    /// The widget at `path`, over the window or in it.
    #[must_use]
    pub fn find(&self, path: &str) -> Option<&Composed> {
        fn find<'a>(node: &'a Composed, path: &str) -> Option<&'a Composed> {
            if node.path == path {
                return Some(node);
            }
            node.children.iter().find_map(|child| find(child, path))
        }
        std::iter::once(&self.root)
            .chain(&self.layers)
            .find_map(|root| find(root, path))
    }
}

/// How a widget's node is laid out, as CSS says it: the declarations of its layout node's style that a
/// host layout engine needs to lay its children out the same way.
#[must_use]
pub fn css(style: &taffy::Style) -> String {
    use std::fmt::Write as _;
    use taffy::{Dimension, Display, FlexDirection, LengthPercentage, LengthPercentageAuto};
    fn dimension(value: Dimension) -> Option<String> {
        let raw = value.into_raw();
        match raw.tag() {
            taffy::CompactLength::LENGTH_TAG => Some(format!("{}px", raw.value())),
            taffy::CompactLength::PERCENT_TAG => Some(format!("{}%", raw.value() * 100.0)),
            _ => None,
        }
    }
    fn length(value: LengthPercentage) -> String {
        let raw = value.into_raw();
        if raw.tag() == taffy::CompactLength::PERCENT_TAG {
            format!("{}%", raw.value() * 100.0)
        } else {
            format!("{}px", raw.value())
        }
    }
    fn auto(value: LengthPercentageAuto) -> String {
        let raw = value.into_raw();
        match raw.tag() {
            taffy::CompactLength::PERCENT_TAG => format!("{}%", raw.value() * 100.0),
            taffy::CompactLength::AUTO_TAG => "auto".into(),
            _ => format!("{}px", raw.value()),
        }
    }
    let mut out = String::new();
    let display = match style.display {
        Display::Flex => "flex",
        Display::Grid => "grid",
        Display::Block => "block",
        Display::None => "none",
    };
    let _ = write!(out, "display:{display};");
    if style.display == Display::Flex {
        let direction = match style.flex_direction {
            FlexDirection::Row => "row",
            FlexDirection::Column => "column",
            FlexDirection::RowReverse => "row-reverse",
            FlexDirection::ColumnReverse => "column-reverse",
        };
        let _ = write!(out, "flex-direction:{direction};");
        if style.flex_wrap == taffy::FlexWrap::Wrap {
            out.push_str("flex-wrap:wrap;");
        }
    }
    if style.display == Display::Grid {
        grid_css(style, &mut out);
    }
    grid_placement_css(style, &mut out);
    let gap = (length(style.gap.height), length(style.gap.width));
    if gap != ("0px".into(), "0px".into()) {
        let _ = write!(out, "gap:{} {};", gap.0, gap.1);
    }
    let padding = [
        length(style.padding.top),
        length(style.padding.right),
        length(style.padding.bottom),
        length(style.padding.left),
    ];
    if padding.iter().any(|side| side != "0px") {
        let _ = write!(out, "padding:{};", padding.join(" "));
    }
    let margin = [
        auto(style.margin.top),
        auto(style.margin.right),
        auto(style.margin.bottom),
        auto(style.margin.left),
    ];
    if margin.iter().any(|side| side != "0px") {
        let _ = write!(out, "margin:{};", margin.join(" "));
    }
    if let Some(align) = style.align_items {
        let _ = write!(out, "align-items:{};", align_name(align));
    }
    if let Some(align) = style.align_self {
        let _ = write!(out, "align-self:{};", align_name(align));
    }
    if let Some(justify) = style.justify_content {
        let _ = write!(out, "justify-content:{};", content_name(justify));
    }
    if style.flex_grow != 0.0 {
        let _ = write!(out, "flex-grow:{};", style.flex_grow);
    }
    if (style.flex_shrink - 1.0).abs() > f32::EPSILON {
        let _ = write!(out, "flex-shrink:{};", style.flex_shrink);
    }
    if let Some(basis) = dimension(style.flex_basis) {
        let _ = write!(out, "flex-basis:{basis};");
    }
    for (name, value) in [
        ("width", style.size.width),
        ("height", style.size.height),
        ("min-width", style.min_size.width),
        ("min-height", style.min_size.height),
        ("max-width", style.max_size.width),
        ("max-height", style.max_size.height),
    ] {
        if let Some(value) = dimension(value) {
            let _ = write!(out, "{name}:{value};");
        }
    }
    out
}

/// A grid's tracks, how it places what it does not name a place for, and how it packs its rows
/// and columns: what a browser needs to lay its children out where the layout did.
fn grid_css(style: &taffy::Style, out: &mut String) {
    use std::fmt::Write as _;
    use taffy::{GridAutoFlow, GridTemplateComponent, RepetitionCount};
    let template = |tracks: &[GridTemplateComponent<_>]| {
        tracks
            .iter()
            .map(|component| match component {
                GridTemplateComponent::Single(track) => track_css(*track),
                GridTemplateComponent::Repeat(repeat) => {
                    let count = match repeat.count {
                        RepetitionCount::AutoFill => "auto-fill".to_owned(),
                        RepetitionCount::AutoFit => "auto-fit".to_owned(),
                        RepetitionCount::Count(count) => count.to_string(),
                    };
                    let tracks: Vec<String> =
                        repeat.tracks.iter().copied().map(track_css).collect();
                    format!("repeat({count},{})", tracks.join(" "))
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let auto = |tracks: &[taffy::TrackSizingFunction]| {
        tracks
            .iter()
            .copied()
            .map(track_css)
            .collect::<Vec<_>>()
            .join(" ")
    };
    for (name, value) in [
        (
            "grid-template-columns",
            template(&style.grid_template_columns),
        ),
        ("grid-template-rows", template(&style.grid_template_rows)),
        ("grid-auto-columns", auto(&style.grid_auto_columns)),
        ("grid-auto-rows", auto(&style.grid_auto_rows)),
    ] {
        if !value.is_empty() {
            let _ = write!(out, "{name}:{value};");
        }
    }
    let flow = match style.grid_auto_flow {
        GridAutoFlow::Row => None,
        GridAutoFlow::Column => Some("column"),
        GridAutoFlow::RowDense => Some("row dense"),
        GridAutoFlow::ColumnDense => Some("column dense"),
    };
    if let Some(flow) = flow {
        let _ = write!(out, "grid-auto-flow:{flow};");
    }
    if let Some(align) = style.align_content {
        let _ = write!(out, "align-content:{};", align_content_name(align));
    }
    if let Some(justify) = style.justify_items {
        let _ = write!(out, "justify-items:{};", align_name(justify));
    }
}

/// Where a grid's child stands in its grid, when it names a place rather than being placed in
/// turn.
fn grid_placement_css(style: &taffy::Style, out: &mut String) {
    use std::fmt::Write as _;
    use taffy::GridPlacement;
    fn line(placement: &GridPlacement) -> Option<String> {
        match placement {
            GridPlacement::Auto => None,
            GridPlacement::Line(line) => Some(line.as_i16().to_string()),
            GridPlacement::Span(span) => Some(format!("span {span}")),
            GridPlacement::NamedLine(name, index) => Some(format!("{name} {index}")),
            GridPlacement::NamedSpan(name, span) => Some(format!("span {name} {span}")),
        }
    }
    for (name, placement) in [
        ("grid-row", &style.grid_row),
        ("grid-column", &style.grid_column),
    ] {
        match (line(&placement.start), line(&placement.end)) {
            (None, None) => {}
            (start, end) => {
                let _ = write!(
                    out,
                    "{name}:{}/{};",
                    start.as_deref().unwrap_or("auto"),
                    end.as_deref().unwrap_or("auto")
                );
            }
        }
    }
    if let Some(justify) = style.justify_self {
        let _ = write!(out, "justify-self:{};", align_name(justify));
    }
}

/// One grid track's size as CSS: a fixed length, a share of the free space, its content's size,
/// or a range between two of those.
fn track_css(track: taffy::TrackSizingFunction) -> String {
    fn compact(raw: taffy::CompactLength) -> String {
        match raw.tag() {
            taffy::CompactLength::LENGTH_TAG => format!("{}px", raw.value()),
            taffy::CompactLength::PERCENT_TAG => format!("{}%", raw.value() * 100.0),
            taffy::CompactLength::FR_TAG => format!("{}fr", raw.value()),
            taffy::CompactLength::MIN_CONTENT_TAG => "min-content".to_owned(),
            taffy::CompactLength::MAX_CONTENT_TAG => "max-content".to_owned(),
            taffy::CompactLength::FIT_CONTENT_PX_TAG => format!("fit-content({}px)", raw.value()),
            taffy::CompactLength::FIT_CONTENT_PERCENT_TAG => {
                format!("fit-content({}%)", raw.value() * 100.0)
            }
            _ => "auto".to_owned(),
        }
    }
    let (min, max) = (compact(track.min.into_raw()), compact(track.max.into_raw()));
    if max.starts_with("fit-content") {
        max
    } else if min == max {
        min
    } else {
        format!("minmax({min},{max})")
    }
}

fn align_content_name(align: taffy::AlignContent) -> &'static str {
    use taffy::AlignContent;
    match align {
        AlignContent::Start | AlignContent::FlexStart => "start",
        AlignContent::End | AlignContent::FlexEnd => "end",
        AlignContent::Center => "center",
        AlignContent::Stretch => "stretch",
        AlignContent::SpaceBetween => "space-between",
        AlignContent::SpaceAround => "space-around",
        AlignContent::SpaceEvenly => "space-evenly",
    }
}

fn align_name(align: taffy::AlignItems) -> &'static str {
    use taffy::AlignItems;
    match align {
        AlignItems::Start | AlignItems::FlexStart => "flex-start",
        AlignItems::End | AlignItems::FlexEnd => "flex-end",
        AlignItems::Center => "center",
        AlignItems::Baseline => "baseline",
        AlignItems::Stretch => "stretch",
    }
}

fn content_name(align: taffy::JustifyContent) -> &'static str {
    use taffy::JustifyContent;
    match align {
        JustifyContent::Start | JustifyContent::FlexStart => "flex-start",
        JustifyContent::End | JustifyContent::FlexEnd => "flex-end",
        JustifyContent::Center => "center",
        JustifyContent::Stretch => "stretch",
        JustifyContent::SpaceBetween => "space-between",
        JustifyContent::SpaceAround => "space-around",
        JustifyContent::SpaceEvenly => "space-evenly",
    }
}
