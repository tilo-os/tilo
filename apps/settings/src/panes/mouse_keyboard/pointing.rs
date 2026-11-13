//! The touchpad's and mouse's choices that are about motion, drawn as pictures:
//! where a secondary click is pressed, how fingers scroll, and which way
//! content follows them.

use provider_settings_ui::bound::SettingRows as _;
use settings_store::hardware::{TouchpadClickMethod, TouchpadScrollMethod};
use ui::framework::ViewCx;
use ui::tilo_resource;
use ui::widget::{Picture, TouchpadGesture};

use super::{CLICK_METHOD_CHOICES, InputPrefs, SCROLL_METHOD_CHOICES};
use crate::i18n::t;
use provider_settings_ui::widgets::Item;

fn choice_label<T: PartialEq>(choices: &[(&str, &str, T)], value: &T) -> String {
    choices
        .iter()
        .find(|(_, _, choice)| choice == value)
        .map(|(_, key, _)| t(key))
        .unwrap_or_default()
}

pub(super) fn click_method(cx: &ViewCx<InputPrefs>) -> Item {
    cx.setting(t("input-click-method"))
        .described(t("input-click-method-caption"))
        .id("input.touchpad_click_method")
        .pictures(
            InputPrefs::paths().touchpad_click_method,
            CLICK_METHOD_CHOICES.iter().map(|(_, _, method)| *method),
            |method: &TouchpadClickMethod| choice_label(&CLICK_METHOD_CHOICES, method),
            |method: &TouchpadClickMethod| {
                Picture::Touchpad(match method {
                    TouchpadClickMethod::Areas => TouchpadGesture::CornerPress,
                    TouchpadClickMethod::Clickfinger => TouchpadGesture::TwoFingerPress,
                    TouchpadClickMethod::Default => TouchpadGesture::Press,
                })
            },
        )
}

pub(super) fn scroll_method(cx: &ViewCx<InputPrefs>) -> Item {
    cx.setting(t("input-scroll-method"))
        .id("input.touchpad_scroll_method")
        .pictures(
            InputPrefs::paths().touchpad_scroll_method,
            SCROLL_METHOD_CHOICES.iter().map(|(_, _, method)| *method),
            |method: &TouchpadScrollMethod| choice_label(&SCROLL_METHOD_CHOICES, method),
            |method: &TouchpadScrollMethod| {
                Picture::Touchpad(match method {
                    TouchpadScrollMethod::Edge => TouchpadGesture::EdgeScroll,
                    TouchpadScrollMethod::TwoFinger => TouchpadGesture::TwoFingerScroll,
                })
            },
        )
}

pub(super) fn scroll_direction(
    cx: &ViewCx<InputPrefs>,
    wheel: bool,
    description: &str,
    field: tilo_resource::Path<InputPrefs, bool>,
    id: &str,
) -> Item {
    cx.setting(t("input-scroll-direction"))
        .described(t(description))
        .id(id)
        .pictures(
            field,
            [true, false],
            |natural: &bool| {
                t(if *natural {
                    "input-scroll-natural"
                } else {
                    "input-scroll-traditional"
                })
            },
            move |natural: &bool| Picture::ScrollDirection {
                natural: *natural,
                wheel,
            },
        )
}
