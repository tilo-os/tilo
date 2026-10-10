//! Exact, read-only action geometry shared by native and retained browser hosts.

pub(crate) mod publication;

pub(crate) struct PresenceGeometry<'a> {
    pub tree: &'a crate::a11y_tree::SemanticTree,
    pub bindings: &'a [crate::widget::ControlBindingMetadata],
    pub published: &'a [(tilo_actions::InvocationId, Option<vello::kurbo::Rect>)],
}

/// Paint semantic activity against this frame's exact action bindings. Both
/// native and retained hosts use this projection; an unresolved action remains
/// in the activity rail rather than guessing a control from its label.
pub(crate) fn draw_action_presence(
    paint: &mut crate::paint::PaintCtx<'_>,
    presence: &mut crate::presence::PresenceCtx,
    geometry: PresenceGeometry<'_>,
    viewport: vello::kurbo::Rect,
    surface: &str,
    now: crate::time::Instant,
) -> bool {
    presence.clear_agent_marker_surface(surface);
    let mut resolved = Vec::new();
    for interaction in presence.visible_interactions(now) {
        let bounds = geometry
            .published
            .iter()
            .find(|(invocation, _)| *invocation == interaction.invocation)
            .map_or_else(
                || interaction_bounds(geometry.tree, geometry.bindings, &interaction),
                |(_, bounds)| *bounds,
            );
        let Some(bounds) = bounds else {
            continue;
        };
        let bounds = bounds.intersect(viewport);
        if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
            continue;
        }
        let user = crate::permissions::UserId(interaction.actor.id.clone());
        let label = presence.interaction_status_label(&interaction);
        presence.draw_action_outline_labeled_within(
            paint,
            &user,
            &label,
            bounds,
            f64::from(paint.theme.tokens.radii.detail),
            viewport,
        );
        presence.draw_agent_action_marker(paint, &interaction, bounds, viewport, surface, now);
        resolved.push(interaction.invocation);
    }
    let rail_painted = presence.draw_interactions_excluding(
        paint.scene,
        paint.transform,
        viewport,
        paint.text,
        paint.theme,
        now,
        &resolved,
    );
    !resolved.is_empty() || rail_painted
}

/// One visible expression of a domain action in the retained UI. The action
/// address remains the execution identity; this route is presentation
/// metadata for focus, presence, animation, and “show me where” behavior.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionUiRoute {
    pub address: tilo_actions::ActionAddress,
    pub control_id: String,
    /// Root-to-control semantic path. Fewer nodes is the cheaper UI route.
    pub path: Vec<crate::a11y_tree::SemanticId>,
    pub bounds: Option<vello::kurbo::Rect>,
    pub focused: bool,
    pub disabled: bool,
}

pub(crate) fn action_ui_routes(
    tree: &crate::a11y_tree::SemanticTree,
    bindings: &[crate::widget::ControlBindingMetadata],
    address: &tilo_actions::ActionAddress,
) -> Vec<ActionUiRoute> {
    let mut routes = Vec::new();
    visit_action_ui_routes(tree, bindings, address, &mut |binding, path, node| {
        routes.push(ActionUiRoute {
            address: address.clone(),
            control_id: binding.control_id.clone(),
            path: path.to_vec(),
            bounds: node.bounds,
            focused: node.state.focused,
            disabled: node.state.disabled,
        });
    });
    routes.sort_by(|left, right| {
        left.disabled
            .cmp(&right.disabled)
            .then_with(|| left.bounds.is_none().cmp(&right.bounds.is_none()))
            .then_with(|| left.path.len().cmp(&right.path.len()))
            .then_with(|| right.focused.cmp(&left.focused))
            .then_with(|| left.control_id.cmp(&right.control_id))
    });
    routes.dedup_by(|left, right| left.path.last() == right.path.last());
    routes
}

type ActionRouteVisitor<'tree, 'visitor> = dyn FnMut(
        &crate::widget::ControlBindingMetadata,
        &[crate::a11y_tree::SemanticId],
        &'tree crate::a11y_tree::SemanticNode,
    ) + 'visitor;

