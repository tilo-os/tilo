//! What an application publishes, and the record of a workspace it acts on.
//!
//! A surface is the whole of one app's actions on one record of one workspace, and
//! [`canonical_document`] is the one spelling of that record. The action itself is
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

    /// The resource `action` is published on now for a caller that named it
    /// on `resource`: `resource` itself, or, where a window's component
    /// published the same app resource again under a new alias (it is
    /// mounted anew as the window settles), the one alias carrying `action`
    /// for it now. `None` when nothing here carries it.
    #[must_use]
    pub fn current_resource<'a>(&'a self, resource: &'a str, action: &str) -> Option<&'a str> {
        let carries = |descriptor: &&PublishedAction| descriptor.action == action;
        if self
            .actions
            .iter()
            .filter(carries)
            .any(|descriptor| descriptor.resource == resource)
        {
            return Some(resource);
        }
        if !resource.starts_with(tilo_actions::BOUND_RESOURCE_PREFIX) {
            return None;
        }
        let logical = tilo_actions::selection::logical_resource_id(resource);
        let mut now = self.actions.iter().filter(carries).filter(|descriptor| {
            descriptor
                .resource
                .starts_with(tilo_actions::BOUND_RESOURCE_PREFIX)
                && tilo_actions::selection::logical_resource_id(&descriptor.resource) == logical
        });
        match (now.next(), now.next()) {
            (Some(only), None) => Some(only.resource.as_str()),
            _ => None,
        }
    }
}

/// Spelling of the package-primary record (route record `""`).
const PRIMARY_DOCUMENT_ALIAS: &str = "@tilo:package-primary";
const MAX_DOCUMENT_ID_BYTES: usize = 128;

