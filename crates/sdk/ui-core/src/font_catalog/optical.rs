//! Optical sizes drawn as separate cuts. A variable face with an `opsz` axis follows the size it is set
//! at by itself ([`OpticalSizing::Auto`]).

use crate::text_render::{OpticalSizing, TextGround};

/// What picks a run's cut besides its family, size, style and weight.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Optical {
    pub(crate) sizing: OpticalSizing,
    pub(crate) ground: TextGround,
}

struct Cut {
    family: &'static str,
    /// The smallest size, in logical pixels, the cut sets on a light ground.
    from_px: f32,
    dark_from_px: f32,
    italic: bool,
    bold: bool,
}

/// A family whose optical sizes are separate cuts, smallest first. The first
/// cut serves every size below the next one's.
struct OpticalFamily {
    family: &'static str,
    cuts: &'static [Cut],
}

/// Where New Computer Modern's Regular cut takes over from Book, on a light ground.
pub(crate) const NCM_REGULAR_FROM_PX: f32 = 24.0;
/// The same on a dark ground, where hairlines thin out sooner.
pub(crate) const NCM_REGULAR_DARK_FROM_PX: f32 = 32.0;
/// Where Book takes over from the 8 pt design: footnotes, superscripts and
/// small captions are set in 8 pt's wider, sturdier shapes below it.
pub(crate) const NCM_BOOK_FROM_PX: f32 = 12.5;

const FAMILIES: [OpticalFamily; 2] = [
    OpticalFamily {
        family: "New Computer Modern",
        cuts: &[
            Cut {
                family: "New Computer Modern 08",
                from_px: 0.0,
                dark_from_px: 0.0,
                italic: false,
                bold: true,
            },
            Cut {
                family: "New Computer Modern Book",
                from_px: NCM_BOOK_FROM_PX,
                dark_from_px: NCM_BOOK_FROM_PX,
                italic: true,
                bold: true,
            },
            Cut {
                family: "New Computer Modern",
                from_px: NCM_REGULAR_FROM_PX,
                dark_from_px: NCM_REGULAR_DARK_FROM_PX,
                italic: true,
                bold: true,
            },
        ],
    },
    OpticalFamily {
        family: "Libertinus Serif",
        cuts: &[
            Cut {
                family: "Libertinus Serif",
                from_px: 0.0,
                dark_from_px: 0.0,
                italic: true,
                bold: true,
            },
            Cut {
                family: "Libertinus Serif Display",
                from_px: NCM_REGULAR_FROM_PX,
                dark_from_px: NCM_REGULAR_DARK_FROM_PX,
                italic: false,
                bold: false,
            },
        ],
    },
];

/// The family a run named `family` is set in at `size` logical pixels: the cut of an optical family
/// that serves that size, ground, style and weight.
pub(crate) fn cut(
    family: &str,
    size: f32,
    optical: Optical,
    italic: bool,
    weight: u16,
) -> Option<&'static str> {
    cut_where(family, size, optical, italic, weight, |_| true)
}

/// [`cut`] among the cuts `installed` says the font system has: a cut a catalog leaves out is served by
/// the next smaller one.
pub(crate) fn cut_where(
    family: &str,
    size: f32,
    optical: Optical,
    italic: bool,
    weight: u16,
    installed: impl Fn(&str) -> bool,
) -> Option<&'static str> {
    let design = match optical.sizing {
        OpticalSizing::Auto => size,
        OpticalSizing::Value(value) => value,
        OpticalSizing::None => return None,
    };
    let entry = family_of(family)?;
    let bold = weight > 500;
    let styled = |cut: &&Cut| (cut.italic || !italic) && (cut.bold || !bold);
    entry
        .cuts
        .iter()
        .rev()
        .filter(styled)
        .find(|cut| {
            let from = match optical.ground {
                TextGround::Light => cut.from_px,
                TextGround::Dark => cut.dark_from_px,
            };
            design >= from && installed(cut.family)
        })
        .or_else(|| {
            entry
                .cuts
                .iter()
                .filter(|cut| cut.italic || !italic)
                .find(|cut| installed(cut.family))
        })
        .or_else(|| entry.cuts.iter().find(|cut| cut.italic || !italic))
        .map(|cut| cut.family)
}

/// The cut text named `family` takes at `size`, and every smaller cut its smaller runs (superscripts,
/// footnotes) can take: what a host that loads faces on demand fetches for a style.
pub(crate) fn cuts_up_to(
    family: &str,
    size: f32,
    optical: Optical,
    italic: bool,
    weight: u16,
) -> Vec<&'static str> {
    let Some(top) = cut(family, size, optical, italic, weight) else {
        return Vec::new();
    };
    let Some(entry) = family_of(family) else {
        return vec![top];
    };
    let bold = weight > 500;
    let mut out: Vec<&'static str> = entry
        .cuts
        .iter()
        .take_while(|cut| cut.family != top)
        .filter(|cut| (cut.italic || !italic) && (cut.bold || !bold))
        .map(|cut| cut.family)
        .collect();
    out.push(top);
    out
}

/// The optical family `family` is (or is a cut of), by its name: what a font list shows for any of its
/// cuts.
pub(crate) fn family_name(family: &str) -> Option<&'static str> {
    family_of(family).map(|entry| entry.family)
}

