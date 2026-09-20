//! Peer-local focus and edge-triggered framing of the retained desktop.
use crate::{
    GroupId, StripId, WorkspaceTree, composition::DesktopSurface, intents::LocalViewIntent,
};
use std::collections::HashMap;
use ui::widget::Widget;
use vello::kurbo::Point;

#[derive(Default)]
pub(crate) struct Navigation {
    active_strip: Option<StripId>,
    groups: HashMap<StripId, GroupId>,
    framed: HashMap<StripId, FrameKey>,
    pending_focus: Option<(StripId, GroupId)>,
    pressed_group: Option<(StripId, GroupId)>,
    cursor: Option<Point>,
    manual_pan: Option<(StripId, GroupId)>,
}

/// Geometry excludes the current pan and animated positions. Text edits and
/// intermediate animation frames must never restart a navigation animation.
#[derive(PartialEq)]
struct FrameKey {
    group: GroupId,
    viewport: f64,
    leading_inset: f64,
    widths: Vec<(GroupId, f64)>,
}

impl DesktopSurface {
    pub(crate) fn inherit_navigation(&mut self, previous: &mut Self) {
        self.navigation.cursor = previous.navigation.cursor;
        if self.workspace_context() != previous.workspace_context() {
            return;
        }
        for strip in self.strip_ids() {
            if let (Some(row), Some(old)) = (self.row_mut(strip), previous.row(strip)) {
                row.inherit_viewport(old);
            }
        }
        // A retained caret is not a new gesture merely because chrome rebuilt.
        self.navigation.pending_focus = previous.navigation.pending_focus;
        self.navigation.manual_pan = previous.navigation.manual_pan;
        self.navigation.framed = std::mem::take(&mut previous.navigation.framed);
        if self.navigation.active_strip != previous.navigation.active_strip {
            if let Some(strip) = self.navigation.active_strip {
                self.navigation.framed.remove(&strip);
            }
        }
        let strips = self.strip_ids();
        self.navigation
            .framed
            .retain(|strip, _| strips.contains(strip));
    }

    /// Hosts call this after applying local intents. The tree owns the local
    /// selection; this compositor-independent layer only presents it.
    pub fn sync_navigation(&mut self, tree: &WorkspaceTree) {
        let active_strip = tree.active_workspace().and_then(|workspace| {
            tree.active_strip
                .get(&workspace.id)
                .copied()
                .filter(|id| workspace.strips.iter().any(|strip| strip.id == *id))
                .or_else(|| workspace.strips.first().map(|strip| strip.id))
        });
        if self.navigation.active_strip != active_strip {
            if let Some(strip) = active_strip {
                self.navigation.framed.remove(&strip);
            }
        }
        self.navigation.active_strip = active_strip;
        if self.external_navigation {
            for strip in self.strip_ids() {
                if let Some(row) = self.row_mut(strip) {
                    row.set_external_navigation(true);
                    row.set_fullscreen(None, Some(strip) != active_strip);
                }
            }
        }
        self.navigation.groups = self
            .strip_ids()
            .into_iter()
            .filter_map(|strip| {
                let row = self.row(strip)?;
                tree.resolve_active_group(strip, |id| {
                    row.groups
                        .iter()
                        .find(|group| group.id == id)
                        .is_none_or(|group| group.is_closing())
                })
                .map(|group| (strip, group))
            })
            .collect();
        self.apply_navigation();
    }

    /// Native click policy keeps a clicked card under the cursor. Native
    /// keyboard/strip navigation and portable focus still use the same framing.
    pub fn keep_clicked_group_in_place(&mut self, strip: StripId, group: GroupId) {
        self.navigation.active_strip = Some(strip);
        self.navigation.groups.insert(strip, group);
        if let Some(key) = self.navigation_key(strip, group) {
            self.navigation.framed.insert(strip, key);
        }
    }

    /// Pan the actual retained row without reframing the selected window on
    /// the following layout. A later explicit selection still reveals it.
    pub fn pan_desktop(&mut self, strip: StripId, offset: f64) -> Option<bool> {
        if self.navigation.active_strip != Some(strip) || !offset.is_finite() {
            return None;
        }
        let row = self.row_mut(strip)?;
        if row.groups.iter().any(|group| group.maximized) {
            return Some(false);
        }
        let moved = row.pan_to(offset);
        if let Some(group) = self.navigation.groups.get(&strip).copied()
            && let Some(key) = self.navigation_key(strip, group)
        {
            self.navigation.framed.insert(strip, key);
            self.navigation.manual_pan = Some((strip, group));
        }
        Some(moved)
    }