/// The one canonical spelling of a live route record: `""` is the
/// package-primary record, reserved spellings are never literal record ids,
/// and everything else must be bounded, trimmed, printable ASCII.
pub fn canonical_document(route_document: &str) -> Result<String, String> {
    if route_document.is_empty() {
        return Ok(PRIMARY_DOCUMENT_ALIAS.into());
    }
    let reserved = route_document.starts_with("@tilo:") || route_document.starts_with('*');
    if reserved
        || route_document.len() > MAX_DOCUMENT_ID_BYTES
        || !route_document.is_ascii()
        || route_document.trim() != route_document
        || route_document.bytes().any(|b| b.is_ascii_control())
    {
        return Err("reserved or invalid route record".into());
    }
    Ok(route_document.into())
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

/// The refusal of a published surface, as the reason a caller is told.
fn failed(reason: String) -> String {
    reason
}

fn validate_view_action_binding(action: &PublishedAction, role: &str) -> Result<(), String> {
    if action.operation_kind != tilo_actions::OperationKind::View {
        return Err(failed(format!(
            "resource view {role} action must be a view operation"
        )));
    }
    if action.contract.confirmation != tilo_actions::ConfirmationPolicy::None {
        return Err(failed(format!(
            "resource view {role} action must not require confirmation"
        )));
    }
    if action.params.iter().any(|parameter| parameter.required) {
        return Err(failed(format!(
            "resource view {role} action must not require caller parameters"
        )));
    }
    Ok(())
}

fn validate_resource_range_binding(
    actions: &[PublishedAction],
    action: &PublishedAction,
    range: &tilo_actions::ResourceViewRangeSpec,
) -> Result<(), String> {
    let names = [&range.start_param, &range.end_param, &range.unit_param];
    let target = actions
        .iter()
        .find(|target| target.resource == action.resource && target.action == range.action)
        .ok_or_else(|| failed("resource range action missing".into()))?;
    if names.iter().any(|name| name.is_empty())
        || names[0] == names[1]
        || names[0] == names[2]
        || names[1] == names[2]
        || target.operation_kind != tilo_actions::OperationKind::View
        || target.contract.confirmation != tilo_actions::ConfirmationPolicy::None
    {
        return Err(failed("invalid resource range binding".into()));
    }
    for (name, ty) in [
        (&range.start_param, tilo_actions::ParamType::Integer),
        (&range.end_param, tilo_actions::ParamType::Integer),
        (&range.unit_param, tilo_actions::ParamType::Text),
    ] {
        if !target
            .params
            .iter()
            .any(|p| p.name.as_ref() == name && p.ty == ty)
        {
            return Err(failed("invalid resource range parameter".into()));
        }
    }
    if target
        .params
        .iter()
        .any(|p| p.required && !names.iter().any(|n| p.name.as_ref() == *n))
    {
        return Err(failed("resource range requires unrelated input".into()));
    }
    Ok(())
}

/// A published surface as the broker takes it: the actions it offers, and why each row it left
/// out was refused.
#[derive(Debug)]
pub struct ValidSurface {
    pub actions: Vec<PublishedAction>,
    /// One reason per row refused, naming its address.
    pub refused: Vec<String>,
}

/// The checks every published surface passes, whichever request carried it:
/// addresses non-empty, unique and separator-free, kinds and labels present,
/// contracts and parameters valid, resource views consistent.
///
/// An address that breaks those rules refuses the whole surface: the table that carried it cannot
/// be trusted to say which row is which. A row that is only incomplete (no kind or label, a contract
/// or a parameter that does not hold) is left out alone, so one unnamed resource does not silence
/// every other action its app offers.
pub fn validate_published_surface(rows: Vec<PublishedAction>) -> Result<ValidSurface, String> {
    let mut seen = std::collections::HashSet::with_capacity(rows.len());
    let mut actions = Vec::with_capacity(rows.len());
    let mut refused = Vec::new();
    for published in rows {
        if published.resource.is_empty()
            || published.action.is_empty()
            || published.resource.contains(tilo_actions::KEY_SEP)
            || published.action.contains(tilo_actions::KEY_SEP)
            || !seen.insert((published.resource.clone(), published.action.clone()))
        {
            return Err(failed(
                "exported action addresses must be non-empty, unique, and contain no separator"
                    .into(),
            ));
        }
        match row_problem(&published) {
            Some(problem) => refused.push(format!(
                "`{}#{}`: {problem}",
                published.resource, published.action
            )),
            None => actions.push(published),
        }
    }
    validate_resource_view_descriptors(&actions)?;
    Ok(ValidSurface { actions, refused })
}

/// What keeps `published`, a row with a sound address, from being offered: `None` when nothing.
fn row_problem(published: &PublishedAction) -> Option<String> {
    if published.resource_kind.trim().is_empty()
        || published.resource_label.trim().is_empty()
        || published.action_label.trim().is_empty()
    {
        return Some(format!(
            "exported actions require non-empty kind and labels: kind {:?}, label {:?}, action \
             label {:?}",
            published.resource_kind, published.resource_label, published.action_label
        ));
    }
    if let Some(reason) = published.contract.validate_declaration().into_iter().next() {
        return Some(format!("invalid exported action contract: {reason}"));
    }
    let mut parameter_names = std::collections::HashSet::new();
    for parameter in &published.params {
        if !parameter_names.insert(parameter.name.as_ref()) {
            return Some("exported action parameter names must be unique".into());
        }
        if let Some(reason) = parameter.validate_declaration().into_iter().next() {
            return Some(format!(
                "invalid exported action parameter `{}`: {reason}",
                parameter.name
            ));
        }
    }
    None
}

/// Recheck the resource-view invariants after decoding an untrusted action
/// surface. A view with a mount must carry that action in the same atomic
/// publication/page so the broker can prove the binding before exposing it.
pub fn validate_resource_view_descriptors(actions: &[PublishedAction]) -> Result<(), String> {
    #[derive(Default)]
    struct ResourceViews<'a> {
        ids: std::collections::HashSet<&'a str>,
        open_actions: std::collections::HashSet<&'a str>,
        primary_count: usize,
    }

    let mut resources = std::collections::HashMap::<&str, ResourceViews<'_>>::new();
    for action in actions {
        let Some(view) = &action.view else {
            continue;
        };
        if view.id.is_empty() || view.id.contains(tilo_actions::KEY_SEP) {
            return Err(failed(
                "resource view id must be non-empty and contain no separator".into(),
            ));
        }
        if view.label.trim().is_empty() {
            return Err(failed("resource view label must be non-empty".into()));
        }
        if view.open_action != action.action {
            return Err(failed(
                "resource view must be attached to its open action".into(),
            ));
        }
        validate_view_action_binding(action, "open")?;

        let seen = resources.entry(&action.resource).or_default();
        if !seen.ids.insert(&view.id) {
            return Err(failed(
                "resource view ids must be unique per resource".into(),
            ));
        }
        if !seen.open_actions.insert(&view.open_action) {
            return Err(failed(
                "at most one resource view may reference an open action".into(),
            ));
        }
        if view.primary {
            seen.primary_count = seen.primary_count.saturating_add(1);
            if seen.primary_count > 1 {
                return Err(failed("at most one resource view may be primary".into()));
            }
        }

        if let Some(range) = &view.range {
            validate_resource_range_binding(actions, action, range)?;
        }

        let Some(mount) = &view.mount else {
            continue;
        };
        if mount.action.trim().is_empty()
            || mount.capability_param.trim().is_empty()
            || mount.view_param.trim().is_empty()
            || mount.capability_param == mount.view_param
        {
            return Err(failed(
                "resource view mount requires an action and distinct parameter names".into(),
            ));
        }
        let mount_action = actions.iter().find(|candidate| {
            candidate.resource == action.resource && candidate.action == mount.action
        });
        let Some(mount_action) = mount_action else {
            return Err(failed(
                "resource view mount action is not exported by its resource".into(),
            ));
        };
        validate_view_action_binding(mount_action, "mount")?;
        for (role, name) in [
            ("capability", mount.capability_param.as_str()),
            ("view", mount.view_param.as_str()),
        ] {
            if !mount_action.params.iter().any(|parameter| {
                parameter.name.as_ref() == name
                    && parameter.ty == tilo_actions::ParamType::Text
                    && !parameter.required
            }) {
                return Err(failed(format!(
                    "resource view mount {role} parameter `{name}` must name an optional text parameter"
                )));
            }
        }
    }
    Ok(())
}
