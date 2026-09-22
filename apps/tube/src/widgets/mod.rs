//! App-local widgets + small composition helpers.
//!
//! `ui` owns the canonical toolkit; these are Tube-specific primitives we
//! either can't express with the stock widgets (a `SponsorBlock` seek bar, a
//! gradient thumbnail, vector icons) or want clickable in our own
//! [`ActionBus`] dialect (`Flex`, `Switch`). The free helpers at the bottom
//! cut the boilerplate of wrapping `ui::Text` and building spacers.

pub mod channel_avatar;
pub mod controls;
pub mod flex;
pub mod image_box;
pub mod large_video_meta;
pub mod player_host;
pub mod seek_bar;
pub mod thumbnail;

pub use channel_avatar::ChannelAvatar;
pub use controls::Switch;
pub use flex::{Align, Axis, Flex, IntoElement, Justify, Len, Surface};
pub use image_box::ImageBox;
pub use large_video_meta::LargeVideoMeta;
pub use player_host::{PlayerHost, SharedPlayer};
pub use seek_bar::SeekBar;
pub use thumbnail::Thumbnail;

use ui::theme::Theme;
use ui::widget::Text;
use vello::kurbo::{Point, Rect};
use vello::peniko::Color;

// --- Taffy layout helpers ------------------------------------------------
//
// `TaffyTree::new_leaf`/`new_with_children`/`layout` are typed fallible for
// API generality, but every leaf widget in this module calls them the exact
// way `ui` itself does internally (see `ui::widget::TaffyTreeExt`'s doc
// comment, which this mirrors): `new_leaf` always inserts and returns `Ok`;
// `layout` is only ever queried here for a node this same widget just built
// and had computed by this frame's `compute_layout` pass. Centralised here
// (rather than one `#[allow]` per call site) so every widget's
// `build_layout`/`apply_layout` reads the same as the rest of the toolkit.

/// Insert a leaf taffy node. See the module doc comment above for why the
/// internal `unwrap` cannot actually panic.
pub(crate) fn leaf_node(taffy: &mut taffy::TaffyTree<()>, style: taffy::Style) -> taffy::NodeId {
    #[allow(
        clippy::unwrap_used,
        reason = "see widgets Taffy layout helpers doc comment"
    )]
    let node = taffy.new_leaf(style).unwrap();
    node
}

/// Insert a parent taffy node over already-built `children`. See the module
/// doc comment above for why the internal `unwrap` cannot actually panic.
pub(crate) fn parent_node(
    taffy: &mut taffy::TaffyTree<()>,
    style: taffy::Style,
    children: &[taffy::NodeId],
) -> taffy::NodeId {
    #[allow(
        clippy::unwrap_used,
        reason = "see widgets Taffy layout helpers doc comment"
    )]
    let node = taffy.new_with_children(style, children).unwrap();
    node
}

/// This node's just-computed layout. See the module doc comment above for
/// why the internal `unwrap` cannot actually panic. Prefer [`layout_rect`]
/// when a plain `origin + layout` rect is all that's needed.
pub(crate) fn node_layout(taffy: &taffy::TaffyTree<()>, node: taffy::NodeId) -> &taffy::Layout {
    #[allow(
        clippy::unwrap_used,
        reason = "see widgets Taffy layout helpers doc comment"
    )]
    let l = taffy.layout(node).unwrap();
    l
}

/// This widget's on-screen rect: `origin` plus the just-computed layout for
/// `node`. See the module doc comment above for why the internal `unwrap`
/// cannot actually panic.
pub(crate) fn layout_rect(
    taffy: &taffy::TaffyTree<()>,
    node: taffy::NodeId,
    origin: Point,
) -> Rect {
    #[allow(
        clippy::unwrap_used,
        reason = "see widgets Taffy layout helpers doc comment"
    )]
    let l = taffy.layout(node).unwrap();
    Rect::new(
        origin.x + f64::from(l.location.x),
        origin.y + f64::from(l.location.y),
        origin.x + f64::from(l.location.x) + f64::from(l.size.width),
        origin.y + f64::from(l.location.y) + f64::from(l.size.height),
    )
}

/// A line of text at `size` in the theme's primary colour.
pub fn text(s: impl AsRef<str>, size: f32) -> Text {
    Text::new(s.as_ref()).with_scaled_size(size)
}

