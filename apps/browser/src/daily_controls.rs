//! Device-local find and zoom state, retained with each browser tab.
use ui::{
    LocalState,
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
    /// `text_scale` is the theme's; these are labelled controls, so their
    /// explicit width grows with the text it has to hold.
    fn button(&self, id: &str, label: &str, action: Action, text_scale: f32) -> Button {
        let pending = self.pending.clone();
        // As tall as the address field and the find field these sit beside,
        // so the toolbar row and the find bar each keep one top and one
        // bottom edge. The 32px face left a step down on either side of them.
        Button::new(id, label, None)
            .with_size(
                crate::browser_widget::labelled_width(72.0, text_scale),
                crate::browser_widget::toolbar_control_height(text_scale),
            )
            .on_effect(move || pending.set(Some(action)))
    }
    pub fn launcher(&self, text_scale: f32) -> Button {
        self.button("browser.find", "Find", Action::Open, text_scale)
            .tooltip("Find in page (⌘F / Ctrl+F)")
    }
    pub fn zoom_reset(&self, text_scale: f32) -> Button {
        self.button(
            "browser.zoom.reset",
            &format!("{:.0}%", 100.0 * 1.2_f64.powf(self.zoom)),
            Action::Zoom(0),
            text_scale,
        )
        .tooltip("Zoom: ⌘/Ctrl + Plus or Minus; reset: ⌘/Ctrl+0")
    }
    pub fn bar(&self, text_scale: f32) -> Box<dyn Widget> {
        // `no_wrap`, as the toolbar row above it: a wrapping row let the
        // full-width find field claim the line and pushed the match count,
        // Previous, Next and Done onto a second line beneath it, so the bar
        // read as a field with a stray button row rather than one control
        // cluster on one centre line.
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
                Text::new(&format!("{} / {}", self.active.max(0), self.count.max(0)))
                    .with_role(TextRole::Body),
            ))
            .push(Box::new(self.button(
                "browser.find.previous",
                "Previous",
                Action::Next(false),
                text_scale,
            )))
            .push(Box::new(self.button(
                "browser.find.next",
                "Next",
                Action::Next(true),
                text_scale,
            )))
            .push(Box::new(self.button(
                "browser.find.close",
                "Done",
                Action::Close,
                text_scale,
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
