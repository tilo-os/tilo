//! Find in the page through the window bar's search: the browser is the
//! [`Findable`] the bar drives while it shows a page. The page's engine
//! finds, steps and reports how many matches there are and which one it
//! shows ([`Findable::take_engine_results`]); the matches are the engine's
//! to paint. On Settings, a page that failed to open and the engine's
//! setup, the bar has no page to find in.
//!
//! On the Library the bar's search narrows the section shown, as its own
//! filter field does outside a Tilo session ([`Filterable`]); closing the
//! search shows the whole section again.

use ui::widget::capability::{Request, RequestMut};
use ui::widget::{Filterable, FindMatch, FindOptions, Findable};

use super::BrowserWidget;

impl BrowserWidget {
    /// Whether the window bar's search finds in the page: one is shown,
    /// not the Library, Settings, a page that failed to open or the
    /// engine's setup.
    fn offers_page_find(&self) -> bool {
        !self.show_library
            && !self.show_settings
            && self.load_error.is_none()
            && self.engine_problem.is_none()
    }

    /// What the browser offers its host, as [`ui::widget::Widget::provide`].
    pub(super) fn provide_capabilities<'a>(&'a self, request: &mut Request<'a, '_>) {
        if request.wants::<dyn ui::widget::Continuable>() {
            request.provide::<dyn ui::widget::Continuable>(self);
        } else if request.wants::<dyn ui::widget::FloatingSurfaceProvider>() {
            request.provide::<dyn ui::widget::FloatingSurfaceProvider>(self);
        } else if request.wants::<dyn ui::widget::controls::ActionControls>() {
            request.provide::<dyn ui::widget::controls::ActionControls>(self);
        } else if request.wants::<dyn ui::content_zoom::ContentZoom>() {
            request.provide::<dyn ui::content_zoom::ContentZoom>(self);
        } else if self.offers_page_find() && request.wants::<dyn Findable>() {
            request.provide::<dyn Findable>(self);
        } else if self.show_library && request.wants::<dyn Filterable>() {
            request.provide::<dyn Filterable>(self);
        }
    }

    /// [`Self::provide_capabilities`], mutably.
    pub(super) fn provide_capabilities_mut<'a>(&'a mut self, request: &mut RequestMut<'a, '_>) {
        if request.wants::<dyn ui::widget::Continuable>() {
            request.provide::<dyn ui::widget::Continuable>(self);
        } else if request.wants::<dyn ui::widget::FloatingSurfaceProvider>() {
            request.provide::<dyn ui::widget::FloatingSurfaceProvider>(self);
        } else if request.wants::<dyn ui::widget::controls::ActionControls>() {
            request.provide::<dyn ui::widget::controls::ActionControls>(self);
        } else if request.wants::<dyn ui::content_zoom::ContentZoom>() {
            request.provide::<dyn ui::content_zoom::ContentZoom>(self);
        } else if self.offers_page_find() && request.wants::<dyn Findable>() {
            request.provide::<dyn Findable>(self);
        } else if self.show_library && request.wants::<dyn Filterable>() {
            request.provide::<dyn Filterable>(self);
        }
    }
}

impl Findable for BrowserWidget {
    fn find(&mut self, query: &str, _options: FindOptions) -> Vec<FindMatch> {
        // The engine answers later, through `take_engine_results`.
        self.daily.search(query, &self.engine);
        Vec::new()
    }

    fn reveal(&mut self, _index: usize) {
        // The engine shows the match it finds or steps to.
    }

    fn clear_find(&mut self) {
        self.daily.stop_search(&self.engine);
    }

    fn step(&mut self, forward: bool) -> bool {
        self.daily.search_step(forward, &self.engine)
    }

    fn content_revision(&self) -> u64 {
        self.daily.search_revision()
    }

    fn take_engine_results(&mut self) -> Option<(usize, Option<usize>)> {
        self.daily.take_search_results()
    }
}

#[cfg(test)]
mod tests {
    use ui::webview::{WebViewCommand, WebViewEvent};
    use ui::widget::{InPlace, SearchDriver};

    use super::BrowserWidget;

    /// The window bar's search finds in the page through its engine: the
    /// engine's report is the bar's count, stepping asks the engine for the
    /// next match, a finished load finds again, and closing stops finding.
    #[test]
    fn the_window_bars_search_finds_in_the_page_through_its_engine() {
        let engine = ui::webview::test_engine::TestEngine::new();
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        assert_eq!(InPlace::of(&mut widget), InPlace::Document);
        let options = ui::widget::FindOptions::default();
        let mut driver = SearchDriver::default();
        driver.open(InPlace::Document);
        driver.drive(&mut widget, "news", options, None, false);
        let id = engine
            .commands()
            .iter()
            .rev()
            .find_map(|command| match command {
                WebViewCommand::Find {
                    id,
                    text,
                    next: false,
                    ..
                } if text == "news" => Some(*id),
                _ => None,
            })
            .expect("the engine is asked to find");
        assert!(!driver.abilities().replace, "a page is not edited");

        widget.daily.apply(&WebViewEvent::FindResult {
            id,
            count: 3,
            active: 1,
            final_update: true,
        });
        driver.drive(&mut widget, "news", options, None, false);
        assert_eq!(driver.position(), Some((1, 3)), "the engine's count");

        driver.drive(&mut widget, "news", options, Some(true), false);
        assert!(
            engine.commands().contains(&WebViewCommand::Find {
                id,
                text: "news".into(),
                forward: true,
                next: true,
            }),
            "stepping asks the engine for the next match"
        );

        let before = engine.commands().len();
        widget.daily.page_loaded();
        driver.drive(&mut widget, "news", options, None, false);
        assert!(
            engine.commands()[before..]
                .iter()
                .any(|command| matches!(command, WebViewCommand::Find { next: false, .. })),
            "a page that finished loading is found in again"
        );

        driver.drive(&mut widget, "news", options, None, true);
        assert_eq!(
            engine.commands().last(),
            Some(&WebViewCommand::StopFinding),
            "closing the search stops finding"
        );

        widget.show_library = true;
        assert_eq!(
            InPlace::of(&mut widget),
            InPlace::List,
            "on the Library the bar's search narrows the section, finding in no page"
        );

        widget.show_library = false;
        widget.engine_problem = Some(super::super::EngineProblem::NotInstalled);
        assert_eq!(
            InPlace::of(&mut widget),
            InPlace::Nothing,
            "the engine's setup has no page to find in"
        );
    }
}
