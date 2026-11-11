//! The desktop's window bar over a headless app's frame.
//!
//! In a Tilo session the desktop draws one bar per window: the app's icon,
//! the slots the app declares there ([`crate::window_toolbar`]), the
//! window's search and History controls and its own controls. Driven
//! headless, an app runs outside a session, so nothing draws that bar and
//! a picture of it would show the app without the chrome a person sees.
//! [`FrameHarness::present_in_window_bar`] makes the harness stand in for the
//! desktop: the app is told the bar hosts its slots, search and History
//! (as the native runner tells it over its shell connection), its frame is
//! laid out under the bar, and every capture paints the bar above it with
//! the shell's own [`TabBar`] and [`WindowSearch`].

use crate::host_core::KeyHost as _;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::FrameHarness;
use crate::widget::DrawChild;
use crate::widget::{TabBar, TabDescriptor, Widget, WindowBar, WindowSearchRequest};

/// The one tab the stand-in window shows.
const TAB: u64 = 1;

/// Room the bar keeps for the window's search control, as the window group
/// keeps it.
const SEARCH_CONTROL_ROOM: f64 =
    crate::widget::window_controls::CONTROL_PX + crate::widget::window_controls::CONTROL_GAP;

impl FrameHarness {
    /// Show the app as a desktop window shows it: its slots, search and
    /// History in the window's bar, its frame under the bar. The window
    /// keeps the size it had; the app gets the part under the bar, and a
    /// capture (`super::write_capture`) names the whole window.
    ///
    /// A header row that cannot stand as slots (it holds a catch-up, a popover) is the window's
    /// bar instead, as a window's frame hosts it: the row fills the window and the window's
    /// controls stand at its trailing end ([`Self::row_holds_bar`]).
    pub fn present_in_window_bar(&mut self) {
        self.present_window_bar(true);
    }

    /// Show the app as a window stands at rest: the bar hidden over the window's top edge, as a
    /// maximized window's is until the person reaches for it, and the app's frame filling the
    /// window. The bar still hosts the app's slots — a capture of this window draws no bar above
    /// the frame — so what the app offers in it is what the person finds on reaching for the top
    /// edge.
    pub fn present_window_bar_at_rest(&mut self) {
        self.present_window_bar(false);
    }

    /// Whether the window's bar stands out at rest, over the app's frame. Always, save for a
    /// harness that shows the window at rest ([`Self::present_window_bar_at_rest`]).
    #[must_use]
    pub fn window_bar_revealed(&self) -> bool {
        self.bar_revealed
    }

    fn present_window_bar(&mut self, revealed: bool) {
        self.invalidate_local();
        self.bar_revealed = revealed;
        if !self.window_bar {
            self.window_bar = true;
            self.host_bar_window();
            if let Some(history) = &mut self.history {
                history.remove_fallback_bar();
                history.set_open_button(false);
                history.set_window_presents(true);
            }
        }
        let _running = self.running();
        // The bar the frame offers its content's header row: a row that takes it fills the
        // window, with the window's controls at its trailing end, and one that does not leaves
        // the bar its band above ([`Self::row_holds_bar`]). Every frame offers it, as a window's
        // frame does, so the row a screen the app opened brought with it is offered too.
        let bar = self.window_bar_offer();
        let presents = self.presents_search();
        crate::widget::window_header::offer_root_header_bar(self.root.as_mut(), Some(bar));
        // A row holding the bar carries the search the app keeps in it; a bar over the frame
        // presents the window's own.
        crate::widget::window_header::offer_window_search(self.root.as_mut(), presents);
        let frame = self.content_viewport(self.viewport);
        if let Err(error) = self.resize(frame) {
            tracing::warn!(%error, "window bar: the frame under it did not lay out");
        }
        self.refresh_row_holds();
    }

    /// This window is the one the app's bar calls act on while the guard lives: what it declares
    /// or asks there is this window's, never another window's a process lays out beside it. None
    /// while the harness shows no bar, where the thread's own window is the app's.
    pub(super) fn running(&self) -> Option<crate::window_toolbar::WindowScope> {
        self.window
            .as_ref()
            .map(|window| crate::window_toolbar::enter(Some(window)))
    }

    /// Take a window of this harness's own to host the app's bar in, standing as the thread's own
    /// while the harness lives: the app declares its slots on it and asks it for what its bar
    /// does, so windows a process lays out beside each other keep their bars apart.
    pub(super) fn host_bar_window(&mut self) {
        // The app may show a notice or reactions on the thread's own bar before this harness hosts
        // a window for it: they are this window's from here on, so a read that comes before the
        // next frame sees what the app already said. Its slots are this window's by the frame
        // that declares them, declared on the window the shell reads them from then on.
        let notice = crate::window_toolbar::notice();
        let reactions = crate::window_toolbar::reactions();
        let window = crate::hosted_window::HostedWindow::new();
        window.notify(notice);
        window.show_reactions(reactions);
        self.window_scope = Some(crate::window_toolbar::enter(Some(&window)));
        self.window = Some(window);
    }

    /// The window this harness shows the app in, once it stands in for the desktop's bar.
    #[must_use]
    pub fn window(&self) -> Option<&crate::hosted_window::HostedWindow> {
        self.window.as_ref()
    }

    /// What this window's bar shows: the slots the app declared on this window, with the notice
    /// up and the reactions shown. The thread's own bar while the harness stands in for none.
    #[must_use]
    pub fn window_toolbar(&self) -> Option<crate::window_toolbar::WindowToolbar> {
        match &self.window {
            Some(window) => window.shown(),
            None => crate::window_toolbar::shown(),
        }
    }

    /// Read whether the app's frame holds the window's bar in a header row of its own, from the
    /// frame just laid out: a row that draws one takes the bar, and the window's controls stand
    /// at its trailing end. A row that appeared with the screen the app opened makes the frame
    /// lay itself out again in the room the row leaves, so the window shows one bar either way.
    fn refresh_row_holds(&mut self) {
        let holds = crate::widget::window_header::content_bar(self.root.as_ref()).is_some();
        if holds == self.row_holds {
            return;
        }
        self.row_holds = holds;
        // The search follows the bar it stands in: the row's own while the row is the bar, the
        // window's while the bar stands over the frame.
        let presents = self.presents_search();
        crate::widget::window_header::offer_window_search(self.root.as_mut(), presents);
        let frame = self.content_viewport(self.viewport);
        if let Err(error) = self.resize(frame) {
            tracing::warn!(%error, "window bar: the frame under it did not lay out");
        }
    }

    /// The window's bar as the frame offers it to the content's header row: the room the
    /// window's furniture takes at its trailing end ([`crate::widget::merged_row_width`]), shown
    /// now, listing nothing else.
    fn window_bar_offer(&self) -> WindowBar {
        WindowBar {
            controls_width: crate::widget::merged_row_width_in(f64::from(self.viewport.0)),
            lists_actions: false,
        }
    }

    /// Whether the window presents the app's search in its own bar: always while its bar stands
    /// over the frame, and while a row holds the bar only where the room it keeps for its
    /// furniture holds one ([`crate::widget::merged_row_holds_search`]) — otherwise the row draws
    /// its own search control.
    fn presents_search(&self) -> bool {
        !self.row_holds || crate::widget::merged_row_holds_search(f64::from(self.viewport.0))
    }

    /// Whether the app's frame holds the window's bar in a header row of its own, rather than
    /// standing under a bar of the window's.
    #[must_use]
    pub fn row_holds_bar(&self) -> bool {
        self.row_holds
    }

    /// Where the content's header row stands and where the window's controls go while the row
    /// holds the bar.
    pub(super) fn row_bar_slot(&self) -> Option<crate::widget::WindowBarSlot> {
        self.row_holds
            .then(|| crate::widget::window_header::content_bar(self.root.as_ref()))
            .flatten()
    }

    /// The height of the header row the window root draws of its own under the window's bar: a
    /// window shows one bar, so a row under it is a second one. `0.0` while the root holds the
    /// bar itself, and for a root that draws no row of its own (a desk of windows, whose windows
    /// each offer their own bar).
    #[must_use]
    pub fn window_header_row_height(&self) -> f64 {
        if self.row_holds {
            return 0.0;
        }
        crate::widget::window_header::root_header_row(self.root.as_ref())
            .map_or(0.0, crate::widget::WindowHeader::header_row_height)
    }

    /// Whether the harness draws the desktop's window bar over the app.
    #[must_use]
    pub fn presents_window_bar(&self) -> bool {
        self.window_bar
    }

    /// Where each column of the window stands and the header row it draws at its top, as the
    /// frame was laid out last. A column's header stands at every time, the bar out or at rest,
    /// so this is what the person reads at the top of each column before reaching for the bar.
    #[must_use]
    pub fn column_frames(&self) -> Vec<crate::widget::ColumnFrame> {
        crate::widget::window_header::root_header_row(self.root.as_ref())
            .map(crate::widget::WindowHeader::column_frames)
            .unwrap_or_default()
    }

