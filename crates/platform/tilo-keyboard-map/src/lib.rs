//! Read-only native keyboard metadata. This service neither captures input nor
//! changes the active layout, and never accepts an include path or environment.

/// Shared touch layout data and geometry for keyboards and source previews.
pub mod touch;

/// A native symbol's printable value and canonical keysym name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Symbol {
    /// Printable UTF-8 supplied by the native keysym compiler.
    pub text: String,
    /// Canonical native symbol or physical-position name.
    pub name: String,
}

/// One requested physical position and its ordered modifier levels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Key {
    /// Canonical native symbol or physical-position name.
    pub name: String,
    /// The focused-window physical identity, independent of the logical layout.
    pub physical: winit::keyboard::PhysicalKey,
    /// Ordered native modifier levels; no silent truncation.
    pub levels: Vec<Vec<Symbol>>,
}

/// An explicit native observation failure; absence never becomes a fake map.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// This host has no native metadata provider.
    Unsupported,
    /// Model, symbol selection, or bounded options are invalid.
    InvalidRequest,
    /// Fixed installed metadata is unavailable or does not compile.
    Unavailable,
    /// Installed metadata contains an unknown key or exceeds output bounds.
    InvalidMetadata,
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

/// Compile an installed model/layout using fixed public metadata only.
/// The returned keycodes identify physical positions, never typed input.
///
/// # Errors
/// Rejects invalid or oversized requests, unavailable native metadata, unknown
/// requested physical positions, and native output exceeding the declared bounds.
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

/// Whether a validated layout has its symbols file in the same fixed public
/// root used by the compiler. Missing user-defined templates are not installed.
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

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod backend;

#[cfg(not(target_os = "linux"))]
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
#[cfg(target_os = "linux")]
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
}
