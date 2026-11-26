// The widgets' tests that drive the app runner: each was the `mod tests` beside the widget it
// exercises, and each builds its widget through the driver, which lives in a crate above the
// toolkit. The tests that only paint, measure or lay a widget out stay beside it. One module per
// file it came from; what they read of a widget's own state is behind tilo-ui's test-support
// build (`tilo_ui::widget::internals`).

#[path = "widgets/floating_panel_resize.rs"]
mod floating_panel_resize;
#[path = "widgets/manual.rs"]
mod manual;
#[path = "widgets/map_answer_tests.rs"]
mod map_answer_tests;
#[path = "widgets/map_canvas_tests.rs"]
mod map_canvas_tests;
#[path = "widgets/map_pointing_tests.rs"]
mod map_pointing_tests;
#[path = "widgets/media_card.rs"]
mod media_card;
#[path = "widgets/media_transport.rs"]
mod media_transport;
#[path = "widgets/modal_dialog_tests.rs"]
mod modal_dialog_tests;
#[path = "widgets/page_sidebar_component_tests.rs"]
mod page_sidebar_component_tests;
#[path = "widgets/page_slide_tests.rs"]
mod page_slide_tests;
#[path = "widgets/property_list_tests.rs"]
mod property_list_tests;
#[path = "widgets/refused_press.rs"]
mod refused_press;
#[path = "widgets/screenshot_markup_a11y_tests.rs"]
mod screenshot_markup_a11y_tests;
#[path = "widgets/scroll_region_copy.rs"]
mod scroll_region_copy;
#[path = "widgets/scrollable_gutter_tests.rs"]
mod scrollable_gutter_tests;
#[path = "widgets/scrollable_tests.rs"]
mod scrollable_tests;
#[path = "widgets/section_tests.rs"]
mod section_tests;
#[path = "widgets/shortcut_recorder_tests.rs"]
mod shortcut_recorder_tests;
#[path = "widgets/slider_tests.rs"]
mod slider_tests;
#[path = "widgets/stage_tests.rs"]
mod stage_tests;
#[path = "widgets/stat_tile_tests.rs"]
mod stat_tile_tests;
#[path = "widgets/text_input_touch_tests.rs"]
mod text_input_touch_tests;
#[path = "widgets/timeline_tests.rs"]
mod timeline_tests;
#[path = "widgets/toggle_group_tests.rs"]
mod toggle_group_tests;
#[path = "widgets/toggle_tests.rs"]
mod toggle_tests;
#[path = "widgets/virtual_list_open_tests.rs"]
mod virtual_list_open_tests;
#[path = "widgets/virtual_list_tests.rs"]
mod virtual_list_tests;
#[path = "widgets/virtual_list_tree_tests.rs"]
mod virtual_list_tree_tests;
