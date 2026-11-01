//! What a capture never shows: a private window leaves the desktop as a
//! placeholder.
//!
//! The person's own display always shows everything. Every copy of the
//! desktop's pixels that leaves it - a screenshot, a recording, the replay
//! buffer, a share stream, the portal's screenshot - draws a protected
//! window as one flat placeholder instead of its content, so a viewer sees
//! that something is withheld without learning what.
//!
//! Protected is: every window of an app known to hold secrets (password
//! managers and authenticator codes, matched on the app id), every window
//! of an app the session marks ([`crate::Compositor::protect_app`]) while it
//! shows a password field or asks for protection itself, and every layer of
//! a hosted surface that says it shows a secret now
//! ([`crate::host::SurfaceHost::withholds_captures`]) — the session's own
//! sheets, panels and lock screen declare that the same way an app's window
//! is withheld, so a capture never shows what the person typed into them.
//!
//! A locked session's outputs show only the lock screen already, so nothing
//! of the windows behind it is captured either; the lock screen itself shows
//! a secret, so a capture of it is the placeholder.

use std::collections::BTreeSet;

/// Apps known to show secrets, by app id, ignoring case. Their windows are
/// protected whether or not they ask: most cannot ask, since only apps
/// built on the toolkit reach the session.
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

/// The colour a protected window is drawn in where its pixels leave the
/// display: the desktop's own quiet grey, opaque, carrying no text.
pub(crate) const PLACEHOLDER: [f32; 4] = [0.16, 0.17, 0.21, 1.0];

/// Whether windows of `app_id` are protected for what the app is, before it
/// asks.
#[must_use]
pub fn holds_secrets(app_id: &str) -> bool {
    SECRET_HOLDING_APP_IDS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(app_id))
}

/// Which apps' windows a capture may not show.
#[derive(Default)]
pub(crate) struct Protection {
    /// Apps the session marked private, beside those known to hold secrets.
    apps: BTreeSet<String>,
}

impl Protection {
    /// Mark the windows of `app_id` private (`private`) or showable again.
    pub(crate) fn app(&mut self, app_id: &str, private: bool) {
        if private {
            self.apps.insert(app_id.to_owned());
        } else {
            self.apps.remove(app_id);
        }
    }

    /// Whether a window of `app_id` is hidden from captures.
    pub(crate) fn covers(&self, app_id: Option<&str>) -> bool {
        let Some(app_id) = app_id else {
            return false;
        };
        holds_secrets(app_id) || self.apps.contains(app_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_manager_is_private_and_an_editor_is_not() {
        assert!(holds_secrets("so.tilo.passwords"));
        assert!(holds_secrets("Bitwarden"));
        assert!(!holds_secrets("org.gnome.TextEditor"));
        assert!(!Protection::default().covers(None));
    }

    #[test]
    fn an_app_the_session_marks_private_stays_private_until_it_says_otherwise() {
        let mut protection = Protection::default();
        protection.app("org.example.ledger", true);
        assert!(protection.covers(Some("org.example.ledger")));
        assert!(!protection.covers(Some("org.example.notes")));
        protection.app("org.example.ledger", false);
        assert!(!protection.covers(Some("org.example.ledger")));
    }
}
