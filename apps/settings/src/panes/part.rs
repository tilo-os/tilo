//! The panes' part of Settings: the areas, effect families, live areas and
//! settings rows they bring ([`crate::area::Part`]).

use crate::categories::Category;
use crate::effects::AreaVocabulary as _;

#[cfg(feature = "tilo-os")]
use crate::panes::{
    about, accounts, agent_models, audio, bluetooth, device_mesh, disk_unlock, display, drives,
    mouse_keyboard, network, notifications, permissions, personal_apps, power, printers, speakers,
    storage, time_date, updates, users,
};
use crate::panes::{
    accessibility, appearance, billing, devices_access, keyboard_shortcuts, region,
};

#[cfg(feature = "tilo-os")]
fn mesh_remote(made: &crate::area::Areas) -> ui::RemoteActions {
    made.find::<device_mesh::DeviceMeshArea>()
        .map(device_mesh::DeviceMeshArea::remote)
        .unwrap_or_default()
}

#[cfg(not(feature = "tilo-os"))]
fn mesh_remote(_made: &crate::area::Areas) -> ui::RemoteActions {
    ui::RemoteActions::current()
}

pub(crate) const AREAS: &[(u16, crate::area::MakeArea)] = &[
    (10, |context, _| {
        Box::new(accessibility::Software::for_device(context.devices))
    }),
    #[cfg(feature = "tilo-os")]
    (20, |context, _| {
        Box::new(notifications::NotificationsArea::new(context))
    }),
    #[cfg(feature = "tilo-os")]
    (30, |context, _| {
        Box::new(personal_apps::DefaultAppsArea::new(context))
    }),
    #[cfg(feature = "tilo-os")]
    (50, |context, _| {
        Box::new(mouse_keyboard::InputArea::new(context))
    }),
    #[cfg(feature = "tilo-os")]
    (60, |context, _| Box::new(power::PowerArea::new(context))),
    #[cfg(feature = "tilo-os")]
    (100, |context, _| {
        Box::new(permissions::PermissionsArea::new(context))
    }),
    #[cfg(feature = "tilo-os")]
    (120, |context, _| {
        Box::new(bluetooth::BluetoothArea::new(context))
    }),
    #[cfg(feature = "tilo-os")]
    (180, |context, _| Box::new(audio::AudioArea::new(context))),
    #[cfg(feature = "tilo-os")]
    (200, |context, _| {
        Box::new(display::DisplayArea::new(context))
    }),
    #[cfg(feature = "tilo-os")]
    (210, |context, _| {
        Box::new(network::NetworkArea::new(context))
    }),
    #[cfg(feature = "tilo-os")]
    (215, |context, _| {
        Box::new(network::mobile::MobileArea::new(context))
    }),
    #[cfg(feature = "tilo-os")]
    (40, |context, _| {
        Box::new(time_date::TimeDateArea::new(context))
    }),
    #[cfg(feature = "tilo-os")]
    (70, |context, _| {
        Box::new(updates::UpdatesArea::new(context))
    }),
    #[cfg(feature = "tilo-os")]
    (80, |context, _| {
        Box::new(disk_unlock::DiskUnlockArea::new(context))
    }),
    #[cfg(feature = "tilo-os")]
    (90, |context, _| Box::new(drives::DrivesArea::new(context))),
    #[cfg(feature = "tilo-os")]
    (110, |context, _| {
        Box::new(printers::PrintersArea::new(context))
    }),
    #[cfg(feature = "tilo-os")]
    (130, |context, _| {
        Box::new(speakers::SpeakersArea::new(context))
    }),
    #[cfg(feature = "tilo-os")]
    (140, |context, _| {
        Box::new(agent_models::AgentModelsArea::new(context))
    }),
    #[cfg(feature = "tilo-os")]
    (150, |context, _| {
        Box::new(device_mesh::DeviceMeshArea::new(context))
    }),
    #[cfg(feature = "tilo-os")]
    (160, |context, made| {
        Box::new(users::UsersArea::new(context, mesh_remote(made)))
    }),
    #[cfg(feature = "tilo-os")]
    (170, |context, made| {
        Box::new(storage::StorageArea::new(context, mesh_remote(made)))
    }),
    #[cfg(feature = "tilo-os")]
    (175, |context, _| Box::new(about::AboutArea::new(context))),
    #[cfg(feature = "tilo-os")]
    (190, |context, _| {
        Box::new(
            accounts::AccountsArea::new(context).with_account_root(context.account_root.clone()),
        )
    }),
    (220, |_, _| Box::new(region::language::LanguageArea::new())),
    (230, |context, made| {
        Box::new(billing::BillingArea::new(context, mesh_remote(made)))
    }),
    (240, |context, made| {
        Box::new(devices_access::DevicesAccessArea::new(
            context,
            mesh_remote(made),
        ))
    }),
];

