//! A bot's write of a tilolang record, decided under the grants the workspace holds for the bot.
//!
//! A bot's run on this device writes a tilolang app's record through the record route
//! ([`Workspaces::record_command`](super::Workspaces::record_command)), signed by the device the
//! service signs its own roles' writes as. What decides it is not that device's authority: each
//! write the route is handed names its action and invoker in its stamp, and a bot's write is
//! decided as every member decides a change the bot signs itself, under the grants the workspace
//! holds for the bot now: an installed bot's as its own device holds them, and a bot the
//! workspace runs without installing it (its moderation bot) what the workspace grants it. The
//! action is decided at the resource it was invoked on: the nearest
//! resource at or above what it writes of a type the action takes, named and kinded as the
//! host's authority names it.

use std::collections::BTreeMap;

use collab_types::Stamp;
use tilo_action_policy::{
    ActionKind, Answer, Authority, Context, Grantee, MemberAccess, TreePath, WorkspacePolicy,
};
use tilo_resource_store::wire::Command;

use super::GovernedWorkspace;
use crate::tl_records::Declared;

/// How far above what it writes the resource an action was invoked on may be: deeper than any
/// record nests.
const DEPTH: usize = 64;

/// One write of a command: who stamped it, and where the search for the resource its action was
/// invoked on starts.
struct Write<'c> {
    stamp: &'c Stamp,
    /// The resource the search starts at: the written resource itself, when the action may have
    /// been invoked on it, else the one it is or was nested in.
    from: Option<String>,
}

/// What decides a bot's writes.
enum Bot {
    /// An installed bot: its access, as its own device holds it in the workspace.
    Installed(MemberAccess),
    /// A bot the workspace runs without installing it: what the workspace grants it.
    Granted(std::sync::Arc<WorkspacePolicy>, String),
}

