// The widgets' tests that drive the app runner: each was the `mod tests` beside the widget it
// exercises, and each builds its widget through the driver, which lives in a crate above the
// toolkit. One module per file it came from.

#[path = "widgets/floating_panel_resize.rs"]
mod floating_panel_resize;
#[path = "widgets/manual.rs"]
mod manual;
#[path = "widgets/media_card.rs"]
mod media_card;
#[path = "widgets/media_transport.rs"]
mod media_transport;
#[path = "widgets/refused_press.rs"]
mod refused_press;
#[path = "widgets/scroll_region_copy.rs"]
mod scroll_region_copy;
