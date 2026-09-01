use ui::Theme;
use ui::widget::{Text, Widget};

/// The graph is validated before any widget reaches the app tree.
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    match crate::screens::router("main", &Theme::default()) {
        Ok(router) => Box::new(router),
        Err(error) => Box::new(Text::new(&format!(
            "Unable to load the generated UI: {error}"
        ))),
    }
}