    /// What the window's header rows say, as the person reads them at the top of each column:
    /// the names of the titles and controls standing in a column's header row, in the order the
    /// window lays them out. A shell whose app declares no bar slots draws these rows as the
    /// window's bar, so this is where its location and commands stand.
    #[must_use]
    pub fn header_row_names(&self) -> Vec<String> {
        self.header_row_parts()
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    /// Each named title and control in a column's header row, and where it stands.
    fn header_row_parts(&self) -> Vec<(String, Rect)> {
        fn visit(node: &super::InspectionNode, rows: &[Rect], parts: &mut Vec<(String, Rect)>) {
            let [x, y, width, height] = node.rect;
            let rect = Rect::new(x, y, x + width, y + height);
            if rect.area() > 0.0
                && rows.iter().any(|row| row.contains(rect.center()))
                && let Some(name) = node
                    .semantics
                    .as_ref()
                    .and_then(|access| access.name.as_deref())
                    .filter(|name| !name.trim().is_empty())
                && parts.last().map(|(last, _)| last.as_str()) != Some(name)
            {
                parts.push((name.to_owned(), rect));
            }
            for child in &node.children {
                visit(child, rows, parts);
            }
        }
        let rows: Vec<Rect> = self
            .column_frames()
            .into_iter()
            .map(|frame| frame.header)
            .filter(|header| header.area() > 0.0)
            .collect();
        let mut parts = Vec::new();
        visit(&self.inspection().root, &rows, &mut parts);
        parts
    }

    /// Lay the whole window out at `viewport`, its bar included, under
    /// `theme`: the app's frame gets the part under the bar, as the desktop
    /// gives it one. The bar itself is laid out when it is drawn
    /// ([`Self::window_bar_layout`], a capture).
    ///
    /// # Errors
    /// As [`Self::relayout_themed`].
    pub fn relayout_window_themed(
        &mut self,
        viewport: (u32, u32),
        theme: crate::theme::Theme,
    ) -> Result<(), crate::widget::WidgetLayoutError> {
        self.present_in_window_bar();
        let frame = self.content_viewport(viewport);
        self.relayout_themed(frame, theme)?;
        // What the app built this frame may draw a row the last one did not (a screen it
        // opened): the window shows one bar either way.
        self.refresh_row_holds();
        Ok(())
    }

    /// Lay the whole window out at `viewport` under `theme` for a person
    /// using `input`, as [`Self::relayout_window_themed`] does: the window is
    /// laid out once, for the person and the size it will be read at, which
    /// [`Self::set_interaction_environment`] would otherwise do first at the
    /// size it was last laid out at.
    ///
    /// # Errors
    /// As [`Self::relayout_themed`].
    pub fn relayout_window_themed_for(
        &mut self,
        viewport: (u32, u32),
        theme: crate::theme::Theme,
        input: crate::interaction::InteractionEnvironment,
    ) -> Result<(), crate::widget::WidgetLayoutError> {
        self.invalidate_local();
        self.take_interaction_environment(input);
        self.relayout_window_themed(viewport, theme)
    }

    /// The History control the window lists beside the app's slots, as the
    /// desktop's bar (or, outside a session, the runner's slot row) shows
    /// it: none while the app has no History to open.
    #[must_use]
    pub fn window_history_control(&self) -> Option<crate::widget::HistoryControl> {
        self.history
            .as_deref()
            .and_then(crate::history_mode::host::window_control)
    }

    /// Press the window's History control as the bar hands the press over:
    /// it runs on the app's History door, as the History shortcut does, and
    /// never reaches the app.
    pub fn press_window_history(&mut self) -> Result<(), String> {
        self.invalidate_local();
        let control = self
            .window_history_control()
            .ok_or("History is unavailable")?;
        let history = self.history.as_mut().ok_or("History is unavailable")?;
        crate::history_mode::host::press_window_control(history.as_mut(), control)?;
        self.relayout();
        Ok(())
    }

    /// Type `query` into the window bar's search, opened with Find or
    /// Replace as the desktop opens it, and carry it to the app's widgets,
    /// which find it in place. Returns the current match and the count, as
    /// the bar shows them.
    pub fn search_in_window(
        &mut self,
        opening: crate::window_toolbar::SearchOpening,
        query: &str,
    ) -> Option<(usize, usize)> {
        if !self.window_search.is_open() {
            let offer = crate::window_toolbar::SearchOffer::of(self.root.as_mut());
            self.window_search.open_as(
                offer.into(),
                crate::tokens::MotionSource::Keyboard,
                opening,
            );
        }
        self.window_search_action(
            "window-search-query",
            &crate::a11y::Action::SetValue(query.into()),
        );
        self.drive_window_search()
    }

    /// Step the window bar's search to the next (or previous) match.
    pub fn step_window_search(&mut self, forward: bool) -> Option<(usize, usize)> {
        self.window_search.step(forward);
        self.drive_window_search()
    }

    /// Type `with` into the open search's replacement and press Replace
    /// (or Replace all); the frame after writes what the app made of it.
    /// Returns whether the search offered Replace on what the window shows.
    pub fn replace_in_window(&mut self, with: &str, all: bool) -> bool {
        self.drive_window_search();
        if !self.window_search.abilities().replace {
            return false;
        }
        self.window_search_action(
            "window-search-replacement",
            &crate::a11y::Action::SetValue(with.into()),
        );
        self.window_search.request_replace(all);
        self.drive_window_search();
        true
    }

    /// The window bar's search's current match and count, as the bar
    /// shows them; `None` with nothing found.
    #[must_use]
    pub fn window_search_position(&self) -> Option<(usize, usize)> {
        self.window_search.position()
    }

    /// The query the window bar's search field shows.
    #[must_use]
    pub fn window_search_query(&self) -> &str {
        self.window_search.query()
    }

    /// What the window bar's search counts: the rows a list or a query view
    /// kept of all of them, the current match of all matches in a document;
    /// `None` before anything is typed.
    #[must_use]
    pub fn window_search_count(&self) -> Option<crate::widget::FilterCount> {
        self.window_search.count()
    }

    /// Whether the window bar's search offers Replace on what the window
    /// shows.
    #[must_use]
    pub fn window_search_replaces(&self) -> bool {
        self.window_search.abilities().replace
    }

    /// Perform `action` on a part of the window bar's search (a
    /// `window-search-*` target: open it, type the query or the
    /// replacement, set an option, step, replace, close), as an agent
    /// performs it on the desktop's window, then carry the search to the
    /// app's widgets. The window carries out what the search asks of it:
    /// it opens in place where the app offers that.
    pub(super) fn perform_window_search_semantic(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
    ) -> Result<(), String> {
        if !self.window_bar {
            return Err(format!(
                "{target} is part of the window bar's search: present the window bar"
            ));
        }
        // The search's parts stand where the bar lays them out; the bar
        // spans the window, as wide as the app under it.
        let width = f64::from(self.viewport.0);
        self.paint_window_bar(&mut Scene::new(), Affine::IDENTITY, width);
        let handled = self.window_search_action(target, action);
        for request in self.window_search.take_requests() {
            match request {
                WindowSearchRequest::Open => {
                    let offer = crate::window_toolbar::SearchOffer::of(self.root.as_mut());
                    self.window_search.open_as(
                        offer.into(),
                        crate::tokens::MotionSource::Action,
                        crate::window_toolbar::SearchOpening::Find,
                    );
                }
                WindowSearchRequest::Close => {
                    self.window_search
                        .close(crate::tokens::MotionSource::Action);
                }
                WindowSearchRequest::Widen(_) => {
                    return Err(format!(
                        "{target} widens the search to the desktop's, which a single app's window does not host"
                    ));
                }
            }
        }
        if !handled {
            return Err(format!(
                "the window bar's search did not take {action:?} on {target} now"
            ));
        }
        // What the action opened (the search, its options) is laid out
        // before the app hears of it, as the next frame lays it out on the
        // desktop: the app is told what the options cover.
        self.paint_window_bar(&mut Scene::new(), Affine::IDENTITY, width);
        self.drive_window_search();
        Ok(())
    }

    fn window_search_action(&mut self, target: &str, action: &crate::a11y::Action) -> bool {
        let mut scratch = automerge::AutoCommit::new();
        self.window_search.perform_semantic_action(
            target,
            action,
            &mut crate::widget::Edits::lend(&mut scratch),
            &automerge::ROOT,
        )
    }

    /// Carry the search to the app's widgets, then run the frame that shows
    /// (and writes) what they made of it.
    fn drive_window_search(&mut self) -> Option<(usize, usize)> {
        self.invalidate_local();
        self.window_search.drive(self.root.as_mut());
        self.relayout();
        self.window_search.position()
    }

    /// One frame ([`FrameHarness::try_relayout`]), then the open search
    /// carried to the app's widgets, as the live window carries it once a
    /// frame, so what the app shows after the query was typed (a program's
    /// output, a page that loaded) is found too; the frame lays out again
    /// when that changed what it shows.
    pub(super) fn try_relayout_following_search(
        &mut self,
    ) -> Result<(), crate::widget::WidgetLayoutError> {
        self.try_relayout()?;
        if self.window_bar
            && self.window_search.wants_drive()
            && self.window_search.drive(self.root.as_mut())
        {
            self.invalidate_local();
            return self.try_relayout();
        }
        Ok(())
    }

    /// Press one of the app's own slots in the window's bar (a mode, an
    /// action, the sidebar toggle), as the desktop hands the press to the
    /// app. The press is a press on the app: it hands the app the keyboard
    /// (`Self::hand_keyboard_to_app`), as in the native window. What the
    /// app asked of the host when its sync took the press (the Undo beside a
    /// notice asks for an undo) is done and shown, as the live window's idle
    /// service does after that sync.
    pub fn press_window_slot(&mut self, event: crate::window_toolbar::ToolbarEvent) {
        let _running = self.running();
        self.invalidate_local();
        if crate::window_toolbar::deliver_to_app(event) {
            self.hand_keyboard_to_app();
        }
        self.relayout();
        self.host_tick();
        self.invalidate_local();
        self.relayout();
    }

    /// Press the action named `name` the app declares in its window's bar,
    /// among its leading actions, its actions and their menu items, as the
    /// person finds it by what it says.
    ///
    /// # Errors
    /// Says what the bar offers when no action is named `name`.
    /// A window whose app declares no slots shows its commands in its shell's header row, which
    /// is the window's bar there: the control named `name` in that row is clicked.
    ///
    /// # Errors
    /// Says what the bar offers when no action is named `name`.
    pub fn press_window_action(&mut self, name: &str) -> Result<(), String> {
        let _running = self.running();
        let Some(toolbar) = self.window_toolbar() else {
            let parts = self.header_row_parts();
            let (_, rect) = parts.iter().find(|(part, _)| part == name).ok_or_else(|| {
                let offered: Vec<&str> = parts.iter().map(|(part, _)| part.as_str()).collect();
                format!("the window's header row has no `{name}`, only {offered:?}")
            })?;
            let at = rect.center();
            self.click_at(at.x, at.y);
            return Ok(());
        };
        let actions = toolbar.leading.iter().chain(&toolbar.actions);
        let mut offered = Vec::new();
        let mut found = None;
        for action in actions.flat_map(|action| std::iter::once(action).chain(&action.items)) {
            if action.name == name && action.items.is_empty() {
                found = Some(action.id.clone());
                break;
            }
            offered.push(action.name.clone());
        }
        let id = found.ok_or_else(|| format!("the bar has no `{name}`, only {offered:?}"))?;
        self.press_window_slot(crate::window_toolbar::ToolbarEvent::Action { id });
        Ok(())
    }

    /// The app's frame for a window of `viewport`: the part under the bar.
    pub(super) fn content_viewport(&self, viewport: (u32, u32)) -> (u32, u32) {
        if self.window_bar {
            (
                viewport.0,
                viewport
                    .1
                    .saturating_sub(self.window_bar_height_px(viewport.0)),
            )
        } else {
            viewport
        }
    }

    /// Where the app's frame stands in a window of `viewport`: how far below the window's top it
    /// starts (the desktop's bar above it) and how tall it is, in the window's own pixels.
    pub fn frame_extent(&self, viewport: (u32, u32)) -> (f64, f64) {
        let (_, height) = self.content_viewport(viewport);
        (
            f64::from(viewport.1.saturating_sub(height)),
            f64::from(height),
        )
    }

    /// Where a point of the window (a capture's picture, as the person sees
    /// it) is in the app's frame under the bar. The bar is the desktop's:
    /// a point on it is refused, never handed to the app.
    pub(super) fn frame_point(&self, x: f64, y: f64) -> Result<(f64, f64), String> {
        let frame_y = self.frame_y(y);
        if frame_y < 0.0 {
            return Err(format!(
                "({x}, {y}) is on the window bar, which is the desktop's: invoke the \
                 action its slot runs instead"
            ));
        }
        Ok((x, frame_y))
    }

    /// [`Self::frame_point`]'s height alone, for the end of a gesture that
    /// may leave the frame.
    pub(super) fn frame_y(&self, y: f64) -> f64 {
        y - self.frame_top()
    }

    /// A rect of the app's frame where the window shows it.
    pub(super) fn window_rect(&self, rect: Rect) -> Rect {
        rect + vello::kurbo::Vec2::new(0.0, self.frame_top())
    }

    /// Where the app's frame starts in the window.
    fn frame_top(&self) -> f64 {
        if self.window_bar {
            f64::from(self.window_bar_height_px(self.viewport.0))
        } else {
            0.0
        }
    }

    /// The window's title and app, as its tab shows them.
    pub(super) fn window_tab(&self) -> TabDescriptor {
        let bound = self
            .root
            .as_any()
            .and_then(|root| root.downcast_ref::<crate::widget::BoundAppWidget>());
        let (kind, title) = match bound {
            Some(bound) => (
                Some(crate::AppWidget::app_kind(bound)),
                crate::AppWidget::app_title(bound).to_owned(),
            ),
            None => self
                .window_app
                .clone()
                .map_or((None, String::new()), |(kind, title)| (Some(kind), title)),
        };
        let mut tab = TabDescriptor::new(TAB, title);
        tab.app_kind = kind;
        tab
    }

    /// The bar as the desktop would draw it for this window now, laid out
    /// across `width`.
    fn window_bar(&self, width: f64) -> TabBar {
        let tab = self.window_tab();
        let mut bar = TabBar::new("headless-window", vec![tab], TAB);
        bar.set_window_controls(true);
        bar.set_search_control(Some(SEARCH_CONTROL_ROOM));
        // The room the window's search keeps: the app says whether it searches in place at all,
        // as its offer does when the desktop opens the search.
        bar.set_search_least(
            (self.window_search.in_place() != crate::widget::InPlace::Nothing)
                .then(|| crate::widget::WindowSearch::least_span(&self.theme)),
        );
        bar.set_toolbar(TAB, self.window_toolbar());
        bar.set_history_control(TAB, self.window_history_control());
        bar.set_group_focused(true);
        // The first layout gives the bar its width; the second measures a
        // compact window's notice strip for it.
        lay_out_bar(&mut bar, width, &self.theme);
        lay_out_bar(&mut bar, width, &self.theme);
        bar
    }

    /// The bar's height in whole logical pixels across a window `width`
    /// wide: its row, and a compact window's notice strip under it. None while the app's frame
    /// holds the bar in a header row of its own: the row is the bar.
    pub(super) fn window_bar_height_px(&self, width: u32) -> u32 {
        if self.row_holds || !self.bar_revealed {
            return 0;
        }
        let height = self.window_bar(f64::from(width)).bar_height().ceil();
        // A bar is a row or two and a strip tall, far inside `u32`.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the bar's height is a small positive layout length"
        )]
        let px = height.max(0.0) as u32;
        px
    }

    /// The window's bar as the desktop draws it across a window `width`
    /// wide, at rest, its labels measured as painted.
    pub(super) fn painted_window_bar(&mut self, width: f64) -> TabBar {
        let _settled = crate::motion::testing::settled();
        let mut bar = self.window_bar(width);
        bar.draw_child(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &self.presence,
        );
        lay_out_bar(&mut bar, width, &self.theme);
        bar
    }

    /// The window's bar as it lays itself out across a window `width` wide, as the desktop draws
    /// it: the app's slots, the bar's own overflow, the History control and the window's own
    /// controls, and the room it leaves empty. A capture, an inspector and a layout check read
    /// the bar through it.
    pub fn window_bar_layout(&mut self, width: f64) -> crate::window_toolbar::BarLayout {
        if !self.window_bar || !self.bar_revealed {
            return crate::window_toolbar::BarLayout {
                spare: 0.0,
                controls: Vec::new(),
                labels: Vec::new(),
            };
        }
        // A row holding the bar is the app's own: its slots are read in the frame, and the window's
        // furniture stands at the row's trailing end.
        if self.row_holds {
            let Some(slot) = self.row_bar_slot() else {
                return crate::window_toolbar::BarLayout {
                    spare: 0.0,
                    controls: Vec::new(),
                    labels: Vec::new(),
                };
            };
            return crate::window_toolbar::BarLayout {
                spare: 0.0,
                controls: self.row_furniture_layout(slot),
                // The app's own row stands in the frame, where its words are read as the
                // widgets that paint them: the bar paints no words of its own.
                labels: Vec::new(),
            };
        }
        self.painted_window_bar(width).bar_layout()
    }

    /// The window's furniture in a row holding the bar, as the layout check reads the bar's
    /// controls: the new-tab door and the window's own close/restore.
    fn row_furniture_layout(
        &self,
        slot: crate::widget::WindowBarSlot,
    ) -> Vec<crate::window_toolbar::BarControl> {
        use crate::widget::window_controls::WindowControl;
        use crate::window_toolbar::BarControl;
        let lookup = crate::i18n::I18nManager::lookup_plain;
        let furniture = crate::widget::window_header::RowFurniture::of(slot);
        let mut out = Vec::new();
        if furniture.new_tab.area() > 0.0 {
            out.push(crate::widget::tabs::new_tab_control(furniture.new_tab));
        }
        for (slot, control, rect) in [
            (
                "minimize",
                WindowControl::Minimize,
                furniture.controls.minimize,
            ),
            (
                "maximize",
                WindowControl::Maximize,
                furniture.controls.maximize,
            ),
            ("close", WindowControl::Close, furniture.controls.close),
        ] {
            if rect.area() <= 0.0 {
                continue;
            }
            let resize = crate::widget::window_controls::Resize::Maximize;
            out.push(
                BarControl::new(slot, control.target(), rect)
                    .named(lookup(control.name_key(resize)))
                    .icon(Some(control.icon(resize).name().to_owned())),
            );
        }
        out
    }

    /// How the window's bar, as the desktop draws it across a window
    /// `width` wide, shows the notice up (none: no notice is up).
    #[must_use]
    pub fn window_bar_notice(&mut self, width: f64) -> Option<WindowBarNotice> {
        Self::shown_notice(&self.painted_window_bar(width))
    }

    /// How the bar `bar` shows the notice up: none while it shows none.
    fn shown_notice(bar: &TabBar) -> Option<WindowBarNotice> {
        let bar_height = bar.bar_height();
        let row = bar.toolbar_row();
        let toolbar = row.toolbar()?;
        let slots = row.slots();
        let plain_leading = slots
            .leading
            .iter()
            .zip(&toolbar.leading)
            .filter(|(rect, action)| !action.navigates && rect.area() > 0.0)
            .count();
        let beside = slots.actions.len() + plain_leading;
        let buttons = |notice: &crate::window_toolbar::ToolbarNotice,
                       rects: &[Rect],
                       labelled: &dyn Fn(usize) -> bool| {
            notice
                .actions
                .iter()
                .zip(rects)
                .enumerate()
                .map(|(i, (action, rect))| WindowBarNoticeButton {
                    name: action.name.clone(),
                    bounds: *rect,
                    labelled: labelled(i),
                })
                .collect()
        };
        // A compact window's notice, in its strip under the row.
        if let Some((notice, text, whole, rects)) = row.strip_notice() {
            return Some(WindowBarNotice {
                text: notice.text.clone(),
                text_bounds: text,
                whole,
                buttons: buttons(notice, &rects, &|_| true),
                beside,
                strip: Some(row.strip_rect()),
                bar_height,
            });
        }
        let notice = toolbar.notice.as_ref()?;
        Some(WindowBarNotice {
            text: notice.text.clone(),
            whole: row.notice_whole().unwrap_or(false),
            buttons: buttons(notice, &slots.notice, &|i| {
                slots.notice_labelled.get(i).copied().unwrap_or(false)
            }),
            text_bounds: slots.location,
            beside,
            strip: None,
            bar_height,
        })
    }

    /// Click the notice's button `index` where the window's bar, as the
    /// desktop draws it across a window `width` wide, puts it (in the
    /// location's place, or in a compact window's strip), and hand the app
    /// what the press runs, as [`Self::press_window_slot`] does.
    ///
    /// # Errors
    /// When no notice with that button is up, or the press runs nothing.
    pub fn click_window_notice_action(&mut self, width: f64, index: usize) -> Result<(), String> {
        let mut bar = self.painted_window_bar(width);
        let at = Self::shown_notice(&bar)
            .and_then(|notice| {
                notice
                    .buttons
                    .get(index)
                    .map(|button| button.bounds.center())
            })
            .ok_or_else(|| format!("no notice button {index} stands in the bar at {width} px"))?;
        let row = bar.toolbar_row_mut();
        if !(row.press(at) && row.release(at)) {
            return Err(format!("the notice button {index} took no press at {at:?}"));
        }
        self.deliver_bar_events(&mut bar)
    }

    /// Run the notice's button `index` through its semantic action, as
    /// assistive technology and automation do, on the window's bar as the
    /// desktop draws it across a window `width` wide, and hand the app
    /// what it runs, as [`Self::press_window_slot`] does.
    ///
    /// # Errors
    /// When the bar lists no such button, or its action runs nothing.
    pub fn invoke_window_notice_action(&mut self, width: f64, index: usize) -> Result<(), String> {
        fn targets(node: &crate::a11y::AccessNode, out: &mut Vec<String>) {
            if let Some(target) = node.action_target.as_deref()
                && target.starts_with(crate::widget::window_toolbar_row::NOTICE_TARGET_PREFIX)
            {
                out.push(target.to_owned());
            }
            for child in &node.children {
                targets(child, out);
            }
        }
        let mut bar = self.painted_window_bar(width);
        let mut found = Vec::new();
        if let Some(tree) = bar.accessibility() {
            targets(&tree, &mut found);
        }
        let target = found
            .get(index)
            .ok_or_else(|| format!("the bar lists no notice button {index} at {width} px"))?;
        if !bar.toolbar_row_mut().perform(target) {
            return Err(format!("{target} ran nothing"));
        }
        self.deliver_bar_events(&mut bar)
    }

    /// Hand the app what was done in `bar`'s slots.
    pub(super) fn deliver_bar_events(&mut self, bar: &mut TabBar) -> Result<(), String> {
        let events = bar.take_toolbar_events();
        if events.is_empty() {
            return Err("the bar ran nothing".into());
        }
        for (_, event) in events {
            self.press_window_slot(event);
        }
        Ok(())
    }

    /// Paint the window's bar across the top of a `width`-wide window.
    pub(super) fn paint_window_bar(&mut self, scene: &mut Scene, transform: Affine, width: f64) {
        if !self.bar_revealed {
            // At rest the bar stands out of the way, over the window's top edge: the frame
            // fills the window, and the bar comes when the person reaches for it.
            return;
        }
        // A capture shows the bar at rest, as a person sees it after it
        // arrived: its slots' cross-fade has ended.
        let _settled = crate::motion::testing::settled();
        assert!(
            self.window_tab().app_kind.is_some(),
            "a hosted window's bar wears its app's icon, as every desktop window's does: \
             mount the app with FrameHarness::for_app"
        );
        let title = self.window_tab().title;
        let frame_top = self.frame_top();
        // The app's header row is the bar: it draws itself, and the window's furniture stands at
        // its trailing end.
        if self.row_holds {
            if let Some(slot) = self.row_bar_slot() {
                let furniture = crate::widget::window_header::RowFurniture::of(slot);
                let search = &mut self.window_search;
                search.set_app_name(&title);
                furniture.place_search(search);
                search.set_frame_origin(Point::new(0.0, frame_top));
                furniture.paint(
                    scene,
                    transform,
                    &mut self.text,
                    &self.theme,
                    &self.presence,
                    search,
                    crate::widget::window_header::RowFurnitureState::default(),
                );
            }
            return;
        }
        let mut bar = self.window_bar(width);
        let search = &mut self.window_search;
        search.set_app_name(&title);
        crate::widget::place_window_search(&bar, search);
        search.set_frame_origin(Point::new(0.0, frame_top));
        let ground = bar.ground_color(&self.theme);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            ground,
            None,
            &Rect::new(0.0, 0.0, width, bar.bar_height()),
        );
        bar.set_popup_area(Rect::new(0.0, 0.0, width, f64::from(self.viewport.1)));
        bar.draw_child(
            scene,
            transform,
            &mut self.text,
            &self.theme,
            &self.presence,
        );
        let search = &mut self.window_search;
        search.set_ground(ground);
        search.draw_child(
            scene,
            transform,
            &mut self.text,
            &self.theme,
            &self.presence,
        );
    }

    /// Paint the bar's top layer over the app's frame: the open search's
    /// options and replacement, under the field.
    pub(super) fn paint_window_bar_top_layer(&mut self, scene: &mut Scene, transform: Affine) {
        self.window_search.draw_overlay(
            scene,
            transform,
            &mut self.text,
            &self.theme,
            &self.presence,
        );
    }
}

