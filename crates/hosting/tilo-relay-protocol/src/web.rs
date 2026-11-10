//! What a page asks a host over HTTPS before it has a session: the host's
//! address, approvals for a member's own devices, and the join steps of a
//! workspace. Served by the host itself.
//!
//! Everything a page does once it is up goes over its iroh session; these
//! routes only get it there. Every body is postcard. Routes (all `POST`,
//! below [`API_PREFIX`]):
//!
//! | Route | Body | Answer |
//! |---|---|---|
//! | `/host` | — | [`HostInfo`] |
//! | `/certify/approve` | `MemberApproval` | `ApproverReply` |
//! | `/join` | [`JoinStep`](crate::workspaces::JoinStep) | [`JoinReply`](crate::workspaces::JoinReply) |
//! | `/fetch` ([`FETCH_PATH`]) | any method: `tilo_net::proxy` wire protocol v1 | the target's response, or the proxy's failure |
//!
//! `/fetch` is the host's fetch proxy: through it a page reads the sites that
//! send no CORS headers. Unlike every other route it takes the method of the
//! request it relays, and it takes its session token in [`SESSION_HEADER`].
//!
//! A host started without a certificate serves only `/install` until it has
//! one: the body is the root-signed `DeviceCert` for its device key as JSON
//! (what a page's root issues) or in its canonical encoding, or
//! [`InstallCertificates`], with the install token the host was
//! provisioned with in [`INSTALL_TOKEN_HEADER`]. `200` answers
//! [`Installed`] as JSON; every other route answers `503` meanwhile. A
//! `GET` with the token answers [`InstallInfo`]: the host's key and, on a
//! node, the key of the session beside it, which the root certifies at the
//! same time as one of the person's devices.
//!
//! `/join` is the route a device calls without a session: a person a host
//! does not know yet joins through it. A device certifies itself over the
//! host's certification ALPN, and `/certify/approve` carries a member's
//! root-signed answer to their own device's request.

use serde::{Deserialize, Serialize};

/// Path prefix of every route a page asks a host.
pub const API_PREFIX: &str = "/peer/v1";
/// Header carrying the session token.
pub const SESSION_HEADER: &str = "x-tilo-session";
/// Header carrying the install token of a host awaiting its certificate.
pub const INSTALL_TOKEN_HEADER: &str = "x-tilo-install-token";
/// The route that installs a waiting host's certificate.
pub const INSTALL_ROUTE: &str = "/install";

/// What a waiting host says about itself on `GET /install` (JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallInfo {
    pub device_public_key: String,
    /// On a node, the session that runs the person's apps and agents beside
    /// the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<NodeSession>,
}

/// The device key of the session beside a node, vouched for by the node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSession {
    pub device_public_key: String,
    /// The node's device key's Ed25519 signature over
    /// [`node_session_message`], hex. A page that got the node's key out of
    /// band certifies the session's key only when this verifies.
    pub signature: String,
}

/// What a node signs to vouch for the session beside it.
#[must_use]
pub fn node_session_message(node: &[u8; 32], session: &[u8; 32]) -> Vec<u8> {
    let mut message = b"tilo node session v1\0".to_vec();
    message.extend_from_slice(node);
    message.extend_from_slice(session);
    message
}

/// `POST /install` with the certificates of a node and of the session
/// beside it (JSON): each in its canonical encoding, base64, as a page's
/// `certifyDevice` resolves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallCertificates {
    pub certificate: String,
    /// The session's certificate, when the host has one beside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_certificate: Option<String>,
}

/// A waiting host's answer to an accepted `/install` (JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub device_public_key: String,
    /// The identity it now serves, hex.
    pub user_public_key: String,
}

/// Largest request body a route accepts.
pub const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
/// What a page needs to know about the host before it has a certificate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    /// The host's device certificate (pin its device key).
    pub host_cert: tilo_user_identity::DeviceCert,
    /// Where the host listens on iroh, which is how a page reaches it for
    /// everything a session and its planes carry. `None` in a host that serves
    /// no iroh endpoint.
    pub address: Option<HostAddress>,
}

