//! What a caller has in its workspace, said once where its work begins.
//!
//! A model that is told nothing looks before it works: which apps there are (`tilo_ls`), what
//! one of them offers (`tilo_ls notes`). Each look is one more ask of the model, and on a large
//! model an ask is many seconds. So a turn is told at its start every app it may use and every
//! action each offers it, by name: one short line an app, and one for each kind of thing an app
//! in use holds. No count of bytes cuts it: what is told follows the workspace, and an action
//! that is not named is one a model guesses at or looks for. What an action takes is not told
//! here: a listing of that one action names it (`tilo_ls notes#rename`), and a call that lacks
//! an argument is answered with what the action takes.
//!
//! An action on a thing in an app is called by the thing's path, so a thing an app holds once
//! of its kind is told by that path, as its row in the app's own listing names it
//! (`accounts/Calendars`): a kind's name alone would leave whoever calls it to look the thing
//! up first. Where an app holds several things of a kind they are told once for the kind, and
//! the app's own listing names each.
//!
//! An app in use is read through the caller's own listing, as the caller: nothing is told of it
//! that `tilo_ls` would not answer the same caller. An installed app that never started has no
//! listing yet: it is told by what its signed package declares, and only where the workspace
//! lets this caller open it. An app the caller may not open is not named at all, nor anything
//! of it. What is told names kinds, actions and the things an app holds once of their kind, so
//! it stays word for word the same until an app is installed, opened or changed, or such a
//! thing is named anew.
//!
//! The framework an app is built with gives the things every app keeps the same actions under
//! the same names (`undo`, `field-edit`, `render`). They are said once, before the apps in use,
//! and left out of each app's line.
//!
//! What changes least is said first, the installed apps before the apps in use: a provider
//! keeps a prompt's beginning for as long as it stays the same.

use super::{
    ActionDescriptor, ActionProvider, InstalledApp, McpServer, Value, app_names, parse_path,
};
use std::fmt::Write as _;

/// What stands before everything told.
const HEAD: &str = "What you have here now, as tilo_ls would answer you (said once, so you need \
not look it up). Every app is named with every action it offers you, by name: tilo_do takes one \
as the path `app#action` with its arguments in `args`. An indented line is a thing in the app \
above it, by the path a call takes: an action on it goes as `app/thing#action`. Where an app \
holds several things of a kind the line says `<its name>`, and tilo_ls of the app names them. \
What an action takes, tilo_ls of its path names; a call that lacks an argument is answered with \
what the action takes.\n";
/// What stands before the apps in use.
const IN_USE: &str = "In use:\n";
/// What stands before the installed apps that never started.
const INSTALLED: &str = "Installed and not started yet, each with what its package declares (an \
action named on one starts it first):\n";
/// The framework's own actions: what the framework an app is built with publishes on the
/// things every app keeps, beside the app's own, under ids no app may give a verb of its own
/// (tilo-app's `reserved_action_ids`). An id the framework gains later and this list lacks is
/// told on each app as before: longer, never wrong.
const FRAMEWORKS_OWN: [&str; 20] = [
    "activate-window",
    "apply",
    "checkpoint",
    "diff",
    "facts",
    "field-edit",
    "group",
    "history",
    "open",
    "open-link",
    "outline",
    "read",
    "redo",
    "render",
    "restore",
    "restore-reviewed",
    "review-restore",
    "share-invite-link",
    "share-link",
    "undo",
];
/// What stands before the framework's own actions, said once before the apps' lines.
const ONCE: &str = "The framework's own actions are the same on every thing that takes them; \
they are said here once and left out of the lines below (tilo_ls of a thing names which it \
takes): ";

/// What one app offers a caller, by name.
struct Offered {
    /// The app by the name a path takes, with the name people call it.
    named: String,
    /// The name a path takes.
    path: String,
    /// The actions on the app itself.
    own: Vec<String>,
    /// The actions on the things it holds, a kind of thing once.
    kinds: Vec<Kept>,
}

