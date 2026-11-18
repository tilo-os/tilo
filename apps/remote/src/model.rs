//! The window's own model state around the resource: a little per-peer view state beside
//! the saved connections, and the form being filled in. Two people browsing one list can
//! look at different machines. The connections themselves, their checks and their verbs
//! live in `remote-model`.

use std::collections::HashMap;

pub use remote_model::{
    AuthMethod, Connection, ConnectionDraft, ConnectionId, ConnectionKind, DraftProblem, Presence,
    Remote, compact_tilo_endpoint,
};

pub(crate) use crate::resource::CATALOG;

/// The draft's problems as the form shows them: each one with the words the person reads,
/// in their language, beside what it is so the form places it at the field that repairs it.
#[must_use]
pub fn draft_problems(draft: &ConnectionDraft) -> Vec<(DraftProblem, String)> {
    draft
        .problems()
        .into_iter()
        .map(|problem| (problem, CATALOG.t(problem.message_key())))
        .collect()
}

/// What the window shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    List,
    Form { editing: Option<ConnectionId> },
    Session { id: ConnectionId },
}

/// The connections in memory with the operations the window needs. No IO, no CRDT.
#[derive(Clone, Debug, Default)]
pub struct Workspace {
    pub connections: Vec<Connection>,
    /// How each connection last answered here.
    pub presence: HashMap<ConnectionId, Presence>,
}

impl Workspace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn connections(&self) -> &[Connection] {
        &self.connections
    }

    pub fn get(&self, id: ConnectionId) -> Option<&Connection> {
        self.connections.iter().find(|c| c.id == id)
    }

    pub fn get_mut(&mut self, id: ConnectionId) -> Option<&mut Connection> {
        self.connections.iter_mut().find(|c| c.id == id)
    }

    /// Insert a new connection, or replace one with the same id.
    pub fn upsert(&mut self, c: Connection) -> ConnectionId {
        let id = c.id;
        if let Some(existing) = self.get_mut(id) {
            *existing = c;
        } else {
            self.connections.push(c);
        }
        id
    }

    pub fn remove(&mut self, id: ConnectionId) -> bool {
        let before = self.connections.len();
        self.connections.retain(|c| c.id != id);
        self.connections.len() != before
    }

    pub fn toggle_favorite(&mut self, id: ConnectionId) -> bool {
        if let Some(c) = self.get_mut(id) {
            c.favorite = !c.favorite;
            c.favorite
        } else {
            false
        }
    }

    pub fn filtered(&self, query: &str) -> Vec<&Connection> {
        let q = query.trim().to_lowercase();
        let mut out: Vec<&Connection> = self
            .connections
            .iter()
            .filter(|c| {
                if q.is_empty() {
                    return true;
                }
                c.name.to_lowercase().contains(&q)
                    || c.host.0.to_lowercase().contains(&q)
                    || c.user.0.to_lowercase().contains(&q)
                    || c.group.0.to_lowercase().contains(&q)
                    || c.kind.tag().to_lowercase().contains(&q)
            })
            .collect();
        out.sort_by(|a, b| {
            b.favorite
                .cmp(&a.favorite)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        out
    }

    /// Reachability this device last observed for `id`.
    pub fn presence_of(&self, id: ConnectionId) -> Presence {
        self.presence.get(&id).copied().unwrap_or(Presence::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use ui::tilo_resource::Atom;

    use super::*;

    fn sample(name: &str, kind: ConnectionKind) -> Connection {
        Connection {
            id: uuid::Uuid::new_v4(),
            name: name.into(),
            host: Atom("host".into()),
            port: kind.default_port(),
            user: Atom("user".into()),
            kind,
            auth: AuthMethod::Password,
            last_used_ms: None,
            group: Atom::default(),
            favorite: false,
        }
    }

    /// The form names a draft's problems in the person's language, never by key.
    #[test]
    fn the_form_names_what_a_draft_needs_in_words() {
        let mut draft = ConnectionDraft {
            name: "studio".into(),
            ..ConnectionDraft::default()
        };
        let said = draft_problems(&draft);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said.iter().all(|(_, words)| !words.starts_with("remote-")));
        draft.host = "10.0.0.5".into();
        draft.port = "2222".into();
        assert!(draft_problems(&draft).is_empty());
    }

    tilo_test_env::scenario! {
        fn roster_upserts_and_filters_favorites_first;

        phase upsert_replaces_same_id;
        {
            let mut w = Workspace::new();
            let mut c = sample("a", ConnectionKind::Ssh);
            let id = w.upsert(c.clone());
            c.name = "renamed".into();
            w.upsert(c);
            assert_eq!(w.connections().len(), 1);
            assert_eq!(w.get(id).unwrap().name, "renamed");
        }

        phase filtered_orders_favorites_first;
        {
            let mut w = Workspace::new();
            let mut a = sample("alpha", ConnectionKind::Ssh);
            let mut b = sample("zeta", ConnectionKind::Vnc);
            b.favorite = true;
            w.upsert(a.clone());
            w.upsert(b);
            a.favorite = false;
            let order: Vec<_> = w.filtered("").iter().map(|c| c.name.clone()).collect();
            assert_eq!(order, vec!["zeta", "alpha"]);
        }

        phase filtered_matches_query;
        {
            let mut w = Workspace::new();
            w.upsert(sample("Build Box", ConnectionKind::Ssh));
            w.upsert(sample("Office NAS", ConnectionKind::Vnc));
            let hits: Vec<_> = w.filtered("build").iter().map(|c| c.name.clone()).collect();
            assert_eq!(hits, vec!["Build Box"]);
        }

        phase reachability_is_observed_per_device;
        {
            let mut w = Workspace::new();
            let a = w.upsert(sample("a", ConnectionKind::Ssh));
            let b = w.upsert(sample("b", ConnectionKind::Ssh));
            assert_eq!(w.presence_of(a), Presence::Unknown, "never probed");
            w.presence.insert(a, Presence::Online);
            w.presence.insert(b, Presence::Offline);
            assert_eq!(w.presence_of(a), Presence::Online);
            assert_eq!(w.presence_of(b), Presence::Offline);
        }
    }
}
