//! [`Banner`] as a described component.

use crate::component::{Component, Described, Event};
use crate::icons::Icon;
use crate::widget::{Button, Press};

use super::{Banner, BannerKind, BannerNotice};

crate::component::choice!(BannerKind, "BannerKind", {
    Neutral => "neutral",
    Info => "info",
    Success => "success",
    Warning => "warning",
    Error => "error",
});

/// A message that stays until it is resolved or dismissed: "You're offline", "Storage is almost
/// full". Shows an optional app name, a title, an optional message, up to two actions and a
/// dismiss button. Its kind sets the icon and colour.
#[derive(Component, Default)]
#[component(name = "Banner", role = "Group", label = "title")]
pub struct BannerProps {
    /// What happened, in a short sentence.
    #[component(required, positional)]
    pub title: String,
    /// What it means or what to do, under the title.
    pub message: Option<String>,
    /// The app's name, shown above the title.
    pub source: Option<String>,
    /// What kind of message it is: `neutral` unless said. A warning or an error is announced at
    /// once; the others when assistive technology is idle.
    pub kind: Option<BannerKind>,
    /// A glyph instead of the kind's own.
    pub icon: Option<Icon>,
    /// The label of its main action, at its trailing edge: "Retry".
    pub action: Option<String>,
    /// The label of a second action, before the main one: "Details".
    pub secondary_action: Option<String>,
    /// Its main action was pressed.
    #[component(event(press))]
    pub act: Event,
    /// Its second action was pressed.
    #[component(event(press))]
    pub secondary_act: Event,
    /// Its dismiss button was pressed: stop showing it. A banner with this has the button, and
    /// closes as it is pressed.
    #[component(event(press))]
    pub dismiss: Event,
}

/// The button of one of a banner's actions.
fn action(key: &str, label: String, event: Event) -> Button {
    let button = Button::new(key, &label);
    match event {
        Event::None => button.with_enabled(false),
        Event::Action(binding) => button.on_action(binding),
        Event::View(intent) => button.on_view(intent.sender()),
    }
}

impl BannerProps {
    fn notice(&self) -> BannerNotice {
        BannerNotice {
            kind: self.kind.unwrap_or(BannerKind::Neutral),
            title: self.title.clone(),
            message: self.message.clone(),
            icon: self.icon,
            source: self.source.clone(),
        }
    }
}

impl Described for Banner {
    type Props = BannerProps;

    fn from_props(key: &str, props: BannerProps) -> Self {
        let notice = props.notice();
        let BannerProps {
            action: main,
            secondary_action,
            act,
            secondary_act,
            dismiss,
            ..
        } = props;
        let mut banner = Self::from_notice(Some(notice)).with_id(key);
        if let Some(label) = main {
            banner = banner.with_action(action(&format!("{key}.action"), label, act));
        }
        if let Some(label) = secondary_action {
            banner = banner.with_secondary_action(action(
                &format!("{key}.secondary"),
                label,
                secondary_act,
            ));
        }
        let dismiss = Press::of(dismiss);
        if !dismiss.is_none() {
            banner = banner.with_dismiss_press(dismiss);
        }
        banner
    }

    /// What it says changes in place, without closing and opening again; a banner with action
    /// buttons is built again when they change.
    fn set_props(&mut self, props: BannerProps) -> bool {
        let actions = props.action.is_some()
            || props.secondary_action.is_some()
            || self.action.is_some()
            || self.secondary.is_some();
        if actions || props.dismiss.is_none() != self.dismiss.is_none() {
            return false;
        }
        self.set_notice(Some(props.notice()));
        true
    }
}
