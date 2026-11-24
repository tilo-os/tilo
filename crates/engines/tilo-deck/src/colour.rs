//! The colours a deck is written with.
//!
//! A deck's colours are its content: an exported file keeps them, whatever theme its reader uses.
//! The app draws the deck from the workspace's theme, so what lives here is the one set of
//! concrete colours the interchange writes for a palette and look, and the readable ink each
//! slide background gets.

use crate::model::{Background, DeckLook, PaletteKind};

/// A colour without transparency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// An authored dark tone in every look: a theme-tracking neutral would flip near-white in the
/// light palette.
const DEEP: Rgb = Rgb(15, 23, 42);

/// The surfaces and inks of each look, as a written file carries them.
struct Look {
    surface: Rgb,
    surface_content: Rgb,
    text: Rgb,
    text_dim: Rgb,
}

impl Look {
    fn of(look: DeckLook) -> Self {
        match look {
            DeckLook::Light => Self {
                surface: Rgb(255, 255, 255),
                surface_content: Rgb(245, 245, 244),
                text: Rgb(28, 25, 23),
                text_dim: Rgb(87, 83, 78),
            },
            DeckLook::Dark => Self {
                surface: Rgb(28, 25, 23),
                surface_content: Rgb(41, 37, 36),
                text: Rgb(250, 250, 249),
                text_dim: Rgb(168, 162, 158),
            },
        }
    }
}

/// The deck's surfaces and inks, resolved for its palette and look.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub accent: Rgb,
    pub accent_soft: Rgb,
    pub accent_deep: Rgb,
    pub surface: Rgb,
    pub surface_dim: Rgb,
    pub deep: Rgb,
    pub text: Rgb,
    pub text_dim: Rgb,
    pub on_accent: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub danger: Rgb,
    pub info: Rgb,
    pub failed: Rgb,
    pub look: DeckLook,
    /// The readable inks for [`Background`] in their order.
    inks: [(Rgb, Rgb); 3],
}

impl Palette {
    #[must_use]
    pub fn resolve(kind: PaletteKind, look: DeckLook) -> Self {
        let c = Look::of(look);
        let accent = match kind {
            PaletteKind::Indigo => Rgb(79, 70, 229),
            PaletteKind::Emerald => Rgb(16, 185, 129),
            PaletteKind::Amber => Rgb(245, 158, 11),
            PaletteKind::Rose => Rgb(236, 72, 153),
            PaletteKind::Sky => Rgb(14, 165, 233),
        };
        let mut palette = Self {
            accent,
            accent_soft: mix(accent, c.surface, 0.78),
            accent_deep: mix(accent, DEEP, 0.62),
            surface: c.surface,
            surface_dim: mix(c.surface, c.surface_content, 0.5),
            deep: DEEP,
            text: c.text,
            text_dim: c.text_dim,
            on_accent: readable_on(accent, Rgb(255, 255, 255), c.text),
            success: Rgb(34, 197, 94),
            warning: Rgb(245, 158, 11),
            danger: Rgb(239, 68, 68),
            info: Rgb(139, 92, 246),
            failed: Rgb(239, 68, 68),
            look,
            inks: [(c.text, c.text_dim); 3],
        };
        palette.inks = crate::model::Background::order().map(|background| {
            let ground = palette.slide_bg(background);
            let text = readable_on(ground, c.text, c.text_dim);
            let dim = if text == c.text {
                c.text_dim
            } else {
                mix(text, ground, 0.3)
            };
            (text, readable_on(ground, dim, text))
        });
        palette
    }

    /// The same palette with the inks that read on `bg`.
    #[must_use]
    pub fn on(&self, bg: Background) -> Self {
        let index = Background::order()
            .iter()
            .position(|candidate| *candidate == bg)
            .unwrap_or(0);
        let (text, text_dim) = self.inks.get(index).copied().unwrap_or((self.text, self.text_dim));
        Self {
            text,
            text_dim,
            ..*self
        }
    }

    /// `None` for `"none"`; a named role or a `#RRGGBB` colour otherwise.
    #[must_use]
    pub fn resolve_role(&self, role: &str) -> Option<Rgb> {
        if role == "none" {
            return None;
        }
        if role.starts_with('#') {
            return parse_hex(role).or(Some(self.accent));
        }
        Some(match role {
            "accent-soft" => self.accent_soft,
            "accent-deep" => self.accent_deep,
            "surface" => self.surface,
            "surface-dim" => self.surface_dim,
            "deep" => self.deep,
            "text" => self.text,
            "text-dim" => self.text_dim,
            "on-accent" => self.on_accent,
            "white" => Rgb(255, 255, 255),
            "success" => self.success,
            "warning" => self.warning,
            "danger" => self.danger,
            "info" => self.info,
            _ => self.accent,
        })
    }

