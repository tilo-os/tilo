//! Key scopes: the reader set a storage unit's content is sealed under.
//!
//! A storage unit — one record, one part of an app's chain — is read by a set
//! of devices, its *reader set*: the workspace's participants, or a narrower
//! set when the policy narrows a record (a person's own record, a Read Deny
//! on a record or its app). A unit seals under the content keys of its scope,
//! one MLS group per reader set, so a membership or device change costs one
//! commit per scope instead of one per unit: a workspace of a thousand
//! records is one group, not a thousand.
//!
//! Only the key comes from the scope. The seal context keeps naming the unit
//! ([`crate::document_channel_id`]), so content stays bound to the record and
//! artifact it was written for, and every stored and wire format that names a
//! unit's channel is unchanged.
//!
//! A scope id is derived from the workspace and the reader set in a domain of
//! its own, so it can never equal a record's channel id. A unit keeps its own
//! channel as its scope when a Read Deny narrows its readers: that group is a
//! reader set of its own, and its keys never reach a device the record's
//! policy excludes.
//!
//! Units sealed before key scopes keep their own groups, and their content
//! keys are handed to the scope that carries them now, so every scope member
//! reads their history whether or not it ever held the old group. A unit a
//! Read Deny narrows is never handed to the broader scope.

use crate::channel::ChannelId;
use crate::op::derived_uuid;

/// Domain of the key-scope derivation, separate from a record channel's.
const KEY_SCOPE_DOMAIN: &[u8] = b"tilo-key-scope-v1\0";

/// The reader set every unit of a workspace shares while no Deny narrows it:
/// the workspace's participants.
pub const WORKSPACE_READERS: &str = "workspace";

/// The id of the key scope that carries the units of `workspace` whose
/// readers are the workspace's participants: one MLS group and one content
/// keyring for all of them.
#[must_use]
pub fn workspace_scope(workspace: &str) -> ChannelId {
    ChannelId(derived_uuid(
        KEY_SCOPE_DOMAIN,
        &[workspace, WORKSPACE_READERS],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_channel_id;

    #[test]
    fn a_workspace_has_one_scope_whatever_its_units() {
        let workspace = "1f0f2e26-2a9a-4d2c-9b0f-1c8f2c0a9a10";
        assert_eq!(workspace_scope(workspace), workspace_scope(workspace));
        assert_ne!(workspace_scope(workspace), workspace_scope("another"));
    }

    #[test]
    fn a_scope_is_never_a_units_channel() {
        let workspace = "1f0f2e26-2a9a-4d2c-9b0f-1c8f2c0a9a10";
        let scope = workspace_scope(workspace);
        for (app, record) in [
            ("so.tilo.notes", "notes"),
            ("so.tilo.chat", "messages"),
            ("so.tilo.media", WORKSPACE_READERS),
        ] {
            assert_ne!(scope, document_channel_id(app, workspace, record));
        }
    }
}
