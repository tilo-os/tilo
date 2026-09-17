//! Live interactive chart catalogue.

use ui::theme::Theme;
use ui::{App, State};

use super::chart_gallery_scene;

pub fn run(_args: &[String]) {
    ui::i18n::I18nManager::init("en-US");
    App::new(State::new(), chart_gallery_scene::build(&Theme::dark())).run();
}
