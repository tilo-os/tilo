//! Receipts of effects outside the workspace's documents.

use serde::{Deserialize, Serialize};

/// How many receipts a workspace keeps: the newest, by when their effects
/// completed.
///
/// A receipt is not history in its own right. It says who performed an
/// effect that left no row in an app's document, so the workspace's activity
/// feed can attribute the row it did leave, and stand the effect in as an
/// entry of its own; that feed holds a window of the newest 200 entries.
/// Receipts past that window serve nothing and would make every change of the
/// layout cost everything ever done, so a workspace keeps a multiple of the
/// window: room for the entries a feed has not built yet, and for a retried
/// effect to find the receipt it already has.
pub const MAX_EFFECT_RECEIPTS: usize = 512;

/// Keep the newest [`MAX_EFFECT_RECEIPTS`] of `receipts`, the oldest dropped.
/// Whether any were.
pub fn bounded_receipts(receipts: &mut Vec<WorkspaceEffectReceipt>) -> bool {
    if receipts.len() <= MAX_EFFECT_RECEIPTS {
        return false;
    }
    receipts.sort_by_key(|receipt| (receipt.completed_at, receipt.invocation.0));
    let excess = receipts.len().saturating_sub(MAX_EFFECT_RECEIPTS);
    receipts.drain(..excess);
    true
}

/// Append-only, broker-authenticated record of an external side effect.
/// Arguments, output values, produced resource addresses, failure messages,
/// and failure details are deliberately absent from this durable shape.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceEffectReceipt {
    pub invocation: crate::InvocationId,
    pub actor: crate::Actor,
    pub context: crate::ActionContext,
    pub status: crate::EffectReceiptStatus,
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
        invocation: crate::InvocationId,
        actor: &crate::Actor,
        mut context: crate::ActionContext,
        status: crate::EffectReceiptStatus,
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
        invocation: crate::InvocationId,
        actor: &crate::Actor,
        mut context: crate::ActionContext,
        status: crate::EffectReceiptStatus,
        completed_at: i64,
    ) -> Self {
        context.facets = vec![
            crate::ActionFacet::new("tilo.receipt", "effect"),
            crate::ActionFacet::new("tilo.invocation", invocation.0.to_string()),
        ];
        Self {
            invocation,
            actor: actor.clone(),
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
            && actor_is_bounded(&self.actor)
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

/// Whether `actor` names someone in no more than 160 characters, none of them control
/// characters.
#[must_use]
pub fn actor_is_bounded(actor: &crate::Actor) -> bool {
    let id = actor.id();
    !id.is_empty() && id.chars().count() <= 160 && !id.chars().any(char::is_control)
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

fn bounded_receipt_context(mut context: crate::ActionContext) -> crate::ActionContext {
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
        if let Some(label) = &mut group.label {
            *label = bounded_receipt_text(label, 200);
        }
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
        let invocation = crate::InvocationId::new();
        let context = crate::ActionContext {
            facets: vec![
                crate::ActionFacet::new("private.argument", "secret text"),
                crate::ActionFacet::new("tilo.invocation", "forged"),
                crate::ActionFacet::new("tilo.receipt", "forged"),
            ],
            what: Some(crate::Did::verb(
                crate::ActionKey::new("so.tilo.notes", "note", "append"),
                [("text".to_owned(), crate::Scalar::text("secret words"))],
            )),
            ..Default::default()
        };
        let receipt = super::WorkspaceEffectReceipt::redacted(
            invocation,
            &crate::MemberId::person("person").into(),
            context,
            crate::EffectReceiptStatus::Succeeded,
            0,
        );
        assert_eq!(
            receipt.context.facets,
            vec![
                crate::ActionFacet::new("tilo.receipt", "effect"),
                crate::ActionFacet::new("tilo.invocation", invocation.0.to_string()),
            ]
        );
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(!json.contains("secret text"));
        assert!(!json.contains("secret words"));
        assert!(receipt.is_redacted());
    }
}

#[cfg(test)]
mod receipt_bound_tests {
    use super::{MAX_EFFECT_RECEIPTS, WorkspaceEffectReceipt, bounded_receipts};

    fn receipt(at: i64) -> WorkspaceEffectReceipt {
        WorkspaceEffectReceipt::redacted(
            crate::InvocationId::new(),
            &crate::MemberId::person("owner").into(),
            crate::ActionContext::default(),
            crate::EffectReceiptStatus::Succeeded,
            at,
        )
    }

    /// Ten workspaces' worth of receipts, ten times the window they serve:
    /// a workspace keeps the newest, and one more effect costs the bound,
    /// not everything ever done.
    #[test]
    fn a_workspace_keeps_the_newest_receipts_whatever_it_did() {
        let mut held: Vec<WorkspaceEffectReceipt> = (0..i64::try_from(MAX_EFFECT_RECEIPTS * 10)
            .unwrap())
            .map(receipt)
            .collect();
        assert!(bounded_receipts(&mut held), "ten times the window is cut");
        assert_eq!(held.len(), MAX_EFFECT_RECEIPTS);
        let oldest = held.iter().map(|held| held.completed_at).min().unwrap();
        assert_eq!(
            oldest,
            i64::try_from(MAX_EFFECT_RECEIPTS * 9).unwrap(),
            "the oldest were the ones dropped"
        );
        let kept_at_bound = held.len();
        held.push(receipt(i64::MAX));
        assert!(bounded_receipts(&mut held));
        assert_eq!(held.len(), kept_at_bound, "one more costs the same");
        // Under the bound nothing is dropped.
        let mut few = vec![receipt(1), receipt(2)];
        assert!(!bounded_receipts(&mut few));
        assert_eq!(few.len(), 2);
    }
}
