//! The class of face a text is set in: what a device that lacks the face
//! itself keeps of it.
//!
//! A page names faces by a CSS family list. A device that has none of them
//! sets the text in its own face of the same class, so a serif heading stays
//! a serif heading. The class is the list's generic family where it has one,
//! and otherwise what its first face that is known by name is. The engine
//! that laid the page out can say more: which generic family it set the list
//! like, on the machine it runs on, which is the only way to learn the class
//! of a face the page brought itself. [`told`] rewrites a list by that.

/// The kind of face a text is set in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontClass {
    Serif,
    Sans,
    Mono,
}

impl FontClass {
    /// The class as CSS names it: its generic family.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Serif => "serif",
            Self::Sans => "sans-serif",
            Self::Mono => "monospace",
        }
    }

    /// The class a generic family of CSS names: `None` for anything else,
    /// and for the generic families that name no class of these three.
    #[must_use]
    pub fn named(generic: &str) -> Option<Self> {
        match generic {
            "serif" => Some(Self::Serif),
            "sans-serif" => Some(Self::Sans),
            "monospace" => Some(Self::Mono),
            _ => None,
        }
    }
}

/// Faces drawn with the same measures, so that one stands in for another
/// without moving a line break: each group is what one of the web's three
/// common faces is called by the foundries that drew it again. Serif, sans,
/// fixed-width, in that order; every name in lower case.
pub const ALIKE: [&[&str]; 3] = [
    &[
        "times new roman",
        "times",
        "liberation serif",
        "tinos",
        "nimbus roman",
        "nimbus roman no9 l",
    ],
    &[
        "arial",
        "helvetica",
        "liberation sans",
        "arimo",
        "nimbus sans",
        "nimbus sans l",
    ],
    &[
        "courier new",
        "courier",
        "liberation mono",
        "cousine",
        "nimbus mono ps",
        "nimbus mono l",
    ],
];

/// Serif faces pages name, beside those of [`ALIKE`].
const SERIF: &[&str] = &[
    "georgia",
    "garamond",
    "eb garamond",
    "palatino",
    "palatino linotype",
    "book antiqua",
    "cambria",
    "constantia",
    "baskerville",
    "didot",
    "bodoni",
    "charter",
    "century",
    "century schoolbook",
    "bookman",
    "cochin",
    "hoefler text",
    "iowan old style",
    "new york",
    "dejavu serif",
    "noto serif",
    "source serif 4",
    "source serif pro",
    "libertinus serif",
    "linux libertine",
    "literata",
    "merriweather",
    "lora",
    "playfair display",
    "pt serif",
    "roboto serif",
    "roboto slab",
    "new computer modern",
    "computer modern",
    "cmu serif",
];

/// Fixed-width faces pages name, beside those of [`ALIKE`].
const MONO: &[&str] = &[
    "monaco",
    "menlo",
    "consolas",
    "lucida console",
    "andale mono",
    "dejavu sans mono",
    "noto sans mono",
    "source code pro",
    "fira code",
    "fira mono",
    "jetbrains mono",
    "sf mono",
    "ubuntu mono",
    "roboto mono",
    "ibm plex mono",
    "cascadia code",
    "cascadia mono",
    "inconsolata",
];

/// The families a CSS list names, in its order, without their quotes.
pub fn family_names(css: &str) -> impl Iterator<Item = &str> {
    css.split(',')
        .map(|family| family.trim().trim_matches(['"', '\'']).trim())
        .filter(|family| !family.is_empty())
}

/// The class a generic family of CSS stands for; `lower` is the family in
/// lower case. An engine goes no further down a list than its first generic
/// family.
#[must_use]
pub fn generic_class(lower: &str) -> Option<FontClass> {
    match lower {
        "monospace" | "ui-monospace" => Some(FontClass::Mono),
        "serif" | "ui-serif" => Some(FontClass::Serif),
        "sans-serif" | "system-ui" | "-apple-system" | "blinkmacsystemfont" | "ui-sans-serif"
        | "ui-rounded" | "cursive" | "fantasy" => Some(FontClass::Sans),
        _ => None,
    }
}

