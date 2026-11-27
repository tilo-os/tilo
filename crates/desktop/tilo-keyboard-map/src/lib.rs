//! Read-only native keyboard metadata: it neither captures input nor changes the
//! active layout, and never accepts an include path or environment.
//!
//! One copy of the keyboard layouts, read by the session's input settings, the installer's picker
//! and the image writer: what this device installs, the short built-in list a host without
//! xkeyboard-config offers, and the drawing of a layout's keys.

mod catalog;
mod drawing;
#[cfg(feature = "touch")]
pub mod touch;

pub use catalog::Layout;
pub use drawing::{Drawing, DrawnKey, Point, draw};

/// The layout a picker shows unless the person chooses another.
pub const DEFAULT_DISPLAY_NAME: &str = "English (US)";

/// The keyboard layouts this device installs and, when its list cannot be read, why: the layouts
/// are then [`fallback`]. Each is named by the id it is typed by (`de`, `us:dvorak`).
///
/// Read once: xkeyboard-config does not change under a running program.
#[must_use]
pub fn installed_layouts() -> (&'static [Layout], Option<String>) {
    static INSTALLED: std::sync::OnceLock<(Vec<Layout>, Option<String>)> =
        std::sync::OnceLock::new();
    let (layouts, error) = INSTALLED.get_or_init(catalog::installed);
    (layouts, error.clone())
}

/// The layouts a host offers without reading an installed set: the ones nearly every machine has,
/// which the installer can write onto a target. What a picker on a host with no xkeyboard-config
/// lists.
#[must_use]
pub fn fallback() -> Vec<Layout> {
    catalog::fallback()
}

/// The names [`fallback`] offers, in picker order.
#[must_use]
pub fn display_names() -> Vec<String> {
    tilo_keyboard_catalog::LAYOUTS
        .iter()
        .map(|layout| layout.label.to_owned())
        .collect()
}

/// The offered layout a picker calls `name`.
#[must_use]
pub fn by_display_name(name: &str) -> Option<Layout> {
    let name = name.trim();
    tilo_keyboard_catalog::LAYOUTS
        .iter()
        .find(|layout| layout.label == name)
        .map(|layout| Layout {
            id: layout.id.to_owned(),
            label: layout.label.to_owned(),
        })
}

/// The offered layout shown by default.
#[must_use]
pub fn default_layout() -> Layout {
    by_display_name(DEFAULT_DISPLAY_NAME).unwrap_or_else(|| {
        catalog::fallback()
            .into_iter()
            .next()
            .unwrap_or_else(|| Layout {
                id: "us".to_owned(),
                label: DEFAULT_DISPLAY_NAME.to_owned(),
            })
    })
}

/// What the offered layout `id` is called; `None` when it is not one the installer offers.
#[must_use]
pub fn label_of(id: &str) -> Option<&'static str> {
    tilo_keyboard_catalog::by_id(id).map(|layout| layout.label)
}

/// Whether `id` is a layout the installer offers.
#[must_use]
pub fn offered(id: &str) -> bool {
    tilo_keyboard_catalog::by_id(id).is_some()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Symbol {
    pub text: String,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Key {
    pub name: String,
    /// The focused window's physical identity, independent of the logical layout.
    pub physical: winit::keyboard::PhysicalKey,
    /// Ordered native modifier levels; no silent truncation.
    pub levels: Vec<Vec<Symbol>>,
}

/// An explicit native observation failure; absence never becomes a fake map.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Unsupported,
    InvalidRequest,
    Unavailable,
    InvalidMetadata,
}

impl Error {
    /// This host has no native metadata provider at all, rather than one that
    /// failed.
    #[must_use]
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "native keyboard metadata is unavailable on this platform",
            Self::InvalidRequest => "invalid keyboard metadata request",
            Self::Unavailable => "installed keyboard metadata is unavailable",
            Self::InvalidMetadata => "installed keyboard metadata exceeded its bounds",
        })
    }
}
impl std::error::Error for Error {}

