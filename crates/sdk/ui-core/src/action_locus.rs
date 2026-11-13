//! Exact, read-only action geometry shared by native and retained browser hosts.

use tilo_sort::CompactSort as _;
/// One marker exported only by the action-presence painter, in surface-local logical coordinates.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PaintedActionMarker {
    pub interaction: tilo_actions::SemanticInteraction,
    pub viewport: [f64; 4],
    pub marker: [f64; 4],
    pub locus: [f64; 4],
    pub protected: [f64; 4],
}

/// Paint semantic activity against this frame's exact action bindings.
pub(crate) fn draw_action_presence(
    paint: &mut crate::paint::PaintCtx<'_>,
    presence: &mut crate::presence::PresenceCtx,
    tree: &crate::a11y_tree::SemanticTree,
    bindings: &[crate::widget::ControlBindingMetadata],
    text_loci: &[(tilo_actions::InvocationId, vello::kurbo::Rect)],
    viewport: vello::kurbo::Rect,
    surface: &str,
    now: crate::time::Instant,
) -> Vec<PaintedActionMarker> {
    let mut painted = Vec::new();
    let mut resolved = presence.take_shown();
    for interaction in presence.visible_interactions(now) {
        let Some(bounds) = text_loci
            .iter()
            .find(|(invocation, _)| *invocation == interaction.invocation)
            .map(|(_, rect)| *rect)
            .or_else(|| interaction_bounds(tree, bindings, &interaction))
        else {
            continue;
        };
        let bounds = bounds.intersect(viewport);
        if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
            continue;
        }
        if !names_a_place(bounds, viewport) {
            continue;
        }
        let user = crate::permissions::UserId(interaction.actor.id.clone());
        let control = control_bounds(tree, bindings, &interaction, bounds).unwrap_or(bounds);
        // An agent's place is washed in its colour and framed in it, brighter the moment its step
        // is taken; anyone else's outline is the selection. One fill either way, at the
        // outline strength or stronger, never a second layer over the control's words.
        let fill = presence
            .agent_locus_tint(&interaction, now, paint.theme)
            .unwrap_or(crate::presence::render::OUTLINE_FILL);
        presence.draw_peer_outline_filled(
            paint.scene,
            paint.theme,
            paint.transform,
            &user,
            bounds,
            f64::from(paint.theme.tokens.radii.detail),
            fill,
        );
        let ink = ink_around(tree, bounds, viewport);
        let place = crate::presence::MarkerPlace {
            locus: bounds,
            control,
            ink: ink.clone(),
        };
        let marker =
            presence.draw_agent_action_marker(paint, &interaction, &place, viewport, surface, now);
        if marker.is_none() {
            let label = presence.interaction_status_label(&interaction);
            presence.draw_action_label_within(paint, &user, &label, bounds, viewport, &ink);
        }
        if let Some(marker) = marker
            && presence.participant_kind(&user) == crate::presence::ParticipantKind::Agent
        {
            painted.push(PaintedActionMarker {
                interaction: interaction.clone(),
                viewport: crate::floating_surface::protocol_rect_array(viewport),
                marker: crate::floating_surface::protocol_rect_array(marker.bounds),
                locus: crate::floating_surface::protocol_rect_array(bounds),
                protected: crate::floating_surface::protocol_rect_array(
                    bounds.union(marker.bounds),
                ),
            });
        }
        resolved.push(interaction.invocation);
    }
    presence.draw_interactions_excluding(
        paint.scene,
        paint.transform,
        viewport,
        paint.text,
        paint.theme,
        now,
        &resolved,
    );
    for interaction in presence.visible_interactions(now) {
        presence.note_drawn(interaction.invocation, now);
    }
    painted
}

/// The text a marker's tag must leave readable: each worded line-sized node
/// the frame shows within `viewport`, the control the marker rests on among
/// them — its words stay readable too.
fn ink_around(
    tree: &crate::a11y_tree::SemanticTree,
    locus: vello::kurbo::Rect,
    viewport: vello::kurbo::Rect,
) -> Vec<vello::kurbo::Rect> {
    let line = locus.height().max(24.0);
    tree.iter()
        .filter(|node| {
            node.name
                .as_deref()
                .is_some_and(|name| !name.trim().is_empty())
                || node
                    .value
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
        })
        .filter_map(|node| node.bounds)
        .map(|bounds| bounds.intersect(viewport))
        .filter(|bounds| bounds.width() > 0.0 && bounds.height() > 0.0)
        // A worded box no taller than a row is a line of words: a list or a
        // window that names itself is no line its tag must leave.
        .filter(|bounds| bounds.height() <= line * 2.0)
        .collect()
}

