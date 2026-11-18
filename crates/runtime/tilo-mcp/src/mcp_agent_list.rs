//! The apps a workspace has installed, as `tilo_ls` lists them, and the workspace's folder.

use std::fmt::Write as _;

use super::{ActionProvider, Failure, McpServer, Reply, Value, app_names, json};

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// The apps: running ones first, then the installed ones to open. An
    /// installed list that could not be read is said, with why: read as
    /// empty it would tell the agent there are no apps.
    pub(super) async fn list_apps(
        &self,
        offset: usize,
        limit: usize,
        structured: bool,
    ) -> Result<Reply, Failure> {
        let (_, running) = self.providers().await?;
        let mut lines: Vec<String> = Vec::new();
        let mut rows: Vec<Value> = Vec::new();
        let mut running_ids: Vec<String> = Vec::new();
        for (_, app) in &running {
            let Some(app_id) = app["app_id"].as_str() else {
                continue;
            };
            if !running_ids.iter().any(|seen| seen == app_id) {
                running_ids.push(app_id.to_owned());
            }
        }
        let (installed, unread) = match self.installed().await {
            Ok(installed) => (installed, None),
            Err(reason) => (Vec::new(), Some(reason)),
        };
        for app_id in &running_ids {
            let label = installed
                .iter()
                .find(|app| app.app_id == *app_id)
                .map_or("", |app| app.label.as_str());
            // A host service (the desktop's layout) runs with the session,
            // and has no window.
            let windowed = running.iter().any(|(provider, app)| {
                (provider.parse::<u64>().is_ok()
                    || tilo_actions::remote::is_resource_provider(provider))
                    && app["app_id"].as_str() == Some(app_id)
            });
            let name = app_names(std::slice::from_ref(app_id)).join("");
            let state = if windowed { "running" } else { "service" };
            lines.push(format!("{name}  {label}  {state}"));
            rows.push(json!({
                "path": name,
                "name": name,
                "label": label,
                "kind": "app",
                "state": state,
                "has_children": true,
                "actions": [],
            }));
        }
        let mut unopened = false;
        for app in &installed {
            if !running_ids.contains(&app.app_id) {
                let state = if app.opens {
                    "installed"
                } else {
                    unopened = true;
                    "installed, not yours to open"
                };
                let name = app_names(std::slice::from_ref(&app.app_id)).join("");
                lines.push(format!("{name}  {}  {state}", app.label));
                rows.push(json!({
                    "path": name,
                    "name": name,
                    "label": app.label,
                    "kind": "app",
                    "state": state,
                    "has_children": true,
                    "actions": [],
                }));
            }
        }
        let total = lines.len();
        let mut text = String::from("apps");
        for line in lines.into_iter().skip(offset).take(limit) {
            text.push('\n');
            text.push_str(&line);
        }
        let end = offset.saturating_add(limit);
        let next = (end < total).then(|| end.to_string());
        if let Some(next) = &next {
            let _ = write!(
                text,
                "\n… {} more: cursor \"{next}\"",
                total.saturating_sub(end)
            );
        }
        if let Some(reason) = unread {
            let _ = write!(
                text,
                "\nthe installed apps could not be read: {}",
                super::answer::words(&reason)
            );
        } else if total == 0 {
            text.push_str("\nnone: no app runs in this workspace and none is installed");
        }
        if unopened {
            text.push_str("\nthe person decides which apps an agent may open in this workspace");
        }
        let mut reply = Reply::ok(text);
        if structured {
            let mut answer = json!({"path":"apps","kind":"apps","actions":[],"rows":rows});
            if let (Some(fields), Some(next)) = (answer.as_object_mut(), next) {
                fields.insert("next_cursor".into(), json!(next));
            }
            reply.structured = Some(answer);
        }
        Ok(reply)
    }

    /// What `tilo_ls files` lists: the workspace's folder as the workspace's own listing offers
    /// it, its files by their paths, each with Files' `read-text` on it; not the apps it offers to
    /// open beside it. The folder is the workspace's, wherever its tree is kept, so it is listed
    /// whether or not Files is open anywhere.
    pub(super) async fn files_resolved(
        &self,
        parsed: &super::ParsedPath,
        forms: &super::ProgramForms,
    ) -> Result<super::Resolved, String> {
        let provider = tilo_actions::remote::workspace_provider(&self.workspace);
        let mut resolved = self
            .resolve_in(
                parsed,
                fs_protocol::tree::FILES_APP.to_owned(),
                provider,
                true,
                Some(forms),
            )
            .await?;
        resolved
            .children
            .retain(|node| node.kind == tilo_actions::remote::FOUND_FILE);
        Ok(resolved)
    }
}