/// The actions on the things of one kind an app holds.
struct Kept {
    /// The kind of thing.
    kind: String,
    /// The one thing of the kind the app holds, by the name a path takes, as its row in the
    /// app's own listing names it; none where the app holds several of the kind.
    only: Option<String>,
    /// The actions on a thing of the kind.
    actions: Vec<String>,
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// What this caller has in its workspace: every app in use with every action it offers the
    /// caller, then every installed app the caller may open with every action its package
    /// declares, each by name. Empty where nothing can be listed.
    pub async fn told_at_start(&self) -> String {
        let Ok((_, providers)) = self.providers().await else {
            return String::new();
        };
        let installed = self.installed().await.unwrap_or_default();
        let in_use = apps_in_use(&providers);
        let mut used: Vec<Offered> = Vec::new();
        for (name, app_id, provider) in &in_use {
            let listed = installed.iter().find(|app| app.app_id == *app_id);
            // An app the workspace names to this caller without letting it open it is not told.
            if listed.is_some_and(|app| !app.opens) {
                continue;
            }
            let label = listed.map_or("", |app| app.label.as_str());
            // An app whose listing cannot be read now is still named.
            let offered = self.offered_by(name, app_id, provider, label).await;
            used.push(offered.unwrap_or_else(|| Offered {
                named: named(name, label),
                path: name.clone(),
                own: Vec::new(),
                kinds: Vec::new(),
            }));
        }
        let mut waiting: Vec<Offered> = may_start(&installed, &in_use)
            .into_iter()
            .map(declared_by)
            .collect();
        waiting.sort_by(|one, other| one.named.cmp(&other.named));
        waiting.dedup_by(|one, other| one.named == other.named);
        put_together(&used, &waiting)
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
        let own = names_of(&found.actions);
        let mut kinds: Vec<Kept> = Vec::new();
        for child in &found.children {
            if kinds.iter().any(|kept| kept.kind == child.kind) {
                continue;
            }
            let theirs = names_of(&child.actions);
            // A kind that offers what the app itself does is said on the app's line already.
            if theirs.is_empty() || theirs == own {
                continue;
            }
            // A thing the app holds once of its kind is told by the name a path takes, the one
            // its row in the app's own listing gives it: a call on it is written from that.
            let alone = !found
                .children
                .iter()
                .any(|other| other.kind == child.kind && other.id != child.id);
            let only = alone
                .then(|| child.name(&found.children))
                .filter(|name| !name.contains('\n'));
            kinds.push(Kept {
                kind: child.kind.clone(),
                only,
                actions: theirs,
            });
        }
        Some(Offered {
            named: named(name, label),
            path: name.to_owned(),
            own,
            kinds,
        })
    }
}

/// The actions `actions` hold, each once by its name, in the order they are offered.
fn names_of(actions: &[ActionDescriptor]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for action in actions {
        let name = action.address.action.as_str();
        if !names.iter().any(|known| known == name) {
            names.push(name.to_owned());
        }
    }
    names
}

/// Whether `name` is one of the framework's own ([`FRAMEWORKS_OWN`]).
fn frameworks_own(name: &str) -> bool {
    FRAMEWORKS_OWN.contains(&name)
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

/// The installed apps this caller may open that are not in use: the only ones of which
/// anything is told before they start.
fn may_start<'a>(
    installed: &'a [InstalledApp],
    in_use: &[(String, String, String)],
) -> Vec<&'a InstalledApp> {
    installed
        .iter()
        .filter(|app| app.opens && !in_use.iter().any(|(_, known, _)| *known == app.app_id))
        .collect()
}

/// What an installed app that never started offers, by what its signed package declares: the
/// surface this process holds for it, else the one the caller's own listing of the workspace
/// carries, else the fleet's. With none of them the app is still named.
fn declared_by(app: &InstalledApp) -> Offered {
    let path = app_names(std::slice::from_ref(&app.app_id)).join("");
    let surface = collab_types::app_surface::bytes_of(&app.app_id)
        .map(<[u8]>::to_vec)
        .or_else(|| app.surface.clone().map(String::into_bytes))
        .or_else(|| app_fleet::surface(&app.app_id).map(|text| text.as_bytes().to_vec()));
    Offered {
        named: named(&path, &app.label),
        path,
        own: surface.map(|bytes| declared(&bytes)).unwrap_or_default(),
        kinds: Vec::new(),
    }
}

/// Every action a surface declares, each once by its name, in the order it declares them; none
/// where it does not parse.
fn declared(surface: &[u8]) -> Vec<String> {
    let Ok(file) = collab_types::surface::SurfaceFile::parse(surface) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for verb in file
        .resources
        .iter()
        .flat_map(|resource| resource.verbs.iter())
    {
        if !names.contains(&verb.id) {
            names.push(verb.id.clone());
        }
    }
    names
}

/// An app by the name a path takes, with the name people call it where that says more.
fn named(name: &str, label: &str) -> String {
    if label.is_empty() || label.eq_ignore_ascii_case(name) {
        name.to_owned()
    } else {
        format!("{name} ({label})")
    }
}

