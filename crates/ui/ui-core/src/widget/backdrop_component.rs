//! [`Backdrop`] as a described component.

use crate::component::{Component, Described};

use super::Backdrop;

/// The shell's wallpaper under everything a view draws: the picture the session handed the
/// host, covering the box, or the shell's gradient where it has no picture.
#[derive(Component, Default)]
#[component(name = "Backdrop", role = "Image", label = "label")]
pub struct BackdropProps {
    /// What it shows, for assistive technology; empty reads as nothing.
    #[component(data)]
    pub label: Option<String>,
}

impl Described for Backdrop {
    type Props = BackdropProps;

    fn from_props(_key: &str, _props: BackdropProps) -> Self {
        Self::new()
    }

    fn set_props(&mut self, _props: BackdropProps) -> bool {
        true
    }
}
