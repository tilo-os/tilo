//! Who may make a change, by the verb it names and the kind it writes: the grant the workspace's
//! governance gives the change's author, read once for a batch of proposals and judged at the
//! governance the change was written under ([`tilo_peer_state::StatePeer::permissions_as_written`]).
//!
//! The store checks that what a change writes lies within what its verb declares it writes; this
//! decides whether its author may invoke that verb on each kind it touches. A step through
//! history that names the verb it takes back is judged by that verb's grant too
//! ([`VerbGrants::may_take_back`]), so taking a change back asks for no less than making it.
//! Platform verbs go on keeping their own checks ([`keeps_its_own_check`]): what they admit is
//! the store's rule, not a grant. An agent's change is judged by the lesser of the agent and the
//! person it acts for, which the authority of the access it was written under already is.

use web_time::SystemTime;

use tilo_action_policy::MemberAccess;
use tilo_resource_store::Schema;

/// The grant lookup for one proposal batch: the author's access at the governance the batch's
/// write names, and the app whose kinds its verbs are judged on.
pub(super) struct VerbGrants {
    access: Option<MemberAccess>,
    app: String,
    author: [u8; 32],
    now: SystemTime,
}

impl VerbGrants {
    /// The lookup for a write of `app` by `author`, its access `access` as the governance the
    /// write was made under names it. No access refuses every change but a platform verb's.
    pub(super) fn of(
        access: Option<MemberAccess>,
        app: &str,
        author: [u8; 32],
        now: SystemTime,
    ) -> Self {
        Self {
            access,
            app: app.to_owned(),
            author,
            now,
        }
    }

    /// What a call of `app`'s action `action` needs as `schema` declares it: the kind it is judged
    /// at and the field it sits under, where its declaration names one. `None` when no declaration
    /// of `app` names it: such a call is refused, never judged at a kind it was given.
    pub(super) fn call_need(
        schema: &Schema,
        app: &str,
        action: &str,
    ) -> Option<tilo_actions::ActionNeed> {
        schema.declared_need(app, action)
    }

    /// Whether `author` may invoke `verb` on a record of `kind`, at the batch's basis: the grant
    /// the workspace's governance gives them at that verb's node, for a change that is undoable
    /// ground. A platform verb keeps its own check. Another author's change is not this batch's.
    pub(super) fn may(&self, author: &[u8; 32], verb: &str, kind: &str) -> bool {
        if keeps_its_own_check(verb) {
            return true;
        }
        if *author != self.author {
            return false;
        }
        let Some(access) = &self.access else {
            return false;
        };
        let node = access
            .policy
            .node()
            .app(self.app.as_str())
            .kind(kind)
            .action(verb);
        access.allows(&node, tilo_action_types::ActionKind::Undoable, self.now)
    }

    /// Whether `author` may take back the step `stamp` names, on a record of `kind`: a step
    /// through history that says which step it moves is judged by the grant of that step's verb
    /// too, so taking a change back does no more than making it did. A step that names no verb
    /// to take back, or no step at all, keeps its own check.
    pub(super) fn may_take_back(
        &self,
        author: &[u8; 32],
        stamp: &collab_types::Stamp,
        kind: &str,
    ) -> bool {
        let Some(tilo_actions::Did::History(step)) = stamp
            .context
            .as_deref()
            .and_then(|context| context.what.as_ref())
        else {
            return true;
        };
        if step.verb.is_empty() {
            return true;
        }
        self.may(author, &step.verb, kind)
    }
}

