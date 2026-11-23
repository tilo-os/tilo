//! Workspace settings: what the sheet shows of the workspace in front, and what the person's
//! choices there change.

use tilo_session_core::desktop::workspace_settings as domain;

#[cfg(target_os = "linux")]
mod agents;
#[cfg(target_os = "linux")]
mod emoji;
#[cfg(target_os = "linux")]
mod shell;

#[cfg(target_os = "linux")]
pub(crate) use shell::*;

/// This machine's applications, as the workspace settings name them.
#[derive(Default)]
pub(crate) struct Directory {
    /// What the apps publish, where this device opens any: the catalogue that names their actions
    /// as the sheet asking to allow one does.
    actions: Option<std::rc::Rc<dyn crate::search::AppActions>>,
    /// The workspace whose published actions name what its grants are for.
    workspace: String,
}

impl domain::AppDirectory for Directory {
    fn app_display_name(&self, app_id: &str) -> Option<String> {
        crate::app_names::listed_name(app_id)
    }

    fn action_display_name(&self, app_id: &str, action: &str) -> Option<String> {
        self.actions
            .as_ref()?
            .published(&self.workspace)
            .into_iter()
            .filter(|published| published.app == app_id)
            .flat_map(|published| published.actions)
            .find(|descriptor| descriptor.address.action == action)
            .map(|descriptor| descriptor.action_label)
            .filter(|label| !label.trim().is_empty())
    }

    /// The packages installed on this computer; else, and on a page, which keeps none of its own,
    /// the fleet's apps that run here: on a page those its workspace's server serves it.
    fn installed_apps(&self) -> Vec<String> {
        #[cfg(target_os = "linux")]
        if let Some(lists) = crate::app_icons::app_lists()
            && !lists.named.is_empty()
        {
            return lists.named.iter().map(|app| app.id.clone()).collect();
        }
        let here: &[&str] = if cfg!(target_arch = "wasm32") {
            &["web"]
        } else {
            &["linux", "macos", "tilo-os"]
        };
        tilo_desktop_search::availability::catalog()
            .iter()
            .filter(|app| app.platforms.iter().any(|platform| here.contains(platform)))
            .map(|app| app.id.to_owned())
            .collect()
    }
}

/// The shared document permission controls used by native and hosted settings.
#[must_use]
pub fn document_access_pane(
    mut routes: Vec<(String, String)>,
    rules: Vec<collab_types::DocumentAccessRule>,
    mut recipients: Vec<tilo_sheets::workspace_settings::Choice>,
) -> tilo_sheets::workspace_settings::DocumentAccess {
    use sheet::Choice;
    use tilo_session_core::desktop::i18n::t;
    use tilo_sheets::workspace_settings as sheet;
    routes = routes
        .into_iter()
        .filter_map(|(app, document)| {
            if app.is_empty() {
                Some((app, document))
            } else {
                let document = collab_types::document_access_target(&app, &document)?.to_owned();
                Some((app, document))
            }
        })
        .collect();
    routes.sort();
    routes.dedup();
    let documents = routes
        .into_iter()
        .map(|(app, document)| {
            if app.is_empty() {
                let label = if document.is_empty() {
                    t("workspace-settings-access-all-apps")
                } else {
                    let name = crate::app_names::listed_name(&document)
                        .unwrap_or_else(|| document.clone());
                    tilo_session_core::desktop::i18n::t_args(
                        "workspace-settings-access-app-default",
                        &[("app", name.into())],
                    )
                };
                return ((app, document), label);
            }
            let name = crate::app_names::listed_name(&app).unwrap_or_else(|| app.clone());
            let label = if document.is_empty()
                || (app == fs_protocol::tree::FILES_APP
                    && document == fs_protocol::tree::document_id())
            {
                name
            } else {
                format!("{name}: {document}")
            };
            ((app, document), label)
        })
        .collect();
    recipients.insert(
        0,
        Choice {
            key: "everyone".into(),
            label: t("workspace-settings-document-everyone"),
        },
    );
    let levels = [
        ("inherit", "workspace-settings-document-inherit"),
        ("no-access", "workspace-settings-document-no-access"),
        ("view", "workspace-settings-document-view"),
        ("edit", "workspace-settings-document-edit"),
    ]
    .into_iter()
    .map(|(key, label)| Choice {
        key: key.into(),
        label: t(label),
    })
    .collect();
    let mut by_document = std::collections::BTreeMap::<_, std::collections::BTreeMap<_, _>>::new();
    for rule in rules {
        by_document
            .entry((rule.app, rule.document))
            .or_default()
            .insert(rule.recipient, rule.access);
    }
    sheet::DocumentAccess {
        title: t("workspace-settings-document-access"),
        documents,
        rules: by_document,
        recipients,
        levels,
        document_label: t("workspace-settings-document-label"),
        access_label: t("workspace-settings-document-access"),
        apply: t("workspace-settings-document-apply"),
        empty: t("workspace-settings-document-empty"),
    }
}