/// Where a host's iroh endpoint is dialled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostAddress {
    /// The endpoint's key and the addresses it answers at.
    pub endpoint_addr: iroh_base::EndpointAddr,
    /// The packet relay it is reached through, when it names one; a page dials
    /// the endpoint through it rather than over the network's own relays.
    pub relay_url: Option<String>,
}

/// The host's fetch proxy, below [`API_PREFIX`].
pub const FETCH_PATH: &str = "/fetch";

/// Bound of [`HereRequest::actions`].
pub const MAX_HERE_ACTIONS: usize = 16;
/// Bound, in characters, of each text field of a [`HereAction`].
pub const MAX_HERE_FIELD_CHARS: usize = 96;
/// Bound, in characters, of [`HerePerson::status`]. The same bound as a
/// person's typed or inferred status line in the UI (`tilo-ui`'s
/// `live_collaboration::MAX_STATUS_CHARS`), so a host line fits wherever a
/// status shows.
pub const MAX_HERE_STATUS_CHARS: usize = 96;
/// Bound, in characters, of [`HereRequest::label`] and [`HerePerson::label`].
pub const MAX_HERE_LABEL_CHARS: usize = 32;
/// Bound, in characters, of [`HerePerson::recent`].
pub const MAX_HERE_RECENT_CHARS: usize = 320;

/// `text` without control characters, cut to the `max` characters a host keeps
/// ([`MAX_HERE_LABEL_CHARS`], [`MAX_HERE_FIELD_CHARS`]): what this device says
/// of itself, from text that is not trusted to be printable.
#[must_use]
pub fn kept(text: &str, max: usize) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .take(max)
        .collect()
}
/// How long a person stays listed after their last post, in seconds: the
/// lifetime of an inferred status in the UI (`tilo-ui`'s
/// `live_collaboration::INFERRED_STATUS_TTL`).
pub const HERE_TTL_SECS: u64 = 5 * 60;

/// One semantic action a person took: the titles of the action, the record
/// it acted on and the app, never keystrokes or bodies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HereAction {
    /// The action's id; a summary cites it.
    pub id: [u8; 16],
    /// The action's title, e.g. "Edited".
    pub action: String,
    /// The record's title, e.g. "Welcome note".
    pub resource: String,
    /// The app's name, e.g. "Notes".
    pub app: String,
    /// When it happened, Unix milliseconds.
    pub at_ms: u64,
}

/// A person's own recent semantic actions in one workspace (titles of
/// actions and records only; never keystrokes or bodies). Each post is the
/// poster's whole current set, so empty `actions` lists them where they are
/// with no line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HereRequest {
    /// The workspace, which the session's person must be admitted to.
    pub workspace_id: u64,
    /// The poster's own display name, at most [`MAX_HERE_LABEL_CHARS`]
    /// characters, possibly empty. Self-asserted like a live participant
    /// label: shown, never trusted for anything else.
    pub label: String,
    /// The tab id of the desktop window the poster has focused in the
    /// workspace's shared desktop, if any. The desktop tree is replicated, so the
    /// id means the same window to every member.
    pub window: Option<u64>,
    /// Their recent actions, at most [`MAX_HERE_ACTIONS`].
    pub actions: Vec<HereAction>,
    /// The poster is an agent the person runs on this device (`tilo mcp`),
    /// [`Self::label`] its name: its session is one run of that agent, which
    /// the person's agent door lists and can stop. Said by the device itself,
    /// as the agent run History groups its changes under is.
    pub agent: bool,
}

