//! The on-screen keyboard (`apps/tl/osk`) as the session runs it: its package, compiled from the
//! sources this build ships ([`compile`]). The keyboard is the platform's input method, drawn by a
//! view of the session's `keyboard` resource, so every session has it, whether or not a person's
//! app store holds the package; one that does, or a view a person bound in its place, is run
//! instead.

use crate::tilolang::Compiled;

/// The keyboard's package id.
pub const KEYBOARD_APP: &str = "so.tilo.osk";

/// The view that draws the keyboard over the session's `keyboard` reading.
pub const KEYBOARD_VIEW: &str = "keyboard";

/// Every file of the keyboard's package, by the name the package knows it by.
const FILES: &[(&str, &str)] = &[("app.tl", include_str!("../../../../apps/tl/osk/app.tl"))];

/// The package's ids lock.
const IDS: &str = include_str!("../../../../apps/tl/osk/app.ids");

/// The keyboard's translations, by locale, travelling in its module as an installed app's do.
const CATALOGUES: [(&str, &str); 1] = [(
    "de-DE",
    include_str!("../../../../apps/tl/osk/locales/de-DE.ftl"),
)];

/// The keyboard's package, compiled over the session's and the platform's descriptions: what the
/// session runs when no installed package draws its keyboard. Compiled once per process.
///
/// # Errors
/// The package's diagnostics, rendered with their file names: every build of the keyboard is
/// checked against them, so this fails loudly in a test.
pub fn compile() -> Result<Compiled, String> {
    static COMPILED: std::sync::OnceLock<Result<Compiled, String>> = std::sync::OnceLock::new();
    COMPILED.get_or_init(compile_source).clone()
}

fn compile_source() -> Result<Compiled, String> {
    let session = crate::platform_apps::session_package()?;
    let lock = crate::tilolang::IdsLock::parse(IDS).map_err(|e| e.to_string())?;
    let source = crate::tilolang::PackageSource::new(
        FILES
            .iter()
            .map(|(path, text)| (format!("apps/tl/osk/{path}"), (*text).to_owned())),
    )
    .with_locales(
        CATALOGUES
            .into_iter()
            .map(|(locale, text)| (locale.to_owned(), text.to_owned())),
    );
    let mut imports: Vec<crate::tilolang::PackageMeta> = crate::platform_apps::platform_packages()?
        .into_iter()
        .filter(|meta| meta.app != crate::platform_apps::SESSION_APP)
        .collect();
    imports.insert(0, session);
    crate::tilolang::compile_package_source(&source, &lock, &imports).map_err(|diagnostics| {
        diagnostics
            .iter()
            .map(|d| d.render(d.file.as_deref().unwrap_or("apps/tl/osk/app.tl")))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

#[cfg(test)]
mod tests {
    /// The keyboard this build ships compiles over the session it runs in: a session never comes
    /// up without one.
    #[test]
    fn the_shipped_keyboard_compiles() {
        let compiled = super::compile().unwrap_or_else(|error| panic!("{error}"));
        assert!(
            compiled
                .meta
                .views
                .iter()
                .any(|view| view.name == super::KEYBOARD_VIEW),
            "the keyboard view is in the package"
        );
    }
}
