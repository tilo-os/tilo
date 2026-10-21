//! The page a reader prints on.
//!
//! A document's page follows the system theme: its window ground and ink.
//! For reading only, a person can choose a warm paper instead: a cream page
//! in a light theme and a warm dark page in a dark one, each printed in the
//! stock theme's ink measured against it. Chrome never reads these; only
//! the page and what is printed on it. High contrast and forced colours
//! reach into the page, so a paper never overrides them.

use serde::{Deserialize, Serialize};
use vello::peniko::Color;

use crate::theme::Theme;

/// What the page is printed on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Paper {
    /// The window's own ground: the page follows the system theme.
    #[default]
    System,
    /// A warm paper tint in the system theme's polarity.
    Warm,
}

impl Paper {
    pub const ALL: [Self; 2] = [Self::System, Self::Warm];

    /// What the reading menu calls it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Theme",
            Self::Warm => "Paper",
        }
    }
}

/// The cream page of the warm paper in a light theme.
pub const WARM_LIGHT_PAGE: Color = Color::from_rgb8(0xF4, 0xEC, 0xDC);
/// The warm page of the warm paper in a dark theme.
pub const WARM_DARK_PAGE: Color = Color::from_rgb8(0x26, 0x21, 0x1B);

/// Resolved colours of one page.
#[derive(Clone, Copy, Debug)]
pub struct ReaderPalette {
    /// The page fill, or `None` when the page is the window ground the
    /// framework already painted.
    pub page: Option<Color>,
    /// The ground the page's ink is measured against.
    pub ground: Color,
    /// Body ink.
    pub ink: Color,
    /// Secondary ink (bylines, captions, quotes).
    pub ink_dim: Color,
    /// Rules: block-quote bar, table rules, dividers.
    pub rule: Color,
    /// The well behind code and the ground of a picture still loading.
    pub well: Color,
    /// The stock theme a warm paper is printed in; `None` for the system's
    /// own.
    stock: Option<Stock>,
}

/// A stock theme a paper borrows its ink from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stock {
    Light,
    Dark,
}

impl Stock {
    fn theme(self) -> Theme {
        match self {
            Self::Light => Theme::light(),
            Self::Dark => Theme::dark(),
        }
    }
}

impl ReaderPalette {
    /// The page `paper` is, under the system `theme`.
    #[must_use]
    pub fn resolve(paper: Paper, theme: &Theme) -> Self {
        if theme.document.palette.high_contrast || theme.forced_colors {
            return Self::of_theme(theme, None, None);
        }
        match paper {
            Paper::System => Self::of_theme(theme, None, None),
            Paper::Warm => {
                if is_dark(theme.tokens.colors.surfaces.content) {
                    Self::of_paper(Stock::Dark, WARM_DARK_PAGE)
                } else {
                    Self::of_paper(Stock::Light, WARM_LIGHT_PAGE)
                }
            }
        }
    }

    /// A page of `page` printed in `stock`'s ink.
    fn of_paper(stock: Stock, page: Color) -> Self {
        Self::of_theme(&stock.theme().on_ground(page), Some(page), Some(stock))
    }

    /// Ink, rules and wells measured from a theme's content ground.
    fn of_theme(theme: &Theme, page: Option<Color>, stock: Option<Stock>) -> Self {
        let colors = &theme.tokens.colors;
        let ground = colors.surfaces.content;
        let on = theme.on_surface(ground);
        ReaderPalette {
            page,
            ground,
            ink: on.ink,
            ink_dim: on.ink_dim,
            rule: crate::surface::edge_on(ground),
            // A whisper of the page's own ink, so the well keeps the paper's
            // tone on every page.
            well: on.tint,
            stock,
        }
    }

    /// The theme what is printed on the page (chips, the scroll bar, find
    /// highlights) is drawn in: the system theme on its own ground, or the
    /// paper's stock theme at the system's scale.
    #[must_use]
    pub fn page_theme(&self, system: &Theme) -> Theme {
        match self.stock {
            None => system.on_ground(self.ground),
            Some(stock) => stock
                .theme()
                .with_text_scale(system.text_scale)
                .with_ui_scale(system.ui_scale)
                .with_reduce_motion(system.reduce_motion)
                .on_ground(self.ground),
        }
    }

    /// Whether the page is dark.
    #[must_use]
    pub fn is_dark(&self) -> bool {
        is_dark(self.ground)
    }
}

fn is_dark(ground: Color) -> bool {
    crate::a11y::relative_luminance(ground) < 0.5
}
