//! A page's popup windows (a sign-in, a payment) in a panel beside it.
//!
//! A popup stays the page's: when the person has signed in it answers the
//! page that opened it (`window.opener`), which a tab of its own could not,
//! and usually closes itself. The panel names the popup's site, so a person
//! can tell the real sign-in page from a lookalike.
use ui::{
    LocalState,
    icons::Icon,
    tokens::TextRole,
    webview::{SharedEngine, WebViewEvent},
    widget::{Button, Column, Container, Row, Spacer, Text, Widget},
};

use crate::CATALOG;

/// The size of a popup that names none, in CSS pixels: about what sign-in
/// pages are laid out for.
const DEFAULT_SIZE: (f32, f32) = (500.0, 600.0);
/// The smallest a popup is shown, however small it asked to be.
const MIN_SIZE: (f32, f32) = (200.0, 150.0);

pub struct PagePopups {
    /// The page engine asked for its popups; another engine (a restarted
    /// one) is asked again, and the popups of the old one close.
    asked: Option<std::sync::Weak<dyn ui::webview::WebViewEngine>>,
    open: Option<Open>,
    close: LocalState<bool>,
}

impl Default for PagePopups {
    fn default() -> Self {
        Self {
            asked: None,
            open: None,
            close: LocalState::new(false),
        }
    }
}

struct Open {
    engine: SharedEngine,
    url: String,
    size: (f32, f32),
}

impl PagePopups {
    /// Take the popups `page` opened and follow the one shown. Returns
    /// whether the panel changed.
    pub fn sync(&mut self, page: &SharedEngine) -> bool {
        let mut changed = false;
        let asked = self
            .asked
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .is_some_and(|asked| std::sync::Arc::ptr_eq(&asked, page));
        if !asked {
            page.accept_popups();
            self.asked = Some(std::sync::Arc::downgrade(page));
            changed |= self.open.take().is_some();
        }
        if self.close.get() {
            self.close.set(false);
            changed |= self.open.take().is_some();
        }
        // The newest is shown; one it replaces closes.
        for popup in page.take_popups() {
            match page.adopt_popup(popup.id) {
                Ok(engine) => {
                    self.open = Some(Open {
                        engine,
                        url: popup.url,
                        size: size_of(popup.width, popup.height),
                    });
                    changed = true;
                }
                Err(error) => tracing::debug!(%error, "a popup closed before it was shown"),
            }
        }
        let Some(open) = &mut self.open else {
            return changed;
        };
        let mut closed = false;
        for event in open.engine.drain_events() {
            match event {
                // It closed itself: the sign-in is done.
                WebViewEvent::TabClosed => closed = true,
                WebViewEvent::UriChanged(url) if url != open.url => {
                    open.url = url;
                    changed = true;
                }
                _ => {}
            }
        }
        // What it opens in a new window opens beside the Browser.
        for (url, background) in open.engine.take_new_tab_requests() {
            if url.starts_with("https://") || url.starts_with("http://") {
                ui::shell_tabs::open_beside(Some(url), background);
            }
        }
        if closed {
            self.open = None;
            changed = true;
        }
        changed
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Close the popup shown, as its window's close button would.
    pub fn close(&mut self) {
        self.open = None;
    }

    /// The width the popup shown asks for, within `room`.
    pub fn width(&self, room: f32) -> Option<f32> {
        self.open.as_ref().map(|open| open.size.0.min(room))
    }

    /// The panel: the popup's site and a close button over its page, which
    /// is at most `max_height` tall with `row` taken by the header.
    pub fn panel(&self, width: f32, max_height: f32, row: f32) -> Option<Box<dyn Widget>> {
        let open = self.open.as_ref()?;
        let close = self.close.clone();
        let header = Row::new()
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(Box::new(
                Text::new(&site_of(&open.url)).with_role(TextRole::Body),
            ))
            .push(Box::new(Spacer::new()))
            .push(Box::new(
                Button::icon_only(
                    "browser.popup.close",
                    Icon::X,
                    &CATALOG.t("browser-popup-close"),
                )
                .on_effect(move || close.set(true)),
            ));
        let page = ui::widget::WebView::new(open.engine.clone()).with_test_id("browser.popup.page");
        let height = open.size.1.min((max_height - row).max(MIN_SIZE.1));
        Some(Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(header))
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_width(width)
                        .with_height(height)
                        .with_child(Box::new(page)),
                )),
        ))
    }
}

