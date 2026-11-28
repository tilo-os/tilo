//! Where a person reaches in a window: the rows of the lists it shows, the destinations of its
//! sidebar and the surface it draws on, each with the point a finger taps.

use super::stage;

/// Whether `node` stands where a finger on the window
/// reaches it: drawn, and at least partly inside the
/// window.
pub(super) fn reachable(node: &ui::headless::InspectionNode, viewport: (u32, u32)) -> bool {
    node.rect[2] > 0.0
        && node.rect[3] > 0.0
        && node.rect[0] + node.rect[2] > 0.0
        && node.rect[1] + node.rect[3] > 0.0
        && node.rect[0] < f64::from(viewport.0)
        && node.rect[1] < f64::from(viewport.1)
}

/// Where a finger taps to open a row of a list: the middle of
/// it, drawn where the person holding the window can reach it.
pub(super) fn row_centre(node: &ui::headless::InspectionNode) -> (f64, f64) {
    (
        node.rect[0] + node.rect[2] / 2.0,
        node.rect[1] + node.rect[3] / 2.0,
    )
}

/// Whether the window draws `node` as a row of a list: a
/// list item, or a destination of the tree a sidebar's list
/// is.
fn is_row(node: &ui::headless::InspectionNode) -> bool {
    node.semantics.as_ref().is_some_and(|access| {
        matches!(
            access.role,
            tilo_ui_base::a11y::Role::ListItem | tilo_ui_base::a11y::Role::TreeItem
        ) && !access.state.disabled
    })
}

/// Where a finger taps to open what the window shows after its list: the middle of the row the
/// window selects for it, drawn where the person holding the window can reach it.
fn row_opened(node: &ui::headless::InspectionNode, viewport: (u32, u32)) -> Option<(f64, f64)> {
    let selected = node
        .semantics
        .as_ref()
        .is_some_and(|access| access.state.selected);
    if selected && is_row(node) && reachable(node, viewport) {
        return Some(row_centre(node));
    }
    node.children
        .iter()
        .find_map(|child| row_opened(child, viewport))
}

/// The destinations the window's own list leads to, by the words each carries and where a finger
/// reaches it: the rows of the first tree the window draws, which is the list its sidebar keeps.
pub(super) fn destinations(
    node: &ui::headless::InspectionNode,
    viewport: (u32, u32),
) -> Vec<(String, (f64, f64))> {
    let Some(sidebar) = first_tree(node) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    unchosen_destinations(sidebar, viewport, &mut out);
    out
}

/// The first list of a sidebar in the window: the tree its destinations stand in.
fn first_tree(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
    let is_tree = node
        .semantics
        .as_ref()
        .is_some_and(|access| access.role == tilo_ui_base::a11y::Role::Tree);
    if is_tree {
        return Some(node);
    }
    node.children.iter().find_map(first_tree)
}

/// The destination rows under `node` no one chose yet, with where a finger reaches each: a section's
/// header is no destination, and the row the window stands on is one already shown.
fn unchosen_destinations(
    node: &ui::headless::InspectionNode,
    viewport: (u32, u32),
    out: &mut Vec<(String, (f64, f64))>,
) {
    let unchosen = node.semantics.as_ref().and_then(|access| {
        let destination = access.role == tilo_ui_base::a11y::Role::TreeItem
            && !access.state.disabled
            && !access.state.selected
            && reachable(node, viewport);
        if !destination {
            return None;
        }
        Some((access.name.clone()?, row_centre(node)))
    });
    if let Some((label, at)) = unchosen {
        out.push((label, at));
    }
    for child in &node.children {
        unchosen_destinations(child, viewport, out);
    }
}