fn family_of(family: &str) -> Option<&'static OpticalFamily> {
    FAMILIES.iter().find(|entry| {
        entry.family.eq_ignore_ascii_case(family)
            || entry
                .cuts
                .iter()
                .any(|cut| cut.family.eq_ignore_ascii_case(family))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIGHT: Optical = Optical {
        sizing: OpticalSizing::Auto,
        ground: TextGround::Light,
    };
    const DARK: Optical = Optical {
        sizing: OpticalSizing::Auto,
        ground: TextGround::Dark,
    };

    fn ncm(size: f32, optical: Optical) -> Option<&'static str> {
        cut("New Computer Modern", size, optical, false, 400)
    }

    #[test]
    fn new_computer_modern_takes_the_cut_its_size_calls_for() {
        // Footnotes and superscripts: the 8 pt design.
        assert_eq!(ncm(10.0, LIGHT), Some("New Computer Modern 08"));
        assert_eq!(ncm(12.0, LIGHT), Some("New Computer Modern 08"));
        for size in [12.5, 14.0, 16.0, 19.0, 23.9] {
            assert_eq!(ncm(size, LIGHT), Some("New Computer Modern Book"), "{size}");
        }
        for size in [24.0, 34.0, 48.0] {
            assert_eq!(ncm(size, LIGHT), Some("New Computer Modern"), "{size}");
        }
    }

    #[test]
    fn a_dark_ground_keeps_book_to_larger_sizes() {
        assert_eq!(ncm(26.0, LIGHT), Some("New Computer Modern"));
        assert_eq!(ncm(26.0, DARK), Some("New Computer Modern Book"));
        assert_eq!(ncm(31.9, DARK), Some("New Computer Modern Book"));
        assert_eq!(ncm(32.0, DARK), Some("New Computer Modern"));
        // Small text is the 8 pt design on either ground.
        assert_eq!(ncm(11.0, DARK), Some("New Computer Modern 08"));
    }

    #[test]
    fn a_cut_without_the_style_passes_it_to_the_next_smaller_cut() {
        // The 8 pt design ships upright only: small italics are Book's.
        assert_eq!(
            cut("New Computer Modern", 11.0, LIGHT, true, 400),
            Some("New Computer Modern Book")
        );
        // Small bold is the shared Bold, which the 8 pt family has.
        assert_eq!(
            cut("New Computer Modern", 11.0, LIGHT, false, 700),
            Some("New Computer Modern 08")
        );
        assert_eq!(
            cut("Libertinus Serif", 40.0, LIGHT, false, 400),
            Some("Libertinus Serif Display")
        );
        assert_eq!(
            cut("Libertinus Serif", 40.0, LIGHT, true, 400),
            Some("Libertinus Serif")
        );
        assert_eq!(
            cut("Libertinus Serif", 40.0, LIGHT, false, 700),
            Some("Libertinus Serif")
        );
    }

    #[test]
    fn a_cut_name_resolves_like_its_family_and_sizing_can_pin_it() {
        assert_eq!(
            cut("New Computer Modern Book", 40.0, LIGHT, false, 400),
            Some("New Computer Modern")
        );
        assert_eq!(
            cut("new computer modern 08", 16.0, LIGHT, false, 400),
            Some("New Computer Modern Book")
        );
        let pinned = Optical {
            sizing: OpticalSizing::Value(10.0),
            ..LIGHT
        };
        assert_eq!(ncm(48.0, pinned), Some("New Computer Modern 08"));
        let none = Optical {
            sizing: OpticalSizing::None,
            ..LIGHT
        };
        assert_eq!(ncm(48.0, none), None);
        assert_eq!(cut("Literata", 48.0, LIGHT, false, 400), None);
        assert_eq!(
            family_name("New Computer Modern 08"),
            Some("New Computer Modern")
        );
        assert_eq!(
            family_name("Libertinus Serif Display"),
            Some("Libertinus Serif")
        );
        assert_eq!(family_name("Literata"), None);
    }

    #[test]
    fn a_cut_the_catalog_leaves_out_is_served_by_the_next_smaller_one() {
        let standalone = |family: &str| family != "New Computer Modern";
        for size in [24.0, 34.0, 48.0] {
            assert_eq!(
                cut_where("New Computer Modern", size, LIGHT, false, 400, standalone),
                Some("New Computer Modern Book"),
                "{size}"
            );
        }
        assert_eq!(
            cut_where("New Computer Modern", 10.0, LIGHT, false, 400, standalone),
            Some("New Computer Modern 08")
        );
        assert_eq!(
            cut_where("New Computer Modern", 48.0, LIGHT, true, 400, |_| true),
            Some("New Computer Modern")
        );
    }

    #[test]
    fn a_style_needs_its_own_cut_and_the_smaller_ones_its_small_runs_take() {
        assert_eq!(
            cuts_up_to("New Computer Modern", 19.0, LIGHT, false, 400),
            ["New Computer Modern 08", "New Computer Modern Book"]
        );
        assert_eq!(
            cuts_up_to("New Computer Modern", 19.0, LIGHT, true, 400),
            ["New Computer Modern Book"]
        );
        assert_eq!(
            cuts_up_to("New Computer Modern", 48.0, LIGHT, false, 400),
            [
                "New Computer Modern 08",
                "New Computer Modern Book",
                "New Computer Modern"
            ]
        );
        assert!(cuts_up_to("Literata", 19.0, LIGHT, false, 400).is_empty());
    }
}
