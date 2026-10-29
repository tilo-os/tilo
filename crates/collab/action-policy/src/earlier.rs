//! The ids apps were published under before (`app so.tilo.lang.tasks "Tasks" (previously:
//! so.tilo.lang.todo)`): the one lookup every place that keeps an app id reads through.
//!
//! What was kept under an earlier id (records, grants, windows, references) stays as written and
//! is the renamed app's: a grant on `app:so.tilo.lang.todo` covers `app:so.tilo.lang.tasks`, a
//! grant to the app Todo is Tasks', `tl.so.tilo.lang.todo.TodoItem` is the kind
//! `tl.so.tilo.lang.tasks.TodoItem`. Nothing is rewritten.
//!
//! Who builds the table decides whom to believe: the installed catalogue honours a claim only
//! from the publisher that signed the earlier id (`tilo_installed_apps::earlier_ids`), a host
//! the packages it loaded.

use std::collections::BTreeMap;

use crate::{Level, Segment, Subject, TreePath};

/// For each app, the ids it was published under before.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EarlierIds {
    by_app: BTreeMap<String, Vec<String>>,
}

impl EarlierIds {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `app` was published as each of `before` earlier. Its own id and an id another
    /// app already claimed are left out: an earlier id has one successor.
    pub fn insert<I, S>(&mut self, app: &str, before: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for old in before {
            let old = old.into();
            if old == app || old.trim().is_empty() || self.successor(&old).is_some() {
                continue;
            }
            self.by_app.entry(app.to_owned()).or_default().push(old);
        }
    }

    /// Forget what `app` was published as before.
    pub fn remove(&mut self, app: &str) {
        self.by_app.remove(app);
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_app.is_empty()
    }

    /// The ids `app` was published under before; none for an app never renamed.
    #[must_use]
    pub fn of(&self, app: &str) -> &[String] {
        self.by_app.get(app).map_or(&[], Vec::as_slice)
    }

    /// The app now published as `old` was before, when it was renamed.
    #[must_use]
    pub fn successor(&self, old: &str) -> Option<&str> {
        self.by_app
            .iter()
            .find(|(_, before)| before.iter().any(|id| id == old))
            .map(|(app, _)| app.as_str())
    }

    /// The app `id` names now: its successor when it is an earlier id, else itself.
    #[must_use]
    pub fn current<'a>(&'a self, id: &'a str) -> &'a str {
        self.successor(id).unwrap_or(id)
    }

    /// The published id `resource` (`tl:<app>:<id>`, `tl:<app>`) under its app's id now, when it
    /// was written under an id the app had before; `None` for any other.
    #[must_use]
    pub fn current_resource(&self, resource: &str) -> Option<String> {
        let rest = resource.strip_prefix("tl:")?;
        let (old, tail) = match rest.split_once(':') {
            Some((old, id)) => (old, Some(id)),
            None => (rest, None),
        };
        let app = self.successor(old)?;
        Some(match tail {
            Some(id) => format!("tl:{app}:{id}"),
            None => format!("tl:{app}"),
        })
    }

    /// Whether `id` names `app`: it is `app`'s id or one it had before.
    #[must_use]
    pub fn names(&self, id: &str, app: &str) -> bool {
        id == app || self.of(app).iter().any(|old| old == id)
    }

    /// The same node as it was written under each id its app had before: the app segment, and a
    /// `tl.<app>.…` kind of that app, under the earlier id. Empty when the node's app was never
    /// renamed.
    #[must_use]
    pub fn earlier_nodes(&self, node: &TreePath) -> Vec<TreePath> {
        let Some(app) = node.id_at(Level::App) else {
            return Vec::new();
        };
        let before = self.of(app);
        if before.is_empty() {
            return Vec::new();
        }
        let own_kind = format!("tl.{app}");
        before
            .iter()
            .map(|old| {
                let mut path = TreePath::root();
                for segment in node.segments() {
                    path = match segment {
                        Segment::Name(Level::App, _) => path.app(old.clone()),
                        Segment::Name(Level::Kind, kind) => match kind.strip_prefix(&own_kind) {
                            Some(rest) if rest.is_empty() || rest.starts_with('.') => {
                                path.kind(format!("tl.{old}{rest}"))
                            }
                            _ => path.kind(kind.clone()),
                        },
                        Segment::Name(level, id) => path.push(*level, id.clone()),
                        Segment::Any(level) => path.any(*level),
                        Segment::Rest => path.rest(),
                    };
                }
                path
            })
            .collect()
    }

