//! Workspace policy as the broker hosts read it. The decisions themselves
//! live on [`tilo_desktop_model::Workspace`]; these tests pin the broker's
//! reliance on them sampling one clock.
#[cfg(test)]
mod retained_clock_tests {
    use collab_types::caps::{CapDecision, CapOverride, ResourceRef, Scope, Subject, Verb};
    use collab_types::role::Role as CapRole;
    #[test]
    fn retained_policy_clock_samples_membership_and_grant_at_same_time() {
        let mut workspace =
            crate::test_support::Workspace::new(crate::test_support::WorkspaceId(2), "Timed room");
        let user = "member".to_string();
        workspace.grant_capability(
            CapOverride::membership(
                Subject::person(&user),
                workspace.room.to_string(),
                Verb::Read,
            )
            .with_expiry(100),
        );
        workspace.grant_capability(
            CapOverride::grant(
                Subject::person(&user),
                Scope::App("app".into()),
                Verb::Write,
            )
            .with_expiry(100),
        );
        let before = workspace.actor_for_person_at(&user, 50);
        assert_eq!(workspace.role_of_at(&user, 50), Some(CapRole::Viewer));
        assert_eq!(
            workspace.decide_at(&before, &ResourceRef::App("app".into()), Verb::Write, 50),
            CapDecision::Allow
        );
        assert_eq!(workspace.role_of_at(&user, 101), None);
        assert_ne!(
            workspace.decide_at(
                &workspace.actor_for_person_at(&user, 101),
                &ResourceRef::App("app".into()),
                Verb::Write,
                101
            ),
            CapDecision::Allow
        );
    }
}

#[cfg(test)]
mod effect_receipt_redaction_tests {
    #[test]
    fn only_broker_owned_terminal_facets_enter_public_receipts() {
        let invocation = tilo_actions::InvocationId::new();
        let context = tilo_actions::ActionContext {
            facets: vec![
                tilo_actions::ActionFacet::new("private.argument", "secret text"),
                tilo_actions::ActionFacet::new("tilo.invocation", "forged"),
                tilo_actions::ActionFacet::new("tilo.receipt", "forged"),
            ],
            ..Default::default()
        };
        let receipt = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
            invocation,
            "person",
            context,
            tilo_actions::EffectReceiptStatus::Succeeded,
            tilo_clock::unix_now_i64(),
        );
        assert_eq!(
            receipt.context.facets,
            vec![
                tilo_actions::ActionFacet::new("tilo.receipt", "effect"),
                tilo_actions::ActionFacet::new("tilo.invocation", invocation.0.to_string()),
            ]
        );
        assert!(
            !serde_json::to_string(&receipt)
                .unwrap()
                .contains("secret text")
        );
    }
}
