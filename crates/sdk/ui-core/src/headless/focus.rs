//! The keyboard's place in a headless frame: traversal order inside a trap,
//! and the trap that holds focus while a dialog or command is open.
//!
//! The policy is the live window's ([`crate::focus_navigation`] and
//! [`crate::history_mode::embedded::FocusRestrictions`]); what differs is that
//! this host resolves a stable identity in its own layout maps, never a Taffy
//! id carried across a rebuild.

use super::FrameHarness;
use crate::host_core::{HostCore as _, KeyHost as _};
use taffy::NodeId;

impl FrameHarness {
    /// Whether the surface this tree draws holds the keyboard, as
    /// [`Self::set_keyboard_held`] last said: its fields paint carets only while it does.
    #[must_use]
    pub const fn keyboard_held(&self) -> bool {
        self.keyboard_held
    }

    /// Say whether the surface this tree draws holds the keyboard. A surface that
    /// lost it keeps its fields' text and selection and paints no caret until it
    /// has it back. Returns whether that changed, so a host redraws the frame the
    /// caret was in.
    /// Quiet: invalidates when the answer changed; a host sets it every frame, and an unchanged
    /// one leaves nothing to bring back.
    pub fn set_keyboard_held(&mut self, held: bool) -> bool {
        if self.keyboard_held == held {
            return false;
        }
        self.keyboard_held = held;
        self.invalidate_local();
        self.root.release_scene_cache();
        if let Some(palette) = self.action_palette.as_deref_mut() {
            palette.release_scene_cache();
        }
        if let Some(history) = self.history.as_deref_mut() {
            history.release_scene_cache();
        }
        true
    }

    /// Whether embedded History and a History holding saved focus leave a
    /// node free to take focus, for asking of every node in one pass: each
    /// History's subtrees are walked once, not once per node.
    pub(super) fn history_focus_admission(&self) -> impl Fn(NodeId) -> bool + '_ {
        let restrictions = crate::history_mode::embedded::FocusRestrictions::collect(
            self.root.as_ref(),
            &self.taffy,
        );
        let saved = self
            .history
            .as_ref()
            .filter(|history| history.has_saved_focus())
            .map(|history| history.owned_nodes(&self.taffy));
        move |node| {
            restrictions.allows(node) && saved.as_ref().is_none_or(|owned| owned.contains(&node))
        }
    }

    /// Whether the walk in [`Self::update_focus_trap`] could find a trap: History
    /// review is open, or a widget it would inspect traps focus. `false` is
    /// exact — that walk would conclude "untrapped" — while `true` may still
    /// end there (a trap under a hidden or denied ancestor).
    pub(super) fn focus_trap_possible(&self) -> bool {
        let _profile = tilo_resource::profiling::Phase::start("ui.focus_trap_probe");
        if self
            .history
            .as_ref()
            .is_some_and(|history| history.review_is_open())
        {
            return true;
        }
        if self.history.is_some()
            || crate::history_mode::embedded::has_surfaces(self.root.as_ref())
            || crate::focus_navigation::hosts_components(self.root.as_ref())
        {
            // The mapped walk inspects composed nodes, some of which are not
            // Widget children of the root; every one it can inspect is mapped.
            let mut sweep = crate::bound_dispatch::NodeSweep::new(&self.bindings, &self.node_map);
            self.node_map.keys().any(|&node| {
                // SAFETY: FrameHarness owns this stable tree; no target reference escapes.
                unsafe { sweep.inspect(node, |widget| widget.traps_focus()) }.unwrap_or(false)
            })
        } else {
            crate::focus_navigation::any_trap(self.root.as_ref())
        }
    }

    /// The node of the widget holding the open trap, as the tree stands now.
    pub(super) fn focus_trap_node(&self) -> Option<NodeId> {
        let mut sweep = crate::bound_dispatch::NodeSweep::new(&self.bindings, &self.node_map);
        self.node_map.keys().copied().find(|&node| {
            // SAFETY: FrameHarness owns this stable tree; no target reference escapes.
            unsafe { sweep.inspect(node, |widget| widget.traps_focus()) }.unwrap_or(false)
        })
    }

    pub(super) fn update_focus_trap(
        &mut self,
        before_layout: Option<crate::widget::layout::CompositionFocus>,
    ) {
        // Untrapped and nothing asking to trap: the walk below could only
        // conclude that again, which leaves focus alone. Skip it, as the live
        // App does between rebuilds, so headless frames cost what live ones
        // do; a trap that appears without a rebuild still engages this frame.
        if !self.focus_trap_active && !self.focus_trap_possible() {
            return;
        }
        let _profile = tilo_resource::profiling::Phase::start("ui.focus_order");
        let restrictions = crate::history_mode::embedded::FocusRestrictions::collect(
            self.root.as_ref(),
            &self.taffy,
        );
        let (mut order, trapped) = self
            .tree()
            .focus_order_under(self.child_node, &restrictions);
        if !self.focus_trap_active && trapped {
            // Capture a stable semantic/owner identity, never carry a stale Taffy id
            // through the portable host's complete layout rebuilds.
            self.focus_before_trap = before_layout.or_else(|| unsafe {
                crate::widget::layout::CompositionFocus::capture(
                    self.focused,
                    &self.node_map,
                    &self.bindings,
                )
            });
            self.focus_trap_node = self.focus_trap_node();
        }
        let mut before = if self.focus_trap_active && !trapped {
            // Bound component delegates can be absent from Widget::children's
            // generic order. Eligibility comes from this newly admitted layout,
            // with the same embedded-history restrictions as mapped traversal.
            let (allowed, _) = self
                .tree()
                .layout_map_focus_order(self.child_node, &restrictions);
            let saved = self.focus_before_trap.take();
            let target = saved.as_ref().and_then(|saved| unsafe {
                saved.resolve(&self.node_map, &self.bindings, Some(&allowed))
            });
            // Focus the trap itself moved, still standing, outlives it: a menu's
            // "Rename" opens the field the person types into next, and the control
            // that opened the menu is gone. So does focus the person put on the
            // trap itself, which never went into what it hides. Focus the trap
            // took into its content goes back to the control it was taken from.
            let trap_node = self.focus_trap_node.take();
            let kept = self
                .focused
                .filter(|id| order.contains(id) && (target.is_none() || Some(*id) == trap_node));
            let restored = match (&saved, kept) {
                (Some(saved), Some(keep)) => unsafe {
                    saved.restore_keeping(&self.node_map, &self.bindings, Some(&allowed), keep)
                },
                (Some(saved), None) => unsafe {
                    saved.restore(&self.node_map, &self.bindings, Some(&allowed))
                },
                (None, _) => None,
            };
            if let Some(target) = kept.or(restored)
                && !order.contains(&target)
            {
                order.push(target);
            }
            restored
        } else {
            None
        };
        // A dialog opens on the control the person types into, ahead of its first control:
        // what they type reaches the form it asks.
        let entry = self.keyboard_entry(&order);
        if let Some(next) = crate::focus_navigation::reconcile_trap(
            &mut self.focus_trap_active,
            &mut before,
            self.focused,
            &order,
            trapped,
            entry,
        ) {
            self.set_focused(next);
        }
    }
}