fn size_of(width: u32, height: u32) -> (f32, f32) {
    let side = |asked: u32, default: f32, min: f32| {
        if asked == 0 {
            default
        } else {
            f32::from(u16::try_from(asked).unwrap_or(u16::MAX)).max(min)
        }
    };
    (
        side(width, DEFAULT_SIZE.0, MIN_SIZE.0),
        side(height, DEFAULT_SIZE.1, MIN_SIZE.1),
    )
}

/// Where the popup is, as a person reads it: its host, or the address
/// itself when it has none.
fn site_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use ui::webview::test_engine::TestEngine;

    fn page() -> (Arc<TestEngine>, SharedEngine) {
        let engine = TestEngine::new();
        let shared: SharedEngine = engine.clone();
        (engine, shared)
    }

    #[test]
    fn a_sign_in_popup_shows_at_the_size_it_asked_for_with_its_site() {
        let (engine, shared) = page();
        let mut popups = PagePopups::default();
        assert!(!popups.sync(&shared));
        assert!(engine.accepts_popups(), "the page's popups stay its own");
        assert!(!popups.sync(&shared));
        engine.push_popup(1 << 30, "https://accounts.example.com/signin?x=1", 450, 700);
        assert!(popups.sync(&shared));
        assert!(popups.is_open());
        assert_eq!(popups.width(1000.0), Some(450.0));
        assert_eq!(popups.width(300.0), Some(300.0));
        let panel = popups.panel(450.0, 900.0, 40.0).unwrap();
        assert!(ui::widget::find_test_id(panel.as_ref(), "browser.popup.page").is_some());
        assert!(ui::widget::find_test_id(panel.as_ref(), "browser.popup.close").is_some());
        assert_eq!(
            site_of("https://accounts.example.com/signin?x=1"),
            "accounts.example.com"
        );
    }

    #[test]
    fn a_popup_that_closes_itself_closes_the_panel() {
        let (engine, shared) = page();
        let mut popups = PagePopups::default();
        popups.sync(&shared);
        engine.push_popup(1 << 30, "https://pay.example.com/", 0, 0);
        popups.sync(&shared);
        assert_eq!(popups.width(1000.0), Some(DEFAULT_SIZE.0));
        engine
            .adopted(1 << 30)
            .unwrap()
            .push_event(WebViewEvent::TabClosed);
        assert!(popups.sync(&shared));
        assert!(!popups.is_open());
    }

    #[test]
    fn a_newer_popup_replaces_the_one_shown_and_the_close_button_closes_it() {
        let (engine, shared) = page();
        let mut popups = PagePopups::default();
        popups.sync(&shared);
        engine.push_popup(1 << 30, "https://a.example/", 300, 300);
        engine.push_popup((1 << 30) + 1, "https://b.example/", 10, 10);
        popups.sync(&shared);
        assert_eq!(
            popups.width(1000.0),
            Some(MIN_SIZE.0),
            "the newest, no smaller than shown"
        );
        let first = engine.adopted(1 << 30).unwrap();
        // Held by the test engine's record and here: the panel let it go,
        // which closes it.
        assert_eq!(Arc::strong_count(&first), 2);
        popups.close.set(true);
        assert!(popups.sync(&shared));
        assert!(!popups.is_open());
    }

    #[test]
    fn a_new_page_engine_is_asked_again_and_the_old_ones_popup_closes() {
        let (engine, shared) = page();
        let mut popups = PagePopups::default();
        popups.sync(&shared);
        engine.push_popup(1 << 30, "https://a.example/", 300, 300);
        popups.sync(&shared);
        let (restarted, restarted_shared) = page();
        assert!(popups.sync(&restarted_shared));
        assert!(restarted.accepts_popups());
        assert!(!popups.is_open());
    }

    #[test]
    fn the_panel_follows_the_popup_to_another_site() {
        let (engine, shared) = page();
        let mut popups = PagePopups::default();
        popups.sync(&shared);
        engine.push_popup(1 << 30, "https://shop.example/", 400, 400);
        popups.sync(&shared);
        engine
            .adopted(1 << 30)
            .unwrap()
            .push_event(WebViewEvent::UriChanged(
                "https://bank.example/approve".into(),
            ));
        assert!(popups.sync(&shared));
        assert_eq!(site_of(&popups.open.as_ref().unwrap().url), "bank.example");
    }
}
