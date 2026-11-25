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
//!   live session — its presence, its windows and its pages — at the epoch the
//!   media channel is at, derived for it alone and sealed to its device key.
//!   They seal what it publishes and open nothing: not another device's
//!   stream, and no stream of its own it was not handed. Its action service asks again
//!   whenever it joins, so keys that a membership change made stale are
//!   replaced on the action service's next join.
//! - [`SHARE_WINDOW`] and [`END_WINDOW_SHARE`]: share one of its own windows
//!   with the workspace's members ([`WindowShare`]), or end that share. The
//!   host writes the grants as the workspace's administrator, as it writes a web
//!   page's share, once it has checked the window is one the asking device
//!   minted: a confined device never writes the workspace's governance
//!   itself, and never shares a window that is not its own.
//! - [`USE_ITEM`]: the text an item of a person's vault keeps for the site the device's page
//!   is at ([`ItemUse`]), for one fill of a field the server itself handed that device. It
//!   decides by the vault's own rule and the person's leave, and answers the text sealed to the
//!   device's key for that one fill and that one site ([`ItemUsed`]): nothing that carries the
//!   answer reads it, and the device learns nothing of an item it was not handed a fill for.
//!
//! Only a device the host placed in a VM asks these, and only of the
//! workspace it was placed in; anyone else is refused.

use serde::{Deserialize, Serialize};

use super::RemoteActionTarget;
use super::browser::PROVIDER;
use crate::api::ActionAddress;

/// The resource these are actions of: the asking device itself, as the
/// workspace's host knows it.
pub const RESOURCE: &str = "environment.device";
/// The media keys of the asking device's own streams.
pub const MEDIA_KEYS: &str = "media-keys";
/// Share one of the asking device's windows with the workspace.
pub const SHARE_WINDOW: &str = "share-window";
/// End the share of one of the asking device's windows.
pub const END_WINDOW_SHARE: &str = "end-window-share";
/// The text a vault item keeps for the site the asking device's page is at, for one fill
/// the server handed that device.
pub const USE_ITEM: &str = "use-item";

/// What [`MEDIA_KEYS`] answers: one sender's media keys
/// (`key_group::SenderMediaKeys`), sealed to its device key, in their
/// wire encoding as hex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaKeys {
    pub sealed: String,
}

/// The window [`SHARE_WINDOW`] and [`END_WINDOW_SHARE`] name: the app it is
/// a window of, and its tab, as the device's session minted it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowShare {
    pub app: String,
    pub tab: u64,
}

/// What [`USE_ITEM`] names: the page action that fills a field from a vault item by reference,
/// as the server handed it to the asking device, the item it names, and the site the device's
/// engine says its page is at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemUse {
    /// The page action that fills: an answer is for this one and no other.
    pub invocation: crate::InvocationId,
    /// The item, by its reference.
    pub item: String,
    /// The site the page is at: its origin, as the engine committed it.
    pub origin: String,
}

/// What [`USE_ITEM`] answers. A refusal is the ask's own: the item is not released there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum ItemUsed {
    /// The text, sealed to the asking device's key for that page action and that site, in
    /// hex, and the kind of field it is for, in the engine's word: `password`, `code` or
    /// `value`.
    Released { sealed: String, into: String },
    /// The person is asked first and nothing was released: what whoever asked is told.
    Asked { words: String },
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
        .filter(|action| [MEDIA_KEYS, SHARE_WINDOW, END_WINDOW_SHARE, USE_ITEM].contains(action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_its_own_actions_are_named_as_its() {
        assert_eq!(action_of(&target(MEDIA_KEYS)), Some(MEDIA_KEYS));
        assert_eq!(action_of(&target(SHARE_WINDOW)), Some(SHARE_WINDOW));
        assert_eq!(action_of(&target(USE_ITEM)), Some(USE_ITEM));
        assert_eq!(action_of(&target("open-browser")), None);
        let other = RemoteActionTarget {
            provider: "record".to_owned(),
            address: ActionAddress::new(RESOURCE, MEDIA_KEYS),
        };
        assert_eq!(action_of(&other), None);
    }
}