/// One visible expression of a domain action in the retained UI.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionUiRoute {
    pub address: tilo_actions::ActionAddress,
    pub control_id: String,
    /// Root-to-control semantic path.
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
    routes.compact_sort_by(|left, right| {
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

fn visit_action_ui_routes<'tree>(
    tree: &'tree crate::a11y_tree::SemanticTree,
    bindings: &[crate::widget::ControlBindingMetadata],
    address: &tilo_actions::ActionAddress,
    visit: &mut ActionRouteVisitor<'tree, '_>,
) {
    let mut unique: Vec<&crate::widget::ControlBindingMetadata> =
        Vec::with_capacity(bindings.len());
    for binding in bindings {
        if !unique.contains(&binding) {
            unique.push(binding);
        }
    }
    for binding in &unique {
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
            let encoded_node_id = percent_encoding::utf8_percent_encode(
                semantic_id.as_str(),
                percent_encoding::NON_ALPHANUMERIC,
            );
            let semantic_address = address.resource == format!("ui:{encoded_node_id}")
                && tree.action(semantic_id, &address.action).is_some();
            let matches_address = semantic_address
                || matches!(
                    &binding.kind,
                    crate::widget::ControlBindingKind::Action(candidate)
                        if candidate.action == address.action
                            && tilo_actions::logical_resource_id(&candidate.resource)
                                == address.resource
                );
            if !matches_address {
                continue;
            }
            let Some(node) = tree.node(semantic_id) else {
                continue;
            };
            visit(binding, path, node);
        }
    }
}

fn interaction_address(
    interaction: &tilo_actions::SemanticInteraction,
) -> tilo_actions::ActionAddress {
    tilo_actions::ActionAddress::new(
        tilo_actions::logical_resource_id(&interaction.resource.id).into_owned(),
        interaction.action.id.clone(),
    )
}

fn record_of(id: &crate::a11y_tree::SemanticId) -> std::borrow::Cow<'_, str> {
    let id = crate::a11y_tree::component_record(id)
        .or_else(|| id.as_str().strip_prefix("r:"))
        .unwrap_or(id.as_str());
    tilo_actions::logical_resource_id(id)
}

/// Resolve only an unambiguous, currently bound control. Labels and positional
/// wire IDs cannot establish resource identity. Domain anchors without a shared
/// resolver remain app-level activity rather than guessed rectangles.
pub(crate) fn interaction_bounds(
    tree: &crate::a11y_tree::SemanticTree,
    bindings: &[crate::widget::ControlBindingMetadata],
    interaction: &tilo_actions::SemanticInteraction,
) -> Option<vello::kurbo::Rect> {
    if let tilo_actions::InteractionAnchor::ListItems { ids } = &interaction.anchor {
        return item_bounds(tree, ids);
    }
    let address = interaction_address(interaction);
    // The resource node is what makes this an action on a record rather than a
    // guessed widget: it must export the very action the invocation names.
    // It is under the id its window published it as — the record
    // id, or that window's transport alias — which for an action the session
    // keeps is not the alias the invocation has, so the record is what the
    // node is found by.
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
    let stands_for_record = |node: &crate::a11y_tree::SemanticNode| {
        node.action(&address.action).is_some()
            && (node.durable_identity || !address.resource.starts_with("ui:"))
    };
    let published = tree.node(&canonical_id).is_some_and(stands_for_record)
        || tree
            .iter()
            .any(|node| stands_for_record(node) && record_of(&node.id) == address.resource);
    if !published {
        return None;
    }
    let candidates = action_ui_routes(tree, bindings, &address)
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
        .filter_map(|route| {
            let presses = route
                .path
                .last()
                .and_then(|target| tree.node(target))
                .is_some_and(|node| crate::a11y::activation_role(node.role));
            Some((route.bounds?, presses))
        })
        .filter(|(bounds, _)| {
            [bounds.x0, bounds.y0, bounds.x1, bounds.y1]
                .into_iter()
                .all(f64::is_finite)
                && bounds.width() > 0.0
                && bounds.height() > 0.0
        })
        .collect::<Vec<_>>();
    if let [(only, _)] = candidates.as_slice() {
        return Some(*only);
    }
    let mut pressed = candidates.iter().filter(|(_, presses)| *presses);
    let (only, _) = pressed.next()?;
    pressed.next().is_none().then_some(*only)
}

