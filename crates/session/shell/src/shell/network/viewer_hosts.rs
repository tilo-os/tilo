//! Who a headless session's viewers watch a room through.
use super::{ShellNetwork, room_topic_id};
use crate::shell::state::RoomId;

impl ShellNetwork {
    /// The peers a headless session's viewers watch `room` through: its
    /// workspace hosts, and the device governing the room, which forwards
    /// a controlling viewer's input as its own. Each is shared every legacy
    /// window of the room's workspace while the session is watched. Called
    /// every tick, so it reads the governor only from an owner already open.
    pub fn viewer_hosts(&self, room: RoomId) -> std::collections::HashSet<u64> {
        let mut hosts = self.workspace_hosts(room_topic_id(room));
        if let Some(owner) = self.opened_native_owner()
            && let Ok(governor) = owner.with_current(room, |view| {
                Ok((
                    view.device().device_public_key,
                    view.room_host().device_public_key,
                ))
            })
            && governor.0 != governor.1
            && let Ok(endpoint) = iroh::EndpointId::from_bytes(&governor.1)
        {
            hosts.insert(tilo_network_actor::behaviour::peer_liveness_handle(
                &endpoint,
            ));
        }
        hosts
    }
}

#[cfg(test)]
mod tests {
    /// A device host forwards a viewer's input with the relay's legacy-window
    /// vocabulary and the governor's handle; both must be what this session
    /// decodes.
    #[test]
    fn viewer_input_from_a_device_host_decodes_as_remote_input() {
        use tilo_relay_protocol::legacy_window::InputEvent as Wire;
        use tilo_window_share::streaming::InputEvent as Session;
        let pairs = [
            (
                Session::PointerMotion { x: 1.5, y: -2.0 },
                Wire::PointerMotion { x: 1.5, y: -2.0 },
            ),
            (
                Session::PointerButton {
                    button: 272,
                    pressed: true,
                },
                Wire::PointerButton {
                    button: 272,
                    pressed: true,
                },
            ),
            (
                Session::PointerEnter { surface: 7 },
                Wire::PointerEnter { surface: 7 },
            ),
            (
                Session::PointerLeave { surface: 7 },
                Wire::PointerLeave { surface: 7 },
            ),
            (
                Session::Key {
                    keysym: 30,
                    pressed: false,
                    modifiers: 4,
                },
                Wire::Key {
                    keysym: 30,
                    pressed: false,
                    modifiers: 4,
                },
            ),
            (
                Session::KeyboardFocus {
                    surface: 7,
                    focused: true,
                },
                Wire::KeyboardFocus {
                    surface: 7,
                    focused: true,
                },
            ),
        ];
        for (session, wire) in pairs {
            assert_eq!(
                postcard::to_allocvec(&session).unwrap(),
                postcard::to_allocvec(&wire).unwrap(),
                "{session:?}"
            );
        }
        // The device host's `legacy_windows::peer_handle` pins the same value.
        let key = iroh::SecretKey::from_bytes(&[1; 32]);
        assert_eq!(
            tilo_network_actor::behaviour::peer_liveness_handle(&key.public()),
            0x90b3_65ba_582f_856f
        );
    }

    /// A relay forwards a browser's input (wheel, a paste, a close, text its
    /// input method committed among it) in its wire vocabulary; each must be
    /// what this session decodes.
    #[test]
    fn viewer_input_from_a_relay_decodes_as_remote_input() {
        use tilo_relay_protocol::legacy_window::InputEvent as Wire;
        use tilo_window_share::streaming::InputEvent as Session;
        let pairs = [
            (
                Session::PointerMotion { x: 1.5, y: -2.0 },
                Wire::PointerMotion { x: 1.5, y: -2.0 },
            ),
            (
                Session::KeyboardFocus {
                    surface: 7,
                    focused: true,
                },
                Wire::KeyboardFocus {
                    surface: 7,
                    focused: true,
                },
            ),
            (
                Session::PointerAxis { dx: 0.0, dy: 30.0 },
                Wire::PointerAxis { dx: 0.0, dy: 30.0 },
            ),
            (
                Session::ClipboardText {
                    text: "pasted".into(),
                },
                Wire::ClipboardText {
                    text: "pasted".into(),
                },
            ),
            (Session::Close, Wire::Close),
            (
                Session::Text {
                    text: "日本".into(),
                },
                Wire::Text {
                    text: "日本".into(),
                },
            ),
        ];
        for (session, wire) in pairs {
            assert_eq!(
                postcard::to_allocvec(&session).unwrap(),
                postcard::to_allocvec(&wire).unwrap(),
                "{session:?}"
            );
        }
        assert_eq!(
            tilo_relay_protocol::legacy_window::MAX_TEXT_BYTES,
            crate::network::input_firewall::MAX_REMOTE_TEXT_BYTES,
            "a relay forwards no text this session's firewall refuses"
        );
    }
}
