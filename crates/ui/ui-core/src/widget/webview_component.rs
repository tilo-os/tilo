//! [`WebView`] as a described component: a page shown in a view.
//!
//! The widget shows the live output of a page engine and forwards input to it; navigation
//! state belongs to the app's record, not this widget. The engine is the window's: the window
//! that draws the view holds one page engine per window ([`crate::webview::window_engine`]) and
//! hands it to every `ui.WebView` it builds, so the address a view names (`uri`) is the only
//! thing that crosses the wire and no live engine does. An app that draws its own page hands
//! its engine in, and reads that engine's events itself.
use crate::component::{Component, Described, Event};
use crate::webview::SharedEngine;

use super::WebView;

/// A page, live: a browser's content, a help document, a web app embedded in a view. Its
/// address bar, navigation and permission prompts belong to its host: this is the page alone,
/// and the app reports what the page navigated to as its events.
#[derive(Component, Default)]
#[component(name = "WebView", role = "Document", label = "title")]
pub struct WebViewProps {
    /// What the page is, for assistive technology.
    pub title: Option<String>,
    /// The page's address, which it opens at first.
    #[component(data)]
    pub uri: Option<String>,
    /// Whether the page's video floats above the app's window as picture-in-picture: the host
    /// shows the picture where the page draws it, and the view puts it back in the page when the
    /// app stops asking.
    pub pip: bool,
    /// The page navigated (the person followed a link): carries the address.
    #[component(event(value = String))]
    pub navigate: Event,
    /// The page could not load: carries what the engine said.
    #[component(event(value = String))]
    pub fail: Event,
    /// The page's engine. A live engine is not a value: it never crosses from an app to the
    /// window, and the window that draws the view holds the one it hands in here.
    #[component(skip)]
    pub engine: Option<SharedEngine>,
}

impl Described for WebView {
    type Props = WebViewProps;

    fn from_props(_key: &str, props: WebViewProps) -> Self {
        let WebViewProps {
            title,
            uri,
            pip,
            navigate,
            fail,
            engine,
        } = props;
        // The window's page reports its navigation to the app; an app's own engine is read by
        // the app, so this view takes nothing from it.
        let window_page = engine.is_none() && crate::webview::window_engine().is_some();
        let engine = engine
            .or_else(crate::webview::window_engine)
            .unwrap_or_else(|| {
                let missing: SharedEngine = tilo_webview::chromium::UnavailableEngine::new(
                    crate::i18n::I18nManager::lookup_plain("webview-unavailable"),
                );
                missing
            });
        let mut view = Self::new(engine);
        if window_page {
            view = view
                .reporting(navigate, fail)
                .with_page(crate::webview::window_page());
        } else {
            view = view.keeping_events(navigate, fail);
        }
        if let Some(title) = title {
            view = view.with_accessible_name(title);
        }
        view = view.with_pip(pip);
        view.open_at(uri);
        view
    }

    /// The title a page is named by changes where the view stands: the page it shows is the
    /// engine's, and keeps showing. The events a host attached and the page it opens at are
    /// its own from one build to the next.
    fn set_props(&mut self, props: WebViewProps) -> bool {
        self.set_accessible_name(props.title.unwrap_or_default());
        self.update_events(props.navigate, props.fail);
        self.set_pip(props.pip);
        self.open_at(props.uri);
        true
    }
}
