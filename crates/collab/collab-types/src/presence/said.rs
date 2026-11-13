//! What one participant said about where they are, in one frame.
//!
//! A participant says it on the workspace's presence track
//! ([`crate::scope::Scope::workspace`]): the session, a page and an agent say
//! the same frame, and every reader takes it from there. The sender is the
//! frame's sealed author, so nothing here names who said it.

use serde::{Deserialize, Serialize};

use crate::canonical::{CanonicalError, canonical_bytes, from_canonical_bytes};
use crate::presence::{Payload, PresenceKey};

/// The app a device's own desktop presence is said under, as every reader of
/// a workspace expects it.
pub const DESKTOP_APP: &str = "tilo-shell";

/// One thing a participant said: the app it is about, the workspace (or
/// record) that app acts in, the scope within it, and what they say there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Presence {
    /// The app that published it, or [`DESKTOP_APP`] for a device's own
    /// desktop presence.
    pub app: String,
    /// The workspace, or record, as its uuid string travels.
    pub cluster: String,
    /// The scope within the app: a pointer space, an editor's widget path, a
    /// resource, the workspace itself.
    pub key: PresenceKey,
    /// What they say there.
    pub payload: Payload,
    /// The run (its `agent-run:` id) the workspace's host says this for: an
    /// agent's or a key's run the host executes, shown as a member of its
    /// own that the host speaks for ([`stand_in`]). A reader takes it only
    /// from the device the workspace's governance pins as its host. `None`
    /// for what a device says of itself.
    #[serde(default)]
    pub on_behalf: Option<String>,
}

/// The publisher a reader keys the member for `run` by, which the host
/// `host` speaks for: one member per run, apart from the host's own and
/// every other run's, the same on every reader.
#[must_use]
pub fn stand_in(host: [u8; 32], run: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("tilo presence: a run the host speaks for");
    hasher.update(&host);
    hasher.update(run.as_bytes());
    *hasher.finalize().as_bytes()
}

impl Presence {
    /// The frame a device's own desktop presence says in `workspace`.
    #[must_use]
    pub fn desktop(workspace: impl Into<String>, payload: Payload) -> Self {
        let cluster = workspace.into();
        Self {
            app: DESKTOP_APP.to_owned(),
            key: PresenceKey::Workspace {
                id: cluster.clone(),
            },
            cluster,
            payload,
            on_behalf: None,
        }
    }

    /// The frame a pointer over `(x, y)` of `tab`'s tile says in
    /// `workspace`, in that tile's logical pixels from its corner.
    #[must_use]
    pub fn pointing(workspace: impl Into<String>, tab: u64, x: f32, y: f32) -> Self {
        let cluster = workspace.into();
        Self {
            app: DESKTOP_APP.to_owned(),
            key: PresenceKey::Pointer {
                space: crate::presence::PointerSpace::Tile {
                    workspace: cluster.clone(),
                    tab,
                },
            },
            cluster,
            payload: Payload::Pointer { x, y },
            on_behalf: None,
        }
    }

    /// The frame saying the pointer left `tab`'s tile of `workspace`: nothing
    /// of this device's is drawn there until it comes back.
    #[must_use]
    pub fn pointer_left(workspace: impl Into<String>, tab: u64) -> Self {
        let cluster = workspace.into();
        Self {
            app: DESKTOP_APP.to_owned(),
            key: PresenceKey::Pointer {
                space: crate::presence::PointerSpace::Tile {
                    workspace: cluster.clone(),
                    tab,
                },
            },
            cluster,
            payload: Payload::Left,
            on_behalf: None,
        }
    }

    /// An app's frame in `workspace`: `payload` under `key`.
    #[must_use]
    pub fn app(
        app: impl Into<String>,
        workspace: impl Into<String>,
        key: PresenceKey,
        payload: Payload,
    ) -> Self {
        Self {
            app: app.into(),
            cluster: workspace.into(),
            key,
            payload,
            on_behalf: None,
        }
    }

    /// This frame, said by the workspace's host for `run`, the agent's or
    /// key's run it executes ([`Self::on_behalf`]).
    #[must_use]
    pub fn spoken_for(mut self, run: impl Into<String>) -> Self {
        self.on_behalf = Some(run.into());
        self
    }

    /// The frame's bytes, as the presence track carries them.
    ///
    /// # Errors
    ///
    /// When the frame cannot be encoded.
    pub fn encode(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(self)
    }

    /// The frame `bytes` name, or `None` when they are not one: bytes of
    /// another version, or trailing bytes after one.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        from_canonical_bytes(bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips() {
        let presence = Presence::desktop(
            "0f8fad5b-d9cb-469f-a165-70867728950e",
            Payload::Status {
                text: "Reviewing".into(),
                automatic: false,
            },
        );
        let bytes = presence.encode().unwrap();
        assert_eq!(Presence::decode(&bytes), Some(presence.clone()));
        assert_eq!(Presence::decode(&[bytes, vec![0]].concat()), None);
        assert_eq!(presence.app, DESKTOP_APP);
    }

    #[test]
    fn a_pointer_names_the_tile_it_was_measured_on() {
        let presence = Presence::pointing("w1", 7, 12.5, -3.0);
        assert_eq!(
            presence.key,
            PresenceKey::Pointer {
                space: crate::presence::PointerSpace::Tile {
                    workspace: "w1".into(),
                    tab: 7,
                },
            }
        );
        assert_eq!(presence.payload, Payload::Pointer { x: 12.5, y: -3.0 });
    }
}
