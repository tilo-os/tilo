//! The panes' part of Settings: the areas, effect families, live
//! areas and settings rows they bring ([`crate::area::Part`]).

use crate::categories::Category;
use crate::effects::AreaVocabulary as _;

use crate::panes::{
    about, accessibility, accounts, agent_models, appearance, audio, bluetooth, device_mesh,
    disk_unlock, display, drives, keyboard_shortcuts, mouse_keyboard, network, notifications,
    permissions, personal_apps, power, printers, region, speakers, storage, time_date, updates,
    users,
};

/// The host client Device mesh carries pairing requests on, which the areas
/// that act through the host share.
fn mesh_remote(made: &crate::area::Areas) -> ui::RemoteActions {
    made.find::<device_mesh::DeviceMeshArea>()
        .map(device_mesh::DeviceMeshArea::remote)
        .unwrap_or_default()
}

/// Every area the window holds, in the order they are made: an area made
/// later may use one made before (Users and Storage act through Device
/// mesh's host client).
pub(crate) const AREAS: &[(u16, crate::area::MakeArea)] = &[
    (10, |context, _| {
        Box::new(accessibility::Software::for_device(context.devices))
    }),
    (20, |context, _| {
        Box::new(notifications::NotificationsArea::new(context))
    }),
    (30, |context, _| {
        Box::new(personal_apps::DefaultAppsArea::new(context))
    }),
    (50, |context, _| {
        Box::new(mouse_keyboard::InputArea::new(context))
    }),
    (60, |context, _| Box::new(power::PowerArea::new(context))),
    (100, |context, _| {
        Box::new(permissions::PermissionsArea::new(context))
    }),
    (120, |context, _| {
        Box::new(bluetooth::BluetoothArea::new(context))
    }),
    (180, |context, _| Box::new(audio::AudioArea::new(context))),
    (200, |context, _| {
        Box::new(display::DisplayArea::new(context))
    }),
    (210, |context, _| {
        Box::new(network::NetworkArea::new(context))
    }),
    (215, |context, _| {
        Box::new(network::mobile::MobileArea::new(context))
    }),
    (40, |context, _| {
        Box::new(time_date::TimeDateArea::new(context))
    }),
    (70, |context, _| {
        Box::new(updates::UpdatesArea::new(context))
    }),
    (80, |context, _| {
        Box::new(disk_unlock::DiskUnlockArea::new(context))
    }),
    (90, |context, _| Box::new(drives::DrivesArea::new(context))),
    (110, |context, _| {
        Box::new(printers::PrintersArea::new(context))
    }),
    (130, |context, _| {
        Box::new(speakers::SpeakersArea::new(context))
    }),
    (140, |context, _| {
        Box::new(agent_models::AgentModelsArea::new(context))
    }),
    (150, |context, _| {
        Box::new(device_mesh::DeviceMeshArea::new(context))
    }),
    (160, |context, made| {
        Box::new(users::UsersArea::new(context, mesh_remote(made)))
    }),
    (170, |context, made| {
        Box::new(storage::StorageArea::new(context, mesh_remote(made)))
    }),
    (175, |context, made| {
        Box::new(about::AboutArea::new(context, mesh_remote(made)))
    }),
    (190, |context, _| {
        Box::new(
            accounts::AccountsArea::new(context).with_account_root(context.account_root.clone()),
        )
    }),
    (220, |_, _| Box::new(region::language::LanguageArea::new())),
];