// Ranking is a public presentation policy. Exact resource geometry needs the
// same admitted candidates, but neither route allocation nor ranked sorting.
fn visit_action_ui_routes<'tree>(
    tree: &'tree crate::a11y_tree::SemanticTree,
    bindings: &[crate::widget::ControlBindingMetadata],
    address: &tilo_actions::ActionAddress,
    visit: &mut ActionRouteVisitor<'tree, '_>,
) {
    for binding in bindings {
        let paths = tree.control_paths_unranked(&binding.control_id);
        let leaves = paths
            .iter()
            .filter(|path| {
                !paths
                    .iter()
                    .any(|child| child.len() > path.len() && child.starts_with(path))
            })
            .collect::<Vec<_>>();
        // The flat binding inventory has no owner path. A reused control key
        // therefore cannot safely select any one of its disjoint expressions.
        if leaves.len() != 1 {
            continue;
        }
        for path in leaves {
            let Some(semantic_id) = path.last() else {
                continue;
            };
            // Match the canonical ResourceId emitted by SemanticTree.
            // Semantic node ids contain punctuation and address delimiters.
            let encoded_node_id = percent_encoding::utf8_percent_encode(
                semantic_id.as_str(),
                percent_encoding::NON_ALPHANUMERIC,
            );
            let semantic_address = address.resource == format!("ui:{encoded_node_id}")
                && tree.action(semantic_id, &address.action).is_some();
            let matches_address = semantic_address
                || matches!(
                    &binding.kind,
                    crate::widget::ControlBindingKind::Action(candidate) if candidate == address
                );
            if !matches_address {
                continue;
            }
            let semantic_bindings = bindings
                .iter()
                .filter(|candidate| candidate.control_id == binding.control_id)
                .filter(|candidate| {
                    matches!(
                        &candidate.kind,
                        crate::widget::ControlBindingKind::SemanticAction(target)
                            if target == binding.control_id.as_str()
                    )
                })
                .count();
            let matching_bindings = bindings
                .iter()
                .filter(|candidate| candidate.control_id == binding.control_id)
                .filter(|candidate| {
                    if semantic_address {
                        semantic_bindings == 0
                            || matches!(
                                &candidate.kind,
                                crate::widget::ControlBindingKind::SemanticAction(target)
                                    if target == binding.control_id.as_str()
                            )
                    } else {
                        matches!(
                            &candidate.kind,
                            crate::widget::ControlBindingKind::Action(candidate_address)
                                if candidate_address == address
                        )
                    }
                })
                .count();
            // One retained control may explicitly present several distinct
            // domain actions. Only bindings for this exact address establish
            // its route; duplicate declarations of the same address remain
            // ambiguous and fail closed.
            if matching_bindings != 1 {
                continue;
            }
            let Some(node) = tree.node(semantic_id) else {
                continue;
            };
            visit(binding, path, node);
        }
    }
}

/// Resolve only an unambiguous, currently bound control. Labels and positional
/// wire IDs cannot establish resource identity. Domain anchors without a shared
/// resolver remain app-level activity rather than guessed rectangles.
pub(crate) fn interaction_bounds(
    tree: &crate::a11y_tree::SemanticTree,
    bindings: &[crate::widget::ControlBindingMetadata],
    interaction: &tilo_actions::SemanticInteraction,
) -> Option<vello::kurbo::Rect> {
    let address = tilo_actions::ActionAddress::new(
        interaction.resource.id.clone(),
        interaction.action.id.clone(),
    );
    let canonical_id = if let Some(encoded) = address.resource.strip_prefix("ui:") {
        crate::a11y_tree::SemanticId(
            percent_encoding::percent_decode_str(encoded)
                .decode_utf8()
                .ok()?
                .into_owned(),
        )
    } else {
        crate::a11y_tree::SemanticId(format!("r:{}", address.resource))
    };
    // Resource publication and durable identity are separate from a control's
    // retained binding. Component aliases require explicit owner translation;
    // they cannot be inferred from a matching leaf address.
    let resource = tree.node(&canonical_id)?;
    if !resource.durable_identity || resource.action(&address.action).is_none() {
        return None;
    }
    let mut candidates = action_ui_routes(tree, bindings, &address)
        .into_iter()
        .filter(|route| match &interaction.anchor {
            tilo_actions::InteractionAnchor::Resource => true,
            tilo_actions::InteractionAnchor::SemanticControl { id } => route
                .path
                .last()
                .filter(|target| target.as_str() == id)
                .and_then(|target| tree.node(target))
                .is_some_and(|node| node.durable_identity),
            _ => false,
        })
        .filter_map(|route| route.bounds)
        .filter(|bounds| {
            [bounds.x0, bounds.y0, bounds.x1, bounds.y1]
                .into_iter()
                .all(f64::is_finite)
                && bounds.width() > 0.0
                && bounds.height() > 0.0
        });
    let only = candidates.next()?;
    candidates.next().is_none().then_some(only)
}

/// Exact resource geometry after the host has translated a checked publication.
pub(crate) fn resource_bounds(
    tree: &crate::a11y_tree::SemanticTree,
    bindings: &[crate::widget::ControlBindingMetadata],
    address: &tilo_actions::ActionAddress,
) -> Option<(vello::kurbo::Rect, u64)> {
    let resource = tree.node(&crate::a11y_tree::SemanticId(format!(
        "r:{}",
        address.resource
    )))?;
    if !resource.durable_identity || resource.action(&address.action).is_none() {
        return None;
    }
    let mut only = None;
    let mut ambiguous = false;
    visit_action_ui_routes(tree, bindings, address, &mut |_, _, node| {
        if let Some(previous) = only {
            ambiguous |= previous != &node.id;
        } else {
            only = Some(&node.id);
        }
    });
    if ambiguous {
        return None;
    }
    // Adjacent deduplication after ranking has cardinality one exactly when
    // every candidate ends at this same node. Mixed endpoints remain ambiguous
    // regardless of interleaving; this does not globally deduplicate ranked UI.
    let node = tree.node(only?)?;
    let bounds = node.bounds?;
    let widget = node.widget_taffy_id?;
    ([bounds.x0, bounds.y0, bounds.x1, bounds.y1]
        .into_iter()
        .all(f64::is_finite)
        && bounds.width() > 0.0
        && bounds.height() > 0.0)
        .then_some((bounds, widget))
}
