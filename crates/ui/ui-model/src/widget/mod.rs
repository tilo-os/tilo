mod tile_host;
pub use {
    tile_host::TileContent, tile_host::TileHost, tile_host::hold_windows,
    tile_host::release_windows, tile_host::resource_unavailable_title,
};
mod app_widget;
#[cfg(feature = "test-support")]
pub use app_widget::BoundAppWidget;
#[cfg(not(feature = "test-support"))]
pub use app_widget::BoundAppWidget;
/// A component's widget as a window's managed root holds it, for the tests above the toolkit.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn bound_app_widget(
    component: crate::application::AppComponent,
) -> Box<dyn tilo_ui_base::widget::Widget> {
    Box::new(app_widget::BoundAppWidget::new(component))
}
pub use {
    app_widget::AppCommand, app_widget::AppProcessIntent, app_widget::AppWidget,
    app_widget::DevRoot, app_widget::HostShellAllow, app_widget::IconDescriptor,
    app_widget::WindowPublication,
};
#[cfg(feature = "webview")]
pub mod webview;
#[cfg(feature = "webview")]
pub use {webview::WebView, webview::WebViewProps};
// History's views: the timeline of a resource's changes, read from its history.
pub mod activity_timeline;
mod activity_timeline_component;
pub mod history;
pub mod history_face;
pub mod history_group;
pub use {activity_timeline::ActivityTimeline, activity_timeline::RowParts};
pub use {
    activity_timeline_component::ActivityArtifact, activity_timeline_component::ActivityEntryProps,
    activity_timeline_component::ActivityTimelineProps,
    activity_timeline_component::DescribedActivityTimeline,
};
pub use {
    history::AuthorRelation, history::EntryKind, history::EntryTarget, history::HistoryAttribution,
    history::HistoryAuthor, history::HistoryClassification, history::HistoryEntry,
    history::HistoryFilter, history::RedactionDisplay, history::format_relative_time,
    history::hue_to_rgb,
};
pub use {history_face::EntryBody, history_face::EntryFace, history_face::HistoryEntryPresenter};
/// An app that sends an effect when pressed, for the tests above the model layer.
#[cfg(any(test, feature = "test-support"))]
pub mod effect_fixture;

#[cfg(test)]
mod model_view_tests;