/// Every area's vocabulary, in the order the union declares them.
pub(crate) const FAMILIES: &[(u16, &crate::effects::EffectFamily)] = &[
    (20, appearance::AppearanceEffect::FAMILY),
    (40, personal_apps::DefaultAppsEffect::FAMILY),
    (60, keyboard_shortcuts::ShortcutEffect::FAMILY),
    (70, mouse_keyboard::InputEffect::FAMILY),
    (80, power::PowerEffect::FAMILY),
    (140, permissions::PermissionsEffect::FAMILY),
    (170, bluetooth::BluetoothEffect::FAMILY),
    (220, audio::AudioEffect::FAMILY),
    (240, display::DisplayEffect::FAMILY),
    (250, network::NetworkEffect::FAMILY),
    (252, network::vpn::VpnEffect::FAMILY),
    (255, network::mobile::MobileEffect::FAMILY),
    (50, time_date::TimeDateEffect::FAMILY),
    (90, updates::UpdatesEffect::FAMILY),
    (100, disk_unlock::DiskUnlockEffect::FAMILY),
    (110, drives::DrivesEffect::FAMILY),
    (120, storage::StorageEffect::FAMILY),
    (130, about::AboutEffect::FAMILY),
    (150, printers::PrinterEffect::FAMILY),
    (160, users::UsersEffect::FAMILY),
    (180, speakers::SpeakerEffect::FAMILY),
    (190, speakers::SpeakerFlow::FAMILY),
    (200, agent_models::AgentModelsEffect::FAMILY),
    (210, device_mesh::DeviceMeshEffect::FAMILY),
    (260, region::language::LanguageEffect::FAMILY),
    (230, accounts::AccountsEffect::FAMILY),
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
                finished: |app| $module::area(app).is_none_or($module::$area::read_finished),
                wait: |app| $module::area(app).and_then($module::$area::wait_read),
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

/// Every area with live rows, in the order their rows are published.
pub(crate) const LIVE_AREAS: &[(u16, crate::effects::LiveArea)] = &[
    live!(10, "settings.rows.notifications", Notifications, notifications::NotificationsArea, read Vec::new),
    live!(20, "settings.rows.default_apps", DefaultApps, personal_apps::DefaultAppsArea, read personal_apps::DefaultAppsEffect),
    // Input reads a small snapshot file the compositor keeps, on a timer of
    // its own; it has nothing slow to put off.
    live!(50, "settings.rows.input", Keyboard, always mouse_keyboard::InputArea),
    live!(90, "settings.rows.bluetooth", Bluetooth, bluetooth::BluetoothArea, read bluetooth::BluetoothEffect),
    live!(140, "settings.rows.audio", Sound, audio::AudioArea, read audio::AudioEffect),
    // Displays probe brightness on threads of their own; the rest is a few
    // small files.
    live!(
        180,
        "settings.rows.displays",
        Displays,
        display::DisplayArea
    ),
    // Networks are read on a thread of their own.
    live!(200, "settings.rows.network", Network, network::NetworkArea),
    // Printers read on a thread of their own.
    live!(
        70,
        "settings.rows.printers",
        Printers,
        printers::PrintersArea
    ),
    live!(80, "settings.rows.users", Users, users::UsersArea, read users::UsersEffect),
    // Speakers ask the broker on a thread of their own.
    live!(
        110,
        "settings.rows.speakers",
        Speakers,
        speakers::SpeakersArea
    ),
    live!(120, "settings.rows.agent_models", AgentModels, agent_models::AgentModelsArea, read agent_models::AgentModelsEffect),
    live!(130, "settings.rows.device_mesh", DeviceMesh, device_mesh::DeviceMeshArea, read device_mesh::DeviceMeshEffect),
    live!(150, "settings.rows.accounts", ExternalAccounts, accounts::AccountsArea, read accounts::AccountsEffect),
    // Disk unlock's rows show only what its pane has already read.
    live!(220, "settings.rows.disk_unlock", Permissions, always disk_unlock::DiskUnlockArea),
    // Remembered drives' rows show only what the Storage pane has read.
    live!(240, "settings.rows.drives", Storage, always drives::DrivesArea),
];

/// The settings these panes show, as Settings' search shows them: a row
/// built by the function its pane builds it with, or what it is set to.
const SETTINGS: &[crate::setting::SettingEntry] = &[
    crate::setting::setting_row!("appearance.hide_top_bar", crate::prefs::DesktopControlsDevice => appearance::hide_top_bar),
    crate::setting::setting_row!("appearance.text_size", crate::prefs::AppearancePrefs => accessibility::text_size),
    crate::setting::setting_row!("appearance.high_contrast", crate::prefs::AppearancePrefs => accessibility::high_contrast),
    crate::setting::setting_row!("appearance.reduce_motion", crate::prefs::AppearancePrefs => accessibility::reduce_motion),
    crate::setting::setting_row!("a11y.on_screen_keyboard", crate::accessibility_settings::AccessibilitySettings => accessibility::on_screen_keyboard),
    crate::setting::setting_row!("a11y.large_cursor", crate::accessibility_settings::AccessibilitySettings => accessibility::large_cursor),
    crate::setting::setting_row!("a11y.cursor_size", crate::accessibility_settings::AccessibilitySettings => accessibility::cursor_size),
    crate::setting::setting_row!("a11y.magnifier", crate::accessibility_settings::AccessibilitySettings => accessibility::magnifier_zoom),
    crate::setting::setting_row!("a11y.colour_filter", crate::accessibility_settings::AccessibilitySettings => accessibility::colour_filter),
    crate::setting::setting_row!("a11y.sticky_keys", crate::accessibility_settings::AccessibilitySettings => accessibility::sticky_keys),
    crate::setting::setting_row!("a11y.slow_keys", crate::accessibility_settings::AccessibilitySettings => accessibility::slow_keys),
    crate::setting::setting_row!("a11y.bounce_keys", crate::accessibility_settings::AccessibilitySettings => accessibility::bounce_keys),
    crate::setting::setting_row!("a11y.mouse_keys", crate::accessibility_settings::AccessibilitySettings => accessibility::mouse_keys),
    crate::setting::setting_row!("a11y.visual_alerts", crate::accessibility_settings::AccessibilitySettings => accessibility::visual_alerts),
    crate::setting::setting_row!("a11y.switch_control", crate::accessibility_settings::AccessibilitySettings => accessibility::switch_control),
    crate::setting::setting_row!("notifications.dnd", crate::prefs::NotificationPrefs => notifications::dnd),
    crate::setting::setting_row!("notifications.banners", crate::prefs::NotificationPrefs => notifications::banners),
    crate::setting::setting_row!("notifications.previews", crate::prefs::NotificationPrefs => notifications::previews),
    crate::setting::setting_row!("notifications.lock_screen", crate::prefs::NotificationPrefs => notifications::lock_screen),
    crate::setting::setting_row!("notifications.sounds", crate::prefs::NotificationPrefs => notifications::play_sounds),
    crate::setting::setting_row!("notifications.badges", crate::prefs::NotificationPrefs => notifications::badges),
    crate::setting::setting_row!("notifications.quiet_hours", crate::prefs::NotificationPrefs => notifications::quiet_hours),
    crate::setting::setting_row!("spelling.check", ui::text_assistance::TextAssistancePrefs => mouse_keyboard::check_spelling),
    crate::setting::setting_row!("spelling.autocorrect", ui::text_assistance::TextAssistancePrefs => mouse_keyboard::auto_correct),
    crate::setting::setting_row!("power.auto_suspend", crate::prefs::PowerPrefs => power::auto_suspend),
    crate::setting::setting_row!("power.lock_on_sleep", crate::prefs::PowerPrefs => power::lock_on_sleep),
    crate::setting::setting_row!("display.night_light", crate::prefs::NightLightPrefs => display::night_light),
    crate::setting::setting_row!("shortcuts.vim", keyboard_shortcuts::DesktopControls => keyboard_shortcuts::vim_mode),
    crate::setting::setting_view_switch!("network.wifi" => network::values::wifi, network::values::wifi_switch),
    crate::setting::setting_view_switch!("network.wired" => network::values::wired, network::values::wired_switch),
    crate::setting::setting_view_switch!("network.airplane" => network::values::airplane, network::values::airplane_switch),
    crate::setting::setting_view_switch!("network.hotspot" => network::values::hotspot, network::values::hotspot_switch),
    crate::setting::setting_view_switch!("network.mobile" => network::mobile::mobile_data, network::mobile::mobile_data_switch),
    crate::setting::setting_view_switch!("bluetooth.enabled" => bluetooth::enabled, bluetooth::enabled_switch),
    crate::setting::setting_value!("display.arrangement" => display::values::arrangement),
    crate::setting::setting_value!("display.brightness" => display::values::brightness),
    crate::setting::setting_bound_switch!("display.auto_brightness" => display::values::auto_brightness, crate::prefs::DisplayPrefs => display::automatic_brightness),
    crate::setting::setting_value!("display.night_light_warmth" => known display::values::night_light_warmth),
    crate::setting::setting_value!("display.scale" => display::values::scale),
    crate::setting::setting_value!("display.resolution" => display::values::resolution),
    crate::setting::setting_value!("display.refresh_rate" => display::values::refresh_rate),
    crate::setting::setting_value!("display.rotation" => display::values::rotation),
    crate::setting::setting_value!("display.hdr" => display::values::hdr),
    crate::setting::setting_value!("display.vrr" => display::values::vrr),
    crate::setting::setting_value!("display.mirroring" => display::values::mirroring),
    crate::setting::setting_value!("display.colour_profile" => display::values::colour_profile),
    crate::setting::setting_value!("sound.volume" => audio::values::volume),
    crate::setting::setting_value!("sound.output_device" => audio::values::output_device),
    crate::setting::setting_value!("sound.input_device" => audio::values::input_device),
    crate::setting::setting_value!("sound.input_level" => audio::values::input_level),
    crate::setting::setting_view_switch!("sound.mute" => audio::values::mute, audio::mute),
    crate::setting::setting_value!("sound.balance" => known audio::values::balance),
    crate::setting::setting_bound_switch!("sound.mono" => audio::values::mono, crate::prefs::AudioPrefs => audio::mono),
    crate::setting::setting_bound_switch!("sound.echo" => audio::values::echo, crate::prefs::AudioPrefs => audio::echo_cancellation),
    crate::setting::setting_bound_switch!("sound.system_sounds" => audio::values::system_sounds, crate::prefs::AudioPrefs => audio::system_sounds),
    crate::setting::setting_value!("keyboard.layout" => mouse_keyboard::values::layout),
    crate::setting::setting_value!("keyboard.key_repeat" => known mouse_keyboard::values::key_repeat),
    crate::setting::setting_value!("keyboard.compose" => known mouse_keyboard::values::compose),
    crate::setting::setting_value!("keyboard.caps_lock" => known mouse_keyboard::values::caps_lock),
    crate::setting::setting_row!("pointing.touchpad", crate::prefs::InputDevicePrefs => mouse_keyboard::touchpad),
    crate::setting::setting_value!("pointing.touchpad_speed" => known mouse_keyboard::values::touchpad_speed),
    crate::setting::setting_row!("pointing.tap_to_click", crate::prefs::InputPrefs => mouse_keyboard::tap_to_click),
    crate::setting::setting_value!("pointing.natural_scrolling" => known mouse_keyboard::values::natural_scrolling),
    crate::setting::setting_row!("pointing.disable_while_typing", crate::prefs::InputPrefs => mouse_keyboard::disable_while_typing),
    crate::setting::setting_value!("pointing.secondary_click" => mouse_keyboard::values::secondary_click),
    crate::setting::setting_value!("pointing.scroll_method" => mouse_keyboard::values::scroll_method),
    crate::setting::setting_row!("pointing.gestures", crate::prefs::InputPrefs => mouse_keyboard::gestures),
    crate::setting::setting_row!("pointing.disable_with_mouse", crate::prefs::InputDevicePrefs => mouse_keyboard::disable_with_mouse),
    crate::setting::setting_value!("pointing.mouse_speed" => known mouse_keyboard::values::mouse_speed),
    crate::setting::setting_value!("pointing.acceleration" => known mouse_keyboard::values::acceleration),
    crate::setting::setting_row!("pointing.left_handed", crate::prefs::InputPrefs => mouse_keyboard::left_handed),
    crate::setting::setting_value!("pointing.scroll_speed" => known mouse_keyboard::values::scroll_speed),
    crate::setting::setting_row!("pointing.middle_click", crate::prefs::InputPrefs => mouse_keyboard::middle_click),
    crate::setting::setting_value!("spelling.language" => known mouse_keyboard::values::spelling_language),
    crate::setting::setting_value!("power.profile" => power::values::profile),
    crate::setting::setting_value!("power.charge_limit" => power::values::charge_limit),
    crate::setting::setting_value!("power.suspend_after" => known power::values::suspend_after),
    crate::setting::setting_value!("power.blank" => known power::values::blank),
    crate::setting::setting_value!("power.lock_after" => known power::values::lock_after),
    crate::setting::setting_value!("power.lid" => known power::values::lid),
    crate::setting::setting_value!("power.power_key" => known power::values::power_key),
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
    crate::setting::setting_view_switch!("time.automatic" => time_date::values::automatic, time_date::values::automatic_switch),
    crate::setting::setting_value!("time.zone" => time_date::values::zone),
    crate::setting::setting_value!("storage.cache" => storage::values::cache),
    crate::setting::setting_value!("storage.clean" => storage::values::clean),
    crate::setting::setting_view_switch!("updates.automatic" => updates::values::automatic, updates::values::automatic_switch),
    crate::setting::setting_view_switch!("updates.metered" => updates::values::metered, updates::values::metered_switch),
];

pub const PART: crate::area::Part = crate::area::Part {
    areas: AREAS,
    families: FAMILIES,
    live_areas: LIVE_AREAS,
    settings: SETTINGS,
};
