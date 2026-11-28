//! A page's body shown in place of the one before it: the shell keeps its other parts and
//! their laid-out nodes, and swaps the body's node under its own.

use super::{Column, NavShell, Part, Pending, Staged, at};

impl NavShell {
    /// Whether the content column comes or goes since the shell was built.
    pub(super) fn content_column_changed(&self) -> bool {
        self.built && self.content_shown != self.content_shown_built
    }

    /// Whether what is staged is bodies alone, each in place of one that stands laid out: the shell
    /// keeps its parts and swaps those bodies' nodes under its own ([`Self::swap_bodies`]).
    pub(super) fn swaps_in_place(&self) -> bool {
        let Pending {
            bodies,
            actions,
            leading,
            controls,
            rail,
            empty,
        } = &self.pending;
        self.held.is_none()
            && empty.is_none()
            && rail.is_none()
            && controls.is_none()
            && actions.iter().all(Option::is_none)
            && leading.iter().all(Option::is_none)
            && Column::ALL
                .into_iter()
                .zip(bodies)
                .all(|(column, body)| match body {
                    None => true,
                    Some(Staged(Some(_))) => self.laid_out_parts.contains(&Part::Body(column)),
                    Some(Staged(None)) => false,
                })
    }

    /// Put the staged bodies in place of those they replace, each body's node in its predecessor's
    /// place under the shell's node, with the style the shell lays it out from
    /// ([`Widget::manual_layout_policy`]).
    pub(super) fn swap_bodies(&mut self, remount: &mut tilo_ui_base::widget::Remount<'_>) -> bool {
        self.bodies_swapped = false;
        let Some(node) = self
            .baseline
            .first()
            .and_then(|&(first, _)| remount.taffy().parent(first))
        else {
            return false;
        };
        let mut children = remount.taffy().children(node).unwrap_or_default();
        let swapped: Vec<(usize, Part)> = self
            .laid_out_parts
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, part)| {
                matches!(part, Part::Body(column) if at(&self.pending.bodies, *column).is_some())
            })
            .collect();
        for &(index, _) in &swapped {
            let Some(&child) = children.get(index) else {
                return false;
            };
            remount.unmount(child);
        }
        self.apply_pending();
        self.tell_parts_input();
        self.refresh_part_inputs();
        for (index, part) in swapped {
            let Some(widget) = self.part_mut(part) else {
                return false;
            };
            let child = remount.mount(node, widget);
            let style = remount.taffy().style(child).cloned().unwrap_or_default();
            if let (Some(place), Some(baseline)) =
                (children.get_mut(index), self.baseline.get_mut(index))
            {
                *place = child;
                *baseline = (child, style);
            }
        }
        self.invalidate();
        remount.set_children(node, &children).is_ok()
    }
}
