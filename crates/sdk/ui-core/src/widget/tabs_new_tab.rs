//! The window bar's own door to another application: the "+" every tabbed app wears, what it is
//! called and how it paints.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::NEW_TAB_TARGET;
use crate::icons;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

/// The catalogue key naming the strip's new-tab door: a "+" needs no words in the row, so the name
/// is what assistive technology reads and the tip of its glyph, in the desktop's language.
const NEW_TAB_KEY: &str = "window-toolbar-new-tab";

pub(crate) fn new_tab_name() -> String {
    crate::i18n::I18nManager::lookup_plain(NEW_TAB_KEY)
}

/// The window's own door to another application, as a bar reads it at `rect`: the "+" every
/// tabbed app wears, in no words a bar would hold room for, named as [`new_tab_name`] says.
pub(crate) fn new_tab_control(rect: Rect) -> crate::window_toolbar::BarControl {
    crate::window_toolbar::BarControl::new("new-tab", NEW_TAB_TARGET, rect)
        .named(new_tab_name())
        .icon(Some(icons::Icon::Plus.name().to_owned()))
        .furniture()
}

/// Paint the new-tab door at `rect`: the shared small button, bare at rest so it never competes
/// with a tab, a lift of the ground under the pointer.
pub(crate) fn paint_new_tab(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    hovered: bool,
) {
    let name = new_tab_name();
    let face = crate::widget::ButtonFace::icon_only(rect, icons::Icon::Plus, &name)
        .control_size(crate::widget::ControlSize::Small)
        .hovered(hovered);
    crate::widget::Button::paint(scene, transform, tr, theme, &face);
}
