//! The papers of the article reader.
//!
//! The article page is a document the reader draws, so it keeps its own
//! paper: by default the window ground of the system theme, or a fixed light,
//! sepia or dark page for long-form reading. Light and dark paper are the
//! stock light and dark themes' content ground and ink; sepia is a warm paper
//! with its own ink, measured against it. Chrome never reads these: only the
//! page and what is printed on it. High contrast reaches into the page, so a
//! sepia paper never overrides a high-contrast system theme.

use vello::peniko::Color;

use crate::model::ReadTheme;
use ui::theme::Theme;

/// Resolved colours of one paper.
#[derive(Clone, Copy, Debug)]
pub struct ReaderPalette {
    /// The page fill, or `None` when the page is the window ground the
    /// framework already painted.
    pub page: Option<Color>,
    /// The ground the page's ink is measured against.
    pub ground: Color,
    /// Body ink.
    pub ink: Color,
    /// Secondary ink (byline, quotes).
    pub ink_dim: Color,
    /// Rules: block-quote bar, the line under the header, dividers.
    pub rule: Color,
    /// The well behind a code block.
    pub well: Color,
    /// The stock theme a fixed paper is printed in; `None` for the
    /// system's own.
    stock: Option<Stock>,
}

/// A stock theme a fixed paper borrows its ink from.
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
    pub fn resolve(mode: ReadTheme, theme: &Theme) -> Self {
        if theme.document.palette.high_contrast || theme.forced_colors {
            return Self::of_theme(theme, None, None);
        }
        match mode {
            ReadTheme::System => Self::of_theme(theme, None, None),
            ReadTheme::Light => Self::fixed(theme, Stock::Light),
            ReadTheme::Dark => Self::fixed(theme, Stock::Dark),
            // Warm paper for long-form reading, printed in the light ink.
            ReadTheme::Sepia => Self::of_paper(Stock::Light, Color::from_rgb8(0xF4, 0xEC, 0xDC)),
        }
    }

    /// The stock paper, unless the system theme already is that paper (a
    /// dark preset reading dark paper keeps its own temperature).
    fn fixed(system: &Theme, stock: Stock) -> Self {
        let paper = stock.theme();
        let page = paper.tokens.colors.surfaces.content;
        if is_dark(system.tokens.colors.surfaces.content) == is_dark(page) {
            Self::of_theme(system, None, None)
        } else {
            Self::of_paper(stock, page)
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
            rule: ui::surface::edge_on(ground),
            // A whisper of the page's own ink, so the well keeps the paper's
            // tone on light, sepia and dark paper alike.
            well: on.tint,
            stock,
        }
    }

    /// The theme what is printed on the page (tag chips, the scroll bar) is
    /// drawn in: the system theme on its own ground, or the paper's stock
    /// theme at the system's scale.
    pub fn page_theme(&self, system: &Theme) -> Theme {
        match self.stock {
            None => system.on_ground(self.ground),
            Some(stock) => stock
                .theme()
                .with_text_scale(system.text_scale)
                .with_ui_scale(system.ui_scale)
                .on_ground(self.ground),
        }
    }

    /// Whether the page is dark.
    pub fn is_dark(&self) -> bool {
        is_dark(self.ground)
    }
}

fn is_dark(ground: Color) -> bool {
    ui::a11y::relative_luminance(ground) < 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReadTheme;

    /// The default paper is the system ground; fixed papers draw their own
    /// page with readable ink; high contrast overrides any paper.
    #[test]
    fn papers_follow_the_system_and_keep_their_ink_readable() {
        // The default paper paints nothing: it is the window ground.
        for theme in [Theme::dark(), Theme::light()] {
            let pal = ReaderPalette::resolve(ReadTheme::System, &theme);
            assert!(pal.page.is_none());
            assert_eq!(pal.ground, theme.tokens.colors.surfaces.content);
        }

        // A fixed paper of the other polarity paints its page.
        let light_on_dark = ReaderPalette::resolve(ReadTheme::Light, &Theme::dark());
        assert_eq!(
            light_on_dark.page,
            Some(Theme::light().tokens.colors.surfaces.content)
        );
        assert!(!light_on_dark.is_dark());
        let dark_on_light = ReaderPalette::resolve(ReadTheme::Dark, &Theme::light());
        assert!(dark_on_light.is_dark());
        // The same polarity keeps the system's own ground.
        assert!(
            ReaderPalette::resolve(ReadTheme::Dark, &Theme::dark())
                .page
                .is_none()
        );

        // Every paper's ink clears AA on its page.
        for mode in ReadTheme::ALL {
            for theme in [Theme::dark(), Theme::light()] {
                let pal = ReaderPalette::resolve(mode, &theme);
                assert!(
                    ui::a11y::contrast_ratio(pal.ink, pal.ground) >= 4.5,
                    "{mode:?} ink on {}",
                    theme.name
                );
                assert!(
                    ui::a11y::contrast_ratio(pal.ink_dim, pal.ground) >= 4.5,
                    "{mode:?} secondary ink on {}",
                    theme.name
                );
            }
        }

        // High contrast reaches the page.
        let hc = Theme::dark().with_high_contrast();
        let sepia = ReaderPalette::resolve(ReadTheme::Sepia, &hc);
        assert!(sepia.page.is_none());
        assert_eq!(sepia.ground, hc.tokens.colors.surfaces.content);
    }
}