/// How the window's bar shows the notice up
/// ([`FrameHarness::window_bar_notice`]).
#[derive(Clone, Debug, PartialEq)]
pub struct WindowBarNotice {
    /// What it says.
    pub text: String,
    /// Where its text stands, its kind's icon first.
    pub text_bounds: Rect,
    /// Its text stands whole, not cut short.
    pub whole: bool,
    /// Its buttons, in order, where they stand (none: it has none, or the
    /// row has no room for its text).
    pub buttons: Vec<WindowBarNoticeButton>,
    /// How many of the app's actions and the leading actions that do not
    /// move through the content stand in the row beside it, not in the
    /// overflow menu: what yields to it.
    pub beside: usize,
    /// Its strip under the bar's row, where the window is compact (none:
    /// it stands inline, in the location's place).
    pub strip: Option<Rect>,
    /// The whole bar's height, the strip included: where the app's frame
    /// starts.
    pub bar_height: f64,
}

/// The notice's button as the window's bar shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowBarNoticeButton {
    pub name: String,
    pub bounds: Rect,
    /// It names itself, not only its icon.
    pub labelled: bool,
}

/// The bar's height in whole logical pixels without a notice strip: one
/// row, the slots in it.
#[cfg(test)]
fn bar_height_px() -> u32 {
    // A fixed layout constant, far inside `u32`.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "TAB_BAR_HEIGHT is a small positive layout constant"
    )]
    let px = crate::widget::tabs::TAB_BAR_HEIGHT.ceil() as u32;
    px
}