    /// Whether the grant node `pattern` covers `node`, or `node` as it was written under an id its
    /// app had before.
    #[must_use]
    pub fn covers(&self, pattern: &TreePath, node: &TreePath) -> bool {
        pattern.covers(node)
            || self
                .earlier_nodes(node)
                .iter()
                .any(|earlier| pattern.covers(earlier))
    }

    /// Whether a grant to `granted` speaks for one of `subjects`: the same subject, or the app
    /// `granted` names was renamed to an app among them.
    #[must_use]
    pub fn subject_matches(&self, granted: &Subject, subjects: &[Subject]) -> bool {
        if subjects.contains(granted) {
            return true;
        }
        let Subject::App(old) = granted else {
            return false;
        };
        self.successor(old).is_some_and(|app| {
            subjects
                .iter()
                .any(|subject| matches!(subject, Subject::App(held) if held == app))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tasks() -> EarlierIds {
        let mut ids = EarlierIds::new();
        ids.insert("so.tilo.lang.tasks", ["so.tilo.lang.todo"]);
        ids
    }

    #[test]
    fn a_node_of_the_renamed_app_reads_under_its_earlier_id() {
        let ids = tasks();
        let node = TreePath::room("r")
            .app("so.tilo.lang.tasks")
            .kind("tl.so.tilo.lang.tasks.TodoItem")
            .resource("i1")
            .action("check");
        let old = TreePath::room("r").app("so.tilo.lang.todo");
        assert!(ids.covers(&old.clone().rest(), &node));
        assert!(
            ids.covers(
                &old.clone()
                    .kind("tl.so.tilo.lang.todo.TodoItem")
                    .any(Level::Resource),
                &node
            )
        );
        // Another app's kind, or a kind that only starts like it, stays as written.
        let other = TreePath::room("r")
            .app("so.tilo.lang.tasks")
            .kind("tl.so.tilo.lang.tasksx.Item");
        assert!(!ids.covers(&old.kind("tl.so.tilo.lang.todox.Item").rest(), &other));
        // The earlier app's node is not the renamed one's: a grant on Tasks says nothing of an
        // app still called Todo elsewhere.
        assert!(!ids.covers(
            &TreePath::room("r").app("so.tilo.lang.tasks"),
            &TreePath::room("r").app("so.tilo.lang.todo")
        ));
    }

    #[test]
    fn a_grant_to_the_earlier_app_is_the_renamed_apps() {
        let ids = tasks();
        let todo = Subject::app("so.tilo.lang.todo");
        assert!(ids.subject_matches(&todo, &[Subject::app("so.tilo.lang.tasks")]));
        assert!(!ids.subject_matches(&todo, &[Subject::app("so.tilo.lang.notes")]));
        assert!(!ids.subject_matches(
            &Subject::app("so.tilo.lang.tasks"),
            &[Subject::app("so.tilo.lang.todo")]
        ));
    }

    #[test]
    fn an_earlier_id_has_one_successor() {
        let mut ids = tasks();
        ids.insert(
            "com.example.other",
            ["so.tilo.lang.todo", "com.example.old"],
        );
        assert_eq!(
            ids.successor("so.tilo.lang.todo"),
            Some("so.tilo.lang.tasks")
        );
        assert_eq!(ids.of("com.example.other"), ["com.example.old"]);
        assert_eq!(ids.current("so.tilo.lang.notes"), "so.tilo.lang.notes");
        assert!(ids.names("so.tilo.lang.todo", "so.tilo.lang.tasks"));
        assert_eq!(
            ids.current_resource("tl:so.tilo.lang.todo:abc"),
            Some("tl:so.tilo.lang.tasks:abc".to_owned())
        );
        assert_eq!(
            ids.current_resource("tl:so.tilo.lang.todo"),
            Some("tl:so.tilo.lang.tasks".to_owned())
        );
        assert_eq!(ids.current_resource("tl:so.tilo.lang.notes:abc"), None);
        assert_eq!(ids.current_resource("notes.page"), None);
    }
}
