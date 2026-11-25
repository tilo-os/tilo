//! A workspace's append-only history stays out of what reads the desktop as
//! it is now: [`history::without_history`] serialisation.

use tilo_actions::receipt::WorkspaceEffectReceipt;
use tilo_actions::{ActionContext, EffectReceiptStatus, InvocationId};
use tilo_desktop_model::*;

fn receipt(n: u128) -> WorkspaceEffectReceipt {
    WorkspaceEffectReceipt {
        invocation: InvocationId(uuid::Uuid::from_u128(n)),
        actor: tilo_actions::MemberId::person("owner").into(),
        context: ActionContext::default(),
        status: EffectReceiptStatus::Succeeded,
        completed_at: 42,
    }
}

/// A desktop whose one workspace has `receipts` effect receipts.
fn desktop(receipts: u128) -> Layout {
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Home");
    workspace.policy.set_owner("owner");
    workspace.strips.push(Strip::new(StripId(7), "Main"));
    workspace.effect_receipts = (0..receipts).map(receipt).collect();
    Layout {
        workspaces: vec![workspace],
        ..Layout::default()
    }
}

tilo_test_env::scenario! {
    fn without_history_leaves_history_out_of_what_it_serialises;

    phase serialising_without_history_leaves_out_every_history_field;
    {
        let tree = desktop(3);
        let full = serde_json::to_value(&tree).unwrap();
        let workspace = &full["workspaces"][0];
        assert_eq!(workspace["effect_receipts"].as_array().unwrap().len(), 3);

        let current: serde_json::Value =
            history::without_history(|| serde_json::to_value(&tree)).unwrap();
        let workspace = &current["workspaces"][0];
        assert!(
            workspace.get("effect_receipts").is_none(),
            "receipts are history"
        );
        // Everything that is not history is still there.
        assert_eq!(workspace["strips"][0]["name"], "Main");
        assert_eq!(workspace["policy"]["owner"], "owner");

        // The switch is scoped: serialising after it writes history again, so
        // persistence and replication are never affected.
        let again = serde_json::to_value(&tree).unwrap();
        assert_eq!(again, full);
    }

    phase a_dump_without_history_is_the_same_size_whatever_the_history;
    {
        let size = |receipts| {
            let tree = desktop(receipts);
            history::without_history(|| serde_json::to_vec(&tree))
                .unwrap()
                .len()
        };
        assert_eq!(size(0), size(10_000));
    }

    phase nested_switches_restore_the_outer_one;
    {
        let tree = desktop(1);
        let inner_then_outer = history::without_history(|| {
            history::without_history(|| ());
            serde_json::to_value(&tree)
        })
        .unwrap();
        assert!(
            inner_then_outer["workspaces"][0]
                .get("effect_receipts")
                .is_none()
        );
    }
}
