//! The SIM's PIN and PUK sheets, raised the way `NetworkManager` asks.
use super::ShellDriver;
use std::collections::HashMap;

impl ShellDriver {
    /// The SIM sheet as `NetworkManager` raises it through the session's
    /// secret agent: its `GetSecrets` for a mobile connection's PIN,
    /// understood by the agent, worded and matched with the SIM's state by
    /// the shell. `lock` is what `ModemManager` says the SIM asks for. A
    /// narrow display is a phone, used by touch: the keypad shows.
    pub(super) fn scene_secret_prompt_sim(&mut self, lock: os_actions::SimLock) {
        use tilo_network_secret_agent::{FLAG_ALLOW_INTERACTION, FLAG_REQUEST_NEW, SecretRequest};
        let Some(connection) = gsm_connection() else {
            return;
        };
        let flags = match lock {
            // NetworkManager asks again after the wrong PIN that locked it.
            os_actions::SimLock::Puk => FLAG_ALLOW_INTERACTION | FLAG_REQUEST_NEW,
            _ => FLAG_ALLOW_INTERACTION,
        };
        let Ok(request) = SecretRequest::parse(
            &connection,
            "/org/freedesktop/NetworkManager/Settings/4",
            "gsm",
            &["pin".to_owned()],
            flags,
        ) else {
            return;
        };
        let sim = shell::shell::portal_dialog::SimUnlock::of(
            lock,
            os_actions::UnlockRetries {
                pin: Some(3),
                puk: Some(10),
            },
            Some("/org/freedesktop/ModemManager1/SIM/0".into()),
        );
        let handle = match lock {
            os_actions::SimLock::Puk => "nm-secrets:shots/gsm-puk",
            _ => "nm-secrets:shots/gsm",
        };
        let sheet = shell::network_secrets::sheet_request(handle, &request, Some(sim));
        if self.fb.scene_size().0 < 600.0 {
            let touch = ui::InteractionEnvironment {
                modality: ui::InputModality::Touch,
                ..ui::InteractionEnvironment::default()
            };
            self.host_mut().apply_input_environment(touch);
        }
        let queue = self.host_mut().portal_dialog_queue();
        if let Ok(reply) = queue.enqueue_network_secrets(sheet) {
            self.held.push(Box::new(reply));
        }
        self.host_mut().pump_portal_dialogs();
        self.step_frames(2);
    }
}

/// A mobile connection as `NetworkManager` hands it to its secret agent.
fn gsm_connection() -> Option<HashMap<String, HashMap<String, zbus::zvariant::OwnedValue>>> {
    use zbus::zvariant::{OwnedValue, Value};
    let value = |value: &'static str| OwnedValue::try_from(Value::from(value)).ok();
    Some(HashMap::from([
        (
            "connection".to_owned(),
            HashMap::from([
                ("id".to_owned(), value("Carrier")?),
                (
                    "uuid".to_owned(),
                    value("5f2c7c9e-0000-4000-8000-000000000004")?,
                ),
                ("type".to_owned(), value("gsm")?),
            ]),
        ),
        (
            "gsm".to_owned(),
            HashMap::from([("apn".to_owned(), value("internet")?)]),
        ),
    ]))
}