/// The class of a face by its name; `lower` is the family in lower case. A
/// name that says nothing of its class has none.
#[must_use]
pub fn named_class(lower: &str) -> Option<FontClass> {
    let [serif, sans, mono] = ALIKE;
    if mono.contains(&lower) || MONO.contains(&lower) {
        return Some(FontClass::Mono);
    }
    if serif.contains(&lower) || SERIF.contains(&lower) {
        return Some(FontClass::Serif);
    }
    if sans.contains(&lower) {
        return Some(FontClass::Sans);
    }
    if lower.contains("mono") || lower.contains("courier") {
        Some(FontClass::Mono)
    } else if lower.contains("sans") || lower.contains("grotesk") || lower.contains("gothic") {
        Some(FontClass::Sans)
    } else if lower.contains("serif") || lower.contains("slab") {
        Some(FontClass::Serif)
    } else {
        None
    }
}

/// The class of face a page's family list asks for, which is what a device
/// that has none of its faces sets the text in: the class of the first
/// family that says one, by being a generic family or a face known by name.
/// A list that says none is the engine's own default, or a face only the
/// page has, and an engine sets what it has no face for in its standard one:
/// a serif.
#[must_use]
pub fn font_class(css: &str) -> FontClass {
    family_names(css)
        .find_map(|name| {
            let lower = name.to_ascii_lowercase();
            generic_class(&lower).or_else(|| named_class(&lower))
        })
        .unwrap_or(FontClass::Serif)
}

/// The family list `css` as a device should read it once the engine said it
/// sets the list like `class`. Where that is the class the list asks for
/// anyway, the list stands. Where it is not, the engine set the text in no
/// face the list names by a known name (it had none of them, or the page
/// brought a face of its own): the faces whose class no name tells stay, for
/// a device that has one of them, and the class the engine set follows as
/// the list's generic family.
#[must_use]
pub fn told(css: &str, class: FontClass) -> String {
    if font_class(css) == class {
        return css.to_owned();
    }
    let mut list: Vec<String> = family_names(css)
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            generic_class(&lower).is_none() && named_class(&lower).is_none()
        })
        .map(|name| {
            if name.contains(' ') {
                format!("\"{name}\"")
            } else {
                name.to_owned()
            }
        })
        .collect();
    list.push(class.name().to_owned());
    list.join(", ")
}

#[cfg(test)]
mod tests {
    use super::{FontClass, font_class, told};

    #[test]
    fn a_list_asks_for_the_class_of_its_first_family_that_says_one() {
        for (css, class) in [
            ("\"Times New Roman\"", FontClass::Serif),
            ("Arial", FontClass::Sans),
            ("monospace", FontClass::Mono),
            ("Harbour, Georgia, serif", FontClass::Serif),
            ("\"Helvetica Neue\", Arial, sans-serif", FontClass::Sans),
            ("Inter, system-ui", FontClass::Sans),
            ("\"Courier New\", Courier", FontClass::Mono),
            ("\"Fira Sans\"", FontClass::Sans),
            ("'Roboto Slab'", FontClass::Serif),
            // A face only its page has says nothing of its class: the engine
            // sets what it has no face for in its standard one.
            ("Harbour", FontClass::Serif),
        ] {
            assert_eq!(font_class(css), class, "{css}");
        }
    }

    #[test]
    fn what_the_engine_set_a_list_like_overrides_what_the_list_asks_for() {
        // The engine set it as the list asks: the list stands.
        assert_eq!(
            told("\"Helvetica Neue\", Arial, sans-serif", FontClass::Sans),
            "\"Helvetica Neue\", Arial, sans-serif"
        );
        // A machine without a Times sets it in its sans.
        assert_eq!(told("\"Times New Roman\"", FontClass::Sans), "sans-serif");
        // A face the page brought, drawn fixed-width, before a serif it did
        // not need: a device that has a face of that name sets it, any other
        // a fixed-width one.
        let own = told("Harbour, Georgia, serif", FontClass::Mono);
        assert_eq!(own, "Harbour, monospace");
        assert_eq!(font_class(&own), FontClass::Mono);
        assert_eq!(
            told("\"Harbour Text\", serif", FontClass::Sans),
            "\"Harbour Text\", sans-serif"
        );
    }
}
