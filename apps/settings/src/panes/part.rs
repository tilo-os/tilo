//! The panes people open often' part of Settings: the areas, panes, effect
//! families, live areas and settings sections they bring ([`crate::area::Part`]).

use crate::categories::Category;
use crate::effects::AreaVocabulary as _;
use crate::pane_model::model_pane;

use crate::panes::{
    accessibility, appearance, audio, bluetooth, disk_unlock, display, keyboard_shortcuts,
    mouse_keyboard, network, notifications, permissions, personal_apps, power,
};

/// Every area the window holds, in the order they are made: an area made
/// later may use one made before (Users and Storage act through Device
/// mesh's host client).
pub(crate) const AREAS: &[(u16, crate::area::MakeArea)] = &[
    (10, |context, _| {
        Box::new(accessibility::Software::for_host(context.cx))
    }),
    (20, |context, _| {
        Box::new(notifications::NotificationsArea::new(context.cx))
    }),
    (30, |context, _| {
        Box::new(personal_apps::DefaultAppsArea::new(context.cx))
    }),
    (50, |context, _| {
        Box::new(mouse_keyboard::InputArea::new(context.cx))
    }),
    (60, |context, _| Box::new(power::PowerArea::new(context.cx))),
    (100, |context, _| {
        Box::new(permissions::PermissionsArea::new(context.cx))
    }),
    (120, |context, _| {
        Box::new(bluetooth::BluetoothArea::new(context.cx))
    }),
    (180, |context, _| {
        Box::new(audio::AudioArea::new(context.cx))
    }),
    (200, |context, _| {
        Box::new(display::DisplayArea::new(context.cx))
    }),
    (210, |context, _| {
        Box::new(network::NetworkArea::new(context.cx))
    }),
];

/// Every area's vocabulary, in the order the union declares them.
pub(crate) const FAMILIES: &[(u16, &crate::effects::EffectFamily)] = &[
    (20, appearance::AppearanceEffect::FAMILY),
    (30, accessibility::AccessibilityEffect::FAMILY),
    (40, personal_apps::DefaultAppsEffect::FAMILY),
    (60, keyboard_shortcuts::ShortcutEffect::FAMILY),
    (70, mouse_keyboard::InputEffect::FAMILY),
    (80, power::PowerEffect::FAMILY),
    (140, permissions::PermissionsEffect::FAMILY),
    (170, bluetooth::BluetoothEffect::FAMILY),
    (220, audio::AudioEffect::FAMILY),
    (240, display::DisplayEffect::FAMILY),
    (250, network::NetworkEffect::FAMILY),
];

/// Every area with live rows, in the order their rows are published.
pub(crate) const LIVE_AREAS: &[(u16, crate::effects::LiveArea)] = &[
    (
        10,
        crate::effects::LiveArea {
            name: "settings.rows.notifications",
            pane: Category::Notifications,
            is_open: |app| {
                notifications::area(app).is_some_and(notifications::NotificationsArea::is_open)
            },
            rows: |app| {
                notifications::area(app)
                    .map(notifications::NotificationsArea::rows)
                    .unwrap_or_default()
            },
            background: Some(crate::effects::BackgroundRead {
                start: |app| {
                    notifications::area(app)
                        .is_some_and(notifications::NotificationsArea::start_read)
                },
                finished: |app| {
                    notifications::area(app)
                        .is_none_or(notifications::NotificationsArea::read_finished)
                },
            }),
        },
    ),
    (
        20,
        crate::effects::LiveArea {
            name: "settings.rows.default_apps",
            pane: Category::DefaultApps,
            is_open: |app| {
                personal_apps::area(app).is_some_and(personal_apps::DefaultAppsArea::is_open)
            },
            rows: |app| {
                personal_apps::area(app)
                    .map(personal_apps::DefaultAppsArea::rows)
                    .unwrap_or_default()
            },
            background: Some(crate::effects::BackgroundRead {
                start: |app| {
                    personal_apps::area(app).is_some_and(personal_apps::DefaultAppsArea::start_read)
                },
                finished: |app| {
                    personal_apps::area(app)
                        .is_none_or(personal_apps::DefaultAppsArea::read_finished)
                },
            }),
        },
    ),
    // Input reads a small snapshot file the compositor keeps, on a timer of
    // its own; it has nothing slow to put off.
    (
        50,
        crate::effects::LiveArea {
            name: "settings.rows.input",
            pane: Category::Keyboard,
            is_open: |_| true,
            rows: |app| {
                mouse_keyboard::area(app)
                    .map(mouse_keyboard::InputArea::rows)
                    .unwrap_or_default()
            },
            background: None,
        },
    ),
    (
        90,
        crate::effects::LiveArea {
            name: "settings.rows.bluetooth",
            pane: Category::Bluetooth,
            is_open: |app| bluetooth::area(app).is_some_and(bluetooth::BluetoothArea::is_open),
            rows: |app| {
                bluetooth::area(app)
                    .map(bluetooth::BluetoothArea::rows)
                    .unwrap_or_default()
            },
            background: Some(crate::effects::BackgroundRead {
                start: |app| bluetooth::area(app).is_some_and(bluetooth::BluetoothArea::start_read),
                finished: |app| {
                    bluetooth::area(app).is_none_or(bluetooth::BluetoothArea::read_finished)
                },
            }),
        },
    ),
    (
        140,
        crate::effects::LiveArea {
            name: "settings.rows.audio",
            pane: Category::Sound,
            is_open: |app| audio::area(app).is_some_and(audio::AudioArea::is_open),
            rows: |app| {
                audio::area(app)
                    .map(audio::AudioArea::rows)
                    .unwrap_or_default()
            },
            background: Some(crate::effects::BackgroundRead {
                start: |app| audio::area(app).is_some_and(audio::AudioArea::start_read),
                finished: |app| audio::area(app).is_none_or(audio::AudioArea::read_finished),
            }),
        },
    ),
    // Displays probe brightness on threads of their own; the rest is a few
    // small files.
    (
        180,
        crate::effects::LiveArea {
            name: "settings.rows.displays",
            pane: Category::Displays,
            is_open: |app| display::area(app).is_some_and(display::DisplayArea::is_open),
            rows: |app| {
                display::area(app)
                    .map(display::DisplayArea::rows)
                    .unwrap_or_default()
            },
            background: None,
        },
    ),
    // Networks are read on a thread of their own.
    (
        200,
        crate::effects::LiveArea {
            name: "settings.rows.network",
            pane: Category::Network,
            is_open: |app| network::area(app).is_some_and(network::NetworkArea::is_open),
            rows: |app| {
                network::area(app)
                    .map(network::NetworkArea::rows)
                    .unwrap_or_default()
            },
            background: None,
        },
    ),
];

