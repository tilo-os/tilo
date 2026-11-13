//! What the device of a session in a workspace's VM asks of the workspace's
//! host (`docs/hosted-browser.md`, "Linux apps" and "Isolation").
//!
//! The session in a workspace's VM runs strangers' code — web pages, Linux
//! programs — so its device is confined to its own record and holds none of
//! the workspace's keys: not a record's, and not the content keys of the
//! workspace's media channel, which open every call, presence and window of
//! the workspace. Yet what it shows must reach the people watching, and what
//! it shares must be written where every member reads it. It asks the host,
//! which certified it and holds the workspace's keys, for exactly that much:
//!
//! - [`MEDIA_KEYS`]: the media keys of its own streams on the workspace's
//!   live plane — its presence, its windows and its pages — at the epoch the
//!   media channel is at, derived for it alone and sealed to its device key.
//!   They seal what it publishes and open nothing: not another device's
//!   stream, and no stream of its own it was not handed. Its plane asks again
//!   whenever it joins, so keys that a membership change made stale are
//!   replaced on the plane's next join.
//! - [`SHARE_WINDOW`] and [`END_WINDOW_SHARE`]: share one of its own windows
//!   with the workspace's members ([`WindowShare`]), or end that share. The
//!   host writes the grants as the workspace's governor, as it writes a web
//!   page's share, once it has checked the window is one the asking device
//!   minted: a confined device never writes the workspace's governance
//!   itself, and never shares a window that is not its own.
//!
//! Only a device the host placed in a VM asks these, and only of the
//! workspace it was placed in; anyone else is refused.

use serde::{Deserialize, Serialize};

use super::RemoteActionTarget;
use super::browser::PROVIDER;
use crate::door::ActionAddress;

/// The resource these are actions of: the asking device itself, as the
/// workspace's host knows it.
pub const RESOURCE: &str = "environment.device";
/// The media keys of the asking device's own streams.
pub const MEDIA_KEYS: &str = "media-keys";
/// Share one of the asking device's windows with the workspace.
pub const SHARE_WINDOW: &str = "share-window";
/// End the share of one of the asking device's windows.
pub const END_WINDOW_SHARE: &str = "end-window-share";

/// What [`MEDIA_KEYS`] answers: one sender's media keys
/// (`channel_crypto::SenderMediaKeys`), sealed to its device key, in their
/// wire encoding as hex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaKeys {
    pub sealed: String,
}

/// The window [`SHARE_WINDOW`] and [`END_WINDOW_SHARE`] name: the app it is
/// a window of, and its tab, as the device's desk minted it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowShare {
    pub app: String,
    pub tab: u64,
}

/// Where the device sends `action`: the workspace's host, under the
/// environment's provider.
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// Whether `target` is one of these actions, and which.
#[must_use]
pub fn action_of(target: &RemoteActionTarget) -> Option<&str> {
    (target.provider == PROVIDER && target.address.resource == RESOURCE)
        .then_some(target.address.action.as_str())
        .filter(|action| [MEDIA_KEYS, SHARE_WINDOW, END_WINDOW_SHARE].contains(action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_its_own_actions_are_named_as_its() {
        assert_eq!(action_of(&target(MEDIA_KEYS)), Some(MEDIA_KEYS));
        assert_eq!(action_of(&target(SHARE_WINDOW)), Some(SHARE_WINDOW));
        assert_eq!(action_of(&target("open-browser")), None);
        let other = RemoteActionTarget {
            provider: "record".to_owned(),
            address: ActionAddress::new(RESOURCE, MEDIA_KEYS),
        };
        assert_eq!(action_of(&other), None);
    }
}
