//! What a caller has in its workspace, said once where its work begins.
//!
//! A model that is told nothing looks before it works: which apps there are (`tilo_ls`), what
//! one of them offers (`tilo_ls notes`), what an action takes (`tilo_ls notes#rename`). Each
//! look is one more ask of the model, and on a large model an ask is many seconds. What those
//! looks answer is the same for every step of a turn and for the turns after it, so it is said
//! once, where the turn's instruction stands.
//!
//! Everything here is read through the caller's own listing, as the caller: the apps the
//! workspace lists to it, and for each app in use the actions that app offers it. Nothing is
//! told that `tilo_ls` would not answer the same caller, and an app the workspace does not let
//! it open is left out. What is told names kinds and actions and no title of anything an app
//! keeps, so it stays word for word the same until an app is installed, opened or changed.

use super::{
    ActionDescriptor, ActionProvider, InstalledApp, McpServer, Value, app_names, parse_path,
};
use std::fmt::Write as _;

/// The most kinds of thing in one app whose actions are told.
const KINDS_TOLD: usize = 3;
/// How much is told of every app in use, the fullest first: the most actions of an app or of a
/// kind of thing in it, and how many kinds beyond the first. The fullest that keeps within the
/// bound is told, so no app is left out for the length of another's line; with no actions at
/// all the apps are still named.
const HOW_FULL: [(usize, usize); 9] = [
    (32, 2),
    (32, 1),
    (32, 0),
    (24, 0),
    (16, 0),
    (12, 0),
    (8, 0),
    (4, 0),
    (0, 0),
];
/// What stands before everything told.
const HEAD: &str = "What you have here now, as tilo_ls would answer you (said once, so you need \
not look it up):\n";
/// What stands before the apps in use.
const IN_USE: &str = "Apps in use, each with what it offers you as tilo_do takes it \
(path `app#action`, the arguments in `args`); a name in brackets is an argument it must be \
given, `…` says it takes more, which tilo_ls of `app#action` names; where `more` stands or an \
app has no actions beside it, tilo_ls of the app names the others:\n";
/// What stands before the installed apps.
const INSTALLED: &str = "Installed and not started yet (tilo_do of `name#open` starts one and \
answers what it offers): ";

/// What one app in use offers a caller.
struct Offered {
    /// The app by the name a path takes, with the name people call it.
    named: String,
    /// The name a path takes.
    path: String,
    /// The actions on the app itself.
    own: Vec<ActionDescriptor>,
    /// The actions on each kind of thing it holds, a kind once.
    kinds: Vec<(String, Vec<ActionDescriptor>)>,
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// What this caller has in its workspace, in at most `most` bytes: the apps in use, each
    /// with the actions it offers the caller as a call writes them (`#rename(title)`), then
    /// the installed apps the caller may open, by name. Empty where nothing can be listed.
    pub async fn told_at_start(&self, most: usize) -> String {
        let Ok((_, providers)) = self.providers().await else {
            return String::new();
        };
        let installed = self.installed().await.unwrap_or_default();
        let in_use = apps_in_use(&providers);
        let mut apps: Vec<Offered> = Vec::new();
        for (name, app_id, provider) in &in_use {
            let listed = installed.iter().find(|app| app.app_id == *app_id);
            // An app the workspace names to this caller without letting it open it is not told.
            if listed.is_some_and(|app| !app.opens) {
                continue;
            }
            let label = listed.map_or("", |app| app.label.as_str());
            apps.extend(self.offered_by(name, app_id, provider, label).await);
        }
        let used = in_use_told(&apps, most);
        let cut = if used.is_empty() { apps.len() } else { 0 };
        put_together(most, &used, &to_start(&installed, &in_use), cut)
    }

    /// What one app in use offers this caller: the actions on the app itself and on each kind
    /// of thing it holds. `None` where its listing cannot be read now.
    async fn offered_by(
        &self,
        name: &str,
        app_id: &str,
        provider: &str,
        label: &str,
    ) -> Option<Offered> {
        let parsed = parse_path(name).ok()?;
        let found = self
            .resolve_published(&parsed, app_id.to_owned(), provider.to_owned(), true, None)
            .await
            .ok()?;
        let mut kinds: Vec<(String, Vec<ActionDescriptor>)> = Vec::new();
        for child in &found.children {
            if child.actions.is_empty()
                || kinds.iter().any(|(kind, _)| *kind == child.kind)
                || same_actions(&child.actions, &found.actions)
            {
                continue;
            }
            if kinds.len() == KINDS_TOLD {
                break;
            }
            kinds.push((child.kind.clone(), child.actions.clone()));
        }
        Some(Offered {
            named: named(name, label),
            path: name.to_owned(),
            own: found.actions,
            kinds,
        })
    }
}

/// Whether `one` and `other` are the same actions in the same order.
fn same_actions(one: &[ActionDescriptor], other: &[ActionDescriptor]) -> bool {
    one.len() == other.len()
        && one
            .iter()
            .zip(other)
            .all(|(a, b)| a.address.action == b.address.action)
}

/// The lines of the apps in use, as full as `most` bytes let them all be ([`HOW_FULL`]); empty
/// where even their names do not fit.
fn in_use_told(apps: &[Offered], most: usize) -> String {
    for (actions, kinds) in HOW_FULL {
        let lines: String = apps
            .iter()
            .map(|app| line_of(app, actions, kinds))
            .collect();
        let whole = HEAD
            .len()
            .saturating_add(IN_USE.len())
            .saturating_add(lines.len());
        if !lines.is_empty() && whole <= most {
            return lines;
        }
    }
    String::new()
}

