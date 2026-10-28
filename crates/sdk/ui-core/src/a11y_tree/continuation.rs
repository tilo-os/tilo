//! Inert identity of a generated SetValue request and its current UI route.
//! This does not identify a document target or a future hit-tested recipient.
use super::{Action, SemanticAction, SemanticNode};
use tilo_actions::{
    ActionSpec, ContinuationArguments, ContinuationEvidence, ContinuationKey, ContinuationQuery,
};

struct Route {
    parts: Vec<Vec<u8>>,
    handler: ContinuationKey,
}

impl Route {
    fn capture(node: &SemanticNode, action: &SemanticAction) -> Option<Self> {
        if !node.durable_identity
            || !matches!(
                action.kind,
                super::SemanticActionKind::Verb(Action::SetValue(_))
            )
        {
            return None;
        }
        let alias = node.widget_taffy_id?;
        alias.checked_add(1)?;
        let target = node.semantic_target.as_deref()?;
        if target.starts_with("ordinary-confirmation:") {
            // That branch uses separate palette state and clears admissions.
            return None;
        }
        let handler_route = match target
            .strip_prefix("tilo.key-action:")
            .and_then(|target| target.rsplit_once(':'))
        {
            Some((_, path)) => {
                for segment in path.split('/') {
                    segment.parse::<u64>().ok()?;
                }
                b"current-alias/perform-action-at/fallback-pointer-events-v1".as_slice()
            }
            None => b"current-alias/perform-semantic-action/fallback-pointer-events-v1".as_slice(),
        };
        let mut bounds = Vec::with_capacity(33);
        match node.bounds {
            None => bounds.push(0),
            Some(rect) => {
                let corners = [rect.x0, rect.y0, rect.x1, rect.y1];
                let center = rect.center();
                if !corners.iter().all(|value| value.is_finite())
                    || !center.x.is_finite()
                    || !center.y.is_finite()
                {
                    return None;
                }
                bounds.push(1);
                for value in corners {
                    bounds.extend_from_slice(&value.to_bits().to_le_bytes());
                }
            }
        }
        Some(Self {
            parts: vec![
                node.id.as_str().as_bytes().to_vec(),
                action.name.as_bytes().to_vec(),
                serde_json::to_vec(&node.role).ok()?,
                alias.to_le_bytes().to_vec(),
                target.as_bytes().to_vec(),
                bounds,
            ],
            handler: ContinuationKey::new(
                "tilo.ui.semantic-set-value.handler.v1",
                [
                    b"raw-utf8/SemanticOperation/current-tree-v1".as_slice(),
                    handler_route,
                ],
            ),
        })
    }

    fn evidence(&self, query: ContinuationQuery<'_>) -> Option<ContinuationEvidence> {
        match query {
            ContinuationQuery::Draft => Some(ContinuationEvidence::Draft {
                resolution_domain: ContinuationKey::new(
                    "tilo.ui.semantic-set-value.route.v1",
                    self.parts.clone(),
                ),
                handler: self.handler.clone(),
            }),
            ContinuationQuery::Exact(arguments) => {
                let value = match arguments {
                    ContinuationArguments::Text(value) => value,
                    ContinuationArguments::Json(arguments) => {
                        tilo_actions::sole_text_argument(arguments)?
                    }
                    ContinuationArguments::Immediate => return None,
                };
                let mut parts = self.parts.clone();
                parts.push(value.as_bytes().to_vec());
                Some(ContinuationEvidence::Exact {
                    target: ContinuationKey::new("tilo.ui.semantic-set-value.request.v1", parts),
                    handler: self.handler.clone(),
                })
            }
        }
    }
}

pub(super) fn annotate(
    spec: ActionSpec,
    node: &SemanticNode,
    action: &SemanticAction,
) -> ActionSpec {
    // The caller has already excluded duplicate/non-durable semantic ids.
    // Actual owner domains and lifetimes must independently cover callback
    // semantics. Fresh tree lookup, authorization and dispatch are unchanged.
    let Some(route) = Route::capture(node, action) else {
        return spec;
    };
    spec.with_continuation_identity(move |query| route.evidence(query))
}
