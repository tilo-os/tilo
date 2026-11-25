//! The driver's window bar: the app shown as a window of its own outside a Tilo session, with the
//! shared row of its parts, its search and History across the window's top
//! ([`super::super::part_bar`]), the one the native runner shows over a platform window.

use super::Driver;
use crate::part_row::WindowAction;
use crate::window_toolbar::{SearchOpening, ToolbarEvent, WindowToolbar};

impl Driver {
    /// While the returned guard lives, the app's window-bar calls act on this driver's window
    /// (once its bar is presented), never on another driver's in the same thread.
    pub(super) fn running(&self) -> Option<crate::window_toolbar::WindowScope> {
        self.window
            .as_ref()
            .map(|(window, _)| crate::window_toolbar::enter(Some(window)))
    }

    /// Show the app as a window of its own shows it: the row of its parts, its search and its
    /// History across the window's top, its frame under the row. The window keeps its size; the
    /// app's frame is the part under the row.
    pub fn present_in_window_bar(&mut self) {
        self.bar_at_rest = false;
        self.open_window_bar();
    }

    /// [`Self::present_in_window_bar`] as a maximized window stands at rest: the bar hosts the
    /// app's parts, its search and its History, but stands out of the way over the window's top
    /// edge until the person reaches for it, and the app's frame fills the window.
    pub fn present_window_bar_at_rest(&mut self) {
        self.bar_at_rest = true;
        self.open_window_bar();
    }

    /// Lay the whole window out at `viewport` under `theme` for a person using `input`, its bar
    /// presented ([`Self::present_in_window_bar`]) and the app's frame under it, in one frame.
    ///
    /// # Errors
    /// As [`Self::relayout_for`].
    pub fn relayout_window_themed_for(
        &mut self,
        viewport: (u32, u32),
        theme: crate::theme::Theme,
        input: crate::interaction::InteractionEnvironment,
    ) -> Result<(), crate::widget::WidgetLayoutError> {
        self.present_in_window_bar();
        self.relayout_for(viewport, theme, input)
    }

    /// Name the window's bar for the app a root that is no app widget stands for: its kind, which
    /// the bar's icon wears, and its title.
    pub fn set_window_app(&mut self, kind: crate::widget::AppKind, title: impl Into<String>) {
        self.window_app = Some((kind, title.into()));
        if self.bar.is_some() {
            self.surface.title.replace(self.window_title());
            self.run_frame();
        }
    }

    /// The bar at rest hosts the app's parts as a shown bar does, and takes none of the window's
    /// height: the app's frame starts at the window's top.
    pub(super) fn rest_bar(&mut self) {
        if crate::window_toolbar::set_hosted(true) {
            self.app.needs_sync = true;
            self.app.layout.layout_dirty = true;
        }
        if !self.app.floating.window_bar.presents() {
            self.app.set_window_presents_history(true);
        }
        if self.app.layout.part_bar_inset > 0.0 {
            self.app.layout.part_bar_inset = 0.0;
            self.app.layout.layout_dirty = true;
        }
    }

    fn open_window_bar(&mut self) {
        if self.bar.is_none() {
            // What the app already said on the thread's bar (its parts, a notice, reactions) is
            // this window's from here on.
            let window = crate::hosted_window::HostedWindow::new();
            window.declare(crate::window_toolbar::declared());
            window.notify(crate::window_toolbar::notice());
            window.show_reactions(crate::window_toolbar::reactions());
            let scope = crate::window_toolbar::enter(Some(&window));
            self.window = Some((window, scope));
            self.bar = Some(Box::new(super::super::part_bar::PartBar::new()));
        }
        self.surface.title.replace(self.window_title());
        self.run_frame();
    }

    /// The window's title, which names its bar: the app's.
    fn window_title(&self) -> String {
        #[cfg(feature = "model")]
        if let Some(bound) = self
            .app
            .root_widget
            .as_any()
            .and_then(|root| root.downcast_ref::<crate::widget::BoundAppWidget>())
        {
            return crate::AppWidget::app_title(bound).to_owned();
        }
        self.window_app
            .as_ref()
            .map(|(_, title)| title.clone())
            .unwrap_or_default()
    }

    /// Whether the window's bar stands across the window's top.
    #[must_use]
    pub fn presents_window_bar(&self) -> bool {
        self.bar
            .as_deref()
            .is_some_and(super::super::part_bar::PartBar::is_shown)
    }

