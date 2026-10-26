//! Switching what a window's content shows: [`ViewTabs`].
//!
//! A window that shows one of several views of its content (a request's
//! Params, Headers, Auth and Body; a container's Logs, Stats, Inspect and
//! Terminal) switches between them with tabs, and only with tabs. The tabs
//! have the window tab bar's look: no track, each tab as wide as its label,
//! the chosen one a selector lifted off the ground it stands on.
//!
//! A segmented control ([`ToggleGroup`]) is for choosing a *value* (an
//! alignment, a density, a unit). The two read differently on purpose, and
//! assistive technology meets them differently: tabs are a tab list whose
//! chosen tab is selected, a segmented control is a group of toggles one of
//! which is checked.
//!
//! The strip is a [`ToggleGroup`] in its tab style underneath, so keyboard
//! (arrows, Home, End), touch (selection commits on release, so a scroll
//! never switches tabs), the published select action, scrolling when the
//! tabs are wider than their room, and theme changes behave the same as
//! every other strip.

use super::toggle_group::{Segment, SegmentedStyle, ToggleGroup};
use crate::a11y::{AccessNode, Role};
use crate::widget::ControlSize;

/// Tabs that switch the view of a window's content.
///
/// ```ignore
/// ViewTabs::new(vec![Segment::new("Logs"), Segment::new("Stats")], 0)
///     .on_view("container-tabs", move |index| intents.push(Intent::ShowTab(index)))
/// ```
pub struct ViewTabs {
    strip: ToggleGroup,
}

impl ViewTabs {
    /// Tabs over `views`, the one at `selected` showing.
    pub fn new(views: Vec<Segment>, selected: usize) -> Self {
        Self::of(ToggleGroup::new(views, selected))
    }

    /// Tabs over `views`, the showing one kept in `state`: choosing a tab
    /// stores its value, and a value set anywhere else (a shortcut, the app)
    /// shows its tab.
    pub fn from_state<T: Clone + PartialEq + 'static>(
        views: Vec<(T, Segment)>,
        state: &crate::LocalState<T>,
    ) -> Self {
        Self::of(ToggleGroup::from_state(views, state))
    }

    fn of(strip: ToggleGroup) -> Self {
        Self {
            strip: strip.with_style(SegmentedStyle::Tabs),
        }
    }

    /// Keep the showing tab's index in `state`.
    #[must_use]
    pub fn bind_selection(self, state: &crate::LocalState<usize>) -> Self {
        self.map(|strip| strip.bind_selection(state))
    }

    /// Called with the tab the person chose.
    #[must_use]
    pub fn on_change<F: FnMut(usize) + 'static>(self, f: F) -> Self {
        self.map(|strip| strip.on_change(f))
    }

    /// Publish choosing a tab as the strip's own select action under `id`,
    /// so pointer, keyboard, assistive technology and agents switch views
    /// through one operation; `f` gets the chosen index.
    #[must_use]
    pub fn on_view<F: FnMut(usize) + 'static>(self, id: &str, f: F) -> Self {
        self.map(|strip| strip.on_view(id, f))
    }

    /// Route every choice through a caller-owned action.
    #[must_use]
    pub fn on_action(self, binding: crate::tilo_actions::ActionBinding<usize>) -> Self {
        self.map(|strip| strip.on_action(binding))
    }

    /// A stable identity for headless driving and accessibility actions.
    #[must_use]
    pub fn with_id(self, id: impl Into<String>) -> Self {
        self.map(|strip| strip.with_id(id))
    }

    /// A Fluent message id naming the tab list for assistive technology.
    #[must_use]
    pub fn with_accessible_name(self, name: impl Into<String>) -> Self {
        self.map(|strip| strip.with_accessible_name(name))
    }

    /// The tabs' height and label role.
    #[must_use]
    pub fn control_size(self, size: ControlSize) -> Self {
        self.map(|strip| strip.control_size(size))
    }

    /// Share the parent's width equally instead of each tab fitting its
    /// label: a narrow panel's two or three views.
    #[must_use]
    pub fn fill(self) -> Self {
        self.map(ToggleGroup::fill)
    }

    #[must_use]
    pub fn with_enabled(self, enabled: bool) -> Self {
        self.map(|strip| strip.with_enabled(enabled))
    }

    /// The tab showing.
    pub fn selected(&self) -> usize {
        self.strip.selected()
    }

    /// Show the tab at `selected` without reporting a change.
    pub fn set_selected(&mut self, selected: usize) {
        self.strip.set_selected(selected);
    }

    /// Place the tabs at `rect` in a host that lays its parts out by hand.
    pub fn apply_layout_external(&mut self, rect: vello::kurbo::Rect) {
        self.strip.apply_layout_external(rect);
    }

    /// Where the tab at `index` stands since the last layout.
    #[must_use]
    pub fn tab_rect(&self, index: usize) -> Option<vello::kurbo::Rect> {
        self.strip.segment_rect(index)
    }

    fn map(self, f: impl FnOnce(ToggleGroup) -> ToggleGroup) -> Self {
        Self {
            strip: f(self.strip),
        }
    }
}

