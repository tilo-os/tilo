//! Shared prepaint solve for retained widgets with allocation-dependent styles.

use std::collections::HashMap;

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};

use super::Widget;
use crate::bound_dispatch::{BoundNodeMap, mutate_bound_node};
use crate::text_render::TextRenderer;

#[derive(Debug, thiserror::Error)]
pub enum WidgetLayoutError {
    #[error(transparent)]
    Taffy(#[from] taffy::TaffyError),
    #[error("widget layout styles did not settle after eight adjustments")]
    DidNotSettle,
}

/// Measure the host tree and settle allocation-dependent styles before calling
/// [`Widget::apply_layout`]. App and the headless runner use this same solver.
/// Manual hosts may use it with the maps from their current `build_layout`.
/// Raw Taffy layout still works for ordinary Grid tracks, but cannot resolve
/// `CardGrid`'s below-minimum single-column fallback without this adjustment.
///
/// This changes styles only: widget state and node identities are retained.
/// Paint-only and scroll-translation frames do not need to call it.
///
/// # Errors
/// Returns a Taffy error or `DidNotSettle`; callers must skip applying/painting
/// an unsettled layout.
///
/// # Safety
/// `nodes` and `bindings` must describe this exact live tree. The caller must
/// exclusively own every referenced widget throughout the call, without
/// moving, rebuilding or destroying widgets or allowing pointers to escape.
#[allow(
    clippy::implicit_hasher,
    reason = "bound dispatch uses the existing default-hasher node map"
)]
pub unsafe fn compute_widget_layout(
    taffy: &mut TaffyTree<()>,
    root: NodeId,
    available: Size<AvailableSpace>,
    nodes: &HashMap<NodeId, *mut dyn Widget>,
    bindings: &BoundNodeMap,
    text: &mut TextRenderer,
) -> Result<(), WidgetLayoutError> {
    for _ in 0..8 {
        taffy.compute_layout_with_measure(root, available, |known, available, node, _, _| {
            // SAFETY: caller owns the stable matching tree, each callback is
            // exclusive, and bound dispatch checks admission and lifetime.
            unsafe {
                mutate_bound_node(bindings, nodes, node, |widget| {
                    widget.measure(known, available, text)
                })
            }
            .unwrap_or(Size::ZERO)
        })?;
        let mut changed = false;
        for &node in nodes.keys() {
            // SAFETY: same contract as measurement; no topology mutation is
            // permitted by adjust_layout_style.
            changed |= unsafe {
                mutate_bound_node(bindings, nodes, node, |widget| {
                    widget.adjust_layout_style(taffy, node)
                })
            }
            .unwrap_or(false);
        }
        if !changed {
            return Ok(());
        }
    }
    Err(WidgetLayoutError::DidNotSettle)
}