/// Whether `verb`'s admission is the store's own, never a grant's: the platform's verbs (every
/// name it writes under, `$create`, `$move`, `$branch`, `$origin` and the outbox's), the verb of
/// migration, which declares what it writes, and the history marks (`checkpoint`, `describe`,
/// `link`), which write no kind for a grant to be judged on. A mark's author is decided by the
/// rule that its stamp names them ([`super::platform_write_belongs`]).
pub(super) fn keeps_its_own_check(verb: &str) -> bool {
    verb.starts_with('$') || matches!(verb, "migrate" | "checkpoint" | "describe" | "link")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tilo_action_policy::{
        Answer, Grant, Grantee, MemberAccess, Role, TreePath, WorkspacePolicy,
    };
    use tilo_action_types::KindSet;
    use tilo_member_types::{Actor, MemberId};

    use super::VerbGrants;

    const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
    const OWNER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const EDITOR: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const DEVICE: [u8; 32] = [7; 32];
    const APP: &str = "so.tilo.tasks";
    const KIND: &str = "tl.so.tilo.tasks.Task";
    const VERB: &str = "archive";

    fn node() -> TreePath {
        TreePath::workspace(WORKSPACE)
            .app(APP)
            .kind(KIND)
            .action(VERB)
    }

    /// A workspace whose editor does or does not hold `archive` on the task kind: the grant a
    /// workspace adds or takes away at that verb's node.
    fn policy(answer: Answer) -> WorkspacePolicy {
        let mut policy = WorkspacePolicy::new(WORKSPACE, OWNER);
        policy.members.insert(EDITOR.to_owned(), Role::Editor);
        policy.grants.push(Grant::new(
            "verb",
            Grantee::person(EDITOR),
            node(),
            KindSet::UNDOABLE,
            answer,
            Grantee::role(Role::Owner),
        ));
        policy
    }

    fn grants(policy: WorkspacePolicy) -> VerbGrants {
        let access = MemberAccess::person(Arc::new(policy), EDITOR);
        VerbGrants::of(Some(access), APP, DEVICE, tilo_clock::wall())
    }

    #[test]
    fn a_member_without_the_verb_may_not_make_its_change_and_one_with_it_may() {
        let refused = grants(policy(Answer::Deny));
        assert!(
            !refused.may(&DEVICE, VERB, KIND),
            "an editor without it is refused"
        );
        assert!(
            refused.may(&DEVICE, "edit", KIND),
            "the editor's ordinary edits stand"
        );
        let admitted = grants(policy(Answer::Allow));
        assert!(
            admitted.may(&DEVICE, VERB, KIND),
            "a granted verb is admitted"
        );
    }

    #[test]
    fn the_lookup_judges_at_the_basis_it_was_built_from() {
        // The change was written while the verb was granted, and arrives after it was taken
        // away: every holder judges it by the grants the write named, which are these.
        let before = grants(policy(Answer::Allow));
        let after = grants(policy(Answer::Deny));
        assert!(
            before.may(&DEVICE, VERB, KIND),
            "the grant it was written under admits it"
        );
        assert!(
            !after.may(&DEVICE, VERB, KIND),
            "the revoking grant refuses it"
        );
    }

    #[test]
    fn an_agents_change_is_judged_by_its_person_too() {
        let policy = policy(Answer::Deny);
        let access = MemberAccess::person(Arc::new(policy), EDITOR)
            .acting_as(Actor::Member(MemberId::agent("scout", EDITOR)), Vec::new());
        let grants = VerbGrants::of(Some(access), APP, DEVICE, tilo_clock::wall());
        assert!(
            !grants.may(&DEVICE, VERB, KIND),
            "an agent does no more than the person it acts for"
        );
    }

    #[test]
    fn another_devices_change_is_no_business_of_this_lookup() {
        let grants = grants(policy(Answer::Allow));
        assert!(!grants.may(&[9; 32], VERB, KIND));
    }

    #[test]
    fn a_platform_verb_keeps_its_own_check() {
        let no_access = VerbGrants::of(None, APP, DEVICE, tilo_clock::wall());
        for verb in [
            "$create", "$move", "$branch", "$origin", "$settle", "migrate",
        ] {
            assert!(
                no_access.may(&DEVICE, verb, KIND),
                "`{verb}` is judged by the store, not a grant"
            );
        }
        assert!(
            !no_access.may(&DEVICE, VERB, KIND),
            "no access admits no app verb"
        );
    }

    /// A change under `verb` whose history step names the invocation it moves and the verb that
    /// invocation was made as, as an undo or a restore writes one.
    fn undo_stamp(verb: &str, takes_back: &str) -> collab_types::Stamp {
        use tilo_actions::{ActionContext, Did, HistoryOp, HistoryStep};
        collab_types::Stamp {
            verb: verb.to_owned(),
            context: Some(Box::new(ActionContext {
                what: Some(Did::History(HistoryStep {
                    op: HistoryOp::Undo,
                    steps: 1,
                    of: None,
                    undoes: Some("the invocation".to_owned()),
                    verb: takes_back.to_owned(),
                })),
                ..ActionContext::default()
            })),
            ..collab_types::Stamp::default()
        }
    }

    /// A step taken back is admitted by the grant of the verb it takes back, as well as by its
    /// own: an editor without that verb may not take its change back, and one with it may. The
    /// path a store's undo writes, which names the verb as its own, is judged the same way.
    #[test]
    fn a_step_taken_back_is_judged_by_the_verb_it_takes_back() {
        let refused = grants(policy(Answer::Deny));
        let undoing = undo_stamp("$undo", VERB);
        assert!(
            !refused.may_take_back(&DEVICE, &undoing, KIND),
            "an editor without the verb it takes back may not take its change back"
        );
        assert!(
            refused.may(&DEVICE, "$undo", KIND),
            "the platform's own verb keeps its check, which is why the step's is asked too"
        );
        let admitted = grants(policy(Answer::Allow));
        assert!(
            admitted.may_take_back(&DEVICE, &undoing, KIND),
            "with the verb it takes back, the step is admitted"
        );
        // A store's undo names the verb it takes back as its own, and `may` asks it directly.
        let named = undo_stamp(VERB, VERB);
        assert!(!refused.may(&DEVICE, &named.verb, KIND));
        assert!(!refused.may_take_back(&DEVICE, &named, KIND));
        assert!(admitted.may(&DEVICE, &named.verb, KIND));
        // A restore names no verb it takes back: its own check stands.
        let restoring = undo_stamp("$restore", "");
        assert!(refused.may_take_back(&DEVICE, &restoring, KIND));
    }

    /// The lookup's cost on a realistic batch: one access read, then a grant question per
    /// touched kind. Run with `--nocapture` to read the numbers.
    #[test]
    fn a_batchs_grant_questions_are_cheap() {
        const TOUCHED: u32 = 1_000;
        let grants = grants(policy(Answer::Allow));
        let at = std::time::Instant::now();
        let mut allowed: u32 = 0;
        for _ in 0..TOUCHED {
            allowed += u32::from(grants.may(&DEVICE, VERB, KIND));
        }
        let took = at.elapsed();
        assert_eq!(allowed, TOUCHED);
        eprintln!(
            "verb grants: {TOUCHED} questions in {took:?} ({:.3} µs each); the old closure \
             answered the batch in one bool",
            took.as_secs_f64() * 1e6 / f64::from(TOUCHED)
        );
    }
}
