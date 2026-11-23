//! Who is here: the agent sessions this device's daemon is serving now, each from the moment its
//! connection opens until its session ends.
//!
//! A device runs its person's agents, and each connection is one agent session. A page of the
//! workspace reads it in who is here, beside the workspace's own roster, from the moment it
//! connects: present and waiting before it asks anything, staying while its session lasts
//! however long it goes quiet, and gone as soon as the session ends or the person stops it. A
//! run after it is a session of its own.
//!
//! The sessions are keyed by the execution the connection was admitted in, so the caller a
//! connection hands this device is all it takes to find its session again: the agent it acts as
//! once its client names itself, what the workspace calls it, and the end of its session.

use std::sync::Mutex;

use tilo_actions::remote::{Completer, Here, Pending, pending};
use tilo_lock::MutexRecover as _;

/// The prefix of a session's entry in who is here: what tells a live session of an agent from a
/// person and from a member, whose ids are their own keys.
pub const SESSION_WHO: &str = "agent-session:";

/// One agent session this device serves: one MCP connection, while it lasts.
struct Live {
    /// The workspace it works in.
    workspace: String,
    /// What the workspace calls it: the client's own name once it said one.
    name: String,
    /// The person stopped this session: it stays ended, however long its connection is held.
    stopped: bool,
}

/// The agent sessions this device serves now, and the watches that follow who is here.
#[derive(Default)]
pub struct WhoIsHere {
    live: Mutex<std::collections::BTreeMap<String, Live>>,
    /// The watches waiting on a change of who is here. Each is answered when it changes, and a
    /// watch answered asks again.
    waits: Mutex<Vec<Completer<()>>>,
}

impl WhoIsHere {
    /// A connection in `workspace` opened: it is here from now on, called `name` until its
    /// client says what it is. `execution` is the run the connection was admitted in.
    pub fn opened(&self, execution: &str, workspace: &str, name: &str) {
        let mut live = self.live.lock_recover();
        live.insert(
            execution.to_owned(),
            Live {
                workspace: workspace.to_owned(),
                name: name.to_owned(),
                stopped: false,
            },
        );
        drop(live);
        self.wake();
    }

    /// The client of `execution`'s connection named itself: the workspace reads the session as
    /// the agent `client` from now on. Whether that changed who is here.
    pub fn named(&self, execution: &str, client: &str) -> bool {
        let changed = {
            let mut live = self.live.lock_recover();
            let Some(session) = live.get_mut(execution) else {
                return false;
            };
            let changed = session.name != client;
            session.name = client.to_owned();
            changed
        };
        if changed {
            self.wake();
        }
        changed
    }

    /// The connection of `execution` ended: its session leaves who is here.
    pub fn closed(&self, execution: &str) {
        if self.live.lock_recover().remove(execution).is_some() {
            self.wake();
        }
    }

    /// The person stopped the session called `name`, in the workspace the caller acts in: it
    /// leaves who is here and stays ended. Whether a session that name was here.
    pub fn stop(&self, workspace: &str, name: &str) -> bool {
        {
            let mut live = self.live.lock_recover();
            let Some((_, session)) = live.iter_mut().find(|(_, session)| {
                session.workspace == workspace && session.name == name && !session.stopped
            }) else {
                return false;
            };
            session.stopped = true;
        }
        self.wake();
        true
    }

    /// Whether the run `execution` was admitted in has ended: the person stopped its session, so
    /// its calls end nothing.
    pub fn ended(&self, execution: &str) -> bool {
        self.live
            .lock_recover()
            .get(execution)
            .is_some_and(|session| session.stopped)
    }

    /// The sessions here in `workspace`, each as who is here names it: its own entry, so a run
    /// after another is a newcomer rather than the run before it moved on.
    pub fn add_here(&self, workspace: &str, here: &mut Vec<Here>) {
        let live = self.live.lock_recover();
        for (execution, session) in &*live {
            if session.workspace != workspace || session.stopped {
                continue;
            }
            here.push(Here {
                who: format!("{SESSION_WHO}{execution}"),
                name: session.name.clone(),
                kind: "agent".to_owned(),
                works_in: None,
            });
        }
    }

    /// A watch of `workspace` to wait on: resolves when who is here may have changed.
    pub fn changed(&self) -> Pending<()> {
        let (complete, waited) = pending();
        let mut waits = self.waits.lock_recover();
        waits.retain(|wait| !wait.is_abandoned());
        waits.push(complete);
        waited
    }

    /// Answer every watch waiting on who is here.
    fn wake(&self) {
        let waits = std::mem::take(&mut *self.waits.lock_recover());
        for wait in waits {
            wait.complete(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session is here from the moment it opens, called what its client says it is, and leaves
    /// as soon as it closes.
    #[test]
    fn a_session_is_here_from_its_opening_until_it_closes() {
        let who_is_here = WhoIsHere::default();
        let listed = |who_is_here: &WhoIsHere| {
            let mut here = Vec::new();
            who_is_here.add_here("studio", &mut here);
            here
        };
        assert!(listed(&who_is_here).is_empty());
        who_is_here.opened("run-1", "studio", "MCP client");
        let [one] = listed(&who_is_here)
            .try_into()
            .unwrap_or_else(|listed: Vec<Here>| {
                panic!("one entry while one session is here: {listed:?}")
            });
        assert_eq!(one.kind, "agent");
        assert_eq!(one.who, format!("{SESSION_WHO}run-1"));
        assert_eq!(one.name, "MCP client");
        who_is_here.named("run-1", "Claude Code");
        let [one] = listed(&who_is_here)
            .try_into()
            .unwrap_or_else(|listed: Vec<Here>| {
                panic!("the same entry moves on instead of another appearing: {listed:?}")
            });
        assert_eq!(
            one.who,
            format!("{SESSION_WHO}run-1"),
            "the run keeps its own entry"
        );
        assert_eq!(one.name, "Claude Code");
        who_is_here.closed("run-1");
        assert!(listed(&who_is_here).is_empty());
    }

    /// The person stopping a session leaves it out of who is here, and its run stays ended.
    #[test]
    fn a_stopped_session_leaves_and_stays_ended() {
        let who_is_here = WhoIsHere::default();
        who_is_here.opened("run-1", "studio", "Claude Code");
        assert!(!who_is_here.stop("other-room", "Claude Code"));
        assert!(who_is_here.stop("studio", "Claude Code"));
        assert!(
            !who_is_here.stop("studio", "Claude Code"),
            "a stop answers once"
        );
        let mut here = Vec::new();
        who_is_here.add_here("studio", &mut here);
        assert!(here.is_empty(), "{here:?}");
        assert!(who_is_here.ended("run-1"));
        assert!(!who_is_here.ended("run-2"));
    }

    /// A watch waiting on who is here is answered when a session opens, and one that gave up is
    /// not kept.
    #[test]
    fn a_change_answers_the_watches_waiting_on_it() {
        let who_is_here = WhoIsHere::default();
        let waiting = who_is_here.changed();
        let gone = who_is_here.changed();
        drop(gone);
        who_is_here.opened("run-1", "studio", "Claude Code");
        let answered = waiting
            .wait_timeout(std::time::Duration::from_secs(1))
            .ok()
            .flatten();
        assert_eq!(answered, Some(()));
    }
}