pub(crate) const FAMILIES: &[(u16, &crate::effects::EffectFamily)] = &[
    (20, appearance::AppearanceEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (40, personal_apps::DefaultAppsEffect::FAMILY),
    (60, keyboard_shortcuts::ShortcutEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (70, mouse_keyboard::InputEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (80, power::PowerEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (140, permissions::PermissionsEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (170, bluetooth::BluetoothEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (220, audio::AudioEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (240, display::DisplayEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (250, network::NetworkEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (252, network::vpn::VpnEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (255, network::mobile::MobileEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (50, time_date::TimeDateEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (90, updates::UpdatesEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (100, disk_unlock::DiskUnlockEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (110, drives::DrivesEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (120, storage::StorageEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (130, about::AboutEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (150, printers::PrinterEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (160, users::UsersEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (180, speakers::SpeakerEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (190, speakers::SpeakerFlow::FAMILY),
    #[cfg(feature = "tilo-os")]
    (200, agent_models::AgentModelsEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (210, device_mesh::DeviceMeshEffect::FAMILY),
    (260, region::language::LanguageEffect::FAMILY),
    #[cfg(feature = "tilo-os")]
    (230, accounts::AccountsEffect::FAMILY),
    (270, billing::BillingEffect::FAMILY),
    (280, devices_access::DevicesAccessEffect::FAMILY),
];

/// One [`crate::effects::LiveArea`]: the area `$module::$area` behind the
/// pane `$pane`, its rows profiled as `$name`. `always` for an area whose
/// rows show only what its pane already read; `read` with the effects its
/// model answers for one whose read blocks on the device and so runs on the
/// background executor ([`crate::effects::BackgroundRead`]).
macro_rules! live {
    (@ $order:literal, $name:literal, $pane:ident, $module:ident :: $area:ident,
        $is_open:expr, $background:expr) => {
        (
            $order,
            crate::effects::LiveArea {
                name: $name,
                pane: Category::$pane,
                is_open: $is_open,
                rows: |app| {
                    $module::area(app)
                        .map($module::$area::rows)
                        .unwrap_or_default()
                },
                background: $background,
            },
        )
    };
    (@@ $order:literal, $name:literal, $pane:ident, $module:ident :: $area:ident, $answers:expr) => {
        live!(@ $order, $name, $pane, $module::$area,
            |app| $module::area(app).is_some_and($module::$area::is_open),
            Some(crate::effects::BackgroundRead {
                start: |app| $module::area(app).is_some_and($module::$area::start_read),
                finished: |app| $module::area(app).is_none_or(|area| area.read.is_finished()),
                wait: |app| $module::area(app).and_then(|area| area.read.wait()),
                answers: $answers,
            }))
    };
    ($order:literal, $name:literal, $pane:ident, always $module:ident :: $area:ident) => {
        live!(@ $order, $name, $pane, $module::$area, |_| true, None)
    };
    ($order:literal, $name:literal, $pane:ident, $module:ident :: $area:ident) => {
        live!(@ $order, $name, $pane, $module::$area,
            |app| $module::area(app).is_some_and($module::$area::is_open), None)
    };
    ($order:literal, $name:literal, $pane:ident, $module:ident :: $area:ident, read Vec::new) => {
        live!(@@ $order, $name, $pane, $module::$area, Vec::new)
    };
    ($order:literal, $name:literal, $pane:ident, $module:ident :: $area:ident,
        read $effects:ident :: $effect:ident) => {
        live!(@@ $order, $name, $pane, $module::$area,
            <$effects::$effect as ui::tilo_resource::Effect>::declarations)
    };
}

pub(crate) const LIVE_AREAS: &[(u16, crate::effects::LiveArea)] = &[
    live!(
        250,
        "settings.rows.devices_access",
        DevicesAccess,
        devices_access::DevicesAccessArea
    ),
    #[cfg(feature = "tilo-os")]
    live!(10, "settings.rows.notifications", Notifications, notifications::NotificationsArea, read Vec::new),
    #[cfg(feature = "tilo-os")]
    live!(20, "settings.rows.default_apps", DefaultApps, personal_apps::DefaultAppsArea, read personal_apps::DefaultAppsEffect),
    #[cfg(feature = "tilo-os")]
    live!(50, "settings.rows.input", Keyboard, always mouse_keyboard::InputArea),
    #[cfg(feature = "tilo-os")]
    live!(90, "settings.rows.bluetooth", Bluetooth, bluetooth::BluetoothArea, read bluetooth::BluetoothEffect),
    #[cfg(feature = "tilo-os")]
    live!(140, "settings.rows.audio", Sound, audio::AudioArea, read audio::AudioEffect),
    #[cfg(feature = "tilo-os")]
    live!(
        180,
        "settings.rows.displays",
        Displays,
        display::DisplayArea
    ),
    #[cfg(feature = "tilo-os")]
    live!(200, "settings.rows.network", Network, network::NetworkArea),
    #[cfg(feature = "tilo-os")]
    live!(
        70,
        "settings.rows.printers",
        Printers,
        printers::PrintersArea
    ),
    #[cfg(feature = "tilo-os")]
    live!(80, "settings.rows.users", Users, users::UsersArea, read users::UsersEffect),
    #[cfg(feature = "tilo-os")]
    live!(
        110,
        "settings.rows.speakers",
        Speakers,
        speakers::SpeakersArea
    ),
    #[cfg(feature = "tilo-os")]
    live!(120, "settings.rows.agent_models", AgentModels, agent_models::AgentModelsArea, read agent_models::AgentModelsEffect),
    #[cfg(feature = "tilo-os")]
    live!(130, "settings.rows.device_mesh", DeviceMesh, device_mesh::DeviceMeshArea, read device_mesh::DeviceMeshEffect),
    #[cfg(feature = "tilo-os")]
    live!(150, "settings.rows.accounts", ExternalAccounts, accounts::AccountsArea, read accounts::AccountsEffect),
    // Disk unlock's rows show only what its pane has already read.
    #[cfg(feature = "tilo-os")]
    live!(220, "settings.rows.disk_unlock", Permissions, always disk_unlock::DiskUnlockArea),
    // Remembered drives' rows show only what the Storage pane has read.
    #[cfg(feature = "tilo-os")]
    live!(240, "settings.rows.drives", Storage, always drives::DrivesArea),
];

const SETTINGS: &[crate::setting::SettingEntry] = &[
    crate::setting::setting_row!("appearance.hide_top_bar", crate::prefs::DesktopControlsDevice => appearance::hide_top_bar),
    crate::setting::setting_row!("appearance.text_size", crate::prefs::AppearancePrefs => accessibility::text_size),
    crate::setting::setting_row!("appearance.high_contrast", crate::prefs::AppearancePrefs => accessibility::high_contrast),
    crate::setting::setting_row!("appearance.reduce_motion", crate::prefs::AppearancePrefs => accessibility::reduce_motion),
    crate::setting::setting_row!("a11y.on_screen_keyboard", crate::accessibility_settings::AccessibilitySettings => accessibility::on_screen_keyboard),
    crate::setting::setting_row!("a11y.large_cursor", crate::accessibility_settings::AccessibilitySettings => accessibility::large_cursor),
    crate::setting::setting_row!("a11y.cursor_size", crate::accessibility_settings::AccessibilitySettings => accessibility::cursor_size),
    crate::setting::setting_row!("a11y.cursor_theme", crate::accessibility_settings::AccessibilitySettings => accessibility::cursor_theme),
    crate::setting::setting_row!("a11y.magnifier", crate::accessibility_settings::AccessibilitySettings => accessibility::magnifier_zoom),
    crate::setting::setting_row!("a11y.colour_filter", crate::accessibility_settings::AccessibilitySettings => accessibility::colour_filter),
    crate::setting::setting_row!("a11y.sticky_keys", crate::accessibility_settings::AccessibilitySettings => accessibility::sticky_keys),
    crate::setting::setting_row!("a11y.slow_keys", crate::accessibility_settings::AccessibilitySettings => accessibility::slow_keys),
    crate::setting::setting_row!("a11y.bounce_keys", crate::accessibility_settings::AccessibilitySettings => accessibility::bounce_keys),
    crate::setting::setting_row!("a11y.mouse_keys", crate::accessibility_settings::AccessibilitySettings => accessibility::mouse_keys),
    crate::setting::setting_row!("a11y.visual_alerts", crate::accessibility_settings::AccessibilitySettings => accessibility::visual_alerts),
    crate::setting::setting_row!("a11y.switch_control", crate::accessibility_settings::AccessibilitySettings => accessibility::switch_control),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("notifications.dnd", crate::prefs::NotificationPrefs => notifications::dnd),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("notifications.banners", crate::prefs::NotificationPrefs => notifications::banners),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("notifications.previews", crate::prefs::NotificationPrefs => notifications::previews),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("notifications.lock_screen", crate::prefs::NotificationPrefs => notifications::lock_screen),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("notifications.sounds", crate::prefs::NotificationPrefs => notifications::play_sounds),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("notifications.badges", crate::prefs::NotificationPrefs => notifications::badges),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("notifications.quiet_hours", crate::prefs::NotificationPrefs => notifications::quiet_hours),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("spelling.check", ui::text_assistance::TextAssistancePrefs => mouse_keyboard::check_spelling),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("spelling.autocorrect", ui::text_assistance::TextAssistancePrefs => mouse_keyboard::auto_correct),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("power.auto_suspend", crate::prefs::PowerPrefs => power::auto_suspend),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("power.lock_on_sleep", crate::prefs::PowerPrefs => power::lock_on_sleep),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("display.night_light", crate::prefs::NightLightPrefs => display::night_light),
    crate::setting::setting_row!("shortcuts.vim", keyboard_shortcuts::DesktopControls => keyboard_shortcuts::vim_mode),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_view_switch!("network.wifi" => network::values::wifi, network::values::wifi_switch),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_view_switch!("network.wired" => network::values::wired, network::values::wired_switch),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_view_switch!("network.airplane" => network::values::airplane, network::values::airplane_switch),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_view_switch!("network.hotspot" => network::values::hotspot, network::values::hotspot_switch),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_view_switch!("network.mobile" => network::mobile::mobile_data, network::mobile::mobile_data_switch),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_view_switch!("bluetooth.enabled" => bluetooth::enabled, bluetooth::enabled_switch),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("display.arrangement" => display::values::arrangement),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("display.brightness" => display::values::brightness),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_bound_switch!("display.auto_brightness" => display::values::auto_brightness, crate::prefs::DisplayPrefs => display::automatic_brightness),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("display.night_light_warmth" => known display::values::night_light_warmth),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("display.scale" => display::values::scale),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("display.resolution" => display::values::resolution),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("display.refresh_rate" => display::values::refresh_rate),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("display.rotation" => display::values::rotation),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("display.hdr" => display::values::hdr),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("display.vrr" => display::values::vrr),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("display.mirroring" => display::values::mirroring),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("display.colour_profile" => display::values::colour_profile),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("sound.volume" => audio::values::volume),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("sound.output_device" => audio::values::output_device),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("sound.input_device" => audio::values::input_device),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("sound.input_level" => audio::values::input_level),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_view_switch!("sound.mute" => audio::values::mute, audio::mute),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("sound.balance" => known audio::values::balance),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_bound_switch!("sound.mono" => audio::values::mono, crate::prefs::AudioPrefs => audio::mono),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_bound_switch!("sound.echo" => audio::values::echo, crate::prefs::AudioPrefs => audio::echo_cancellation),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_bound_switch!("sound.system_sounds" => audio::values::system_sounds, crate::prefs::AudioPrefs => audio::system_sounds),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("keyboard.layout" => mouse_keyboard::values::layout),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("keyboard.key_repeat" => known mouse_keyboard::values::key_repeat),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("keyboard.compose" => known mouse_keyboard::values::compose),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("keyboard.caps_lock" => known mouse_keyboard::values::caps_lock),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("pointing.touchpad", crate::prefs::InputDevicePrefs => mouse_keyboard::touchpad),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("pointing.touchpad_speed" => known mouse_keyboard::values::touchpad_speed),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("pointing.tap_to_click", crate::prefs::InputPrefs => mouse_keyboard::tap_to_click),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("pointing.natural_scrolling" => known mouse_keyboard::values::natural_scrolling),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("pointing.disable_while_typing", crate::prefs::InputPrefs => mouse_keyboard::disable_while_typing),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("pointing.secondary_click" => mouse_keyboard::values::secondary_click),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("pointing.scroll_method" => mouse_keyboard::values::scroll_method),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("pointing.gestures", crate::prefs::InputPrefs => mouse_keyboard::gestures),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("pointing.disable_with_mouse", crate::prefs::InputDevicePrefs => mouse_keyboard::disable_with_mouse),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("pointing.mouse_speed" => known mouse_keyboard::values::mouse_speed),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("pointing.acceleration" => known mouse_keyboard::values::acceleration),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("pointing.left_handed", crate::prefs::InputPrefs => mouse_keyboard::left_handed),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("pointing.scroll_speed" => known mouse_keyboard::values::scroll_speed),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_row!("pointing.middle_click", crate::prefs::InputPrefs => mouse_keyboard::middle_click),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("spelling.language" => known mouse_keyboard::values::spelling_language),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("power.profile" => power::values::profile),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("power.charge_limit" => power::values::charge_limit),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("power.suspend_after" => known power::values::suspend_after),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("power.blank" => known power::values::blank),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("power.lock_after" => known power::values::lock_after),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("power.lid" => known power::values::lid),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("power.power_key" => known power::values::power_key),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("power.critical" => known power::values::critical),
    crate::setting::setting_value!("appearance.style" => known appearance::values::style),
    crate::setting::setting_value!("appearance.schedule" => known appearance::values::schedule),
    crate::setting::setting_value!("appearance.wallpaper" => appearance::values::wallpaper),
    crate::setting::setting_value!("appearance.ui_scale" => known appearance::values::ui_scale),
    crate::setting::setting_bound_switch!("a11y.screen_reader" => accessibility::values::screen_reader, crate::accessibility_settings::AccessibilitySettings => accessibility::values::screen_reader_switch),
    crate::setting::setting_value!("a11y.dictation_language" => known accessibility::values::dictation_language),
    crate::setting::setting_row!("region.currency", tilo_locale::Locale => region::currency),
    crate::setting::setting_row!("region.date", tilo_locale::Locale => region::date_format),
    crate::setting::setting_row!("region.time", tilo_locale::Locale => region::time_format),
    crate::setting::setting_row!("region.measurement", tilo_locale::Locale => region::measurement),
    crate::setting::setting_row!("region.numbers", tilo_locale::Locale => region::number_style),
    crate::setting::setting_row!("region.first_day", tilo_locale::Locale => region::first_day),
    crate::setting::setting_value!("region.language" => known region::language::value),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_view_switch!("time.automatic" => time_date::values::automatic, time_date::values::automatic_switch),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("time.zone" => time_date::values::zone),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("storage.cache" => storage::values::cache),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_value!("storage.clean" => storage::values::clean),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_view_switch!("updates.automatic" => updates::values::automatic, updates::values::automatic_switch),
    #[cfg(feature = "tilo-os")]
    crate::setting::setting_view_switch!("updates.metered" => updates::values::metered, updates::values::metered_switch),
];

pub const PART: crate::area::Part = crate::area::Part {
    areas: AREAS,
    families: FAMILIES,
    live_areas: LIVE_AREAS,
    settings: SETTINGS,
};