/// Lay `bar` out across `width` as the window group does: the bar is the
/// first child of a column as wide as the window.
fn lay_out_bar(bar: &mut TabBar, width: f64, theme: &crate::theme::Theme) {
    use crate::num::f32_of_f64;
    use std::collections::HashMap;
    use taffy::{AvailableSpace, Dimension, NodeId, Style, TaffyTree};

    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = bar.build_layout(&mut taffy, theme, &mut node_map);
    let height = bar.bar_height();
    let Ok(root) = taffy.new_with_children(
        Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: Dimension::length(f32_of_f64(width)),
                height: Dimension::length(f32_of_f64(height)),
            },
            ..Default::default()
        },
        &[node],
    ) else {
        return;
    };
    if crate::widget::solve_layout(
        &mut taffy,
        root,
        taffy::Size {
            width: AvailableSpace::Definite(f32_of_f64(width)),
            height: AvailableSpace::Definite(f32_of_f64(height)),
        },
    )
    .is_err()
    {
        return;
    }
    bar.apply_layout(&taffy, node, Point::ZERO);
}

#[cfg(test)]
#[path = "window_bar_layout_tests.rs"]
mod layout_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::WindowSearch;

    #[test]
    fn a_window_bar_capture_hosts_the_apps_slots_above_its_frame() {
        let _hosted = HostedReset;
        let theme = crate::theme::Theme::dark().with_reduce_motion(true);
        let mut harness = FrameHarness::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(crate::widget::Column::new()),
            (800, 600),
            theme,
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture".into()),
        );
        assert!(!crate::window_toolbar::is_hosted());
        harness.present_in_window_bar();
        assert!(
            crate::window_toolbar::is_hosted(),
            "the app hears the bar hosts it"
        );
        assert!(harness.presents_window_bar());
        assert_eq!(
            harness.viewport,
            (800, 600 - bar_height_px()),
            "the window keeps its size; the app gets the part under the bar"
        );
        assert_eq!(harness.content_viewport((800, 644)), (800, 600));
        crate::window_toolbar::declare(Some(crate::window_toolbar::WindowToolbar {
            sidebar: Some(true),
            actions: vec![crate::window_toolbar::ToolbarAction::new(
                "new",
                "plus",
                "New event",
            )],
            ..crate::window_toolbar::WindowToolbar::default()
        }));
        let bar = harness.window_bar(800.0);
        assert!((bar.bar_height() - crate::widget::tabs::TAB_BAR_HEIGHT).abs() < f64::EPSILON);
        let rects = bar.test_rects();
        let sidebar = rects
            .iter()
            .find(|(name, _)| name == "window_sidebar")
            .map_or(Rect::ZERO, |(_, rect)| *rect);
        assert!(sidebar.area() > 0.0, "the declared slots stand in the bar");
        let (_, _, _, actions) = bar.toolbar_rects();
        assert_eq!(actions.len(), 1);
        crate::window_toolbar::declare(None);
    }

    /// A shell's header row is the window's bar, whatever it holds — a popover, as a window's
    /// catch-up opens, or plain buttons: the row fills the window, its title and the window's
    /// controls stand in it, and no second row stacks above.
    #[test]
    fn a_header_row_that_keeps_a_popover_is_the_window_bar() {
        let _hosted = HostedReset;
        let row = |actions: Vec<crate::widget::Element>| {
            crate::widget::NavShell::new()
                .content(Box::new(crate::widget::Text::new("Body")))
                .content_header("Design", actions)
        };
        let popover = crate::widget::Button::new("catch-up", "Mia changed 3 messages")
            .with_popover(crate::widget::Text::new("What changed"));
        let mut harness = harness_of(Box::new(row(vec![Box::new(popover)])));
        harness.present_in_window_bar();
        assert!(harness.row_holds_bar(), "the row is the window's bar");
        assert_eq!(
            harness.frame_extent((800, 600)).0,
            0.0,
            "no bar band stands above the row"
        );
        let bar = harness.window_bar_layout(800.0);
        assert!(
            bar.controls.iter().any(|control| control.slot == "close"),
            "the window's controls stand at the row's end: {bar:?}"
        );
        assert_eq!(
            harness.window_header_row_height(),
            0.0,
            "the row is the bar, not a second one under it"
        );

        // A row of plain buttons is the bar all the same: its buttons stand in the row, never
        // again as the bar's slots.
        let mut plain = harness_of(Box::new(row(vec![Box::new(crate::widget::Button::new(
            "action", "New",
        ))])));
        plain.present_in_window_bar();
        assert!(plain.row_holds_bar());
        assert_eq!(plain.frame_extent((800, 600)).0, 0.0);
        assert_eq!(plain.window_header_row_height(), 0.0);
        assert_eq!(
            plain.window_toolbar(),
            None,
            "the bar declares none of its buttons"
        );
    }

    /// A root that draws no header row of its own — a desk of windows — offers each window its
    /// own bar: a row its windows draw is no second row under the bar over the desk.
    #[test]
    fn a_desk_of_windows_is_no_second_row_under_the_bar() {
        use crate::component::Described as _;
        let _hosted = HostedReset;
        let inner = crate::widget::NavShell::new()
            .content(Box::new(crate::widget::Text::new("Body")))
            .content_header(
                "Design",
                vec![Box::new(
                    crate::widget::Button::new("catch-up", "Mia changed 3 messages")
                        .with_popover(crate::widget::Text::new("What changed")),
                )],
            );
        let desk = crate::widget::WindowFrame::from_props(
            "desk",
            crate::widget::WindowFrameProps {
                body: Some(Box::new(inner)),
                ..crate::widget::WindowFrameProps::default()
            },
        );
        let mut harness = harness_of(Box::new(desk));
        harness.present_in_window_bar();
        assert!(!harness.row_holds_bar(), "the desk draws no row of its own");
        assert_eq!(
            harness.window_header_row_height(),
            0.0,
            "the row its window draws is no second row under the bar over the desk"
        );
    }

    fn harness_of(root: Box<dyn Widget>) -> FrameHarness {
        FrameHarness::new_with_controlled_inputs(
            crate::State::new(),
            root,
            (800, 600),
            crate::theme::Theme::dark().with_reduce_motion(true),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture".into()),
        )
    }

    fn focused_name(harness: &FrameHarness) -> Option<String> {
        ["first", "chosen"]
            .into_iter()
            .find(|name| {
                harness
                    .access_node_named(name, None)
                    .is_ok_and(|node| node.state.focused)
            })
            .map(str::to_owned)
    }

    /// A press on one of the app's own slots in the window's bar is a press
    /// on the app: with nothing in the window holding the keyboard, the
    /// control the app declares its default takes it, never the first
    /// button merely for being first, so what is typed next reaches the
    /// app. Typing in the bar's search keeps the keyboard where it is, and
    /// a control that holds it keeps it.
    #[test]
    fn a_slot_press_hands_the_keyboard_to_the_apps_default_control() {
        let root = crate::widget::Column::new()
            .push(Box::new(crate::widget::Button::new("first", "First")))
            .push(Box::new(
                crate::widget::Button::new("chosen", "Chosen").with_default_focus(),
            ));
        let mut harness = harness_of(Box::new(root));
        harness.relayout();
        assert_eq!(harness.focused(), None, "nothing holds the keyboard");

        harness
            .press_window_slot(crate::window_toolbar::ToolbarEvent::Search { query: "a".into() });
        assert_eq!(harness.focused(), None, "the bar's search keeps it");

        harness.press_window_slot(crate::window_toolbar::ToolbarEvent::Action {
            id: "mode:1".into(),
        });
        assert_eq!(focused_name(&harness).as_deref(), Some("chosen"));

        harness.click_named("first").unwrap();
        assert_eq!(focused_name(&harness).as_deref(), Some("first"));
        harness.press_window_slot(crate::window_toolbar::ToolbarEvent::ToggleSidebar);
        assert_eq!(
            focused_name(&harness).as_deref(),
            Some("first"),
            "a control that holds the keyboard keeps it"
        );
        let _ = crate::window_toolbar::take_events();
    }

    /// A window that takes the keyboard (it opened, or was brought
    /// forward) hands it to the app's default control when nothing holds
    /// it; a window of buttons only keeps none focused, since Enter would
    /// press the first.
    #[test]
    fn a_window_taking_the_keyboard_hands_it_to_the_apps_default_control() {
        let root = crate::widget::Column::new()
            .push(Box::new(crate::widget::Button::new("first", "First")))
            .push(Box::new(
                crate::widget::Button::new("chosen", "Chosen").with_default_focus(),
            ));
        let mut harness = harness_of(Box::new(root));
        harness.relayout();
        harness.send(&winit::event::WindowEvent::Focused(true));
        assert_eq!(focused_name(&harness).as_deref(), Some("chosen"));

        let buttons = crate::widget::Column::new()
            .push(Box::new(crate::widget::Button::new("first", "First")));
        let mut harness = harness_of(Box::new(buttons));
        harness.relayout();
        harness.send(&winit::event::WindowEvent::Focused(true));
        assert_eq!(harness.focused(), None, "never a button for being first");
    }

    /// A spec aims at the picture it captures: with the bar over the app, a
    /// click on a control where the capture shows it presses that control.
    #[test]
    fn a_click_aimed_at_the_captured_window_lands_where_it_is_painted() {
        let _hosted = HostedReset;
        let root = crate::widget::Column::new()
            .push(Box::new(crate::widget::Button::new("first", "First")))
            .push(Box::new(crate::widget::Button::new("chosen", "Chosen")));
        let mut harness = harness_of(Box::new(root));
        harness.present_in_window_bar();
        harness.relayout();
        let first = harness
            .targets()
            .into_iter()
            .find_map(|(name, rect)| (name == "first").then(|| harness.window_rect(rect)))
            .expect("the first button is a target");
        let bar = f64::from(bar_height_px());
        assert!(first.y0 >= bar, "the app's frame starts under the bar");
        let centre = first.center();
        harness
            .perform(&super::super::Action::ClickAt(centre.x, centre.y))
            .unwrap();
        assert_eq!(focused_name(&harness).as_deref(), Some("first"));
        assert!(
            harness
                .perform(&super::super::Action::ClickAt(centre.x, bar / 2.0))
                .is_err(),
            "the bar is the desktop's"
        );
    }

    /// An app that is only its name.
    pub(super) struct NamedApp;

    impl Widget for NamedApp {
        fn name(&self) -> &str {
            "NamedApp"
        }
        fn build_layout(
            &mut self,
            tree: &mut taffy::TaffyTree<()>,
            _: &crate::theme::Theme,
            nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            use crate::widget::TaffyTreeExt as _;
            tree.widget_leaf(taffy::Style::default(), nodes, self)
        }
    }

    impl crate::widget::AppWidget for NamedApp {
        fn app_kind(&self) -> crate::widget::AppKind {
            crate::widget::AppKind::Files
        }
        fn app_title(&self) -> &str {
            "Files"
        }
        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Glyph("F")
        }
    }

    // Every hosted bar wears its app's icon at its leading edge, whatever
    // the location says (a cloud folder, a page, nothing).
    #[test]
    fn every_hosted_bar_wears_its_apps_icon() {
        let _hosted = HostedReset;
        let mut harness = FrameHarness::for_app(
            crate::State::new(),
            NamedApp,
            (800, 600),
            crate::theme::Theme::dark(),
        );
        harness.present_in_window_bar();
        crate::window_toolbar::declare(Some(crate::window_toolbar::WindowToolbar {
            location: Some(crate::window_toolbar::ToolbarLocation {
                label: "Trip to Lisbon".into(),
                ..crate::window_toolbar::ToolbarLocation::default()
            }),
            ..crate::window_toolbar::WindowToolbar::default()
        }));
        let tab = harness.window_tab();
        assert_eq!(tab.app_kind, Some(crate::widget::AppKind::Files));
        assert_eq!(tab.title, "Files");
        let mut scene = Scene::new();
        harness.paint_window_bar(&mut scene, Affine::IDENTITY, 800.0);
        crate::window_toolbar::declare(None);
    }

    /// A compact (420 px) window with a long title and no location: the
    /// title takes the room the row leaves it, with no slots and beside one
    /// trailing action alike, and is cut short only where none is left.
    #[test]
    fn a_compact_windows_long_title_takes_the_room_the_row_leaves_it() {
        use crate::window_toolbar::{ToolbarAction, WindowToolbar};
        const TITLE: &str = "Video Player Library";
        let _hosted = HostedReset;
        let mut harness = FrameHarness::for_app(
            crate::State::new(),
            NamedApp,
            (420, 800),
            crate::theme::Theme::dark(),
        );
        // The location names it in the body style.
        let style = harness.theme.text(crate::tokens::TextRole::Body);
        let title_w = f64::from(harness.text.measure_style(TITLE, None, &style).0);
        let one_action = WindowToolbar {
            actions: vec![ToolbarAction::new("open", "folder-open", "Open")],
            ..WindowToolbar::default()
        };
        for toolbar in [None, Some(one_action)] {
            let mut tab = TabDescriptor::new(TAB, TITLE);
            tab.app_kind = Some(crate::widget::AppKind::Files);
            let mut bar = TabBar::new("headless-window", vec![tab], TAB);
            bar.set_window_controls(true);
            bar.set_search_control(Some(SEARCH_CONTROL_ROOM));
            let slots = toolbar.is_some();
            bar.set_toolbar(TAB, toolbar);
            // Painting measures the title; the layout after it sizes it.
            for _ in 0..2 {
                lay_out_bar(&mut bar, 420.0, &harness.theme);
                bar.draw(
                    &mut Scene::new(),
                    Affine::IDENTITY,
                    &mut harness.text,
                    &harness.theme,
                    &harness.presence,
                );
            }
            if slots {
                let row = bar.toolbar_row();
                let location = row.slots().location;
                assert_eq!(
                    row.toolbar()
                        .and_then(|toolbar| toolbar.location.as_ref())
                        .map(|location| location.label.as_str()),
                    Some(TITLE),
                    "the title stands in the location's place"
                );
                assert!(
                    location.width() >= title_w,
                    "beside one action the title stands whole: {location:?}, {title_w} px"
                );
                assert_eq!(row.slots().actions.len(), 1, "and the action beside it");
            } else {
                assert_eq!(
                    bar.visible_title(0, &mut harness.text, &harness.theme)
                        .as_deref(),
                    Some(TITLE),
                    "with no slots the title stands whole"
                );
            }
        }
    }

    /// A compact window's notice has a strip of its own under the bar's
    /// row: its whole text and its actions as buttons that name themselves,
    /// the row standing as it does without a notice, and the app's frame
    /// moved down by the strip until the notice goes, then back. A wide
    /// window's notice stands inline, with no strip. Either button reaches
    /// the app, by pointer and by its semantic action, at either width.
    #[test]
    fn a_compact_windows_notice_has_its_own_strip_and_a_wide_ones_stands_inline() {
        use crate::widget::toast::ToastKind;
        use crate::window_toolbar::{ToolbarAction, ToolbarLocation, ToolbarNotice, WindowToolbar};
        let _hosted = HostedReset;
        let mut harness = FrameHarness::for_app(
            crate::State::new(),
            NamedApp,
            (420, 800),
            crate::theme::Theme::dark(),
        );
        harness.present_in_window_bar();
        crate::window_toolbar::declare(Some(WindowToolbar {
            sidebar: Some(false),
            location: Some(ToolbarLocation {
                label: "Launch checklist".into(),
                ..ToolbarLocation::default()
            }),
            actions: vec![
                ToolbarAction::new("share", "share", "Share"),
                ToolbarAction::new("pin", "pin", "Pin"),
            ],
            ..WindowToolbar::default()
        }));
        let quiet = harness.content_viewport((420, 800)).1;
        assert_eq!(quiet, 800 - bar_height_px(), "without a notice, one row");
        let text = "Save failed: your disk is full, so the note was not written";
        crate::window_toolbar::notify(Some(
            ToolbarNotice::new(text, ToastKind::Error)
                .action(ToolbarAction::new(
                    "save-status-retry",
                    "rotate-cw",
                    "Try again",
                ))
                .action(ToolbarAction::new(
                    "open-settings",
                    "settings",
                    "Open Settings",
                )),
        ));

        let shown = harness.window_bar_notice(420.0).expect("the notice is up");
        let strip = shown
            .strip
            .expect("a compact window gives the notice a strip");
        assert_eq!(shown.text, text);
        assert!(shown.whole, "its whole text stands: {shown:?}");
        assert_eq!(
            shown
                .buttons
                .iter()
                .map(|button| button.name.as_str())
                .collect::<Vec<_>>(),
            ["Try again", "Open Settings"],
            "both stand in the strip, in order: {shown:?}"
        );
        for button in &shown.buttons {
            assert!(
                button.labelled
                    && button.bounds.width() > crate::widget::window_controls::CONTROL_PX
                    && strip.contains(button.bounds.center()),
                "{} names itself in the strip: {shown:?}",
                button.name
            );
        }
        assert!(
            (strip.y0 - crate::widget::tabs::TAB_BAR_HEIGHT).abs() < 0.5
                && (shown.bar_height - strip.y1).abs() < 0.5,
            "the strip stands under the row, in the bar: {shown:?}"
        );
        let bar = harness.window_bar(420.0);
        let row = bar.toolbar_row();
        assert_eq!(
            row.toolbar()
                .and_then(|toolbar| toolbar.location.as_ref())
                .map(|location| location.label.as_str()),
            Some("Launch checklist"),
            "the row stands as it does without a notice"
        );
        assert!(row.slots().notice.is_empty());
        let pushed = harness.content_viewport((420, 800)).1;
        assert!(
            f64::from(quiet - pushed) >= strip.height() - 1.0,
            "the app's frame moves down by the strip: {quiet} to {pushed}"
        );

        let wide = harness.window_bar_notice(1280.0).expect("the notice is up");
        assert!(wide.strip.is_none(), "a wide window's notice stands inline");
        assert!(
            wide.whole
                && wide.buttons.len() == 2
                && wide.buttons.iter().all(|button| button.labelled),
            "with Try again and Open Settings named after it: {wide:?}"
        );
        for (width, index, id) in [
            (420.0, 0, "save-status-retry"),
            (420.0, 1, "open-settings"),
            (1280.0, 0, "save-status-retry"),
            (1280.0, 1, "open-settings"),
        ] {
            harness
                .click_window_notice_action(width, index)
                .unwrap_or_else(|why| panic!("{id} at {width} px: {why}"));
            assert!(
                crate::window_toolbar::take_notice_press(id),
                "a click on {id} reaches the app at {width} px"
            );
            harness
                .invoke_window_notice_action(width, index)
                .unwrap_or_else(|why| panic!("{id} at {width} px: {why}"));
            assert!(
                crate::window_toolbar::take_notice_press(id),
                "{id}'s semantic action reaches the app at {width} px"
            );
        }
        assert_eq!(
            harness.content_viewport((1280, 800)).1,
            800 - bar_height_px()
        );

        crate::window_toolbar::notify(None);
        assert_eq!(
            harness.content_viewport((420, 800)).1,
            quiet,
            "the app's frame moves back up once the notice goes"
        );
        crate::window_toolbar::declare(None);
    }

    /// The 1st of the calendar's weeks across a month: Mon, Jun 29 to
    /// Sun, Jul 5, 2026.
    fn week_across_a_month() -> crate::format::Period {
        let day = |m, d| chrono::NaiveDate::from_ymd_opt(2026, m, d).unwrap_or_default();
        crate::format::Period::Days(day(6, 29), day(7, 5))
    }

    /// The calendar's whole bar (the sidebar, previous, next and Today, a
    /// week as its period, five views, New event and a menu of more) in a
    /// compact (420 px) window that lists History, painted once so its
    /// views are measured.
    fn compact_calendar_bar() -> (HostedReset, FrameHarness, TabBar) {
        use crate::window_toolbar::{
            ToolbarAction, ToolbarLocation, ToolbarView, ToolbarViews, WindowToolbar,
        };
        let hosted = HostedReset;
        let mut harness = FrameHarness::for_app(
            crate::State::new(),
            NamedApp,
            (420, 800),
            crate::theme::Theme::dark(),
        );
        harness.present_in_window_bar();
        let view = |id: &str, label: &str| ToolbarView {
            id: id.into(),
            label: label.into(),
        };
        crate::window_toolbar::declare(Some(
            WindowToolbar {
                sidebar: Some(true),
                leading: vec![
                    ToolbarAction::new("previous", "chevron-left", "Previous week").navigating(),
                    ToolbarAction::new("next", "chevron-right", "Next week").navigating(),
                    ToolbarAction::new("today", "calendar", "Today").labelled(),
                ],
                location: Some(ToolbarLocation::period(week_across_a_month())),
                views: Some(ToolbarViews {
                    name: "View".into(),
                    choices: vec![
                        view("month", "Month"),
                        view("week", "Week"),
                        view("day", "Day"),
                        view("agenda", "Agenda"),
                        view("tasks", "Tasks"),
                    ],
                    chosen: "week".into(),
                }),
                actions: vec![
                    ToolbarAction::new("new-event", "plus", "New event"),
                    ToolbarAction::new("export", "upload", "Export calendar").in_menu(),
                    ToolbarAction::new("import", "download", "Import calendar").in_menu(),
                ],
                ..WindowToolbar::default()
            }
            .bounded(),
        ));
        let mut bar = harness.window_bar(420.0);
        // The window lists its History beside the slots, as the calendar's does.
        bar.set_history_control(TAB, Some(crate::widget::HistoryControl::of(false)));
        lay_out_bar(&mut bar, 420.0, &harness.theme);
        // The bar measures its views as it paints, as a capture paints it.
        bar.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut harness.text,
            &harness.theme,
            &harness.presence,
        );
        (hosted, harness, bar)
    }

    /// A compact (420 px) window's bar, beside its search and History, has
    /// no room for the calendar's views beside its navigation and period:
    /// the views and Today (which jumps rather than steps) go into the
    /// overflow menu, previous and next stay, and the period stays, elided.
    #[test]
    fn a_compact_window_keeps_its_navigation_and_location_and_folds_the_views() {
        use crate::a11y::{AccessNode, Role};
        fn find<'a>(node: &'a AccessNode, role: Role, name: &str) -> Option<&'a AccessNode> {
            if node.role == role && node.name.as_deref() == Some(name) {
                return Some(node);
            }
            node.children
                .iter()
                .find_map(|child| find(child, role, name))
        }
        let (_hosted, _harness, bar) = compact_calendar_bar();
        let rects = bar.test_rects();
        let rect = |name: &str| {
            rects
                .iter()
                .find(|(n, _)| n == name)
                .map_or(Rect::ZERO, |(_, r)| *r)
        };
        let sidebar = rect("window_sidebar");
        let previous = rect("window_leading:0");
        let next = rect("window_leading:1");
        let location = rect("window_location");
        let more = rect("window_more");
        let history = rect(crate::widget::tabs::WINDOW_HISTORY_TARGET);
        for (name, slot) in [
            ("the sidebar's toggle", sidebar),
            ("previous", previous),
            ("next", next),
            ("the period", location),
            ("the overflow button", more),
            ("History", history),
        ] {
            assert!(slot.area() > 0.0, "{name} stands in the bar: {rects:?}");
        }
        assert!(
            sidebar.x1 <= previous.x0 && previous.x1 <= next.x0 && next.x1 <= location.x0,
            "in their order: {rects:?}"
        );
        assert!(
            location.width() >= f64::from(crate::tokens::Metrics::BASE.location_min_width),
            "the period keeps at least its least width: {location:?}"
        );
        assert!(location.x1 <= more.x0, "{rects:?}");
        assert!(
            more.x1 + SEARCH_CONTROL_ROOM <= history.x0,
            "the window's search keeps its room before History: {rects:?}"
        );
        assert_eq!(rect("window_leading:2"), Rect::ZERO, "Today folds");
        assert_eq!(rect("window_views_menu"), Rect::ZERO, "so do the views");
        let tree = Widget::accessibility(&bar).expect("the bar is reachable");
        for label in ["Month", "Week", "Day", "Agenda", "Tasks"] {
            let item = find(&tree, Role::MenuItem, label)
                .unwrap_or_else(|| panic!("{label} is in the overflow menu"));
            assert_eq!(item.state.checked, Some(label == "Week"));
        }
        let today = find(&tree, Role::MenuItem, "Today").expect("Today is in the overflow menu");
        assert_eq!(
            today.action_target.as_deref(),
            Some("window-toolbar-leading:today"),
            "and runs as its button does"
        );
        crate::window_toolbar::declare(None);
    }

    /// A compact window's bar has no room for the week as `Mon, Jun 29 –
    /// Sun, Jul 5`: it names it `Jun 29 – Jul 5`, whole, instead of cutting
    /// the wider form short.
    #[test]
    fn a_compact_window_names_its_period_in_a_form_that_fits() {
        let (_hosted, mut harness, bar) = compact_calendar_bar();
        let location = bar
            .test_rects()
            .into_iter()
            .find(|(name, _)| name == "window_location")
            .map_or(Rect::ZERO, |(_, rect)| rect);
        assert!(location.area() > 0.0, "the period stands in the bar");
        let toolbar = crate::window_toolbar::declared().expect("the calendar declared its bar");
        let declared = toolbar.location.as_ref().expect("with its period");
        let forms = week_across_a_month().forms();
        assert_eq!(declared.label, forms[0]);
        let style = harness.theme.text(crate::tokens::TextRole::Body);
        let widest = f64::from(harness.text.measure_style(&forms[0], None, &style).0);
        assert!(
            widest > location.width(),
            "the widest form does not fit: {location:?}"
        );
        let (shown, _) = crate::widget::window_toolbar_row::location_shown(
            declared,
            bar.toolbar_row().slots().form,
            &harness.theme,
        );
        assert_eq!(
            shown, forms[1],
            "the bar names the week in its narrower form"
        );
        let shown_width = f64::from(harness.text.measure_style(shown, None, &style).0);
        assert!(shown_width <= location.width(), "whole");
        crate::window_toolbar::declare(None);
    }

    #[test]
    #[should_panic(expected = "wears its app's icon")]
    fn a_bar_with_no_app_to_wear_refuses_to_paint() {
        let _hosted = HostedReset;
        let mut harness = FrameHarness::for_root(
            crate::State::new(),
            crate::widget::Column::new(),
            (800, 600),
            crate::theme::Theme::dark(),
        );
        harness.present_in_window_bar();
        harness.paint_window_bar(&mut Scene::new(), Affine::IDENTITY, 800.0);
    }

    /// A text the window's search finds in place and can replace in.
    struct Draft {
        text: String,
        found: Vec<crate::widget::find_bar::FindMatch>,
    }

    impl Widget for Draft {
        crate::provides!(dyn crate::widget::Findable);
        fn name(&self) -> &str {
            "Draft"
        }
        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }
        fn build_layout(
            &mut self,
            tree: &mut taffy::TaffyTree<()>,
            _: &crate::theme::Theme,
            nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            use crate::widget::TaffyTreeExt as _;
            tree.widget_leaf(taffy::Style::default(), nodes, self)
        }
    }

    impl crate::widget::AppWidget for Draft {
        fn app_kind(&self) -> crate::widget::AppKind {
            crate::widget::AppKind::Files
        }
        fn app_title(&self) -> &str {
            "Writer"
        }
        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Glyph("W")
        }
    }

    impl crate::widget::Findable for Draft {
        fn find(
            &mut self,
            query: &str,
            _options: crate::widget::find_bar::FindOptions,
        ) -> Vec<crate::widget::find_bar::FindMatch> {
            self.found = if query.is_empty() {
                Vec::new()
            } else {
                self.text
                    .match_indices(query)
                    .map(|(start, m)| {
                        crate::widget::find_bar::FindMatch::new(start, start + m.len())
                    })
                    .collect()
            };
            self.found.clone()
        }
        fn reveal(&mut self, _index: usize) {}
        fn supports_replace(&self) -> bool {
            true
        }
        fn content_revision(&self) -> u64 {
            u64::try_from(self.text.len()).unwrap_or(u64::MAX)
        }
    }

    // A scenario drives the window bar's search through its parts, as an
    // agent does on the desktop: open it, type, step, open the options and
    // type the replacement; what the window does not host is refused.
    #[test]
    fn a_scenario_drives_the_window_bars_search_through_its_parts() {
        use crate::a11y::Action;
        let _hosted = HostedReset;
        let mut harness = FrameHarness::for_app(
            crate::State::new(),
            Draft {
                text: "an apple and an end".into(),
                found: Vec::new(),
            },
            (800, 600),
            crate::theme::Theme::dark(),
        );
        assert!(
            harness
                .perform_semantic_named("window-search-open", None, &Action::Activate)
                .is_err(),
            "the search lives in the window bar"
        );
        harness.present_in_window_bar();
        let mut perform =
            |target: &str, action: Action| harness.perform_semantic_named(target, None, &action);
        perform("window-search-open", Action::Activate).unwrap();
        perform("window-search-query", Action::SetValue("an".into())).unwrap();
        perform("window-search-next", Action::Activate).unwrap();
        perform("window-search-options", Action::Expand).unwrap();
        perform("window-search-replacement", Action::SetValue("the".into())).unwrap();
        assert!(
            perform("window-search-hex", Action::Activate).is_err(),
            "a text offers no hex mode"
        );
        assert!(
            perform("window-search-everywhere", Action::Activate).is_err(),
            "the desktop's search is not the window's"
        );
        assert_eq!(harness.window_search_position(), Some((2, 3)));
        assert!(harness.window_search.options_open());
        assert_eq!(harness.window_search.replacement(), "the");
    }

    // What the app shows after the query was typed is found on the next
    // frame, as the live window drives its search once a frame.
    #[test]
    fn text_that_arrives_after_the_query_is_found_on_the_next_frame() {
        use crate::a11y::Action;
        let _hosted = HostedReset;
        let mut harness = FrameHarness::for_app(
            crate::State::new(),
            Draft {
                text: "an apple".into(),
                found: Vec::new(),
            },
            (800, 600),
            crate::theme::Theme::dark(),
        );
        harness.present_in_window_bar();
        harness
            .perform_semantic_named("window-search-open", None, &Action::Activate)
            .unwrap();
        harness
            .perform_semantic_named("window-search-query", None, &Action::SetValue("an".into()))
            .unwrap();
        assert_eq!(harness.window_search_position(), Some((1, 1)));
        harness
            .widget_mut::<Draft>()
            .expect("the app is a draft")
            .text
            .push_str(" and an end");
        harness.relayout();
        assert_eq!(
            harness.window_search_position(),
            Some((1, 3)),
            "the count follows the new text"
        );
    }

    /// A list the window's search narrows.
    struct Rows {
        rows: Vec<&'static str>,
        filter: String,
    }

    impl Widget for Rows {
        crate::provides!(dyn crate::widget::Filterable);
        fn name(&self) -> &str {
            "Rows"
        }
        fn build_layout(
            &mut self,
            tree: &mut taffy::TaffyTree<()>,
            _: &crate::theme::Theme,
            nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            use crate::widget::TaffyTreeExt as _;
            tree.widget_leaf(taffy::Style::default(), nodes, self)
        }
    }

    impl crate::widget::AppWidget for Rows {
        fn app_kind(&self) -> crate::widget::AppKind {
            crate::widget::AppKind::Files
        }
        fn app_title(&self) -> &str {
            "Remote"
        }
        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Glyph("R")
        }
    }

    impl crate::widget::Filterable for Rows {
        fn set_filter(&mut self, query: &str) {
            query.clone_into(&mut self.filter);
        }
        fn filter_count(&self) -> Option<crate::widget::FilterCount> {
            let shown = self
                .rows
                .iter()
                .filter(|row| row.contains(self.filter.as_str()))
                .count();
            Some(crate::widget::FilterCount::new(shown, self.rows.len()))
        }
    }

    // What a scenario types into the bar's search narrows the app's list
    // and stands in the bar's field, as on the desktop.
    #[test]
    fn a_scenario_narrows_a_list_through_the_window_bars_search() {
        use crate::a11y::Action;
        let _hosted = HostedReset;
        let mut harness = FrameHarness::for_app(
            crate::State::new(),
            Rows {
                rows: vec!["build-host", "build-arm", "laptop"],
                filter: String::new(),
            },
            (800, 600),
            crate::theme::Theme::dark(),
        );
        harness.present_in_window_bar();
        harness
            .perform_semantic_named("window-search-open", None, &Action::Activate)
            .unwrap();
        harness
            .perform_semantic_named(
                "window-search-query",
                None,
                &Action::SetValue("build".into()),
            )
            .unwrap();
        assert_eq!(harness.window_search.query(), "build");
        assert_eq!(
            harness.window_search.count(),
            Some(crate::widget::FilterCount::new(2, 3))
        );
    }

    /// Hosting is per thread: a test leaves it as it found it.
    /// A text that shows its current match clear of what the window's
    /// search covers of it, as it was told.
    struct Covered {
        text: String,
        covers: Vec<Option<Rect>>,
        /// What covered it when each match was revealed.
        revealed_under: Vec<Option<Rect>>,
    }

    impl Widget for Covered {
        crate::provides!(dyn crate::widget::Findable);
        fn name(&self) -> &str {
            "Covered"
        }
        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }
        fn build_layout(
            &mut self,
            tree: &mut taffy::TaffyTree<()>,
            _: &crate::theme::Theme,
            nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            use crate::widget::TaffyTreeExt as _;
            tree.widget_leaf(taffy::Style::default(), nodes, self)
        }
    }

    impl crate::widget::AppWidget for Covered {
        fn app_kind(&self) -> crate::widget::AppKind {
            crate::widget::AppKind::Files
        }
        fn app_title(&self) -> &str {
            "Writer"
        }
        fn app_icon(&self) -> crate::widget::IconDescriptor {
            crate::widget::IconDescriptor::Glyph("W")
        }
    }

    impl crate::widget::Findable for Covered {
        fn find(
            &mut self,
            query: &str,
            _options: crate::widget::find_bar::FindOptions,
        ) -> Vec<crate::widget::find_bar::FindMatch> {
            if query.is_empty() {
                return Vec::new();
            }
            self.text
                .match_indices(query)
                .map(|(start, m)| crate::widget::find_bar::FindMatch::new(start, start + m.len()))
                .collect()
        }
        fn reveal(&mut self, _index: usize) {
            self.revealed_under
                .push(self.covers.last().copied().flatten());
        }
        fn set_find_cover(&mut self, cover: Option<Rect>) {
            self.covers.push(cover);
        }
        fn content_revision(&self) -> u64 {
            u64::try_from(self.text.len()).unwrap_or(u64::MAX)
        }
    }

    /// A document's slots in the bar: navigation, a location and actions,
    /// which leave a narrow window's search little room before the location.
    fn writer_slots() -> crate::window_toolbar::WindowToolbar {
        use crate::window_toolbar::{
            ToolbarAction, ToolbarLocation, ToolbarNavigation, WindowToolbar,
        };
        WindowToolbar {
            navigation: Some(ToolbarNavigation {
                back: true,
                forward: true,
            }),
            location: Some(ToolbarLocation {
                label: "Quarterly report".into(),
                value: "Quarterly report".into(),
                ..ToolbarLocation::default()
            }),
            actions: vec![
                ToolbarAction::new("bold", "bold", "Bold"),
                ToolbarAction::new("share", "upload", "Share"),
            ],
            ..WindowToolbar::default()
        }
    }

    fn covered_window(width: u32) -> FrameHarness {
        covered_window_with(width, Some(writer_slots()))
    }

    fn covered_window_with(
        width: u32,
        slots: Option<crate::window_toolbar::WindowToolbar>,
    ) -> FrameHarness {
        let mut harness = FrameHarness::for_app(
            crate::State::new(),
            Covered {
                text: "a report on the report".into(),
                covers: Vec::new(),
                revealed_under: Vec::new(),
            },
            (width, 800),
            crate::theme::Theme::dark(),
        );
        harness.present_in_window_bar();
        // The app declares its slots on the window its bar shows, never on the thread's.
        crate::window_toolbar::declare(slots);
        harness
    }

    fn query_rect(harness: &FrameHarness) -> Rect {
        harness
            .window_search
            .test_rects()
            .into_iter()
            .find(|(name, _)| name == "window-search-query")
            .expect("the query is shown")
            .1
    }

    // In a narrow window the search reaches back over the app's slots
    // rather than leave a query too narrow to type in; in a wide one it
    // stops at the location, so Back and Forward stay.
    #[test]
    fn the_query_keeps_its_minimum_width_in_a_narrow_window() {
        use crate::a11y::Action;
        let _hosted = HostedReset;
        let theme = crate::theme::Theme::dark();
        for width in [420_u32, 1280] {
            let mut harness = covered_window(width);
            harness
                .perform_semantic_named("window-search-open", None, &Action::Activate)
                .unwrap();
            harness
                .perform_semantic_named("window-search-options", None, &Action::Expand)
                .unwrap();
            let query = query_rect(&harness);
            assert!(
                query.width() >= WindowSearch::query_min_width(&theme),
                "{width}: the query is wide enough to type in: {query:?}"
            );
            let (row, reach) = harness
                .window_bar(f64::from(width))
                .toolbar_search_slot()
                .expect("the app's slots are in the bar");
            assert!(
                query.x0 >= row.x0,
                "{width}: the search stays on the app's slots: {query:?} {row:?}"
            );
            if width == 1280 {
                assert!(query.x0 >= reach, "a wide window keeps the location");
            }
        }
        // A window without slots: the search follows its title, and reaches
        // back over it rather than past the app's icon.
        let mut harness = covered_window_with(420, None);
        harness
            .perform_semantic_named("window-search-open", None, &Action::Activate)
            .unwrap();
        let query = query_rect(&harness);
        assert!(
            query.width() >= WindowSearch::query_min_width(&theme),
            "after the title, the query is wide enough to type in: {query:?}"
        );
        let start = harness.window_bar(420.0).search_cramped_start();
        assert!(
            query.x0 >= start,
            "the app's icon stays: {query:?} starts before {start}"
        );
        crate::window_toolbar::declare(None);
    }

    // The options open over the app: it is told what they cover, in its
    // own coordinates (under the bar), before its current match is shown
    // again; closed, they cover nothing.
    #[test]
    fn the_app_hears_what_the_open_options_cover_before_its_match_is_shown() {
        use crate::a11y::Action;
        let _hosted = HostedReset;
        let mut harness = covered_window(420);
        let mut perform =
            |target: &str, action: Action| harness.perform_semantic_named(target, None, &action);
        perform("window-search-open", Action::Activate).unwrap();
        perform("window-search-query", Action::SetValue("report".into())).unwrap();
        perform("window-search-options", Action::Expand).unwrap();
        let panel = harness
            .window_search
            .options_rect()
            .expect("the options are open");
        let under_bar = panel - vello::kurbo::Vec2::new(0.0, f64::from(bar_height_px()));
        let app = harness
            .widget_mut::<Covered>()
            .expect("the window holds the document");
        assert_eq!(app.covers.last().copied().flatten(), Some(under_bar));
        assert_eq!(
            app.revealed_under.last().copied().flatten(),
            Some(under_bar),
            "the current match is shown again, knowing what covers it"
        );
        harness
            .perform_semantic_named("window-search-options", None, &Action::Collapse)
            .unwrap();
        let app = harness
            .widget_mut::<Covered>()
            .expect("the window holds the document");
        assert_eq!(
            app.covers.last().copied(),
            Some(None),
            "closed, they cover nothing"
        );
        crate::window_toolbar::declare(None);
    }

    /// The colour of the frame's pixel at logical `(x, y)`.
    fn pixel(harness: &FrameHarness, x: u32, y: u32) -> [u8; 4] {
        let rgba = harness.render_rgba();
        let at = usize::try_from((y * harness.viewport.0 + x) * 4).unwrap_or(usize::MAX);
        rgba.get(at..at + 4)
            .and_then(|px| <[u8; 4]>::try_from(px).ok())
            .unwrap_or_default()
    }

    /// At compact width an app's palette stands under the window's bar,
    /// across the window less its insets, and the bar keeps its controls:
    /// they still take their presses. The window under the bar dims behind
    /// the palette while it is open and not after, and a press on that
    /// scrim closes it, as Escape does.
    #[test]
    fn a_compact_palette_keeps_the_window_bar_clear_over_a_scrim() {
        let _hosted = HostedReset;
        let theme = crate::theme::Theme::dark().with_reduce_motion(true);
        let inset = f64::from(theme.tokens.spacing.s2);
        let mut harness = FrameHarness::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(crate::widget::Column::new()),
            (420, 800),
            theme,
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture".into()),
        );
        harness.present_in_window_bar();
        crate::window_toolbar::declare(Some(crate::window_toolbar::WindowToolbar {
            actions: vec![crate::window_toolbar::ToolbarAction::new(
                "new",
                "plus",
                "New event",
            )],
            ..crate::window_toolbar::WindowToolbar::default()
        }));
        harness.relayout();
        let (w, h) = harness.viewport;
        // Sampled in the inset beside where the card stands: always scrim.
        let ground = pixel(&harness, 2, h - 40);

        harness.open_caller_search("").expect("the palette opens");
        harness.relayout();
        let palette = harness
            .caller_palette
            .palette
            .as_ref()
            .expect("the palette is open");
        let card = harness.window_rect(palette.rect());
        let bar = Rect::new(0.0, 0.0, f64::from(w), harness.frame_top());
        assert!(bar.height() > 0.0);
        assert!(
            card.y0 >= bar.y1,
            "the card {card:?} stands under the bar {bar:?}"
        );
        assert!(
            (card.width() - (f64::from(w) - 2.0 * inset)).abs() < 0.5,
            "the card takes the window's width less its insets: {card:?}"
        );
        let scrim = palette.scrim_rect().expect("a scrim behind the palette");
        assert!(
            (harness.window_rect(scrim).y0 - bar.y1).abs() < 1e-9,
            "the scrim dims the window under the bar, not the bar"
        );
        let dimmed = pixel(&harness, 2, h - 40);
        assert!(
            dimmed[..3].iter().map(|c| u32::from(*c)).sum::<u32>()
                < ground[..3].iter().map(|c| u32::from(*c)).sum::<u32>(),
            "the window behind the palette is dimmed: {ground:?} then {dimmed:?}"
        );

        let mut window_bar = harness.painted_window_bar(f64::from(w));
        let (_, _, _, actions) = window_bar.toolbar_rects();
        let at = actions
            .first()
            .expect("the action stands in the bar")
            .center();
        let row = window_bar.toolbar_row_mut();
        assert!(row.press(at) && row.release(at), "the bar takes its press");
        assert!(
            window_bar
                .take_toolbar_events()
                .iter()
                .any(|(_, event)| matches!(
                    event,
                    crate::window_toolbar::ToolbarEvent::Action { id } if id == "new"
                )),
            "the bar's control runs while the palette is open"
        );

        // In the inset beside the card: the scrim, wherever the card ends.
        // Delivered as the platform delivers it, through the host's input
        // policy, where the palette takes the window's pointer.
        let scale = harness.input_scale;
        harness.send_host_event(&crate::synthetic_input::cursor_moved(
            2.0 * scale,
            (f64::from(h) - 40.0) * scale,
        ));
        harness.send_host_event(&crate::synthetic_input::mouse_press());
        harness.send_host_event(&crate::synthetic_input::mouse_release());
        assert!(
            harness.caller_palette.palette.is_none(),
            "a press on the scrim closes the palette"
        );
        harness.relayout();
        assert_eq!(
            pixel(&harness, 2, h - 40),
            ground,
            "and the scrim is gone with it"
        );
        crate::window_toolbar::declare(None);
    }

    /// An app whose frame declares the window's bar, as an app's own does when it draws.
    struct DeclaresBar(crate::window_toolbar::WindowToolbar);

    impl Widget for DeclaresBar {
        fn name(&self) -> &str {
            "DeclaresBar"
        }

        fn update(&mut self, _: &crate::widget::AppModel, _: &crate::widget::ModelPath) -> bool {
            crate::window_toolbar::declare(Some(self.0.clone()));
            false
        }

        fn build_layout(
            &mut self,
            tree: &mut taffy::TaffyTree<()>,
            _: &crate::theme::Theme,
            nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            use crate::widget::TaffyTreeExt as _;
            tree.widget_leaf(taffy::Style::default(), nodes, self)
        }
    }

    /// Two windows a process lays out keep their bars apart: each app declares its slots on its
    /// own window, the frame of one reads nothing of the other's, and the thread keeps none of
    /// them once the windows are gone.
    #[test]
    fn windows_a_process_lays_out_keep_their_bars_apart() {
        use crate::window_toolbar::{ToolbarAction, WindowToolbar};
        let _hosted = HostedReset;
        let bar = |id: &str, name: &str| WindowToolbar {
            actions: vec![ToolbarAction::new(id, "plus", name)],
            ..WindowToolbar::default()
        };
        let mut notes = harness_of(Box::new(DeclaresBar(bar("new-note", "New note"))));
        let mut tasks = harness_of(Box::new(DeclaresBar(bar("new-task", "New task"))));
        notes.present_in_window_bar();
        tasks.present_in_window_bar();
        // Each window runs the frame that declares its bar, as a live window does.
        notes.relayout();
        tasks.relayout();
        let names = |harness: &mut FrameHarness| -> Vec<String> {
            harness
                .window_bar_layout(800.0)
                .controls
                .into_iter()
                .filter(|control| control.slot == "action")
                .map(|control| control.name)
                .collect()
        };
        assert_eq!(names(&mut notes), vec!["New note"], "notes' bar is its own");
        assert_eq!(names(&mut tasks), vec!["New task"], "tasks' bar is its own");
        assert_eq!(
            notes.window_toolbar().map(|bar| bar.actions.len()),
            Some(1),
            "notes' window shows the one action its app declares"
        );
        // A press the desktop hands one window reaches that window's app, not the other's.
        let press = crate::window_toolbar::ToolbarEvent::Action {
            id: "new-note".into(),
        };
        notes.press_window_slot(press.clone());
        let note = notes.window().expect("notes' window");
        assert_eq!(
            note.take_events(),
            vec![press],
            "the press reaches the app the window runs"
        );
        assert!(
            tasks
                .window()
                .expect("tasks' window")
                .take_events()
                .is_empty(),
            "the window standing beside it heard none of it"
        );
        tasks.relayout();
        assert_eq!(
            names(&mut tasks),
            vec!["New task"],
            "tasks kept its own bar"
        );
        drop(notes);
        drop(tasks);
        assert!(
            !crate::window_toolbar::is_hosted(),
            "no window stands as the thread's own once both are gone"
        );
    }

    pub(super) struct HostedReset;

    impl Drop for HostedReset {
        fn drop(&mut self) {
            crate::window_toolbar::set_hosted(false);
        }
    }
}