/// The strip's node as a tab list: the strip a [`Role::TabBar`], each
/// choice a [`Role::Tab`] that is selected rather than checked.
fn as_tab_list(mut node: AccessNode) -> AccessNode {
    node.role = Role::TabBar;
    for tab in &mut node.children {
        if tab.role == Role::ToggleButton {
            tab.role = Role::Tab;
            tab.state.selected = tab.state.checked == Some(true);
            tab.state.checked = None;
        }
    }
    node
}

impl crate::widget::Decorator for ViewTabs {
    type Inner = ToggleGroup;

    fn inner(&self) -> &ToggleGroup {
        &self.strip
    }

    fn inner_mut(&mut self) -> &mut ToggleGroup {
        &mut self.strip
    }

    fn accessibility(&self) -> Option<AccessNode> {
        crate::widget::Widget::accessibility(&self.strip).map(as_tab_list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::Action;
    use crate::widget::Widget;

    fn tabs() -> ViewTabs {
        ViewTabs::new(
            ["Params", "Headers", "Auth", "Body"]
                .into_iter()
                .map(Segment::new)
                .collect(),
            1,
        )
        .with_id("request-tabs")
    }

    #[test]
    fn view_tabs_are_a_tab_list_whose_showing_tab_is_selected() {
        let node = tabs().accessibility().expect("tabs describe themselves");
        assert_eq!(node.role, Role::TabBar);
        let roles: Vec<Role> = node.children.iter().map(|tab| tab.role).collect();
        assert_eq!(roles, vec![Role::Tab; 4]);
        let selected: Vec<bool> = node.children.iter().map(|tab| tab.state.selected).collect();
        assert_eq!(selected, vec![false, true, false, false]);
        assert!(
            node.children.iter().all(|tab| tab.state.checked.is_none()),
            "a tab is selected, never checked"
        );
        let names: Vec<_> = node
            .children
            .iter()
            .map(|tab| tab.name.clone().unwrap_or_default())
            .collect();
        assert_eq!(names, ["Params", "Headers", "Auth", "Body"]);
    }

    #[test]
    fn a_segmented_control_stays_a_group_of_toggles() {
        let strip = ToggleGroup::new(vec![Segment::new("Left"), Segment::new("Right")], 0);
        let node = strip.accessibility().expect("a strip describes itself");
        assert_eq!(node.role, Role::Group);
        assert!(node.children.iter().all(|c| c.role == Role::ToggleButton));
    }

    #[test]
    fn activating_a_tab_from_assistive_technology_shows_it_and_reports_it() {
        let chosen = std::rc::Rc::new(std::cell::Cell::new(None));
        let seen = std::rc::Rc::clone(&chosen);
        let mut tabs = tabs().on_change(move |index| seen.set(Some(index)));
        let target = tabs
            .accessibility()
            .and_then(|node| {
                node.children
                    .get(3)
                    .and_then(|tab| tab.action_target.clone())
            })
            .expect("each tab is an action target");
        let acted = tabs.perform_semantic_action(
            &target,
            &Action::Activate,
            &mut crate::widget::Edits::lend(&mut automerge::AutoCommit::new()),
            &automerge::ROOT,
        );
        assert!(acted);
        assert_eq!(tabs.selected(), 3);
        assert_eq!(chosen.get(), Some(3));
    }

    #[test]
    fn a_hand_placed_strip_names_where_each_tab_stands() {
        let theme = crate::theme::Theme::dark();
        let mut tabs = tabs();
        let strip = vello::kurbo::Rect::new(10.0, 20.0, 410.0, 60.0);
        tabs.apply_layout_external(strip);
        tabs.draw(
            &mut vello::Scene::new(),
            vello::kurbo::Affine::IDENTITY,
            &mut crate::text_render::TextRenderer::new(),
            &theme,
            &crate::presence::PresenceCtx::default(),
        );
        tabs.apply_layout_external(strip);
        let rects: Vec<_> = (0..4).map(|i| tabs.tab_rect(i).expect("a tab")).collect();
        assert_eq!(tabs.tab_rect(4), None);
        for pair in rects.windows(2) {
            if let [left, right] = pair {
                assert!(
                    left.x1 <= right.x0,
                    "tabs run left to right without overlap"
                );
            }
        }
        for rect in &rects {
            assert!(rect.width() > 0.0);
            assert!(rect.x0 >= strip.x0 && rect.x1 <= strip.x1);
        }
    }
}