impl HereRequest {
    /// Whether the request is within the published bounds: at most
    /// [`MAX_HERE_ACTIONS`] actions with distinct ids, each with a non-empty
    /// action title and text fields of at most [`MAX_HERE_FIELD_CHARS`]
    /// characters without control characters. A host refuses a request past
    /// them rather than shortening it, which would summarise something the
    /// person did not send.
    ///
    /// # Errors
    ///
    /// Names the first bound broken.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.label.chars().count() > MAX_HERE_LABEL_CHARS {
            return Err("the label is too long");
        }
        if self.label.chars().any(char::is_control) {
            return Err("the label holds a control character");
        }
        if self.actions.len() > MAX_HERE_ACTIONS {
            return Err("too many actions");
        }
        for (index, action) in self.actions.iter().enumerate() {
            if action.action.trim().is_empty() {
                return Err("an action needs a title");
            }
            for field in [&action.action, &action.resource, &action.app] {
                if field.chars().count() > MAX_HERE_FIELD_CHARS {
                    return Err("an action field is too long");
                }
                if field.chars().any(char::is_control) {
                    return Err("an action field holds a control character");
                }
            }
            if self
                .actions
                .iter()
                .take(index)
                .any(|seen| seen.id == action.id)
            {
                return Err("action ids repeat");
            }
        }
        Ok(())
    }
}

/// Everyone listed in a workspace now, the caller included.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HereReply {
    /// Ordered by device key.
    pub people: Vec<HerePerson>,
}

/// One person on one device and what they are doing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HerePerson {
    /// The session's authenticated device key (what the live plane calls
    /// the publisher).
    pub device: [u8; 32],
    /// The person's user root.
    pub user: [u8; 32],
    /// The display name of their latest post; self-asserted, display only.
    pub label: String,
    /// The device says it runs an agent ([`HereRequest::agent`]), which
    /// [`Self::label`] names. It is what a surface draws the device as: an
    /// agent's face, never a person's, whether or not the agent's own
    /// presence reached it.
    pub agent: bool,
    /// The window of their latest post ([`HereRequest::window`]).
    pub window: Option<u64>,
    /// One line, present tense, e.g. "Drafting the welcome note in Notes";
    /// at most [`MAX_HERE_STATUS_CHARS`] characters.
    pub status: String,
    /// What they did in the last minutes, at most [`MAX_HERE_RECENT_CHARS`]
    /// characters.
    pub recent: String,
    /// The action ids the text cites.
    pub sources: Vec<[u8; 16]>,
    /// `true`: model prose that passed grounding; `false`: the host's
    /// deterministic summary.
    pub generated: bool,
    /// When the line last changed, Unix milliseconds.
    pub updated_ms: u64,
    /// When the person last changed the profile this host keeps for them
    /// (their name and picture), Unix milliseconds; 0 when it keeps none.
    /// A page fetches the profile again when it changes.
    pub profile_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn here_requests_past_their_bounds_are_refused() {
        let action = |id: u8, title: &str| HereAction {
            id: [id; 16],
            action: title.into(),
            resource: "Welcome note".into(),
            app: "Notes".into(),
            at_ms: 1,
        };
        let request = |actions| HereRequest {
            workspace_id: 1,
            label: "Ada".into(),
            window: None,
            actions,
            agent: false,
        };
        assert!(request(Vec::new()).validate().is_ok());
        let full: Vec<_> = (0..16u8).map(|id| action(id, "Edited")).collect();
        assert_eq!(full.len(), MAX_HERE_ACTIONS);
        assert!(request(full.clone()).validate().is_ok());
        let mut over = full;
        over.push(action(200, "Edited"));
        assert!(request(over).validate().is_err());
        let long = "é".repeat(MAX_HERE_FIELD_CHARS);
        assert!(request(vec![action(1, &long)]).validate().is_ok());
        let longer = format!("{long}x");
        for refused in [
            vec![action(1, &longer)],
            vec![action(1, " ")],
            vec![action(1, "Edited\nnow")],
            vec![action(1, "Edited"), action(1, "Opened")],
        ] {
            assert!(request(refused).validate().is_err());
        }
        for label in ["x".repeat(MAX_HERE_LABEL_CHARS + 1), "A\u{7}da".into()] {
            let mut labelled = request(Vec::new());
            labelled.label = label;
            assert!(labelled.validate().is_err());
        }
    }
}