    /// The host leaves the window's frame to the app, as a desktop without server-side
    /// decorations does: the window's bar carries its minimize, maximize and close at its
    /// trailing end, and a press on the bar's ground moves the window.
    pub fn host_window_controls(&mut self) {
        self.surface.frames_itself.set(true);
        self.run_frame();
    }

    /// Whether the window's bar carries the window's own controls.
    #[must_use]
    pub fn hosts_window_controls(&self) -> bool {
        self.bar
            .as_deref()
            .is_some_and(super::super::part_bar::PartBar::shows_window_controls)
    }

    /// Whether the window is maximized: its bar's maximize control and a double press on its
    /// ground toggle it.
    #[must_use]
    pub fn window_maximized(&self) -> bool {
        self.surface.maximized.get()
    }

    /// What the window's own controls asked of the window since the last call, oldest first:
    /// move, maximize, minimize, close.
    pub fn take_window_actions(&mut self) -> Vec<WindowAction> {
        std::mem::take(&mut *self.surface.window_actions.borrow_mut())
    }

    /// Hand the window what the bar's controls asked of it through automation, as a press
    /// hands it ([`super::super::part_bar::BarWindow::act`]).
    pub(super) fn apply_window_actions(&mut self) {
        let Some(bar) = self.bar.as_mut() else {
            return;
        };
        for action in bar.take_window_actions() {
            super::super::part_bar::BarWindow::act(&self.surface, action);
        }
    }

    /// The window the app's bar is this driver's own on, once it is presented.
    #[must_use]
    pub fn window(&self) -> Option<&crate::hosted_window::HostedWindow> {
        self.window.as_ref().map(|(window, _)| window)
    }

    /// What the window's bar shows: the parts the app declared, with its notice and reactions.
    #[must_use]
    pub fn window_toolbar(&self) -> Option<WindowToolbar> {
        match self.window() {
            Some(window) => window.shown(),
            None => crate::window_toolbar::shown(),
        }
    }

    /// The window's bar as automation reaches it: the app's parts, the window's History, its
    /// search and its own controls, each with the target [`Self::perform_window_part`] runs.
    #[must_use]
    pub fn window_parts(&self) -> Vec<crate::a11y::AccessNode> {
        self.bar
            .as_deref()
            .map(super::super::part_bar::PartBar::parts)
            .unwrap_or_default()
    }

    /// Run `action` on the bar's part `target`, as an assistive technology or an agent does:
    /// what it runs reaches the app as a press on it does.
    ///
    /// # Errors
    /// When the bar is not presented, or the part does not take the action now.
    pub fn perform_window_part(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
    ) -> Result<(), String> {
        if !self.presents_window_bar() {
            return Err(format!(
                "{target} is part of the window's bar: present it (present_in_window_bar)"
            ));
        }
        crate::motion::note_source(crate::tokens::MotionSource::Action);
        let running = self.running();
        let took = self
            .bar
            .as_mut()
            .is_some_and(|bar| bar.perform(&mut self.app, target, action));
        drop(running);
        self.apply_window_actions();
        self.run_frame();
        if took {
            Ok(())
        } else {
            Err(format!(
                "the window's bar did not take {action:?} on {target} now"
            ))
        }
    }

    /// Press the part named `name` in the window's bar (one of the app's actions, a menu item,
    /// the window's History), or the button by that name in the app's header row across the
    /// window's top, as the person finds it by what it says.
    ///
    /// # Errors
    /// When neither the bar nor the header row shows anything by that name.
    pub fn press_window_action(&mut self, name: &str) -> Result<(), String> {
        fn find(
            nodes: &[crate::a11y::AccessNode],
            name: &str,
            offered: &mut Vec<String>,
        ) -> Option<String> {
            for node in nodes {
                if let Some(target) = node.action_target.as_deref() {
                    let label = node.name.clone().unwrap_or_default();
                    if label == name {
                        return Some(target.to_owned());
                    }
                    offered.push(label);
                }
                if let Some(found) = find(&node.children, name, offered) {
                    return Some(found);
                }
            }
            None
        }
        let mut offered = Vec::new();
        if let Some(target) = find(&self.window_parts(), name, &mut offered) {
            return self.perform_window_part(&target, &crate::a11y::Action::Activate);
        }
        // An app whose own header row carries its actions across the window's top shows them
        // there, beside the bar's: the person clicks the one they read.
        let spot = self.header_row_button(name).ok_or_else(|| {
            format!(
                "the window's bar has no `{name}`, only {offered:?}, nor its header row, only {:?}",
                self.header_row_names()
            )
        })?;
        let at = spot.center();
        self.click_at(at.x, at.y);
        Ok(())
    }