/// The node [`interaction_bounds`] finds `interaction` on, reached through the widget that draws it: a
/// synthetic child a widget publishes (a terminal's prompt line) belongs to the widget above it.
pub(crate) fn interaction_node<'tree>(
    tree: &'tree crate::a11y_tree::SemanticTree,
    bindings: &[crate::widget::ControlBindingMetadata],
    interaction: &tilo_actions::SemanticInteraction,
) -> Option<&'tree crate::a11y_tree::SemanticNode> {
    let bounds = interaction_bounds(tree, bindings, interaction)?;
    let mut owned = None;
    let mut behind = None;
    for node in tree.iter().filter(|node| node.bounds == Some(bounds)) {
        behind = Some(node);
        if node.widget_taffy_id.is_some() {
            owned = Some(node);
        }
    }
    if owned.is_some() {
        return owned;
    }
    let mut parents = std::collections::HashMap::new();
    for node in tree.iter() {
        for child in &node.children {
            parents.insert(child.clone(), node.id.clone());
        }
    }
    let mut cursor = &behind?.id;
    for _ in 0..tree.len() {
        let parent = parents.get(cursor)?;
        if let Some(node) = tree.node(parent)
            && node.widget_taffy_id.is_some()
        {
            return Some(node);
        }
        cursor = parent;
    }
    None
}

fn control_bounds(
    tree: &crate::a11y_tree::SemanticTree,
    bindings: &[crate::widget::ControlBindingMetadata],
    interaction: &tilo_actions::SemanticInteraction,
    locus: vello::kurbo::Rect,
) -> Option<vello::kurbo::Rect> {
    let address = interaction_address(interaction);
    let mut inside = action_ui_routes(tree, bindings, &address)
        .into_iter()
        .filter_map(|route| route.bounds)
        .filter(|bounds| centred_in(*bounds, locus));
    let only = inside.next();
    if only.is_some() && inside.next().is_none() {
        return only;
    }
    locus_toggle(tree, locus)
}

fn locus_toggle(
    tree: &crate::a11y_tree::SemanticTree,
    locus: vello::kurbo::Rect,
) -> Option<vello::kurbo::Rect> {
    let toggles = tree
        .iter()
        .filter(|node| {
            matches!(
                node.role,
                crate::a11y::Role::Checkbox
                    | crate::a11y::Role::Switch
                    | crate::a11y::Role::RadioButton
            )
        })
        .filter_map(|node| node.bounds)
        .filter(|bounds| centred_in(*bounds, locus));
    toggles.reduce(|smallest, bounds| {
        if bounds.area() < smallest.area() {
            bounds
        } else {
            smallest
        }
    })
}

fn centred_in(inner: vello::kurbo::Rect, outer: vello::kurbo::Rect) -> bool {
    outer.contains(inner.center())
}

fn item_bounds(
    tree: &crate::a11y_tree::SemanticTree,
    ids: &[String],
) -> Option<vello::kurbo::Rect> {
    let [item] = ids else {
        return None;
    };
    if item.is_empty() {
        return None;
    }
    let mut only = None;
    for node in tree.nodes_showing_item(item) {
        let bounds = node.bounds?;
        if only.is_some_and(|seen| seen != bounds) {
            // Two nodes claim the same item: the view cannot say which.
            return None;
        }
        only = Some(bounds);
    }
    let only = only?;
    ([only.x0, only.y0, only.x1, only.y1]
        .into_iter()
        .all(f64::is_finite)
        && only.width() > 0.0
        && only.height() > 0.0)
        .then_some(only)
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

fn names_a_place(bounds: vello::kurbo::Rect, viewport: vello::kurbo::Rect) -> bool {
    bounds.width() < viewport.width() - 1.0 || bounds.height() < viewport.height() - 1.0
}

#[cfg(test)]
mod tests {
    use vello::kurbo::Rect;

    #[test]
    fn a_step_naming_no_place_is_left_to_the_activity_rail() {
        let viewport = Rect::new(0.0, 0.0, 1280.0, 800.0);
        assert!(!super::names_a_place(viewport, viewport));
        assert!(!super::names_a_place(
            viewport.inflate(-0.5, -0.5),
            viewport
        ));
        assert!(super::names_a_place(
            Rect::new(0.0, 48.0, 1280.0, 800.0),
            viewport
        ));
        assert!(super::names_a_place(
            Rect::new(240.0, 0.0, 1280.0, 800.0),
            viewport
        ));
    }
}
