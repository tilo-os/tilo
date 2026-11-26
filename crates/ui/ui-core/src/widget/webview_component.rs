//! [`WebView`] as a described component: a page shown in a view.
//!
//! The widget shows the live output of a page engine and forwards input to it; navigation
//! state belongs to the app's record, not this widget. The engine is the window's: the window
//! that draws the view holds one page engine per window ([`crate::webview::window_engine`]) and
//! hands it to every `ui.WebView` it builds, so the address a view names (`uri`) is the only
//! thing that crosses the wire and no live engine does. An app that draws its own page hands
//! its engine in, and reads that engine's events itself.
use tilo_ui_base::{component::Component, component::Described, component::Event};
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
    /// Which of the window's pages it shows: the one the person browsed, or the window's second
    /// one beside or over it (`panel`: an extension's side panel, a sign-in page). The window
    /// holds both engines and hands in the one this names; without it the view shows the page.
    pub surface: Option<String>,
    /// The page navigated (the person followed a link): carries the address.
    #[component(event(value = String))]
    pub navigate: Event,
    /// The page could not load: carries what the engine said.
    #[component(event(value = String))]
    pub fail: Event,
    /// The shell returned the picture to the page: the person closed the floated window, or the
    /// shell handed it back. The view stops floating when the app stops asking; this is how the
    /// app learns the person is done.
    #[component(event)]
    pub picture_in_picture_closed: Event,
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
            surface,
            navigate,
            fail,
            picture_in_picture_closed,
            engine,
        } = props;
        // The window's page reports its navigation to the app; an app's own engine is read by
        // the app, so this view takes nothing from it. The window's second page is read by the
        // window's session too, so this view shows it and reports nothing.
        let shown = crate::webview::window_surface();
        let window_page = engine.is_none()
            && shown == crate::webview::Surface::Page
            && crate::webview::window_engine().is_some();
        let engine = engine
            .or_else(crate::webview::window_engine)
            .unwrap_or_else(|| {
                let missing: SharedEngine = tilo_webview::chromium::UnavailableEngine::new(
                    tilo_ui_base::i18n::I18nManager::lookup_plain("webview-unavailable"),
                );
                missing
            });
        let mut view = Self::new(engine);
        view.set_surface(match surface.as_deref() {
            Some("panel") => crate::webview::Surface::Panel,
            _ => shown,
        });
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
        view.set_pip_closed(picture_in_picture_closed);
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
        self.set_pip_closed(props.picture_in_picture_closed);
        self.set_pip(props.pip);
        self.open_at(props.uri);
        true
    }
}