    #[must_use]
    pub fn slide_bg(&self, bg: Background) -> Rgb {
        match bg {
            Background::Surface => self.surface,
            Background::Accent => self.accent,
            Background::Deep => self.deep,
        }
    }

    /// The ink text in role `role` takes on a slide of `bg`, with `ground` the colour the text
    /// stands on (a shape's fill where one covers it).
    #[must_use]
    pub fn text_ink(&self, role: &str, ground: Rgb) -> Rgb {
        match role {
            "text" => readable_on(ground, self.text, self.text_dim),
            "text-dim" => {
                let text = readable_on(ground, self.text, self.text_dim);
                let soft = mix(text, ground, 0.3);
                if contrast(soft, ground) >= 4.5 { soft } else { text }
            }
            other => self.resolve_role(other).unwrap_or(self.text),
        }
    }
}

impl Background {
    /// Every background, in the order [`Palette`] holds its inks.
    #[must_use]
    pub fn order() -> [Self; 3] {
        [Self::Surface, Self::Accent, Self::Deep]
    }
}

/// `colour` as `#RRGGBB`.
#[must_use]
pub fn to_hex(colour: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", colour.0, colour.1, colour.2)
}

/// The colour `#RRGGBB` names, or `#RRGGBBAA` ignoring its alpha.
#[must_use]
pub fn parse_hex(text: &str) -> Option<Rgb> {
    let hex = text.strip_prefix('#')?;
    let byte = |at: usize| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok();
    if hex.len() != 6 && hex.len() != 8 {
        return None;
    }
    Some(Rgb(byte(0)?, byte(2)?, byte(4)?))
}

/// `share` of the way from `from` to `to`.
#[must_use]
pub fn mix(from: Rgb, to: Rgb, share: f32) -> Rgb {
    let at = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * share).round() as u8;
    Rgb(at(from.0, to.0), at(from.1, to.1), at(from.2, to.2))
}

/// The WCAG contrast ratio of two colours, from 1 to 21.
#[must_use]
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let la = luminance(a);
    let lb = luminance(b);
    let (light, dark) = if la > lb { (la, lb) } else { (lb, la) };
    (light + 0.05) / (dark + 0.05)
}

fn luminance(c: Rgb) -> f32 {
    let channel = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(c.0) + 0.7152 * channel(c.1) + 0.0722 * channel(c.2)
}

/// `preferred` where it clears AA on `ground`, else whichever of `fallback` and `other` reads
/// better there.
#[must_use]
pub fn readable_on(ground: Rgb, preferred: Rgb, other: Rgb) -> Rgb {
    if contrast(preferred, ground) >= 4.5 {
        return preferred;
    }
    // A slide stands on its own ground whatever the look: white or near-black is the ink a
    // theme picks when neither of its own does.
    [other, Rgb(255, 255, 255), Rgb(0, 0, 0)]
        .into_iter()
        .max_by(|a, b| contrast(*a, ground).total_cmp(&contrast(*b, ground)))
        .unwrap_or(preferred)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deck_inks_read_on_every_background_in_both_looks() {
        for look in [DeckLook::Light, DeckLook::Dark] {
            for kind in [
                PaletteKind::Indigo,
                PaletteKind::Emerald,
                PaletteKind::Amber,
                PaletteKind::Rose,
                PaletteKind::Sky,
            ] {
                let palette = Palette::resolve(kind, look);
                for background in Background::order() {
                    let on = palette.on(background);
                    let ground = on.slide_bg(background);
                    // The theme's own ink, resolved for this slide's ground, is what a box set
                    // to `text` draws with: it clears AA, and the dim ink never reads worse.
                    assert!(
                        contrast(on.text, ground) >= 4.5,
                        "{:?} on {background:?} in {look:?} ({kind:?})",
                        on.text
                    );
                    assert!(contrast(on.text_dim, ground) >= 4.5);
                    assert_eq!(on.text_ink("text", ground), on.text);
                }
            }
        }
        assert_eq!(to_hex(Rgb(255, 0, 0)), "#FF0000");
        assert_eq!(parse_hex("#FF0000FF"), Some(Rgb(255, 0, 0)));
        assert_eq!(parse_hex("#F00"), None);
    }
}
