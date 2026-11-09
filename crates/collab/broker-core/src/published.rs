//! What an application publishes, and the record of a workspace it acts on.
//!
//! A surface is the whole of one app's actions on one record of one workspace, and
//! [`canonical_record`] is the one spelling of that record. The action itself is
//! [`tilo_actions::published`]'s.

use tilo_action_policy::TreePath;
pub use tilo_actions::published::{PublishedAction, PublishedInput};

use crate::route::CrdtRoute;

/// A live application's complete, level-triggered action surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedSurface {
    pub peer_id: u64,
    pub app_id: String,
    pub route: CrdtRoute,
    pub actions: Vec<PublishedAction>,
    /// False for a catalogue retained after the app closes: what it last
    /// published, offered so invoking it launches the app.
    pub live: bool,
}

impl PublishedSurface {
    /// Copy only the selected descriptor and its routing metadata.
    pub fn action_projection(
        &self,
        resource: &str,
        action: &str,
    ) -> Option<(Self, PublishedAction)> {
        let descriptor = self
            .actions
            .iter()
            .find(|descriptor| descriptor.resource == resource && descriptor.action == action)?;
        Some((
            Self {
                peer_id: self.peer_id,
                app_id: self.app_id.clone(),
                route: self.route.clone(),
                actions: Vec::new(),
                live: self.live,
            },
            descriptor.clone(),
        ))
    }
}

/// Spelling of the package-primary record (route record `""`).
const PRIMARY_RECORD_ALIAS: &str = "@tilo:package-primary";
const MAX_RECORD_ID_BYTES: usize = 128;

/// The one canonical spelling of a live route record: `""` is the
/// package-primary record, reserved spellings are never literal record ids,
/// and everything else must be bounded, trimmed, printable ASCII.
pub fn canonical_record(route_record: &str) -> Result<String, String> {
    if route_record.is_empty() {
        return Ok(PRIMARY_RECORD_ALIAS.into());
    }
    let reserved = route_record.starts_with("@tilo:") || route_record.starts_with('*');
    if reserved
        || route_record.len() > MAX_RECORD_ID_BYTES
        || !route_record.is_ascii()
        || route_record.trim() != route_record
        || route_record.bytes().any(|b| b.is_ascii_control())
    {
        return Err("reserved or invalid route record".into());
    }
    Ok(route_record.into())
}

/// The node of `action` published by `app` in `workspace`.
pub fn published_node(
    workspace: &tilo_desktop_model::Workspace,
    app: &str,
    action: &PublishedAction,
) -> TreePath {
    tilo_actions::action_node(
        &workspace.node(),
        app,
        &action.resource_kind,
        &action.resource,
        action.contract.field.as_deref(),
        &action.action,
    )
}