/// The sections of the person's settings the panes own.
pub(crate) const SECTIONS: &[(u16, crate::area::SectionEntry)] = &[
    (
        10,
        crate::area::section_entry!(keyboard_shortcuts::DesktopControls),
    ),
    (30, crate::area::section_entry!(personal_apps::DefaultApps)),
    (
        40,
        crate::area::section_entry!(display::shared_displays::SharedDisplayPrefs),
    ),
    (
        50,
        crate::area::section_entry!(display::shared_displays::AutomaticDisplayPrefs),
    ),
    (
        60,
        crate::area::section_entry!(mouse_keyboard::keyboard_editor::OskSourceSettings),
    ),
];

/// Every pane, by the categories it shows.
pub(crate) const PANES: &[crate::area::PaneEntry] = &[
    crate::area::PaneEntry {
        categories: &[Category::Network],
        build: |app, _| model_pane(network::area(app).map(network::NetworkArea::model)),
    },
    crate::area::PaneEntry {
        categories: &[Category::Bluetooth],
        build: |app, _| model_pane(bluetooth::area(app).map(bluetooth::BluetoothArea::model)),
    },
    crate::area::PaneEntry {
        categories: &[Category::Sound],
        build: |app, _| model_pane(audio::area(app).map(audio::AudioArea::model)),
    },
    crate::area::PaneEntry {
        categories: &[Category::Displays],
        build: |app, _| model_pane(display::area(app).map(display::DisplayArea::model)),
    },
    crate::area::PaneEntry {
        categories: &[Category::Keyboard, Category::Pointing, Category::Spelling],
        build: mouse_keyboard::build,
    },
    crate::area::PaneEntry {
        categories: &[Category::KeyboardShortcuts],
        build: |app, _| Box::new(keyboard_shortcuts::ShortcutPane::new(app.host().clone())),
    },
    crate::area::PaneEntry {
        categories: &[Category::Power],
        build: |app, _| {
            power::area(app).map_or_else(
                || crate::widgets::page(Vec::new()),
                |area| {
                    Box::new(power::PowerPane::new(app.host().clone(), area))
                        as Box<dyn ui::widget::Widget>
                },
            )
        },
    },
    crate::area::PaneEntry {
        categories: &[Category::Appearance],
        build: |app, _| {
            Box::new(appearance::AppearancePane::new(
                app.host().clone(),
                app.wallpaper_status().clone(),
            ))
        },
    },
    crate::area::PaneEntry {
        categories: &[Category::Accessibility],
        build: |app, _| {
            Box::new(accessibility::AccessibilityPane::new(
                app.host().clone(),
                accessibility::software(app),
            ))
        },
    },
    crate::area::PaneEntry {
        categories: &[Category::Notifications],
        build: |app, _| {
            notifications::area(app).map_or_else(
                || crate::widgets::page(Vec::new()),
                |area| {
                    Box::new(notifications::NotificationsPane::new(
                        app.host().clone(),
                        area,
                    )) as Box<dyn ui::widget::Widget>
                },
            )
        },
    },
    crate::area::PaneEntry {
        categories: &[Category::DefaultApps],
        build: |app, _| {
            personal_apps::area(app).map_or_else(
                || crate::widgets::page(Vec::new()),
                |area| {
                    Box::new(personal_apps::PersonalAppsPane::new(
                        app.host().clone(),
                        area,
                    )) as Box<dyn ui::widget::Widget>
                },
            )
        },
    },
    crate::area::PaneEntry {
        categories: &[Category::Permissions],
        build: |app, _| {
            permissions::area(app).map_or_else(
                || crate::widgets::page(Vec::new()),
                |area| {
                    permissions::build_with_disk_unlock(
                        app.host(),
                        area,
                        model_pane(disk_unlock::area(app).map(disk_unlock::DiskUnlockArea::model)),
                    )
                },
            )
        },
    },
];

/// The part.
pub const PART: crate::area::Part = crate::area::Part {
    areas: AREAS,
    panes: PANES,
    families: FAMILIES,
    live_areas: LIVE_AREAS,
    sections: SECTIONS,
};
