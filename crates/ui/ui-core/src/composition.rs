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
    pub semantics: Option<crate::a11y::AccessNode>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub focusable: bool,
    /// Its children stand where its layout node's style puts them, so a
    /// host layout engine lays them out from `layout`; otherwise the
    /// widget places them itself and a host places them at their `rect`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub flows: bool,
    /// It shows as a picture of its paint ([`page::shows_paint`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<Picture>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Composed>,
}

/// The picture of a widget's paint a page shows.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Picture {
    /// What it was drawn from: the picture is drawn again when this changes.
    pub version: u64,
    /// The picture, as a `data:` address of a PNG
    /// ([`crate::headless::FrameHarness::paint_pictures`]); none until a host draws it.
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
            picture: None,
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
