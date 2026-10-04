//! Exact, read-only action geometry shared by native and retained browser hosts.

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
    for binding in bindings {
        if bindings
            .iter()
            .filter(|candidate| candidate.control_id == binding.control_id)
            .count()
            != 1
        {
            continue;
        }
        let paths = tree.control_paths(&binding.control_id);
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
            let Some(node) = tree.node(semantic_id) else {
                continue;
            };
            routes.push(ActionUiRoute {
                address: address.clone(),
                control_id: binding.control_id.clone(),
                path: path.clone(),
                bounds: node.bounds,
                focused: node.state.focused,
                disabled: node.state.disabled,
            });
        }
    }
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