    /// Hand the app `event` about one of its own parts, as the bar delivers a press on it: the
    /// app takes the keyboard, and the frame after shows what it did.
    pub fn press_window_part(&mut self, event: ToolbarEvent) {
        let running = self.running();
        self.app.deliver_app_part_event(event);
        self.app.needs_sync = true;
        self.app.layout.layout_dirty = true;
        drop(running);
        self.run_frame();
    }

    /// The History control the window's bar lists beside the app's parts: none while the app has
    /// no History to open.
    #[must_use]
    pub fn window_history_control(&self) -> Option<crate::widget::HistoryControl> {
        self.app.window_history_control()
    }

    /// Press the window's History control in its bar's menu, as the person does: it runs on the
    /// app's History API.
    ///
    /// # Errors
    /// When the app has no History to open, or the bar is not presented.
    pub fn press_window_history(&mut self) -> Result<(), String> {
        self.window_history_control()
            .ok_or("History is unavailable")?;
        let target = format!(
            "{}{}",
            crate::widget::window_toolbar_row::ACTION_TARGET_PREFIX,
            crate::part_row::HISTORY
        );
        self.perform_window_part(&target, &crate::a11y::Action::Activate)
    }

    /// Type `query` into the window's search, opened with Find or Replace as its chord opens it,
    /// and carry it to the app's widgets, which find it in place. The current match and the
    /// count, as the bar shows them.
    pub fn search_in_window(
        &mut self,
        opening: SearchOpening,
        query: &str,
    ) -> Option<(usize, usize)> {
        if !self.presents_window_bar() {
            return None;
        }
        let running = self.running();
        if let Some(bar) = self.bar.as_mut()
            && !bar.search().is_open()
        {
            bar.open_search(&mut self.app, opening);
        }
        drop(running);
        self.perform_window_part(
            "window-search-query",
            &crate::a11y::Action::SetValue(query.into()),
        )
        .ok()?;
        self.window_search_position()
    }

    /// Step the window's search to the next (or previous) match.
    pub fn step_window_search(&mut self, forward: bool) -> Option<(usize, usize)> {
        self.bar.as_mut()?.search_mut().step(forward);
        self.run_frame();
        self.window_search_position()
    }

    /// Type `with` into the open search's replacement and press Replace (or Replace all); the
    /// frame after writes what the app made of it. Whether the search offered Replace on what the
    /// window shows.
    pub fn replace_in_window(&mut self, with: &str, all: bool) -> bool {
        if !self.window_search_replaces() {
            return false;
        }
        if self
            .perform_window_part(
                "window-search-replacement",
                &crate::a11y::Action::SetValue(with.into()),
            )
            .is_err()
        {
            return false;
        }
        if let Some(bar) = self.bar.as_mut() {
            bar.search_mut().request_replace(all);
        }
        self.run_frame();
        true
    }

    /// The window's search's current match and count; `None` with nothing found.
    #[must_use]
    pub fn window_search_position(&self) -> Option<(usize, usize)> {
        self.bar.as_ref()?.search().position()
    }

    /// The query the window's search field shows.
    #[must_use]
    pub fn window_search_query(&self) -> String {
        self.bar
            .as_ref()
            .map(|bar| bar.search().query().to_owned())
            .unwrap_or_default()
    }

    /// What the window's search counts: the rows a list kept of all of them, the current match of
    /// all matches in a document; `None` before anything is typed.
    #[must_use]
    pub fn window_search_count(&self) -> Option<crate::widget::FilterCount> {
        self.bar.as_ref()?.search().count()
    }

    /// Whether the window's search offers Replace on what the window shows.
    #[must_use]
    pub fn window_search_replaces(&self) -> bool {
        self.bar
            .as_ref()
            .is_some_and(|bar| bar.search().abilities().replace)
    }
}
