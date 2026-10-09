//! The public receipt of an effect, shared by all broker hosts.
use tilo_desktop_model::WorkspaceEffectReceipt;

fn bounded_receipt_text(value: &str, max: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

fn redact_receipt_context(context: &mut tilo_actions::ActionContext) {
    if let Some(item) = &mut context.item {
        item.id = bounded_receipt_text(&item.id, 512);
        item.label = bounded_receipt_text(&item.label, 200);
    }
    // What the effect was is public; what it was given is not.
    if let Some(what) = &mut context.what {
        what.forget_args();
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
    context.facets.truncate(32);
    for facet in &mut context.facets {
        facet.key = bounded_receipt_text(&facet.key, 96);
        facet.value = bounded_receipt_text(&facet.value, 160);
    }
}

/// Construct the same bounded public receipt for native and standalone owners.
/// No invocation arguments, output, streamed items or private failure text enter it.
pub fn redacted_effect_receipt(
    invocation: tilo_actions::InvocationId,
    actor: &str,
    mut context: tilo_actions::ActionContext,
    status: tilo_actions::EffectReceiptStatus,
) -> WorkspaceEffectReceipt {
    let completed_at = tilo_clock::unix_now_i64();
    // Arbitrary application facets may contain private argument summaries.
    // Only broker-owned terminal identifiers enter this public audit.
    context.facets.clear();
    context
        .facets
        .push(tilo_actions::ActionFacet::new("tilo.receipt", "effect"));
    context.facets.push(tilo_actions::ActionFacet::new(
        "tilo.invocation",
        invocation.0.to_string(),
    ));
    redact_receipt_context(&mut context);
    WorkspaceEffectReceipt {
        invocation,
        actor: bounded_receipt_text(actor, 160),
        context,
        status,
        completed_at,
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
        let receipt = super::redacted_effect_receipt(
            invocation,
            "person",
            context,
            tilo_actions::EffectReceiptStatus::Succeeded,
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
