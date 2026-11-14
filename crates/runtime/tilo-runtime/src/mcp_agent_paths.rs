//! The names an action answers by, and how close a name that runs nothing is
//! to one that would: an answer names a resource whole
//! (`tasks.tracker/issues/7`), and that is a path the next call takes.

use super::{ActionHost, Levels, McpServer, Node, ROOT_PEEK, Resolved};

/// The close matches an action no resource offers is told with.
pub(super) const CLOSE_MATCHES: usize = 5;

/// The action ids closest to `name`, best first, at most `limit`: those it is
/// part of, one of whose words it is, or whose start it shares.
pub(super) fn close_matches(name: &str, ids: &[String], limit: usize) -> Vec<String> {
    let lower = name.to_lowercase();
    let mut scored: Vec<(u8, &String)> = ids
        .iter()
        .filter_map(|id| {
            let named = id.to_lowercase();
            let rank = if named.contains(&lower) {
                0
            } else if lower.contains(&named) {
                1
            } else if shared_start(&named, &lower) >= 3 {
                2
            } else {
                return None;
            };
            Some((rank, id))
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, id)| id.clone())
        .collect()
}

/// How many bytes the start of `a` and `b` share, to the last `-`.
fn shared_start(a: &str, b: &str) -> usize {
    let common = a
        .bytes()
        .zip(b.bytes())
        .take_while(|(left, right)| left == right)
        .count();
    a.get(..common)
        .and_then(|start| start.rfind('-'))
        .unwrap_or(common)
}

/// The app a name beginning with a kind names (`tasks.tracker` is the Tasks
/// app's tracker), when the name is one kind and no app: a kind is the app's
/// own name and one more word, where a package's id is a domain and its
/// names.
pub(super) fn kind_address(name: &str) -> Option<&str> {
    let (app, kind) = name.split_once('.')?;
    (!app.is_empty() && !kind.is_empty() && !kind.contains('.')).then_some(app)
}

impl Node {
    /// Whether `address` names this resource whole, as an action answers it
    /// (`tasks.tracker/issues/7`): the id it is published under, or its key.
    pub(super) fn address_is(&self, address: &str) -> bool {
        self.id == address || self.key == address
    }
}

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// The resource the app lists under exactly `address`, with its actions
    /// and what it holds: an answer's own naming of what it made or changed.
    pub(super) async fn by_address(
        &self,
        app_id: &str,
        provider: &str,
        address: &str,
        levels: &mut Levels,
    ) -> Result<Option<Resolved>, String> {
        let root = self.app_children(provider, levels).await?;
        let mut found = root
            .children
            .iter()
            .find(|node| node.address_is(address))
            .cloned();
        // What a folder of a resource holds is a level further down than
        // the app's own listing reaches (`notes.notebook/page/<id>/body`).
        if found.is_none() {
            let containers: Vec<Node> = root
                .children
                .iter()
                .filter(|node| node.container)
                .take(ROOT_PEEK)
                .cloned()
                .collect();
            for container in containers {
                let inside = self.level(provider, &container.id, levels).await?;
                if let Some(node) = inside.children.iter().find(|node| node.address_is(address)) {
                    found = Some(node.clone());
                    break;
                }
            }
        }
        let Some(node) = found else {
            return Ok(None);
        };
        let here = self.level(provider, &node.id, levels).await?;
        Ok(Some(Resolved {
            app_id: app_id.to_owned(),
            provider: provider.to_owned(),
            path: address.to_owned(),
            actions: if here.own.is_empty() {
                node.actions.clone()
            } else {
                here.own.clone()
            },
            children: here.children.clone(),
            node: Some(node.clone()),
            chain: vec![node],
            levels: std::mem::take(levels),
            revision: here.revision,
        }))
    }
}
