//! Interactive component gallery — the reference showcase of the `ui`
//! toolkit. Opens a live window with a card per component family (buttons,
//! controls, text/selection, badges/chips/avatars, feedback/loading, metrics,
//! charts/gauges, navigation, banners, empty state, and an editable table).
//!
//! Run:  cargo run -p ui-examples --example ui-lab -- gallery
//! For the offscreen light+dark renders, see `gallery_shots`.

use tilo_ui::theme::Theme;
use tilo_ui::widget::{Scrollable, Widget};
use tilo_ui::{App, State};

use super::gallery_scene;

fn build_gallery(state: &mut State, theme: &Theme) -> Box<dyn Widget> {
    Box::new(Scrollable::new_vertical().with_child(gallery_scene::build_showcase(state, theme)))
}

pub fn run(_args: &[String]) {
    let wake_scope = tilo_ui::wake::WakeScope::new();
    let _wake_entry = wake_scope.enter();
    tilo_ui::i18n::I18nManager::init("en-US");

    let mut state = State::new();
    // The live window opens in dark; dimmed-label colours are baked from this
    // theme (the offscreen `gallery_shots` example rebuilds per theme for a
    // correct light render).
    let root_widget = build_gallery(&mut state, &Theme::dark());

    let app = App::from_scoped_parts(wake_scope, state, root_widget);
    app.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_gallery_has_a_scrollable_viewport() {
        let mut state = State::new();
        let root = build_gallery(&mut state, &Theme::dark());

        assert_eq!(root.name(), "Scrollable");
        assert_eq!(root.children().len(), 1);
    }
}