/// Where a finger taps the surface a window draws on — the map its places stand on — and what the
/// surface is called: points of a grid over it, those nothing drawn on it stands on first, so a
/// press there is the surface's own (a click on free canvas).
pub(super) fn surface_presses(
    staged: &stage::Staged,
    viewport: (u32, u32),
) -> Vec<(String, (f64, f64))> {
    fn first_surface(
        node: &ui::headless::InspectionNode,
        viewport: (u32, u32),
    ) -> Option<&ui::headless::InspectionNode> {
        let drawn_on = node.widget == "Map"
            && node.rect[2] > 0.0
            && node.rect[3] > 0.0
            && reachable(node, viewport);
        if drawn_on {
            return Some(node);
        }
        node.children
            .iter()
            .find_map(|child| first_surface(child, viewport))
    }
    let inspection = staged.harness().inspection();
    let Some(surface) = first_surface(&inspection.root, viewport) else {
        return Vec::new();
    };
    let name = surface
        .semantics
        .as_ref()
        .and_then(|access| access.name.clone())
        .unwrap_or_else(|| "surface".to_owned());
    let [x, y, width, height] = surface.rect;
    let places: Vec<[f64; 4]> = surface
        .children
        .iter()
        .filter(|child| child.widget == "MapPin")
        .map(|child| child.rect)
        .collect();
    let inside = |point: (f64, f64)| {
        point.0 >= 0.0
            && point.1 >= 0.0
            && point.0 < f64::from(viewport.0)
            && point.1 < f64::from(viewport.1)
    };
    let (mut free, mut covered) = (Vec::new(), Vec::new());
    for across in 1..=6 {
        for down in 1..=6 {
            let point = (
                x + width * f64::from(across) / 7.0,
                y + height * f64::from(down) / 7.0,
            );
            if !inside(point) {
                continue;
            }
            let stands = places.iter().any(|place| {
                let [x, y, width, height] = *place;
                point.0 >= x && point.0 <= x + width && point.1 >= y && point.1 <= y + height
            });
            if stands {
                covered.push((name.clone(), point));
            } else {
                free.push((name.clone(), point));
            }
        }
    }
    free.into_iter().chain(covered).take(3).collect()
}

/// Where a finger taps to open a row when the window keeps none chosen: the middle of the first row
/// of its list, drawn where the person holding the window can reach it.
pub(super) fn first_row(
    node: &ui::headless::InspectionNode,
    viewport: (u32, u32),
) -> Option<(f64, f64)> {
    if is_row(node) && reachable(node, viewport) {
        return Some(row_centre(node));
    }
    node.children
        .iter()
        .find_map(|child| first_row(child, viewport))
}

/// Whether the window shows its way back where a finger reaches it: how a person holding a narrow
/// window comes back to the list it opened something from: the chip the shell's header row holds.
pub(super) fn back_chip(staged: &stage::Staged, viewport: (u32, u32)) -> bool {
    fn in_frame(node: &ui::headless::InspectionNode, viewport: (u32, u32)) -> bool {
        (reachable(node, viewport) && node.test_id.as_deref() == Some(tilo_ui_nav::WINDOW_BACK))
            || node.children.iter().any(|child| in_frame(child, viewport))
    }
    in_frame(&staged.harness().inspection().root, viewport)
}

/// Whether the window's header row carries its sidebar toggle where a finger reaches it: how a
/// person holding a window whose list stands over what it shows as a drawer brings that list
/// forward.
pub(super) fn sidebar_toggle(staged: &stage::Staged, viewport: (u32, u32)) -> bool {
    fn in_frame(node: &ui::headless::InspectionNode, viewport: (u32, u32)) -> bool {
        (reachable(node, viewport) && node.test_id.as_deref() == Some(tilo_ui_nav::WINDOW_SIDEBAR))
            || node.children.iter().any(|child| in_frame(child, viewport))
    }
    in_frame(&staged.harness().inspection().root, viewport)
}

/// Where a finger taps to open a row of the list the window shows now: the row it keeps chosen, or
/// else the first of the list.
pub(super) fn tappable_row(staged: &stage::Staged, viewport: (u32, u32)) -> Option<(f64, f64)> {
    let root = &staged.harness().inspection().root;
    row_opened(root, viewport).or_else(|| first_row(root, viewport))
}

/// Where a finger taps to open a row of the list in front of
/// what the window shows: the first row the list does not keep chosen,
/// which opens what the window is not already showing, or else the chosen
/// one.
pub(super) fn unchosen_row(staged: &stage::Staged, viewport: (u32, u32)) -> Option<(f64, f64)> {
    fn unchosen(node: &ui::headless::InspectionNode, viewport: (u32, u32)) -> Option<(f64, f64)> {
        let selected = node
            .semantics
            .as_ref()
            .is_some_and(|access| access.state.selected);
        if !selected && is_row(node) && reachable(node, viewport) {
            return Some(row_centre(node));
        }
        node.children
            .iter()
            .find_map(|child| unchosen(child, viewport))
    }
    unchosen(&staged.harness().inspection().root, viewport)
        .or_else(|| tappable_row(staged, viewport))
}
