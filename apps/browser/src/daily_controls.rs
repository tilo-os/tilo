//! Device-local find and zoom state, retained with each browser tab.
use ui::{
    LocalState,
    icons::Icon,
    tokens::TextRole,
    webview::{SharedEngine, WebViewCommand, WebViewEvent},
    widget::{Button, Container, Row, Text, TextInput, Widget},
};

pub(crate) const FIND_INPUT: &str = "browser.find.input";
#[derive(Clone, Copy)]
pub(crate) enum Action {
    Open,
    Close,
    Next(bool),
    Zoom(i32),
}
pub(crate) struct DailyControls {
    pub open: bool,
    pub focus_pending: Option<&'static str>,
    query: LocalState<String>,
    pending: LocalState<Option<Action>>,
    sent: String,
    id: u32,
    count: i32,
    active: i32,
    zoom: f64,
    buffered: bool,
}
impl Default for DailyControls {
    fn default() -> Self {
        Self {
            open: false,
            focus_pending: None,
            query: LocalState::new(String::new()),
            pending: LocalState::new(None),
            sent: String::new(),
            id: 0,
            count: 0,
            active: 0,
            zoom: 0.0,
            buffered: false,
        }
    }
}
impl DailyControls {
    pub fn act(&mut self, action: Action, engine: &SharedEngine) {
        match action {
            Action::Open => {
                if !self.open {
                    self.sent.clear();
                    self.count = 0;
                    self.active = 0;
                }
                self.buffered = false;
                self.open = true;
                self.focus_pending = Some(FIND_INPUT);
            }
            Action::Close => {
                self.open = false;
                self.focus_pending = Some("browser.surface");
                engine.send(WebViewCommand::StopFinding);
            }
            Action::Next(forward) => {
                if !self.sent.is_empty() {
                    engine.send(WebViewCommand::Find {
                        id: self.id,
                        text: self.sent.clone(),
                        forward,
                        next: true,
                    });
                }
            }
            Action::Zoom(step) => {
                self.zoom = if step == 0 {
                    0.0
                } else {
                    (self.zoom + f64::from(step)).clamp(-5.0, 9.0)
                };
                engine.send(WebViewCommand::SetZoomLevel(self.zoom));
            }
        }
    }
    /// Review fixture: the bar open on a query with matches found.
    pub(crate) fn stage_for_review(&mut self) {
        self.open = true;
        self.query.set("web browser".into());
        self.sent = "web browser".into();
        self.count = 12;
        self.active = 3;
    }
    pub fn buffer_text(&mut self, text: &str) {
        if !self.buffered {
            self.query.set(String::new());
            self.buffered = true;
        }
        self.query.update(|query| query.push_str(text));
    }
    pub fn take_buffered_cursor(&mut self) -> Option<usize> {
        std::mem::take(&mut self.buffered).then(|| self.query.get().len())
    }
    pub fn navigation_started(&mut self, engine: &SharedEngine) -> bool {
        if !self.open {
            return false;
        }
        self.open = false;
        self.focus_pending = None;
        self.count = 0;
        self.active = 0;
        self.id = self.id % (i32::MAX as u32) + 1;
        engine.send(WebViewCommand::StopFinding);
        true
    }
    pub fn sync(&mut self, engine: &SharedEngine) -> bool {
        let mut dirty = false;
        if let Some(action) = self.pending.get() {
            self.pending.set(None);
            self.act(action, engine);
            dirty = true;
        }
        let query: String = self.query.get().chars().take(1024).collect();
        if self.open && query != self.sent {
            self.sent = query;
            self.id = self.id % (i32::MAX as u32) + 1;
            self.count = 0;
            self.active = 0;
            if self.sent.is_empty() {
                engine.send(WebViewCommand::StopFinding);
            } else {
                engine.send(WebViewCommand::Find {
                    id: self.id,
                    text: self.sent.clone(),
                    forward: true,
                    next: false,
                });
            }
            dirty = true;
        }
        dirty
    }
    pub fn apply(&mut self, event: &WebViewEvent) -> bool {
        match event {
            WebViewEvent::FindResult {
                id, count, active, ..
            } if self.open && *id == self.id => {
                if *count >= 0 {
                    self.count = *count;
                }
                if *active >= 0 {
                    self.active = *active;
                }
                true
            }
            WebViewEvent::ZoomChanged(level) => {
                self.zoom = *level;
                true
            }
            _ => false,
        }
    }
    /// The closure a command's menu row or button runs: it queues `action`
    /// for the next `sync`, the same hand-off every control here uses.
    pub fn command(&self, action: Action) -> impl FnMut() + 'static {
        let pending = self.pending.clone();
        move || pending.set(Some(action))
    }
    /// The zoom row's label: the page's zoom level is always stated while it
    /// is not the default.
    pub fn zoom_label(&self) -> String {
        if self.zoom.abs() < f64::EPSILON {
            "Actual size".into()
        } else {
            format!("Actual size ({:.0}%)", 100.0 * 1.2_f64.powf(self.zoom))
        }
    }
    pub fn bar(&self, theme: &ui::theme::Theme) -> Box<dyn Widget> {
        // Room for "12 of 345", so the count never wraps under the field.
        let counter_width = theme.tokens.spacing.s16 + theme.tokens.spacing.s4;
        // `no_wrap` keeps the field, the match count and the commands on one
        // centre line: a wrapping row let the full-width field claim the
        // line and pushed the commands onto a second one.
        let counter = if self.sent.is_empty() {
            String::new()
        } else {
            format!("{} of {}", self.active.max(0), self.count.max(0))
        };
        let icon = |id: &str, icon: Icon, name: &str, action: Action| {
            Button::icon_only(id, icon, name).on_effect(self.command(action))
        };
        let row = Row::new()
            .no_wrap()
            .push(Box::new(
                TextInput::from_state(&self.query)
                    .with_id(FIND_INPUT)
                    .with_kind(ui::widget::TextInputKind::Search)
                    .with_accessible_name("Find in page")
                    .with_placeholder("Find in page"),
            ))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_min_width(counter_width)
                    .with_child(Box::new(
                        Text::new(&counter)
                            .with_role(TextRole::Caption)
                            .numeric()
                            .dimmed(),
                    )),
            ))
            .push(Box::new(icon(
                "browser.find.previous",
                Icon::ChevronUp,
                "Previous match",
                Action::Next(false),
            )))
            .push(Box::new(icon(
                "browser.find.next",
                Icon::ChevronDown,
                "Next match",
                Action::Next(true),
            )))
            .push(Box::new(icon(
                "browser.find.close",
                Icon::X,
                "Close find",
                Action::Close,
            )));
        Box::new(Container::new().with_child(Box::new(row)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Find queries, buffered text and zoom bounds per tab.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn find_and_zoom_state_is_per_tab_race_free_and_bounded() {
        // Phase `changing_query_ignores_stale_results_and_keeps_other_tab_untouched`.
        {
            let capture = ui::webview::test_engine::TestEngine::new();
            let engine: SharedEngine = capture.clone();
            let mut first = DailyControls::default();
            let second = DailyControls::default();
            first.act(Action::Open, &engine);
            first.query.set("needle".into());
            assert!(first.sync(&engine));
            assert!(capture.commands().contains(&WebViewCommand::Find {
                id: 1,
                text: "needle".into(),
                forward: true,
                next: false
            }));
            first.query.set("other".into());
            first.sync(&engine);
            assert!(!first.apply(&WebViewEvent::FindResult {
                id: 1,
                count: 9,
                active: 3,
                final_update: true
            }));
            assert!(first.apply(&WebViewEvent::FindResult {
                id: 2,
                count: 2,
                active: 1,
                final_update: true
            }));
            assert_eq!((first.count, first.active), (2, 1));
            assert!(!second.open);
            assert!(second.query.get().is_empty());
            first.act(Action::Close, &engine);
            assert!(capture.commands().contains(&WebViewCommand::StopFinding));
        }

        // Phase `buffered_find_text_and_navigation_reset_do_not_lose_or_reuse_matches`.
        {
            let capture = ui::webview::test_engine::TestEngine::new();
            let engine: SharedEngine = capture;
            let mut controls = DailyControls::default();
            controls.act(Action::Open, &engine);
            controls.buffer_text("N");
            controls.buffer_text("eedle");
            assert_eq!(controls.take_buffered_cursor(), Some(6));
            assert_eq!(controls.query.get(), "Needle");
            controls.sync(&engine);
            assert!(controls.navigation_started(&engine));
            assert!(!controls.open);
            assert!(!controls.apply(&WebViewEvent::FindResult {
                id: 1,
                count: 3,
                active: 1,
                final_update: true
            }));
            assert_eq!((controls.count, controls.active), (0, 0));
        }

        // Phase `zoom_is_bounded_and_reset_is_explicit_per_tab`.
        {
            let capture = ui::webview::test_engine::TestEngine::new();
            let engine: SharedEngine = capture.clone();
            let mut first = DailyControls::default();
            let second = DailyControls::default();
            for _ in 0..20 {
                first.act(Action::Zoom(1), &engine);
            }
            assert!((first.zoom - 9.0).abs() < f64::EPSILON);
            assert!(second.zoom.abs() < f64::EPSILON);
            first.act(Action::Zoom(0), &engine);
            assert_eq!(
                capture.commands().last(),
                Some(&WebViewCommand::SetZoomLevel(0.0))
            );
        }
    }
}
