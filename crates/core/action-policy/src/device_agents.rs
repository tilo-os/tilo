//! This device letting the person's agents from their other devices act here.
//!
//! An agent that comes from another of the person's devices signs with a key of its own. Whether
//! this computer accepts it is a grant the workspace keeps for this device, so it is the one
//! policy model every grant uses: the same grant the workspace's governance records, which the
//! person sets through Settings and an agent or a script calls alike. It names no one — its
//! grantee is an agent id no agent acts under — so it decides nothing by itself; the host that
//! admits an agent from another device reads it.
//!
//! Its answer is the person's: [`Answer::Ask`] accepts such an agent and puts a step its grants
//! ask first to the person on this computer, and [`Answer::Allow`] lets those steps go unasked.
//! No grant is off, as the switch it replaces was off unless the person turned it on.

use crate::{Answer, Grant, GrantId, Grantee, KindSet, Lifetime, TreePath, WorkspacePolicy};

/// What a device's grant id begins with.
const PREFIX: &str = "device-agents:";

/// The id of `device`'s grant in a workspace.
#[must_use]
pub fn grant_id(device: &[u8; 32]) -> GrantId {
    GrantId::new(type_and_device(device))
}

fn type_and_device(device: &[u8; 32]) -> String {
    format!("{PREFIX}{}", hex::encode(device))
}

/// The grant this computer keeps in `workspace`: the person's agents from their other devices
/// act here, with `answer` deciding whether a step they would ask about is put to the person
/// (`Ask`) or let go (`Allow`). `granted_by` is the person who set it.
#[must_use]
pub fn grant(workspace: &str, device: &[u8; 32], answer: Answer, granted_by: Grantee) -> Grant {
    Grant {
        id: grant_id(device),
        grantee: Grantee::agent(type_and_device(device)),
        node: TreePath::workspace(workspace),
        kinds: KindSet::ALL,
        answer,
        lifetime: Lifetime::UntilRevoked,
        granted_by,
    }
}

/// The answer `policy` keeps for `device`'s grant, or `None` when nobody turned it on here.
#[must_use]
pub fn answer(policy: &WorkspacePolicy, device: &[u8; 32]) -> Option<Answer> {
    let id = grant_id(device);
    policy
        .grants
        .iter()
        .find(|grant| grant.id == id && grant.kinds == KindSet::ALL)
        .map(|grant| grant.answer)
}

/// Whether `grant` is a device's grant of agents from the person's other devices.
#[must_use]
pub fn is_grant(grant: &Grant) -> bool {
    grant.id.as_str().starts_with(PREFIX)
}

/// The device a grant was kept for, when it is a device's grant ([`is_grant`]).
#[must_use]
pub fn device_of(grant: &Grant) -> Option<[u8; 32]> {
    let hex = grant.id.as_str().strip_prefix(PREFIX)?;
    hex::decode(hex).ok()?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Acting, Context, Role};

    const DEVICE: [u8; 32] = [7; 32];

    fn policy() -> WorkspacePolicy {
        WorkspacePolicy::new("w", "owner")
    }

    /// No grant is off; a grant kept for one device answers for that device and no other, and
    /// names no agent, so it decides nothing for anyone by itself.
    #[test]
    fn a_devices_grant_answers_only_where_it_was_kept_and_decides_nothing_by_itself() {
        let mut policy = policy();
        assert_eq!(answer(&policy, &DEVICE), None, "off unless turned on");
        let kept = grant("w", &DEVICE, Answer::Ask, Grantee::person("owner"));
        assert!(is_grant(&kept));
        assert_eq!(device_of(&kept), Some(DEVICE));
        policy.grants.push(kept);
        assert_eq!(answer(&policy, &DEVICE), Some(Answer::Ask));
        assert_eq!(
            answer(&policy, &[8; 32]),
            None,
            "another device's is not this one's"
        );

        let cx = Context::at_secs(1);
        let node = TreePath::workspace("w");
        for (name, acting) in [
            ("a person", Acting::person("someone")),
            (
                "an agent",
                Acting::agent("claude-code-1a2b3c").acting_for(Acting::person("someone")),
            ),
        ] {
            let authority = crate::Authority::of_acting(&acting, &policy, &[]);
            for kind in crate::ActionKind::ALL {
                let decided = authority.decide(&node, kind, &cx);
                assert!(
                    decided.answer == Answer::Deny,
                    "{name} is not decided by the device's grant: {decided:?}"
                );
            }
        }
        // The person's own grants still decide their own actions.
        policy.members.insert("editor".into(), Role::Editor);
        assert_eq!(
            crate::Authority::of_acting(&Acting::person("editor"), &policy, &[])
                .decide(&node, crate::ActionKind::Read, &cx)
                .answer,
            Answer::Allow
        );
    }

    /// A grant that is not one of these is no device grant.
    #[test]
    fn only_a_device_grant_is_one() {
        let other = Grant::new(
            "role:editor:0",
            Grantee::role(Role::Editor),
            TreePath::workspace("w"),
            KindSet::READ,
            Answer::Allow,
            Grantee::person("owner"),
        );
        assert!(!is_grant(&other));
        assert_eq!(device_of(&other), None);
    }
}