/// Compile an installed model and layout from fixed public metadata only.
pub fn compile(
    layout: &str,
    variant: &str,
    model: &str,
    options: &str,
    names: &[String],
) -> Result<Vec<Key>, Error> {
    if !valid_name(layout, false)
        || !valid_name(variant, true)
        || !matches!(model, "pc104" | "pc105" | "jp106" | "kr106")
        || options.len() > 512
        || !options
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_:,-".contains(&b))
        || names.is_empty()
        || names.len() > 128
        || names.iter().any(|name| !valid_name(name, false))
    {
        return Err(Error::InvalidRequest);
    }
    backend::compile(layout, variant, model, options, names)
}

/// Whether a validated layout's symbols file is in the same fixed public root the
/// compiler reads.
pub fn is_installed(layout: &str) -> Result<bool, Error> {
    if !valid_name(layout, false) {
        return Err(Error::InvalidRequest);
    }
    backend::is_installed(layout)
}

fn valid_name(name: &str, empty: bool) -> bool {
    (empty || !name.is_empty())
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

#[cfg(all(target_os = "linux", feature = "native"))]
#[path = "linux.rs"]
mod backend;

#[cfg(not(all(target_os = "linux", feature = "native")))]
mod backend {
    use super::{Error, Key};
    pub(super) fn is_installed(_layout: &str) -> Result<bool, Error> {
        Err(Error::Unsupported)
    }
    pub(super) fn compile(
        _layout: &str,
        _variant: &str,
        _model: &str,
        _options: &str,
        _names: &[String],
    ) -> Result<Vec<Key>, Error> {
        Err(Error::Unsupported)
    }
}

#[cfg(test)]
#[cfg(all(target_os = "linux", feature = "native"))]
mod tests {
    use super::*;

    #[test]
    fn installed_layouts_preserve_physical_identity_and_reject_path_authority() {
        let names = vec!["AD06".to_owned()];
        let us = compile("us", "", "pc105", "", &names).unwrap();
        let de = compile("de", "", "pc105", "", &names).unwrap();
        let greek = compile("gr", "", "pc105", "", &names).unwrap();
        assert_eq!(
            us[0].physical,
            winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyY)
        );
        assert_eq!(us[0].physical, de[0].physical);
        assert_eq!(us[0].physical, greek[0].physical);
        assert_eq!(us[0].levels[0][0].text, "y");
        assert_eq!(de[0].levels[0][0].text, "z");
        assert_eq!(greek[0].levels[0][0].text, "υ");
        for (layout, model, options) in [
            ("../../tmp/layout", "pc105", ""),
            ("us", "/tmp/model", ""),
            ("us", "pc105", "/tmp/options"),
        ] {
            assert_eq!(
                compile(layout, "", model, options, &names),
                Err(Error::InvalidRequest)
            );
        }
        assert_eq!(
            compile("us", "", "pc105", "", &["NO_SUCH_KEY".to_owned()]),
            Err(Error::InvalidMetadata)
        );
    }

    /// A drawing lays every position of the model out with its outline, and prints the legend the
    /// layout gives it in the layout's own language.
    #[test]
    fn a_drawing_has_a_placed_key_for_every_position_and_its_legend() {
        let drawing = draw("de", "pc105", "", false).unwrap();
        assert_eq!(drawing.layout, "de");
        assert_eq!(drawing.model, "pc105");
        assert!(drawing.width > 0.0 && drawing.height > 0.0);
        assert!(drawing.level_count >= 1);
        let z = drawing
            .keys
            .iter()
            .find(|key| key.name == "AD06")
            .expect("the Y/Z position is drawn");
        assert_eq!(z.levels[0], "z", "German prints z where US prints y");
        assert_eq!(z.outline.len(), 4, "a key body is a quad");
        let widest = usize::try_from(drawing.level_count).unwrap_or_default();
        assert!(
            drawing.keys.iter().any(|key| key.levels.len() == widest),
            "some key supplies every level the drawing reports"
        );
    }

    /// Dvorak and Colemak are `us` variants, so an id names a layout and its variant.
    #[test]
    fn a_source_id_names_a_layout_and_its_variant() {
        let dvorak = by_display_name("Dvorak").expect("offered");
        assert_eq!(dvorak.id, "us:dvorak");
        assert_eq!(dvorak.xkb_layout(), "us");
        assert_eq!(dvorak.xkb_variant(), "dvorak");
        let us = by_display_name(DEFAULT_DISPLAY_NAME).expect("offered");
        assert_eq!(us.xkb_variant(), "");
    }
}
