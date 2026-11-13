//! The settings sections as the areas and the window act on them: each
//! section's published actions, held from the view the window is attached
//! through, and the dispatcher that runs them.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::rc::Rc;

use ui::framework::{Actions, Dispatcher, ViewCx};
use ui::storage::SettingsSchema;

use crate::resource::Settings;

pub trait Section:
    SettingsSchema
    + ui::tilo_resource::Resource<State = Self>
    + ui::tilo_resource::Fields
    + Clone
    + PartialEq
    + Send
    + Sync
{
}

impl<T> Section for T where
    T: SettingsSchema
        + ui::tilo_resource::Resource<State = T>
        + ui::tilo_resource::Fields
        + Clone
        + PartialEq
        + Send
        + Sync
{
}

#[derive(Clone, Default)]
pub struct Sections {
    attached: Option<Attached>,
}

#[derive(Clone)]
struct Attached {
    actions: Actions<Settings>,
    dispatcher: Dispatcher<Settings>,
    held: Rc<HashMap<TypeId, Box<dyn Any>>>,
}

impl Sections {
    #[must_use]
    pub fn of(cx: &ViewCx<Settings>) -> Self {
        Self {
            attached: Some(Attached {
                actions: cx.actions(),
                dispatcher: cx.dispatcher(),
                held: Rc::new(held_sections(cx)),
            }),
        }
    }

    #[must_use]
    pub fn actions(&self) -> Option<&Actions<Settings>> {
        Some(&self.attached.as_ref()?.actions)
    }

    #[must_use]
    pub fn dispatcher(&self) -> Option<&Dispatcher<Settings>> {
        Some(&self.attached.as_ref()?.dispatcher)
    }

    fn section<T: Section>(&self) -> Option<&Actions<T>> {
        self.attached
            .as_ref()?
            .held
            .get(&TypeId::of::<T>())?
            .downcast_ref::<Actions<T>>()
    }

    /// `decl`, a bound field edit or verb of section `T`, as an operation a
    /// live row offers (one app's notification rule on that app's row).
    /// `None` outside a view, or where the section is read only here.
    #[must_use]
    pub fn operation<T: Section>(
        &self,
        decl: &ui::tilo_resource::ActionDecl<T>,
    ) -> Option<ui::tilo_actions::ActionSpec> {
        self.section::<T>()?.operation(decl)
    }

    pub fn invoke<T: Section>(
        &self,
        invoke: impl Into<ui::tilo_resource::Invoke<T>>,
    ) -> Result<(), String> {
        let (Some(dispatcher), Some(actions)) = (self.dispatcher(), self.section::<T>()) else {
            return Err(format!("{} is not kept here", T::LABEL));
        };
        dispatcher.invoke_on(actions, invoke.into());
        Ok(())
    }
}

fn hold<T: Section>(cx: &ViewCx<Settings>, held: &mut HashMap<TypeId, Box<dyn Any>>) {
    let section = cx.settings::<T>();
    if let Some(actions) = cx.record_actions(&section) {
        held.insert(TypeId::of::<T>(), Box::new(actions));
    }
}

fn held_sections(cx: &ViewCx<Settings>) -> HashMap<TypeId, Box<dyn Any>> {
    let mut held = HashMap::new();
    hold::<crate::prefs::AppearancePrefs>(cx, &mut held);
    hold::<crate::prefs::AppearanceDevicePrefs>(cx, &mut held);
    hold::<crate::accessibility_settings::AccessibilitySettings>(cx, &mut held);
    hold::<crate::accessibility_settings::AccessibilityDevicePrefs>(cx, &mut held);
    hold::<crate::prefs::InputPrefs>(cx, &mut held);
    hold::<crate::prefs::InputDevicePrefs>(cx, &mut held);
    hold::<ui::text_assistance::TextAssistancePrefs>(cx, &mut held);
    hold::<crate::prefs::PowerPrefs>(cx, &mut held);
    hold::<crate::prefs::DeskWidgetsPrefs>(cx, &mut held);
    hold::<crate::prefs::UpdatePrefs>(cx, &mut held);
    hold::<crate::prefs::NotificationPrefs>(cx, &mut held);
    hold::<crate::prefs::AudioPrefs>(cx, &mut held);
    hold::<crate::prefs::AudioDevicePrefs>(cx, &mut held);
    hold::<crate::prefs::DisplayPrefs>(cx, &mut held);
    hold::<crate::prefs::NightLightPrefs>(cx, &mut held);
    hold::<tilo_locale::Locale>(cx, &mut held);
    hold::<crate::continuity::ContinuityPrivacy>(cx, &mut held);
    hold::<crate::shortcut_settings::ShortcutPrefs>(cx, &mut held);
    hold::<crate::prefs::DesktopControls>(cx, &mut held);
    hold::<crate::prefs::DesktopControlsDevice>(cx, &mut held);
    hold::<crate::prefs::DefaultApps>(cx, &mut held);
    hold::<crate::prefs::ClockPrefs>(cx, &mut held);
    hold::<crate::prefs::SharedDisplayPrefs>(cx, &mut held);
    hold::<crate::prefs::AutomaticDisplayPrefs>(cx, &mut held);
    hold::<crate::prefs::AgentProviders>(cx, &mut held);
    hold::<crate::prefs::HostedAgentProviders>(cx, &mut held);
    hold::<crate::prefs::OskSourceSettings>(cx, &mut held);
    hold::<crate::prefs::DeviceVpnPrefs>(cx, &mut held);
    hold::<crate::prefs::NetworkUsage>(cx, &mut held);
    hold::<crate::prefs::AgentDoorDevice>(cx, &mut held);
    held
}
