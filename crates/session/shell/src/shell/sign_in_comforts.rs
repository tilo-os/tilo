//! The login screens follow the comforts a person turns on in their session
//! (docs/security/sign-in.md §3.4): the screen reader, text size, contrast,
//! language and keyboard layout. `tilo-authd` keeps the device's copy and
//! mirrors it into the volume header for the boot.
//!
//! Only a change is carried over, and only the fields that changed: signing
//! in leaves the login screens as they are, and a person who makes their text
//! larger does not turn off the screen reader someone turned on at the login
//! screen. Their `apply_to_sign_in` switch keeps the two apart.

use std::sync::mpsc;

use settings_store::sign_in_comforts::SignInComforts;
use tilo_accessibility_settings::AccessibilitySettings;
use tilo_auth::client::Client;
use tilo_auth::protocol::Request;

/// The person's comforts as the login screens would take them.
fn persons() -> (SignInComforts, bool) {
    let access = AccessibilitySettings::load();
    let theme = ui::theme::Theme::configured_comforts();
    let defaults = SignInComforts::default();
    let comforts = SignInComforts {
        screen_reader: access.screen_reader,
        screen_reader_rate: access.screen_reader_rate,
        screen_reader_voice: access.screen_reader_voice,
        language: settings_store::appearance::LANGUAGE.get(),
        xkb_layout: None,
        xkb_variant: None,
        text_scale: theme.text_scale.unwrap_or(defaults.text_scale),
        high_contrast: theme.high_contrast.unwrap_or(defaults.high_contrast),
    }
    .with_keyboard_source(&settings_store::hardware::RichInputConfig::load().keyboard_layout)
    .clamped();
    (comforts, access.apply_to_sign_in)
}

#[derive(Default)]
pub struct SignInMirror {
    seen: Option<SignInComforts>,
    /// Sends each change to `tilo-authd` in order, off the frame.
    worker: Option<mpsc::Sender<serde_json::Map<String, serde_json::Value>>>,
}

impl std::fmt::Debug for SignInMirror {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignInMirror")
            .field("seen", &self.seen)
            .finish_non_exhaustive()
    }
}

impl SignInMirror {
    /// Look at the person's comforts after their settings changed.
    pub fn observe(&mut self) {
        let (now, apply) = persons();
        if let Some(changes) = self.change(now, apply) {
            self.send(changes);
        }
    }

    /// The fields to change at the login screens, if `now` changed what
    /// they take and the person lets them follow. The first look is where
    /// the session starts from, not a change.
    fn change(
        &mut self,
        now: SignInComforts,
        apply: bool,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        let before = self.seen.replace(now)?;
        let changes = SignInComforts::changes(&before, self.seen.as_ref()?);
        (apply && !changes.is_empty()).then_some(changes)
    }

    fn send(&mut self, changes: serde_json::Map<String, serde_json::Value>) {
        let worker = self.worker.get_or_insert_with(|| {
            let (sender, changes) = mpsc::channel();
            std::thread::Builder::new()
                .name("sign-in-comforts".into())
                .spawn(move || {
                    for comforts in changes {
                        mirror(comforts);
                    }
                })
                .map_err(|error| tracing::warn!(%error, "sign-in comforts worker did not start"))
                .ok();
            sender
        });
        if worker.send(changes).is_err() {
            self.worker = None;
        }
    }
}

fn mirror(comforts: serde_json::Map<String, serde_json::Value>) {
    let reply = Client::connect(std::path::Path::new(tilo_auth::SOCKET))
        .and_then(|mut client| client.call(&Request::SetSignInComforts { comforts }));
    match reply {
        Ok(reply) if reply.ok => {}
        Ok(reply) => tracing::warn!(?reply.error, "the login screens kept their comforts"),
        Err(error) => tracing::debug!(%error, "no sign-in daemon to take the comforts"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_change_the_person_lets_through_reaches_the_login_screens() {
        let mut mirror = SignInMirror::default();
        let start = SignInComforts::default().with_keyboard_source("de");
        assert_eq!(mirror.change(start.clone(), true), None);
        assert_eq!(mirror.change(start.clone(), true), None);
        let reader = SignInComforts {
            screen_reader: true,
            ..start.clone()
        };
        assert_eq!(
            mirror.change(reader.clone(), true),
            serde_json::json!({"screen_reader": true})
                .as_object()
                .cloned()
        );
        let larger = SignInComforts {
            text_scale: 1.5,
            ..reader
        };
        assert_eq!(mirror.change(larger, false), None);
        assert_eq!(
            mirror.change(start, true),
            serde_json::json!({"screen_reader": false, "text_scale": 1.0})
                .as_object()
                .cloned()
        );
    }
}