/// One app's line: its name, at most `most` of the actions on it, and those of `kinds` more
/// kinds of thing in it. Where the app itself offers nothing, its first kind of thing stands
/// for it. With no actions at all, its name.
fn line_of(app: &Offered, most: usize, kinds: usize) -> String {
    let mut line = app.named.clone();
    if most > 0 {
        let mut held = app.kinds.iter();
        if app.own.is_empty() {
            if let Some((kind, actions)) = held.next() {
                each(&mut line, &app.path, kind, actions, most);
            }
        } else {
            let _ = write!(line, ": {}", actions_told(&app.own, most));
        }
        for (kind, actions) in held.take(kinds) {
            each(&mut line, &app.path, kind, actions, most);
        }
    }
    line.push('\n');
    line
}

/// What each thing of `kind` in the app at `path` offers, on a line of its own under the app's.
fn each(line: &mut String, path: &str, kind: &str, actions: &[ActionDescriptor], most: usize) {
    let kind = kind.rsplit('.').next().unwrap_or(kind);
    let _ = write!(
        line,
        "\n  each {kind} in it ({path}/<its name>): {}",
        actions_told(actions, most)
    );
}

/// Each app among `providers` once, by the name a path takes, with its id and the newest of
/// its providers (the last one listed), in the order of their names.
fn apps_in_use(providers: &[(String, Value)]) -> Vec<(String, String, String)> {
    let mut in_use: Vec<(String, String, String)> = Vec::new();
    for (provider, app) in providers {
        let Some(app_id) = app["app_id"].as_str() else {
            continue;
        };
        match in_use
            .iter_mut()
            .find(|(_, known, _)| known.as_str() == app_id)
        {
            Some((_, _, newest)) => newest.clone_from(provider),
            None => in_use.push((
                app_names(std::slice::from_ref(&app_id.to_owned())).join(""),
                app_id.to_owned(),
                provider.clone(),
            )),
        }
    }
    in_use.sort();
    in_use
}

/// The installed apps the caller may open that are not in use, each by the name a path takes.
fn to_start(installed: &[InstalledApp], in_use: &[(String, String, String)]) -> Vec<String> {
    let mut names: Vec<String> = installed
        .iter()
        .filter(|app| app.opens && !in_use.iter().any(|(_, known, _)| *known == app.app_id))
        .map(|app| {
            named(
                &app_names(std::slice::from_ref(&app.app_id)).join(""),
                &app.label,
            )
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Everything told, in at most `most` bytes: the lines `used` of the apps in use, then of
/// `names` as many as fit, and how many apps were left out, `cut` of them before.
fn put_together(most: usize, used: &str, names: &[String], mut cut: usize) -> String {
    let mut told = String::new();
    if !used.is_empty() {
        told.push_str(HEAD);
        told.push_str(IN_USE);
        told.push_str(used);
    }
    let head = if told.is_empty() { HEAD.len() } else { 0 };
    let mut started = String::new();
    for name in names {
        let between = if started.is_empty() { 0 } else { 2 };
        let whole = told
            .len()
            .saturating_add(head)
            .saturating_add(INSTALLED.len())
            .saturating_add(started.len())
            .saturating_add(between)
            .saturating_add(name.len())
            .saturating_add(1);
        if whole <= most {
            if !started.is_empty() {
                started.push_str(", ");
            }
            started.push_str(name);
        } else {
            cut = cut.saturating_add(1);
        }
    }
    if !started.is_empty() {
        if told.is_empty() {
            told.push_str(HEAD);
        }
        told.push_str(INSTALLED);
        told.push_str(&started);
        told.push('\n');
    }
    if cut > 0 && !told.is_empty() {
        let more = format!("and {cut} more, which tilo_ls names\n");
        if told.len().saturating_add(more.len()) <= most {
            told.push_str(&more);
        }
    }
    told
}

/// An app by the name a path takes, with the name people call it where that says more.
fn named(name: &str, label: &str) -> String {
    if label.is_empty() || label.eq_ignore_ascii_case(name) {
        name.to_owned()
    } else {
        format!("{name} ({label})")
    }
}

/// At most `most` of `actions` as a call writes them, each once: `#rename(title)`, the arguments
/// it must be given by name and `…` where it takes more; then how many more there are.
fn actions_told(actions: &[ActionDescriptor], most: usize) -> String {
    let mut seen: Vec<&str> = Vec::new();
    let mut told = String::new();
    for action in actions {
        let id = action.address.action.as_str();
        if seen.contains(&id) {
            continue;
        }
        seen.push(id);
        if seen.len() > most {
            continue;
        }
        if !told.is_empty() {
            told.push_str(", ");
        }
        told.push('#');
        told.push_str(id);
        let mut arguments: Vec<&str> = action
            .params
            .iter()
            .filter(|param| param.required)
            .map(|param| {
                let name: &str = param.name.as_ref();
                name
            })
            .collect();
        if action.params.iter().any(|param| !param.required) {
            arguments.push("…");
        }
        if !arguments.is_empty() {
            let _ = write!(told, "({})", arguments.join(", "));
        }
    }
    let more = seen.len().saturating_sub(most);
    if more > 0 {
        let _ = write!(told, ", and {more} more");
    }
    told
}
