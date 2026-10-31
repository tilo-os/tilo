//! Which app opens a kind of file or link: the workspace's choice and each
//! person's own, in the words the Apps & Files tab uses.

use tilo_desktop_model::{UserId, Workspace};
use tilo_sort::CompactSort as _;

use crate::i18n::{t, t_args};

/// The row label for a resource kind in the desktop's language.
pub(crate) fn resource_kind_label(resource: &desktop_policy::ResourceKind) -> String {
    if let Some(ext) = resource.extension_name() {
        t_args(
            "workspace-settings-resource-extension",
            &[("ext", ext.into())],
        )
    } else if let Some(scheme) = resource.scheme_name() {
        t_args(
            "workspace-settings-resource-scheme",
            &[("scheme", scheme.into())],
        )
    } else {
        resource.display_label()
    }
}

pub fn next_workspace_default(
    current: Option<&desktop_policy::WorkspaceDefaultPolicy>,
    resource: &desktop_policy::ResourceKind,
    candidates: &[desktop_policy::AppCandidate],
) -> desktop_policy::WorkspaceDefaultPolicy {
    let apps: Vec<_> = candidates.iter().filter(|a| a.handles(resource)).collect();
    match current.unwrap_or(&desktop_policy::WorkspaceDefaultPolicy::FollowPersonal) {
        desktop_policy::WorkspaceDefaultPolicy::FollowPersonal => apps.first().map_or(
            desktop_policy::WorkspaceDefaultPolicy::FollowPersonal,
            |a| desktop_policy::WorkspaceDefaultPolicy::App(a.package_id.clone()),
        ),
        desktop_policy::WorkspaceDefaultPolicy::App(id) => apps
            .iter()
            .position(|a| &a.package_id == id)
            .and_then(|i| apps.get(i.saturating_add(1)))
            .map_or(
                desktop_policy::WorkspaceDefaultPolicy::FollowPersonal,
                |a| desktop_policy::WorkspaceDefaultPolicy::App(a.package_id.clone()),
            ),
    }
}

pub fn next_user_default(
    current: Option<&desktop_policy::UserWorkspaceChoice>,
    resource: &desktop_policy::ResourceKind,
    candidates: &[desktop_policy::AppCandidate],
) -> desktop_policy::UserWorkspaceChoice {
    let apps: Vec<_> = candidates.iter().filter(|a| a.handles(resource)).collect();
    match current.unwrap_or(&desktop_policy::UserWorkspaceChoice::FollowWorkspace) {
        desktop_policy::UserWorkspaceChoice::FollowWorkspace => {
            desktop_policy::UserWorkspaceChoice::PersonalDefault
        }
        desktop_policy::UserWorkspaceChoice::PersonalDefault => apps
            .first()
            .map_or(desktop_policy::UserWorkspaceChoice::FollowWorkspace, |a| {
                desktop_policy::UserWorkspaceChoice::App(a.package_id.clone())
            }),
        desktop_policy::UserWorkspaceChoice::App(id) => apps
            .iter()
            .position(|a| &a.package_id == id)
            .and_then(|i| apps.get(i.saturating_add(1)))
            .map_or(desktop_policy::UserWorkspaceChoice::FollowWorkspace, |a| {
                desktop_policy::UserWorkspaceChoice::App(a.package_id.clone())
            }),
    }
}

/// One kind of file or link, and both choices for it as the person reads
/// them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultAppChoice {
    pub resource: desktop_policy::ResourceKind,
    /// The kind named in the person's words.
    pub label: String,
    /// The workspace's choice: the app it names, or that it follows each
    /// person's own.
    pub workspace: String,
    /// The person's own choice.
    pub yours: String,
}

/// Every kind of file and link some app opens here, with the workspace's
/// choice and `local_user`'s own, in the tab's order.
#[must_use]
pub fn default_app_choices(
    ws: &Workspace,
    local_user: &UserId,
    candidates: &[desktop_policy::AppCandidate],
) -> Vec<DefaultAppChoice> {
    let mut resources: Vec<_> = candidates
        .iter()
        .flat_map(|candidate| candidate.resources.iter().cloned())
        .collect();
    resources.compact_sort();
    resources.dedup();
    resources
        .into_iter()
        .map(|resource| {
            let (workspace, yours) = default_app_labels(ws, local_user, &resource, candidates);
            DefaultAppChoice {
                label: resource_kind_label(&resource),
                resource,
                workspace,
                yours,
            }
        })
        .collect()
}

/// Both choices for one kind, as the person reads them.
pub(crate) fn default_app_labels(
    ws: &Workspace,
    local_user: &UserId,
    resource: &desktop_policy::ResourceKind,
    candidates: &[desktop_policy::AppCandidate],
) -> (String, String) {
    let policy = ws
        .default_apps
        .policy
        .get(resource)
        .unwrap_or(&desktop_policy::WorkspaceDefaultPolicy::FollowPersonal);
    let workspace = match policy {
        desktop_policy::WorkspaceDefaultPolicy::FollowPersonal => {
            t("workspace-settings-follow-personal")
        }
        desktop_policy::WorkspaceDefaultPolicy::App(id) => app_name(id, candidates),
    };
    let choice = ws
        .default_apps
        .user_choices
        .get(local_user)
        .and_then(|choices| choices.get(resource))
        .unwrap_or(&desktop_policy::UserWorkspaceChoice::FollowWorkspace);
    let yours = match choice {
        desktop_policy::UserWorkspaceChoice::FollowWorkspace => {
            t("workspace-settings-use-workspace-choice")
        }
        desktop_policy::UserWorkspaceChoice::PersonalDefault => {
            t("workspace-settings-use-personal-default")
        }
        desktop_policy::UserWorkspaceChoice::App(id) => app_name(id, candidates),
    };
    (workspace, yours)
}

/// What the tab's introduction says of the workspace's own choices.
#[must_use]
pub fn default_apps_status(ws: &Workspace, candidates: &[desktop_policy::AppCandidate]) -> String {
    let custom_rules = ws.default_apps.policy.len();
    if custom_rules == 0 {
        let resources = candidates
            .iter()
            .flat_map(|candidate| candidate.resources.iter())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        t_args(
            "workspace-settings-apps-file-types",
            &[("count", resources.into())],
        )
    } else {
        t_args(
            "workspace-settings-apps-custom-rules",
            &[("count", custom_rules.into())],
        )
    }
}

pub(crate) fn app_name(id: &str, candidates: &[desktop_policy::AppCandidate]) -> String {
    candidates.iter().find(|a| a.package_id == id).map_or_else(
        || t("workspace-settings-unavailable-app"),
        |a| a.display_name.clone(),
    )
}