    /// An explicit app selection reveals even an already-selected app after
    /// manual panning. Clearing only this strip leaves retained peers intact.
    pub fn reveal_on_next_layout(&mut self, strip: StripId) {
        self.navigation.manual_pan = None;
        self.navigation.framed.remove(&strip);
    }

    fn navigation_key(&self, strip: StripId, group: GroupId) -> Option<FrameKey> {
        let row = self.row(strip)?;
        if row.get_rect().width() <= 0.0
            || row.group_drag_active()
            || row.wants_pointer_capture()
            || !row.group_layout_is_current()
            || !row.group_is_laid_out(group)
        {
            return None;
        }
        let widths = row
            .groups
            .iter()
            .map(|group| (group.id, group.get_rect().width()))
            .collect::<Vec<_>>();
        if widths.iter().any(|(_, width)| *width <= 0.0) {
            return None;
        }
        Some(FrameKey {
            group,
            viewport: row.get_rect().width(),
            leading_inset: row.leading_inset(),
            widths,
        })
    }

    pub(crate) fn apply_navigation(&mut self) {
        let strips = self.strip_ids();
        let count: usize = strips
            .iter()
            .filter_map(|id| self.row(*id))
            .map(|row| row.groups.len())
            .sum();
        let focused = self.navigation.active_strip.and_then(|strip| {
            self.navigation
                .groups
                .get(&strip)
                .map(|group| (strip, *group))
        });
        for strip in strips {
            let target = self.navigation.groups.get(&strip).copied();
            if let Some(row) = self.row_mut(strip) {
                for group in &mut row.groups {
                    group.set_focused(
                        count > 1 && !group.is_closing() && focused == Some((strip, group.id)),
                    );
                }
            }
            let Some(group) = target else {
                self.navigation.framed.remove(&strip);
                continue;
            };
            let Some(key) = self.navigation_key(strip, group) else {
                continue;
            };
            if self.navigation.manual_pan == Some((strip, group)) {
                self.navigation.framed.insert(strip, key);
                continue;
            }
            if self.navigation.framed.get(&strip) == Some(&key) {
                continue;
            }
            if let Some(row) = self.row_mut(strip) {
                row.scroll_group_into_view_animated(group, ui::clock::Instant::now());
            }
            self.navigation.framed.insert(strip, key);
        }
    }

    pub(crate) fn observe_focus_request(
        &mut self,
        target: Option<taffy::NodeId>,
        tree: &taffy::TaffyTree<()>,
    ) {
        let Some(target) = target else {
            return;
        };
        if let Some(focused) = self.strip_ids().into_iter().find_map(|strip| {
            self.row(strip)?
                .group_for_focus_target(target, tree)
                .map(|group| (strip, group))
        }) {
            self.navigation.pending_focus = Some(focused);
        }
    }

    pub fn take_group_focus_intent(&mut self) -> Option<LocalViewIntent> {
        // Finish the physical click before moving its target. Some real apps
        // activate or claim keyboard focus on release.
        if self.navigation.pressed_group.is_some() {
            return None;
        }
        self.navigation
            .pending_focus
            .take()
            .map(|(strip, group)| LocalViewIntent::Group { strip, group })
    }

    pub(crate) fn observe_navigation_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale: f64,
    ) {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.navigation.cursor = Some(Point::new(position.x / scale, position.y / scale));
            }
            WindowEvent::CursorLeft { .. } => self.navigation.cursor = None,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(point) = self.navigation.cursor else {
                    return;
                };
                self.navigation.pressed_group = self.strip_ids().into_iter().find_map(|strip| {
                    let row = self.row(strip)?;
                    if !row.get_rect().contains(point) {
                        return None;
                    }
                    let visible = row.children();
                    row.groups.iter().find_map(|group| {
                        (visible
                            .iter()
                            .any(|widget| std::ptr::addr_eq(*widget, group as &dyn Widget))
                            && !group.is_closing()
                            && group.get_rect().contains(point))
                        .then_some((strip, group.id))
                    })
                });
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if let Some((strip, group)) = self.navigation.pressed_group.take() {
                    if self.row(strip).is_some_and(|row| !row.group_drag_active()) {
                        self.navigation.pending_focus = Some((strip, group));
                    }
                }
            }
            _ => {}
        }
    }
}
