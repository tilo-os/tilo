//! The tree a chain's items make, as the chain publishes them: which item
//! sits under which, and the items as that tree rather than a flat list.

use std::collections::HashMap;

/// How many parents `item` sits under, among those published; a chain that
/// comes back to itself counts as the top.
fn depth(item: usize, parent_of: &[Option<usize>]) -> usize {
    let mut current = parent_of.get(item).copied().flatten();
    let mut steps: usize = 0;
    while let Some(parent) = current {
        steps = steps.saturating_add(1);
        if steps > parent_of.len() {
            return 0;
        }
        current = parent_of.get(parent).copied().flatten();
    }
    steps
}

/// `items` as the tree `tree` places them in: each under the one it sits
/// under, and only those whose parent is not published here at the top. An
/// item whose parent chain comes back to itself is kept at the top rather
/// than hidden.
pub(super) fn nest(
    items: Vec<tilo_ui_base::published::SharedResource>,
    tree: &[(tilo_resource::Id, Option<tilo_resource::Id>)],
) -> Vec<tilo_ui_base::published::SharedResource> {
    if tree.is_empty() {
        return items;
    }
    let at: HashMap<&str, usize> = items
        .iter()
        .enumerate()
        .map(|(at, item)| (item.id(), at))
        .collect();
    let above: HashMap<&str, &str> = tree
        .iter()
        .filter_map(|(id, parent)| Some((id.as_str(), parent.as_ref()?.as_str())))
        .collect();
    let mut parent_of: Vec<Option<usize>> = items
        .iter()
        .map(|item| {
            above
                .get(item.id())
                .and_then(|parent| at.get(parent))
                .copied()
        })
        .collect();
    // A parent chain that comes back to itself would publish no root.
    for start in 0..items.len() {
        let mut current = parent_of.get(start).copied().flatten();
        let mut steps = 0;
        while let Some(parent) = current {
            if parent == start || steps > items.len() {
                if let Some(entry) = parent_of.get_mut(start) {
                    *entry = None;
                }
                break;
            }
            current = parent_of.get(parent).copied().flatten();
            steps = steps.saturating_add(1);
        }
    }
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    for (item, parent) in parent_of.iter().enumerate() {
        if let Some(parent) = parent
            && let Some(list) = children.get_mut(*parent)
            && *parent != item
        {
            list.push(item);
        }
    }
    // Below their parents, so a parent is built over children already
    // nested; a cycle left over keeps its place in the order.
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|item| std::cmp::Reverse(depth(*item, &parent_of)));
    let mut built: Vec<Option<tilo_ui_base::published::SharedResource>> =
        items.iter().map(|_| None).collect();
    for item in order {
        let mut below: Vec<tilo_ui_base::published::SharedResource> = items
            .get(item)
            .map(|node| node.children().as_slice().to_vec())
            .unwrap_or_default();
        for child in children.get(item).into_iter().flatten() {
            below.extend(
                built
                    .get_mut(*child)
                    .and_then(Option::take)
                    .or_else(|| items.get(*child).cloned()),
            );
        }
        if let (Some(entry), Some(node)) = (built.get_mut(item), items.get(item)) {
            *entry = Some(node.over(tilo_ui_base::published::SharedChildren::new(below)));
        }
    }
    (0..items.len())
        .filter(|item| parent_of.get(*item).is_some_and(Option::is_none))
        .filter_map(|item| built.get_mut(item).and_then(Option::take))
        .collect()
}
