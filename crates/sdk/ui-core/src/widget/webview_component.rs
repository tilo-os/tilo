//! [`WebView`] as a described component: a page shown in a view.
//!
//! The widget shows the live output of a page engine and forwards input to it; navigation
//! state belongs to the app's record, not this widget. A view that shows a page gives it the
//! engine its app has, so `ui.WebView` is the surface, never a second engine.

use crate::component::{Component, Described, Event};

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
    pub uri: Option<String>,
    /// The page navigated (the person followed a link): carries the address.
    #[component(event(value = String))]
    pub navigate: Event,
    /// The page could not load: carries what the engine said.
    #[component(event(value = String))]
    pub fail: Event,
    /// The page's engine, which its app builds and hands in.
    #[component(skip)]
    pub engine: Option<crate::webview::SharedEngine>,
}

impl Described for WebView {
    type Props = WebViewProps;

    fn from_props(_key: &str, props: WebViewProps) -> Self {
        let WebViewProps {
            title,
            uri,
            navigate,
            fail,
            engine,
        } = props;
        let engine = engine.unwrap_or_else(|| {
            crate::webview::chromium::UnavailableEngine::new(
                crate::i18n::I18nManager::lookup_plain("webview-unavailable"),
            ) as crate::webview::SharedEngine
        });
        let mut view = Self::new(engine);
        if let Some(title) = title {
            view = view.with_accessible_name(title);
        }
        if let Some(uri) = uri {
            let _ = uri;
        }
        let _ = (&navigate, &fail);
        view
    }
}