/// The framework's own actions that any of `apps` offers this caller, each once by its name,
/// in the order of their names, on a line of their own; empty where it is offered none.
fn said_once(apps: &[Offered]) -> String {
    let mut found: Vec<&str> = apps
        .iter()
        .flat_map(|app| {
            app.own
                .iter()
                .chain(app.kinds.iter().flat_map(|kept| kept.actions.iter()))
        })
        .map(String::as_str)
        .filter(|name| frameworks_own(name))
        .collect();
    found.sort_unstable();
    found.dedup();
    if found.is_empty() {
        return String::new();
    }
    let told: Vec<String> = found.into_iter().map(|name| format!("#{name}")).collect();
    format!("{ONCE}{}\n", told.join(", "))
}

/// Every one of `names` that is an app's own, as a call's path ends (`#rename`). The
/// framework's own are left out: they are said once ([`said_once`]).
fn told(names: &[String]) -> String {
    names
        .iter()
        .filter(|name| !frameworks_own(name))
        .map(|name| format!("#{name}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One app's lines: its name with every action on it, then each kind of thing in it with every
/// action on that: a thing the app holds once of its kind by the path a call takes, the things
/// of a kind it holds several of once for the kind. A kind that offers only the framework's own
/// has nothing of its own to tell and gets no line; an app that offers nothing of its own is
/// still named.
fn lines_of(app: &Offered) -> String {
    let mut lines = app.named.clone();
    let own = told(&app.own);
    if !own.is_empty() {
        let _ = write!(lines, ": {own}");
    }
    for kept in &app.kinds {
        let theirs = told(&kept.actions);
        if theirs.is_empty() {
            continue;
        }
        if let Some(name) = &kept.only {
            let _ = write!(lines, "\n  {}/{name}: {theirs}", app.path);
        } else {
            let kind = kept.kind.rsplit('.').next().unwrap_or(&kept.kind);
            let _ = write!(
                lines,
                "\n  each {kind} in it ({}/<its name>): {theirs}",
                app.path
            );
        }
    }
    lines.push('\n');
    lines
}

/// Everything told: every installed app to start, then what is said once and every app in use.
/// Empty where there is no app to tell of.
fn put_together(used: &[Offered], waiting: &[Offered]) -> String {
    if used.is_empty() && waiting.is_empty() {
        return String::new();
    }
    // What changes least stands first: a provider keeps a prompt's beginning for as long as it
    // stays the same, from ask to ask and from one turn of a conversation to the next. The line
    // of an installed app changes only when the app is installed, changed or first started;
    // the apps in use change as the things in them do.
    let mut told = String::from(HEAD);
    if !waiting.is_empty() {
        told.push_str(INSTALLED);
        for app in waiting {
            told.push_str(&lines_of(app));
        }
    }
    if !used.is_empty() {
        told.push_str(&said_once(used));
        told.push_str(IN_USE);
        for app in used {
            told.push_str(&lines_of(app));
        }
    }
    told
}

#[cfg(test)]
mod tests {
    use super::{InstalledApp, Kept, Offered, declared_by, may_start, put_together};

    /// What the framework gives the things an app keeps, by name.
    const THE_FRAMEWORKS: [&str; 6] = ["apply", "group", "field-edit", "outline", "undo", "redo"];

    /// An app in use that offers `own` itself and, on the one kind of thing it keeps, `kept`
    /// and everything the framework gives.
    fn app(name: &str, own: &[&str], kept: &[&str]) -> Offered {
        let on_the_kind = kept.iter().chain(THE_FRAMEWORKS.iter());
        Offered {
            named: name.to_owned(),
            path: name.to_owned(),
            own: own.iter().map(|action| (*action).to_owned()).collect(),
            kinds: vec![Kept {
                kind: format!("{name}.book"),
                only: None,
                actions: on_the_kind.map(|action| (*action).to_owned()).collect(),
            }],
        }
    }

    /// A package's surface that declares `verbs` on one kind of thing.
    fn surface(verbs: &[&str]) -> String {
        let mut text = String::from(
            "format = 2\n\n[[resources]]\nkind = \"plums.orchard\"\nlabel = \"Orchard\"\nversion = 1\n",
        );
        for verb in verbs {
            text.push_str("\n[[resources.verbs]]\nid = \"");
            text.push_str(verb);
            text.push_str("\"\nkind = \"undoable\"\n");
        }
        text
    }

    /// No count of bytes cuts what is told: an app that offers six hundred actions is told
    /// with every one of them by its name, and nothing says that more were left out.
    #[test]
    fn every_action_is_told_by_its_name_and_no_count_cuts_it() {
        let many: Vec<String> = (0..600).map(|at| format!("prune-row-{at}")).collect();
        let named: Vec<&str> = many.iter().map(String::as_str).collect();
        let told = put_together(&[app("orchard", &named, &["graft"])], &[]);
        for action in &many {
            let written = format!("#{action}");
            assert!(
                told.split([',', '\n', ' ']).any(|word| word == written),
                "{action} is not told"
            );
        }
        assert!(told.len() > 6_000, "{} bytes", told.len());
        assert!(!told.contains("more") && !told.contains('…'), "{told}");
        // An action is told by its name alone: what it takes is its own listing's.
        assert!(!told.contains("#graft("), "{told}");
        assert!(
            told.contains("\n  each book in it (orchard/<its name>): #graft\n"),
            "{told}"
        );
    }

    /// A thing an app holds once of its kind is told by the path a call takes, so a call on it
    /// is written from what is told; the things of a kind it holds several of are told once
    /// for the kind.
    #[test]
    fn a_thing_an_app_holds_once_is_told_by_the_path_a_call_takes() {
        let mut orchard = app("orchard", &["plant"], &["graft"]);
        orchard.kinds.push(Kept {
            kind: "orchard.shed".to_owned(),
            only: Some("Tool shed".to_owned()),
            actions: vec!["lend".to_owned(), "undo".to_owned()],
        });
        let told = put_together(&[orchard], &[]);
        assert!(
            told.ends_with(
                "orchard: #plant\n  each book in it (orchard/<its name>): #graft\n  \
                 orchard/Tool shed: #lend\n"
            ),
            "{told}"
        );
        // What is told says how such a line is read.
        assert!(told.contains("`app/thing#action`"), "{told}");
    }

    /// The framework's own actions are the same on every thing every app keeps: they are said
    /// once, before the apps, and no app's line repeats them. So one more app in use costs
    /// what that app offers and nothing of the framework's.
    #[test]
    fn the_frameworks_own_actions_are_said_once_and_no_apps_line_repeats_them() {
        let apps = [
            app("notes", &["new-note"], &["new-tag"]),
            app("tasks", &["create"], &["create-area"]),
            app("sheets", &["new-sheet"], &["add-row"]),
        ];
        let told = put_together(&apps, &[]);
        for theirs in ["#apply", "#field-edit", "#undo", "#redo"] {
            assert_eq!(told.matches(theirs).count(), 1, "{theirs} in:\n{told}");
        }
        assert!(
            told.find("#undo") < told.find("\nnotes: "),
            "what is said once stands before the apps:\n{told}"
        );
        let sheets = "sheets: #new-sheet\n  each book in it (sheets/<its name>): #add-row\n";
        assert!(told.ends_with(sheets), "{told}");
        let without = put_together(&apps[..2], &[]);
        assert_eq!(
            told.len().saturating_sub(without.len()),
            sheets.len(),
            "one more app costs its own lines:\n{told}\nand without it:\n{without}"
        );
        // A thing that offers only the framework's own has nothing of its own to tell.
        let bare = put_together(&[app("plums", &[], &[])], &[]);
        assert!(
            bare.ends_with("\nplums\n") && !bare.contains("each book"),
            "{bare}"
        );
    }

    /// What is told of an app that never started is what its package declares, and only where
    /// the workspace lets this caller open the app: one it may not open is not named, nor any
    /// action of it, though the workspace's listing carries its surface all the same.
    #[test]
    fn an_app_its_caller_may_not_open_is_not_named_nor_any_action_of_it() {
        let installed = |opens: bool| InstalledApp {
            app_id: "example.plums.orchard-of-the-test".to_owned(),
            label: "Orchard".to_owned(),
            opens,
            surface: Some(surface(&["prune", "graft", "prune"])),
        };
        let open = [installed(true)];
        let waiting: Vec<Offered> = may_start(&open, &[]).into_iter().map(declared_by).collect();
        let told = put_together(&[], &waiting);
        assert!(
            told.ends_with("\norchard-of-the-test (Orchard): #prune, #graft\n"),
            "{told}"
        );
        // The same app, closed to this caller: nothing of it is told.
        let closed = [installed(false)];
        let waiting: Vec<Offered> = may_start(&closed, &[])
            .into_iter()
            .map(declared_by)
            .collect();
        let told = put_together(&[], &waiting);
        assert!(
            !told.contains("orchard") && !told.contains("prune") && !told.contains("graft"),
            "{told}"
        );
        // And an app in use is not told a second time by what its package declares.
        let in_use = [(
            "orchard-of-the-test".to_owned(),
            "example.plums.orchard-of-the-test".to_owned(),
            "71".to_owned(),
        )];
        assert!(may_start(&open, &in_use).is_empty());
    }
}
