//! The apps a workspace has installed, as `tilo_ls` lists them, and the workspace's folder.

use std::fmt::Write as _;

use super::{ActionProvider, Failure, McpServer, Reply, Value, app_names, json};
use tilo_workspace_records::search::FOUND_FILE;

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
        let (_, running) = self.tree.providers().await?;
        let mut lines: Vec<String> = Vec::new();
        let mut rows: Vec<Value> = Vec::new();
        let running_ids = running_app_ids(&running);
        let (installed, unread) = match self.tree.installed().await {
            Ok(installed) => (installed, None),
            Err(reason) => (Vec::new(), Some(reason)),
        };
        // What each app holds rows of: a document this is served of holds
        // what the app has, the workspace's own folder holds its files, and
        // a service holds only windows. An app is no resource of its own.
        // Who works in which app's window now, as their presence says: the window a person has
        // in front of them is the one they mean by "this".
        let here = self.host.here(&self.caller()).unwrap_or_default();
        let mut listed: Vec<(String, &str, String, bool, Vec<String>)> = Vec::new();
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
            let workers = if windowed {
                people_working_in(&here, app_id)
            } else {
                Vec::new()
            };
            let state = running_state(windowed, &workers);
            let name = app_names(std::slice::from_ref(app_id)).join("");
            let holds = windowed || super::names_files(app_id);
            listed.push((name, label, state, holds, workers));
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
                let state = state.to_owned();
                let name = app_names(std::slice::from_ref(&app.app_id)).join("");
                // Installing starts a document; a walk never makes one.
                let holds = super::names_files(&app.app_id);
                listed.push((name, &app.label, state, holds, Vec::new()));
            }
        }
        for (name, label, state, holds, workers) in listed {
            lines.push(format!("{name}  {label}  {state}"));
            rows.push(json!({
                "works_in_it": workers,
                "path": name,
                "name": name,
                "label": label,
                "kind": "app",
                "state": state,
                "has_children": holds,
                "reads": false,
                "actions": [],
            }));
        }
        let total = lines.len();
        let mut text = String::from("apps");
        for line in lines.into_iter().skip(offset).take(limit) {
            text.push('\n');
            text.push_str(&line);
        }
        // A page is one page of the listing typed as it is in text: rows past
        // the cursor's offset belong to the next page, not this one.
        rows = rows.into_iter().skip(offset).take(limit).collect();
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
                reason.words()
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
    /// it, its files and folders by their paths (a file with Files' `read-text` on it, a folder
    /// with its `list`); not the apps it offers to open beside it. The folder is the
    /// workspace's, wherever its tree is kept, so it is listed whether or not Files is open
    /// anywhere.
    pub(super) async fn files_resolved(
        &self,
        parsed: &super::Address,
        forms: &super::ProgramForms,
        reaching: usize,
    ) -> Result<super::Resolved, String> {
        let mut resolved = self.folder_resolved(parsed, forms, reaching).await?;
        resolved.children.retain(|node| {
            node.resource_type == FOUND_FILE || node.resource_type == fs_protocol::tree::FOLDER_KIND
        });
        Ok(resolved)
    }

    /// `parsed` resolved in the workspace's own folder, wherever its tree is kept — never in a
    /// window of Files, whose rows are that device's own folder: the workspace's files and
    /// folders are the rows the workspace's listing offers, and its reads are answered by
    /// whoever keeps the tree. `reaching` is how many of the folder's entries to read for a
    /// listing's page and one past it ([`super::McpServer::files_resolved`]), or every one.
    pub(super) async fn folder_resolved(
        &self,
        parsed: &super::Address,
        forms: &super::ProgramForms,
        reaching: usize,
    ) -> Result<super::Resolved, String> {
        self.tree
            .resolve_in(
                parsed,
                fs_protocol::tree::FILES_APP.to_owned(),
                tilo_actions::remote::workspace_provider(&self.workspace),
                true,
                Some(forms),
                &mut super::Levels::new(),
                reaching,
            )
            .await
            .map_err(String::from)
    }
}

/// Each running app once, in the order the providers serve them.
fn running_app_ids(running: &[(String, Value)]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for (_, app) in running {
        if let Some(app_id) = app["app_id"].as_str()
            && !ids.iter().any(|seen| seen == app_id)
        {
            ids.push(app_id.to_owned());
        }
    }
    ids
}

/// The people whose focused window is one of `app_id`'s, by name.
fn people_working_in(here: &[tilo_actions::remote::Here], app_id: &str) -> Vec<String> {
    here.iter()
        .filter(|member| member.kind == "person")
        .filter(|member| {
            member
                .works_in
                .as_ref()
                .is_some_and(|window| window.app == app_id)
        })
        .map(|member| member.name.clone())
        .collect()
}

/// A running app's state: a service has no window, and a windowed app names who works in it.
fn running_state(windowed: bool, workers: &[String]) -> String {
    match (windowed, workers) {
        (false, _) => "service".to_owned(),
        (true, []) => "running".to_owned(),
        (true, names) => format!("running, {} works in it", names.join(" and ")),
    }
}
