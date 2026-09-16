//! Device-local find and zoom state, retained with each browser tab.
use ui::{
    LocalState,
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
    pub fn apply(&mut self, event: WebViewEvent) -> bool {
        match event {
            WebViewEvent::FindResult {
                id, count, active, ..
            } if self.open && id == self.id => {
                if count >= 0 {
                    self.count = count;
                }
                if active >= 0 {
                    self.active = active;
                }
                true
            }
            WebViewEvent::ZoomChanged(level) => {
                self.zoom = level;
                true
            }
            _ => false,
        }
    }
    fn button(&self, id: &str, label: &str, action: Action) -> Button {
        let pending = self.pending.clone();
        Button::new(id, label, None)
            .with_size(72.0, 32.0)
            .on_effect(move || pending.set(Some(action)))
    }
    pub fn launcher(&self) -> Button {
        self.button("browser.find", "Find", Action::Open)
            .tooltip("Find in page (⌘F / Ctrl+F)")
    }
    pub fn zoom_reset(&self) -> Button {
        self.button(
            "browser.zoom.reset",
            &format!("{:.0}%", 100.0 * 1.2_f64.powf(self.zoom)),
            Action::Zoom(0),
        )
        .tooltip("Zoom: ⌘/Ctrl + Plus or Minus; reset: ⌘/Ctrl+0")
    }
    pub fn bar(&self) -> Box<dyn Widget> {
        let row = Row::new()
            .push(Box::new(
                TextInput::from_state(&self.query)
                    .with_id(FIND_INPUT)
                    .with_kind(ui::widget::TextInputKind::Search)
                    .with_accessible_name("Find in page")
                    .with_placeholder("Find in page"),
            ))
            .push(Box::new(
                Text::new(&format!("{} / {}", self.active.max(0), self.count.max(0)))
                    .with_size(13.0),
            ))
            .push(Box::new(self.button(
                "browser.find.previous",
                "Previous",
                Action::Next(false),
            )))
            .push(Box::new(self.button(
                "browser.find.next",
                "Next",
                Action::Next(true),
            )))
            .push(Box::new(self.button(
                "browser.find.close",
                "Done",
                Action::Close,
            )));
        Box::new(Container::new().with_child(Box::new(row)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changing_query_ignores_stale_results_and_keeps_other_tab_untouched() {
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
        assert!(!first.apply(WebViewEvent::FindResult {
            id: 1,
            count: 9,
            active: 3,
            final_update: true
        }));
        assert!(first.apply(WebViewEvent::FindResult {
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
    #[test]
    fn zoom_is_bounded_and_reset_is_explicit_per_tab() {
        let capture = ui::webview::test_engine::TestEngine::new();
        let engine: SharedEngine = capture.clone();
        let mut first = DailyControls::default();
        let second = DailyControls::default();
        for _ in 0..20 {
            first.act(Action::Zoom(1), &engine);
        }
        assert_eq!(first.zoom, 9.0);
        assert_eq!(second.zoom, 0.0);
        first.act(Action::Zoom(0), &engine);
        assert_eq!(
            capture.commands().last(),
            Some(&WebViewCommand::SetZoomLevel(0.0))
        );
    }
}
