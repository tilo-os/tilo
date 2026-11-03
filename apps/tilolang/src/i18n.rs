//! The host's fluent catalogue: chrome only. What an app says comes from the app's own source.
//! The catalogue is the UI host's (`tilo_ui_host::i18n`), which says the window's chrome; the
//! standard library's frames say its phrases through the instance.

pub use tilo_ui_host::i18n::{CATALOG, t, t_args};

/// Load the chrome's catalogue and speak the installed package's language: the person's when
/// the package ships it, else the package's source language. The toolkit's own words and the
/// platform's formats follow it, so the window of an app written in one language reads in that
/// language throughout, its chrome included. A process presents one window, so the language is
/// the window's; calling it again is free.
pub fn ensure() {
    static SPOKEN: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    tilo_ui_host::i18n::ensure();
    if SPOKEN.get().is_some() {
        return;
    }
    let Some(bundle) = crate::installed_bundle() else {
        return;
    };
    ui::i18n::speak_window_language(&bundle.catalogues());
    let _ = SPOKEN.set(());
}