/// A line of text in an explicit colour.
pub fn text_c(s: impl AsRef<str>, size: f32, color: Color) -> Text {
    Text::new(s.as_ref())
        .with_scaled_size(size)
        .with_color(color)
}

/// Dim secondary text (channel names, metadata).
pub fn dim(theme: &Theme, s: impl AsRef<str>, size: f32) -> Text {
    text_c(s, size, theme.text_dim)
}

/// A fixed-size empty box, useful as a gap/spacer.
pub fn gap(w: f32, h: f32) -> Flex {
    Flex::row().fixed(w, h)
}

/// A flexible spacer that eats remaining space on the main axis.
pub fn spring() -> Flex {
    Flex::row().grow(1.0)
}

/// A 1 px horizontal rule on the page ground, derived against
/// `surfaces.base` (HIG §9.5); see [`divider_on`] for rules inside a card.
pub fn divider(theme: &Theme) -> Flex {
    divider_on(theme.tokens.colors.surfaces.base)
}

/// A 1 px rule inside a surface other than the page: derived against
/// `ground` (HIG §9.5), so a rule inside a raised card registers in the dark
/// preset, where the declared `border_standard` is three shades from
/// `raised` and vanishes.
pub fn divider_on(ground: Color) -> Flex {
    Flex::row().fill_w().h(Len::Px(1.0)).surface(Surface {
        fill: Some(ui::surface::edge_on(ground)),
        radius: 0.0,
        border: None,
    })
}

/// Card surface treatment used by panels across the app: raised fill with
/// its derived rest edge (HIG §9.5).
pub fn card_surface(theme: &Theme) -> Surface {
    let fill = theme.tokens.colors.surfaces.raised;
    Surface {
        fill: Some(fill),
        radius: f64::from(theme.tokens.radii.lg),
        border: Some((1.0, ui::surface::edge_on(fill))),
    }
}

/// A subtle "chip"/pill surface.
pub fn chip_surface(theme: &Theme, active: bool) -> Surface {
    if active {
        Surface {
            fill: Some(theme.accent_primary),
            radius: f64::from(theme.tokens.radii.full),
            border: None,
        }
    } else {
        subtle_surface(theme)
    }
}

/// The colour of a *resting* subtle surface — icon buttons, inactive chips,
/// the small veneer panels that float on the base background.
///
/// The neutral ramp runs darkest→lightest, so the high stops (`[6]`..`[8]`,
/// slate-400..slate-200) are near-*white*. Using them as a fill on the dark
/// base painted a bright box that no dark-theme foreground could read on top of
/// (white-on-slate-200 ≈ 1.2:1). `[2]` is a genuine one-step-up surface in both
/// themes (slate-800 in dark, light-grey in light), keeping `text_primary` and
/// `text_dim` readable. Centralised here so every veneer stays theme-correct.
pub fn subtle_fill(theme: &Theme) -> Color {
    theme.tokens.colors.neutral.stops[2]
}

/// The hover/active step of a subtle surface — one ramp step toward the
/// foreground from [`subtle_fill`], so a hover is visible while text stays
/// readable.
pub fn hover_fill(theme: &Theme) -> Color {
    theme.tokens.colors.neutral.stops[3]
}

/// The derived rest edge of a subtle surface (HIG §9.5 "Chip quiet").
pub fn subtle_edge(theme: &Theme) -> Color {
    ui::surface::edge_on(subtle_fill(theme))
}

/// A resting subtle [`Surface`] with a full pill radius and its derived edge.
pub fn subtle_surface(theme: &Theme) -> Surface {
    Surface {
        fill: Some(subtle_fill(theme)),
        radius: f64::from(theme.tokens.radii.full),
        border: Some((1.0, subtle_edge(theme))),
    }
}

/// A hover [`Surface`] with a full pill radius; the edge derives from the
/// hover fill so it keeps registering as the face lifts.
pub fn hover_pill_surface(theme: &Theme) -> Surface {
    let fill = hover_fill(theme);
    Surface {
        fill: Some(fill),
        radius: f64::from(theme.tokens.radii.full),
        border: Some((1.0, ui::surface::edge_on(fill))),
    }
}
