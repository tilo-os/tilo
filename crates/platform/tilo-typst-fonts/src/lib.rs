//! The fonts `typst-assets` bundles, on every target.
//!
//! Native builds embed them ([`embedded`]). A page cannot afford megabytes of
//! font tables in its module, so the browser bundle ships the same files as
//! host data, which an app requests by the names in [`HOST_FONTS`]
//! (`ui::host_data`). [`EMBEDDED`] says which of the two this build does, so
//! an app needs no target code of its own.

/// Whether this build carries the fonts. When `false`, request each of
/// [`HOST_FONTS`] from the host instead.
pub const EMBEDDED: bool = !cfg!(target_arch = "wasm32");

/// The host-data names under which the browser bundle serves the fonts.
/// `crates/browser/tilo-browser-runtime/scripts/build.sh` copies them from
/// `typst-assets` and checks them against this list.
pub const HOST_FONTS: [&str; 17] = [
    "typst/LibertinusSerif-Regular.otf",
    "typst/LibertinusSerif-Bold.otf",
    "typst/LibertinusSerif-Italic.otf",
    "typst/LibertinusSerif-BoldItalic.otf",
    "typst/LibertinusSerif-Semibold.otf",
    "typst/LibertinusSerif-SemiboldItalic.otf",
    "typst/NewCMMath-Bold.otf",
    "typst/NewCMMath-Book.otf",
    "typst/NewCMMath-Regular.otf",
    "typst/NewCM10-Regular.otf",
    "typst/NewCM10-Bold.otf",
    "typst/NewCM10-Italic.otf",
    "typst/NewCM10-BoldItalic.otf",
    "typst/DejaVuSansMono-Bold.ttf",
    "typst/DejaVuSansMono-BoldOblique.ttf",
    "typst/DejaVuSansMono-Oblique.ttf",
    "typst/DejaVuSansMono.ttf",
];

/// Whether this target's bundled set should be completed with the system
/// font catalog's color-emoji face (Windows, which has no emoji face Typst
/// can use).
pub const BORROW_SYSTEM_EMOJI: bool = cfg!(target_os = "windows");

/// The embedded font files; empty where [`EMBEDDED`] is `false`.
#[must_use]
pub fn embedded() -> Vec<&'static [u8]> {
    imp::embedded()
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    pub fn embedded() -> Vec<&'static [u8]> {
        typst_assets::fonts().collect()
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    pub const fn embedded() -> Vec<&'static [u8]> {
        Vec::new()
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;

    #[test]
    fn native_builds_embed_every_font_the_host_would_serve() {
        const { assert!(EMBEDDED) };
        assert_eq!(embedded().len(), HOST_FONTS.len());
    }
}
