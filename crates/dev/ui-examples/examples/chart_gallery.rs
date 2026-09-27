//! Live interactive chart catalogue.

use tilo_ui::theme::Theme;
use tilo_ui::{App, State};

use super::chart_gallery_scene;

pub fn run(_args: &[String]) {
    tilo_ui::i18n::I18nManager::init("en-US");
    App::new(State::new(), chart_gallery_scene::build(&Theme::dark())).run();
}