impl Bot {
    fn allows(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> bool {
        let answer = match self {
            Self::Installed(access) => access.authority().decide(node, kind, cx).answer,
            Self::Granted(policy, bot) => {
                Authority::person(policy.party_of(Grantee::bot(bot.as_str())))
                    .decide(node, kind, cx)
                    .answer
            }
        };
        answer == Answer::Allow
    }
}

/// Refuse `command`, a write of `app`'s record, when a bot stamped a write of it that the
/// workspace's grants to that bot do not allow; the reason, as said to whoever ran the bot. A
/// write by anyone else is decided by the runtime that stamped it.
pub(super) fn decide(
    governed: &mut GovernedWorkspace,
    (surfaces, host): (&collab_types::app_surface::Surfaces, [u8; 32]),
    app: &str,
    command: &Command,
) -> Result<(), String> {
    let mut writes = Vec::new();
    let mut placed = BTreeMap::new();
    let store = governed.store_with_surfaces(surfaces);
    gather(store.index(), command, &mut writes, &mut placed);
    // Each bot's invocations, each with the resources its writes start from.
    let mut invoked: BTreeMap<(String, &str, &str), Vec<Option<String>>> = BTreeMap::new();
    for write in &writes {
        let Some(bot) = write.stamp.invoked_by().and_then(|actor| {
            actor
                .member_id()
                .and_then(|member| member.bot_id().map(str::to_owned))
        }) else {
            continue;
        };
        invoked
            .entry((
                bot,
                write.stamp.invocation.as_str(),
                write.stamp.verb.as_str(),
            ))
            .or_default()
            .push(write.from.clone());
    }
    if invoked.is_empty() {
        return Ok(());
    }
    let now = tilo_clock::wall();
    let workspace = governed.workspace().to_owned();
    let mut access: BTreeMap<String, Bot> = BTreeMap::new();
    for (bot, _, _) in invoked.keys() {
        if access.contains_key(bot) {
            continue;
        }
        let peer = governed.peer();
        let held = match peer.governance().installation(bot, now) {
            Some(row) if row.withdrawn_at_ns.is_none() && row.acting => Bot::Installed(
                peer.permissions(row.device_public_key, now)
                    .map_err(|error| format!("the bot {bot} holds no access here: {error}"))?,
            ),
            Some(_) => return Err(format!("the bot {bot} does not act in this workspace")),
            None => Bot::Granted(
                peer.permissions(host, now)
                    .map_err(|error| format!("this workspace's policy is not here: {error}"))?
                    .policy,
                bot.clone(),
            ),
        };
        access.insert(bot.clone(), held);
    }
    let packages =
        crate::tl_records::built_in(app)?.ok_or_else(|| crate::tl_records::runs_elsewhere(app))?;
    let index = governed.store_with_surfaces(surfaces).index();
    let up = |id: &str| -> Option<(String, Option<String>)> {
        if let Some((kind, parent)) = placed.get(id) {
            return Some(((*kind).to_owned(), parent.map(str::to_owned)));
        }
        let kind = index.kind_anywhere(id)?;
        let parent = index.get(&kind, id)?.parent.clone();
        Some((kind, parent))
    };
    let cx = Context::at_time(now);
    for ((bot, _, verb), starts) in &invoked {
        let refused = |why: &str| format!("the bot {bot} may not {why}");
        let declared = packages
            .declared(verb)
            .ok_or_else(|| refused(&format!("invoke {verb}, which {app} does not declare")))?;
        let nodes = invoked_at(&workspace, &declared, starts, &up);
        if nodes.is_empty() {
            return Err(refused(&format!(
                "{}: it writes nothing in a resource {} is invoked on",
                declared.name, declared.name
            )));
        }
        let held = access.get(bot).ok_or_else(|| {
            refused(&format!(
                "{} in {app}: the bot's access is not read here",
                declared.name
            ))
        })?;
        if !nodes
            .iter()
            .any(|node| held.allows(node, declared.kind, &cx))
        {
            return Err(refused(&format!("{} in {app}", declared.name)));
        }
    }
    Ok(())
}

/// Each write of `command`, and where each resource it puts is nested.
fn gather<'c>(
    index: &tilo_resource_store::Index,
    command: &'c Command,
    writes: &mut Vec<Write<'c>>,
    placed: &mut BTreeMap<&'c str, (&'c str, Option<&'c str>)>,
) {
    match command {
        Command::Put {
            kind,
            id,
            resource,
            stamp,
        } => {
            placed.insert(id, (kind, resource.parent.as_deref()));
            // A resource it makes was not there to be invoked on: the one it is put in was.
            let made = index.get(kind, id).is_none();
            writes.push(Write {
                stamp,
                from: if made {
                    resource.parent.clone()
                } else {
                    Some(id.clone())
                },
            });
        }
        Command::Edit { id, stamp, .. } => writes.push(Write {
            stamp,
            from: Some(id.clone()),
        }),
        // A resource it removes was taken out of the one it was nested in.
        Command::Remove { kind, id, stamp } => writes.push(Write {
            stamp,
            from: index.get(kind, id).and_then(|held| held.parent.clone()),
        }),
        Command::Undo { stamp, .. } => writes.push(Write { stamp, from: None }),
        Command::Batch(commands) => {
            for command in commands {
                gather(index, command, writes, placed);
            }
        }
        _ => {}
    }
}

/// Where a resource it names is placed: its kind, and the resource above it.
type Placement = Option<(String, Option<String>)>;

/// Every node the action `declared` may have been invoked at, by writes starting from `starts`:
/// each resource at or above them of a type it takes, or its package's own node for an action
/// invoked on no resource, as the host's authority names it.
fn invoked_at(
    workspace: &str,
    declared: &Declared,
    starts: &[Option<String>],
    up: &dyn Fn(&str) -> Placement,
) -> Vec<TreePath> {
    let app = TreePath::workspace(workspace).app(declared.app.clone());
    if declared.targets.is_empty() {
        let node = app.kind(declared.package.clone());
        let node = if tilo_action_policy::roles::requires_document_keys(&declared.app, "") {
            node.resource("")
        } else {
            node
        };
        return vec![node.action(declared.name.clone())];
    }
    let mut nodes = Vec::new();
    for start in starts {
        let mut at = start.clone();
        for _ in 0..DEPTH {
            let Some(id) = at.take() else {
                break;
            };
            let Some((kind, parent)) = up(&id) else {
                break;
            };
            if declared.targets.contains(&kind) {
                nodes.push(
                    app.clone()
                        .kind(kind)
                        .resource(tilolang_tilo::publish::resource_id(&declared.app, &id))
                        .action(declared.name.clone()),
                );
            }
            at = parent;
        }
    }
    nodes
}
