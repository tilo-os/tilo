//! A write to the person's own store naming its invoker is decided as that
//! invoker, bounded by the device that signed it.

use collab_types::ChangeAuthor as _;
use tilo_action_policy::{Acting, ActionKind, Answer, Context, TreePath, roles};

use super::OwnStoreAuthor;

const STORE: &str = "personal-store";
const DEVICE: &str = "0a0a";

fn author() -> OwnStoreAuthor<'static> {
    OwnStoreAuthor {
        device: DEVICE.to_owned(),
        store: STORE,
        subtree: None,
    }
}

fn write() -> TreePath {
    TreePath::workspace(STORE)
        .app("so.tilo.files")
        .kind("files.tree")
        .resource("tree")
        .action("write")
}

fn answer(invoker: Option<&Acting>, node: &TreePath, kind: ActionKind) -> Answer {
    author().answer(invoker, node, kind, &Context::at_secs(1))
}

tilo_test_env::scenario! {
    fn an_agents_write_to_the_own_store_is_decided_as_the_agent;

    phase the_devices_own_write_is_its_own;
    {
        assert_eq!(answer(None, &write(), ActionKind::Undoable), Answer::Allow);
    }

    phase an_agent_the_device_signs_for_writes_as_its_approval_allows;
    {
        let agent = Acting::agent("claude").in_session("session-1");
        assert_eq!(answer(Some(&agent), &write(), ActionKind::Undoable), Answer::Allow);
        let governs = roles::governance_action(STORE, roles::governance::INVITE);
        assert_eq!(answer(None, &governs, roles::GOVERNANCE_KIND), Answer::Allow);
        assert_eq!(
            answer(Some(&agent), &governs, roles::GOVERNANCE_KIND),
            Answer::Deny,
            "the approval never governs"
        );
    }

    phase an_agent_working_for_someone_else_does_nothing_here;
    {
        let elsewhere = Acting::agent("claude").acting_for(Acting::person("someone-else"));
        assert_eq!(answer(Some(&elsewhere), &write(), ActionKind::Undoable), Answer::Deny);
    }
}
