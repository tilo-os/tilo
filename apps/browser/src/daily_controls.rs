//! Device-local find and zoom state, retained with each browser tab.
//!
//! While the desktop's window bar hosts the window its search drives find;
//! outside a Tilo session the window's own find bar does.
use ui::{
    LocalState,
    icons::Icon,
    tokens::TextRole,
    webview::{SharedEngine, WebViewCommand, WebViewEvent},
    widget::{Button, Container, Row, Text, TextInput, Widget},
};

pub(crate) const FIND_INPUT: &str = "browser.find.input";
/// Zoom steps, `1.2` apart each: five out, nine in.
const ZOOM_STEPS: std::ops::RangeInclusive<i32> = -5..=9;

fn zoom_min() -> f64 {
    f64::from(*ZOOM_STEPS.start())
}

fn zoom_max() -> f64 {
    f64::from(*ZOOM_STEPS.end())
}

fn zoom_levels() -> Vec<f32> {
    ZOOM_STEPS
        .map(|level| ui::num::f32_of_f64(1.2_f64.powf(f64::from(level))))
        .collect()
}
#[derive(Clone, Copy)]
pub(crate) enum Action {
    Open,
    Close,
    Next(bool),
    Zoom(i32),
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum BarSearch {
    #[default]
    Off,
    Finding,
    Reported,
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
    search: BarSearch,
    /// Moves when a page finished loading under the bar's search, so the bar
    /// finds again in it.
    revision: u64,
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
            search: BarSearch::Off,
            revision: 0,
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
                    (self.zoom + f64::from(step)).clamp(zoom_min(), zoom_max())
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
        if self.search != BarSearch::Off {
            // The page the matches were in is going: none until the next one
            // has loaded.
            self.count = 0;
            self.active = 0;
            self.search = BarSearch::Reported;
            self.id = self.id % (i32::MAX as u32) + 1;
        }
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
        let mut dirty = if let Some(action) = self.pending.get() {
            self.pending.set(None);
            self.act(action, engine);
            true
        } else {
            false
        };
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
            } if (self.open || self.search != BarSearch::Off) && *id == self.id => {
                if *count >= 0 {
                    self.count = *count;
                }
                if *active >= 0 {
                    self.active = *active;
                }
                if self.search != BarSearch::Off {
                    self.search = BarSearch::Reported;
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
    pub fn search(&mut self, query: &str, engine: &SharedEngine) {
        let query: String = query.chars().take(1024).collect();
        if query.is_empty() {
            self.stop_search(engine);
            return;
        }
        self.search = BarSearch::Finding;
        self.sent = query;
        self.id = self.id % (i32::MAX as u32) + 1;
        self.count = 0;
        self.active = 0;
        engine.send(WebViewCommand::Find {
            id: self.id,
            text: self.sent.clone(),
            forward: true,
            next: false,
        });
    }

    pub fn search_step(&mut self, forward: bool, engine: &SharedEngine) -> bool {
        if self.search == BarSearch::Off || self.sent.is_empty() {
            return false;
        }
        self.act(Action::Next(forward), engine);
        true
    }

    pub fn stop_search(&mut self, engine: &SharedEngine) {
        if self.search != BarSearch::Off {
            engine.send(WebViewCommand::StopFinding);
        }
        self.search = BarSearch::Off;
        self.sent.clear();
        self.count = 0;
        self.active = 0;
    }

    /// The engine's latest report for the bar's search, once: how many matches,
    /// and which one (0-based) it shows.
    pub fn take_search_results(&mut self) -> Option<(usize, Option<usize>)> {
        if self.search != BarSearch::Reported {
            return None;
        }
        self.search = BarSearch::Finding;
        let total = usize::try_from(self.count).unwrap_or(0);
        let current = usize::try_from(self.active)
            .ok()
            .and_then(|active| active.checked_sub(1));
        Some((total, current))
    }

    pub fn page_loaded(&mut self) {
        if self.search != BarSearch::Off {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn search_revision(&self) -> u64 {
        self.revision
    }

    /// The closure a menu row or button runs: it queues `action` for the next
    /// `sync`.
    pub fn command(&self, action: Action) -> impl FnMut() + 'static {
        let pending = self.pending.clone();
        // As tall as the address and find fields beside it, so the row keeps one
        // top and bottom edge.
        move || pending.set(Some(action))
    }
    pub fn zoom_factor(&self) -> f32 {
        ui::num::f32_of_f64(1.2_f64.powf(self.zoom))
    }
    pub fn zoom_by(&mut self, step: i32, engine: &SharedEngine) -> Result<(), String> {
        if !self.can_zoom(step) {
            return Err(format!("The page is at {}% already", self.zoom_percent()));
        }
        self.act(Action::Zoom(step), engine);
        Ok(())
    }
    pub fn zoom_actions(&self) -> Vec<ui::tilo_actions::ActionSpec> {
        use crate::effects::BrowserEffect as E;
        use ui::tilo_resource::Effect as _;
        [
            (E::ZoomIn, 1, "The page is at its largest"),
            (E::ZoomOut, -1, "The page is at its smallest"),
            (E::ResetZoom, 0, "The page is at its own size"),
        ]
        .into_iter()
        .map(|(zoom, step, limit)| {
            let zoom = zoom.bound();
            if self.can_zoom(step) {
                zoom
            } else {
                zoom.unavailable("zoom_limit", limit)
            }
        })
        .collect()
    }
    pub fn zoom_percent(&self) -> f64 {
        self.zoom_model().percent()
    }

    fn zoom_model(&self) -> ui::content_zoom::ZoomModel {
        let mut model = ui::content_zoom::ZoomModel::stepping(zoom_levels(), 1.0);
        model.adopt(self.zoom_factor());
        model
    }
    pub fn can_zoom(&self, step: i32) -> bool {
        if !(zoom_min()..=zoom_max()).contains(&self.zoom) {
            // A site's own remembered zoom can sit outside these steps: only
            // going back to the page's own size is meaningful.
            return step.signum() == 0;
        }
        self.zoom_model()
            .can_step(crate::daily_controls::zoom_step(step))
    }

    /// The zoom row's label: the page's size while it is not the default.
    pub fn zoom_label(&self) -> String {
        let catalog = &crate::resource::CATALOG;
        if self.zoom.abs() < f64::EPSILON {
            return catalog.t("browser-menu-actual-size");
        }
        catalog.t_args(
            "browser-menu-actual-size-zoomed",
            &[(
                "percent",
                ui::i18n::ArgValue::Number(self.zoom_model().percent()),
            )],
        )
    }
    pub fn bar(&self, theme: &ui::theme::Theme) -> Box<dyn Widget> {
        // Room for "12 of 345", so the count never wraps under the field.
        let counter_width = theme.tokens.spacing.s16 + theme.tokens.spacing.s4;
        // One line for the field, the count and the commands: a wrapping row
        // pushed the commands onto a second one.
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
            .with_id("browser.find")
            .with_padding(theme.tokens.spacing.s3, theme.tokens.spacing.s1)
            .push(Box::new(ui::window_toolbar::exempt!(Fallback, {
                // While the bar hosts the window, its search finds in the page.
                TextInput::from_state(&self.query)
                    .with_id(FIND_INPUT)
                    .with_kind(ui::widget::TextInputKind::Search)
                    .with_accessible_name(crate::resource::CATALOG.t("browser-menu-find"))
                    .with_placeholder(crate::resource::CATALOG.t("browser-menu-find"))
            })))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_width(counter_width)
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
        Box::new(row)
    }
}

/// The toolkit step a menu step stands for: `1` in, `-1` out, `0` back to the
/// page's own size.
pub(crate) fn zoom_step(step: i32) -> ui::content_zoom::ZoomStep {
    match step.signum() {
        1 => ui::content_zoom::ZoomStep::In,
        -1 => ui::content_zoom::ZoomStep::Out,
        _ => ui::content_zoom::ZoomStep::Reset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn find_and_zoom_state_is_per_tab_race_free_and_bounded() {
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
