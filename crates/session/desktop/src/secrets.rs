//! Apps known to show secrets, whose windows every desktop, native or in a
//! browser, withholds from what leaves the device: captures, recordings of
//! a window's record for a report.

/// Apps known to show secrets — password managers and authenticator codes.
/// Their windows are protected whether or not they ask, since most cannot:
/// only apps built on the Tilo toolkit speak the shell-ui request. Matched
/// against the app id, ignoring case.
const SECRET_HOLDING_APP_IDS: &[&str] = &[
    "so.tilo.passwords",
    "so.tilo.totp",
    "org.keepassxc.keepassxc",
    "com.bitwarden.desktop",
    "bitwarden",
    "1password",
    "org.gnome.world.secrets",
    "org.gnome.seahorse.application",
    "com.belmoussaoui.authenticator",
];

/// Whether windows of `app_id` are protected for what the app is, before
/// any request.
pub fn holds_secrets(app_id: &str) -> bool {
    SECRET_HOLDING_APP_IDS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(app_id))
}
