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

    tilo_test_env::scenario! {
        /// Which optical cut a size is set in, and what serves it when the catalog leaves that cut
        /// out.
        /// Each phase was a test of its own and builds its own fixture.
        #[allow(clippy::too_many_lines, reason = "one row per case")]
        fn an_optical_cut_follows_its_size_and_falls_back_to_what_the_catalog_has;

        phase a_cut_follows_its_size_ground_style_weight_and_sizing;
        // `cut` for each family, size, ground, style, weight and sizing; then the family each cut
        // belongs to.
        {
            const NCM: &str = "New Computer Modern";
            const BOOK: &str = "New Computer Modern Book";
            const EIGHT: &str = "New Computer Modern 08";
            const LIBERTINUS: &str = "Libertinus Serif";
            const DISPLAY: &str = "Libertinus Serif Display";
            let pinned = Optical {
                sizing: OpticalSizing::Value(10.0),
                ..LIGHT
            };
            let none = Optical {
                sizing: OpticalSizing::None,
                ..LIGHT
            };
            // (case, family, size, optical, italic, weight, the cut)
            let cases = [
                // Footnotes and superscripts: the 8 pt design.
                ("footnote 10", NCM, 10.0, LIGHT, false, 400, Some(EIGHT)),
                ("footnote 12", NCM, 12.0, LIGHT, false, 400, Some(EIGHT)),
                (
                    "just under Book",
                    NCM,
                    12.49,
                    LIGHT,
                    false,
                    400,
                    Some(EIGHT),
                ),
                ("size zero", NCM, 0.0, LIGHT, false, 400, Some(EIGHT)),
                ("a negative size", NCM, -1.0, LIGHT, false, 400, Some(EIGHT)),
                (
                    "a size that is no number",
                    NCM,
                    f32::NAN,
                    LIGHT,
                    false,
                    400,
                    Some(EIGHT),
                ),
                ("body 12.5", NCM, 12.5, LIGHT, false, 400, Some(BOOK)),
                ("body 14", NCM, 14.0, LIGHT, false, 400, Some(BOOK)),
                ("body 16", NCM, 16.0, LIGHT, false, 400, Some(BOOK)),
                ("body 19", NCM, 19.0, LIGHT, false, 400, Some(BOOK)),
                ("body 23.9", NCM, 23.9, LIGHT, false, 400, Some(BOOK)),
                ("title 24", NCM, 24.0, LIGHT, false, 400, Some(NCM)),
                ("title 34", NCM, 34.0, LIGHT, false, 400, Some(NCM)),
                ("title 48", NCM, 48.0, LIGHT, false, 400, Some(NCM)),
                // A dark ground keeps Book to larger sizes.
                ("26 on light", NCM, 26.0, LIGHT, false, 400, Some(NCM)),
                ("26 on dark", NCM, 26.0, DARK, false, 400, Some(BOOK)),
                ("31.9 on dark", NCM, 31.9, DARK, false, 400, Some(BOOK)),
                ("32 on dark", NCM, 32.0, DARK, false, 400, Some(NCM)),
                // Small text is the 8 pt design on either ground.
                ("11 on dark", NCM, 11.0, DARK, false, 400, Some(EIGHT)),
                // The 8 pt design ships upright only: small italics are Book's.
                ("small italic", NCM, 11.0, LIGHT, true, 400, Some(BOOK)),
                (
                    "a negative size in italic",
                    NCM,
                    -1.0,
                    LIGHT,
                    true,
                    400,
                    Some(BOOK),
                ),
                // Small bold is the shared Bold, which the 8 pt family has.
                ("small bold", NCM, 11.0, LIGHT, false, 700, Some(EIGHT)),
                (
                    "a bold italic title",
                    NCM,
                    48.0,
                    LIGHT,
                    true,
                    700,
                    Some(NCM),
                ),
                (
                    "Libertinus display",
                    LIBERTINUS,
                    40.0,
                    LIGHT,
                    false,
                    400,
                    Some(DISPLAY),
                ),
                (
                    "Libertinus display italic",
                    LIBERTINUS,
                    40.0,
                    LIGHT,
                    true,
                    400,
                    Some(LIBERTINUS),
                ),
                (
                    "Libertinus display bold",
                    LIBERTINUS,
                    40.0,
                    LIGHT,
                    false,
                    700,
                    Some(LIBERTINUS),
                ),
                (
                    "weight 500 is not bold",
                    LIBERTINUS,
                    40.0,
                    LIGHT,
                    false,
                    500,
                    Some(DISPLAY),
                ),
                (
                    "weight 501 is bold",
                    LIBERTINUS,
                    40.0,
                    LIGHT,
                    false,
                    501,
                    Some(LIBERTINUS),
                ),
                (
                    "Libertinus 23.9",
                    LIBERTINUS,
                    23.9,
                    LIGHT,
                    false,
                    400,
                    Some(LIBERTINUS),
                ),
                (
                    "Libertinus 24",
                    LIBERTINUS,
                    24.0,
                    LIGHT,
                    false,
                    400,
                    Some(DISPLAY),
                ),
                (
                    "Libertinus 31.9 on dark",
                    LIBERTINUS,
                    31.9,
                    DARK,
                    false,
                    400,
                    Some(LIBERTINUS),
                ),
                (
                    "Libertinus 32 on dark",
                    LIBERTINUS,
                    32.0,
                    DARK,
                    false,
                    400,
                    Some(DISPLAY),
                ),
                // A cut's name resolves like its family.
                (
                    "named by its Book cut",
                    BOOK,
                    40.0,
                    LIGHT,
                    false,
                    400,
                    Some(NCM),
                ),
                (
                    "named by its 8 pt cut in lower case",
                    "new computer modern 08",
                    16.0,
                    LIGHT,
                    false,
                    400,
                    Some(BOOK),
                ),
                (
                    "sizing pinned to 10",
                    NCM,
                    48.0,
                    pinned,
                    false,
                    400,
                    Some(EIGHT),
                ),
                ("sizing none", NCM, 48.0, none, false, 400, None),
                (
                    "sizing none in italic bold",
                    NCM,
                    48.0,
                    none,
                    true,
                    700,
                    None,
                ),
                (
                    "a family with no cuts",
                    "Literata",
                    48.0,
                    LIGHT,
                    false,
                    400,
                    None,
                ),
                ("no family", "", 48.0, LIGHT, false, 400, None),
            ];
            for (case, family, size, optical, italic, weight, want) in cases {
                assert_eq!(
                    cut(family, size, optical, italic, weight),
                    want,
                    "case: {case}"
                );
            }
            for (name, want) in [
                (EIGHT, Some(NCM)),
                (DISPLAY, Some(LIBERTINUS)),
                ("LIBERTINUS SERIF DISPLAY", Some(LIBERTINUS)),
                (NCM, Some(NCM)),
                ("Literata", None),
                ("", None),
            ] {
                assert_eq!(family_name(name), want, "family_name({name:?})");
            }
        }

        phase a_cut_the_catalog_leaves_out_is_served_by_the_next_smaller_one;
        {
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
            // Too small for every cut the catalog has: the smallest one it does have.
            assert_eq!(
                cut_where("New Computer Modern", 10.0, LIGHT, false, 400, |family| {
                    family == "New Computer Modern"
                }),
                Some("New Computer Modern"),
                "only the Regular cut installed, at a footnote's size"
            );
            // A small italic whose Book is left out is the Regular's, never the upright-only 8 pt.
            assert_eq!(
                cut_where("New Computer Modern", 11.0, LIGHT, true, 400, |family| {
                    family != "New Computer Modern Book"
                }),
                Some("New Computer Modern"),
                "a small italic without Book"
            );
            // A display size whose Display cut is left out stays in the text cut.
            assert_eq!(
                cut_where("Libertinus Serif", 40.0, LIGHT, false, 400, |family| {
                    family != "Libertinus Serif Display"
                }),
                Some("Libertinus Serif"),
                "Libertinus without its Display cut"
            );
            // With no cut installed the family still names one: its smallest in the style.
            assert_eq!(
                cut_where("New Computer Modern", 48.0, LIGHT, false, 400, |_| false),
                Some("New Computer Modern 08"),
                "nothing installed, upright"
            );
            assert_eq!(
                cut_where("New Computer Modern", 48.0, LIGHT, true, 400, |_| false),
                Some("New Computer Modern Book"),
                "nothing installed, italic"
            );
            // Sizing turned off, and a family with no cuts, stay no cut whatever is installed.
            let none = Optical {
                sizing: OpticalSizing::None,
                ..LIGHT
            };
            assert_eq!(
                cut_where("New Computer Modern", 48.0, none, false, 400, |_| false),
                None,
                "sizing none, nothing installed"
            );
            assert_eq!(
                cut_where("Literata", 48.0, LIGHT, false, 400, |_| false),
                None,
                "a family with no cuts, nothing installed"
            );
        }

        phase a_style_needs_its_own_cut_and_the_smaller_ones_its_small_runs_take;
        {
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
            // Boundaries: the smallest cut alone, an italic title (the 8 pt design has no italic), a
            // family whose larger cut lacks the weight, a cut's own name, and sizing turned off.
            assert_eq!(
                cuts_up_to("New Computer Modern", 10.0, LIGHT, false, 400),
                ["New Computer Modern 08"]
            );
            assert_eq!(
                cuts_up_to("New Computer Modern", 48.0, LIGHT, true, 400),
                ["New Computer Modern Book", "New Computer Modern"]
            );
            assert_eq!(
                cuts_up_to("Libertinus Serif", 40.0, LIGHT, false, 400),
                ["Libertinus Serif", "Libertinus Serif Display"]
            );
            assert_eq!(
                cuts_up_to("Libertinus Serif", 40.0, LIGHT, false, 700),
                ["Libertinus Serif"]
            );
            assert_eq!(
                cuts_up_to("New Computer Modern Book", 19.0, LIGHT, false, 400),
                ["New Computer Modern 08", "New Computer Modern Book"]
            );
            let none = Optical {
                sizing: OpticalSizing::None,
                ..LIGHT
            };
            assert!(cuts_up_to("New Computer Modern", 48.0, none, false, 400).is_empty());
        }
    }
}
