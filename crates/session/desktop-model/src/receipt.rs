//! Receipts of effects outside the workspace's documents.

use serde::{Deserialize, Serialize};

/// Append-only, broker-authenticated record of an external side effect.
/// Arguments, output values, produced resource addresses, failure messages,
/// and failure details are deliberately absent from this durable shape.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceEffectReceipt {
    pub invocation: tilo_actions::InvocationId,
    pub actor: String,
    pub context: tilo_actions::ActionContext,
    pub status: tilo_actions::EffectReceiptStatus,
    pub completed_at: i64,
}

impl WorkspaceEffectReceipt {
    /// The bounded public receipt of `invocation`'s terminal `status`: every
    /// text is bounded and stripped of control characters, what was done
    /// keeps none of its arguments, and the context keeps only the receipt's
    /// own facets. Arbitrary application facets and arguments may carry
    /// private values, so none enter this audit.
    #[must_use]
    pub fn redacted(
        invocation: tilo_actions::InvocationId,
        actor: &str,
        mut context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
        completed_at: i64,
    ) -> Self {
        if let Some(what) = &mut context.what {
            what.forget_args();
        }
        Self::recorded(invocation, actor, context, status, completed_at)
    }

    /// [`Self::redacted`], keeping what was done with its arguments: for a
    /// receipt whose arguments are the record itself, such as the grant a
    /// person gave.
    #[must_use]
    pub fn recorded(
        invocation: tilo_actions::InvocationId,
        actor: &str,
        mut context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
        completed_at: i64,
    ) -> Self {
        context.facets = vec![
            tilo_actions::ActionFacet::new("tilo.receipt", "effect"),
            tilo_actions::ActionFacet::new("tilo.invocation", invocation.0.to_string()),
        ];
        Self {
            invocation,
            actor: bounded_receipt_text(actor, 160),
            context: bounded_receipt_context(context),
            status,
            completed_at,
        }
    }

    /// Whether this receipt is exactly what [`Self::recorded`] produces for
    /// its own invocation.
    #[must_use]
    pub fn is_redacted(&self) -> bool {
        let facets = &self.context.facets;
        !self.invocation.0.is_nil()
            && !self.actor.is_empty()
            && self.actor == bounded_receipt_text(&self.actor, 160)
            && self.completed_at >= 0
            && self.context == bounded_receipt_context(self.context.clone())
            && facets.len() == 2
            && facets
                .iter()
                .any(|facet| facet.key == "tilo.receipt" && facet.value == "effect")
            && facets.iter().any(|facet| {
                facet.key == "tilo.invocation" && facet.value == self.invocation.0.to_string()
            })
    }
}

fn bounded_receipt_text(value: &str, max: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

fn bounded_receipt_context(
    mut context: tilo_actions::ActionContext,
) -> tilo_actions::ActionContext {
    if let Some(item) = &mut context.item {
        item.id = bounded_receipt_text(&item.id, 512);
        item.label = bounded_receipt_text(&item.label, 200);
    }
    if let Some(app) = &mut context.app {
        app.id = bounded_receipt_text(&app.id, 160);
        app.label = bounded_receipt_text(&app.label, 160);
    }
    if let Some(resource) = &mut context.resource {
        resource.id = bounded_receipt_text(&resource.id, 512);
        resource.kind = bounded_receipt_text(&resource.kind, 160);
        resource.label = bounded_receipt_text(&resource.label, 200);
    }
    context.purpose = context
        .purpose
        .as_deref()
        .map(|value| bounded_receipt_text(value, 512))
        .filter(|value| !value.is_empty());
    if let Some(group) = &mut context.group {
        group.id = bounded_receipt_text(&group.id, 160);
        group.label = bounded_receipt_text(&group.label, 200);
        group.kind = bounded_receipt_text(&group.kind, 160);
    }
    for facet in &mut context.facets {
        facet.key = bounded_receipt_text(&facet.key, 96);
        facet.value = bounded_receipt_text(&facet.value, 160);
    }
    context
}

#[cfg(test)]
mod effect_receipt_redaction_tests {
    #[test]
    fn only_broker_owned_terminal_facets_and_no_arguments_enter_public_receipts() {
        let invocation = tilo_actions::InvocationId::new();
        let context = tilo_actions::ActionContext {
            facets: vec![
                tilo_actions::ActionFacet::new("private.argument", "secret text"),
                tilo_actions::ActionFacet::new("tilo.invocation", "forged"),
                tilo_actions::ActionFacet::new("tilo.receipt", "forged"),
            ],
            what: Some(tilo_actions::Did::verb(
                tilo_actions::ActionKey::new("so.tilo.notes", "note", "append"),
                [(
                    "text".to_owned(),
                    tilo_actions::Scalar::text("secret words"),
                )],
            )),
            ..Default::default()
        };
        let receipt = super::WorkspaceEffectReceipt::redacted(
            invocation,
            "person",
            context,
            tilo_actions::EffectReceiptStatus::Succeeded,
            0,
        );
        assert_eq!(
            receipt.context.facets,
            vec![
                tilo_actions::ActionFacet::new("tilo.receipt", "effect"),
                tilo_actions::ActionFacet::new("tilo.invocation", invocation.0.to_string()),
            ]
        );
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(!json.contains("secret text"));
        assert!(!json.contains("secret words"));
        assert!(receipt.is_redacted());
    }
}
