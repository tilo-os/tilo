//! [`Backdrop`] as a described component.

use tilo_ui_base::{component::Component, component::Described};

use super::{Backdrop, Ground};

tilo_ui_base::choice!(crate::widget::backdrop::Ground, "BackdropGround", {
    Wallpaper => "wallpaper",
    Trusted => "trusted",
    Scrim => "scrim",
});

/// What stands under everything a view draws: the shell's wallpaper, the ground of a surface the
/// system itself draws before anything else (setup), or a scrim that dims what it covers.
#[derive(Component, Default)]
#[component(name = "Backdrop", role = "Image", label = "label")]
pub struct BackdropProps {
    /// What it shows, for assistive technology; empty reads as nothing.
    #[component(data)]
    pub label: Option<String>,
    /// Which ground it is: the wallpaper unless said.
    pub ground: Option<Ground>,
}

impl Described for Backdrop {
    type Props = BackdropProps;

    fn from_props(_key: &str, props: BackdropProps) -> Self {
        Self::new().with_ground(props.ground.unwrap_or_default())
    }

    fn set_props(&mut self, props: BackdropProps) -> bool {
        self.ground = props.ground.unwrap_or_default();
        true
    }
}
